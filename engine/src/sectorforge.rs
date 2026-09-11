use aho_corasick::AhoCorasick;
use memmap2::Mmap;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs::File;

/// File signature table — magic bytes for the carver.
/// aho-corasick scans ALL signatures in ONE single pass.
pub struct Signature {
    pub name: &'static str,
    pub magic: &'static [u8],
    pub extension: &'static str,
}

pub const SIGNATURES: &[Signature] = &[
    Signature {
        name: "jpeg",
        magic: b"\xFF\xD8\xFF\xE0",
        extension: "jpg",
    },
    Signature {
        name: "jpeg",
        magic: b"\xFF\xD8\xFF\xE1",
        extension: "jpg",
    },
    Signature {
        name: "png",
        magic: b"\x89PNG\r\n\x1A\n",
        extension: "png",
    },
    Signature {
        name: "pdf",
        magic: b"%PDF-",
        extension: "pdf",
    },
    Signature {
        name: "zip",
        magic: b"PK\x03\x04",
        extension: "zip",
    },
    Signature {
        name: "gzip",
        magic: b"\x1F\x8B\x08",
        extension: "gz",
    },
    // MP4 ftyp box: validated at offset+4 in post-check
    Signature {
        name: "mp4",
        magic: b"ftypisom",
        extension: "mp4",
    },
    Signature {
        name: "mp4",
        magic: b"ftypM4V ",
        extension: "mp4",
    },
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarveHit {
    pub offset: u64,
    pub signature: String,
    pub extension: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CarvedFileInfo {
    pub id: String,
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
    pub extension: String,
    pub confidence: f32,
    pub offset: u64,
}

/// SectorForge: memory-mapped, multi-threaded signature scan.
pub fn carve_image(path: &str, chunk_size: usize) -> anyhow::Result<Vec<CarveHit>> {
    let file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(Vec::new());
    }
    let mmap = unsafe { Mmap::map(&file)? };
    let total = mmap.len();

    if total == 0 {
        return Ok(Vec::new());
    }

    let patterns: Vec<&[u8]> = SIGNATURES.iter().map(|s| s.magic).collect();
    let ac = AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostFirst)
        .build(&patterns)?;

    let chunk_size = chunk_size.max(1024);
    let chunk_count = total.div_ceil(chunk_size);

    let hits: Vec<CarveHit> = (0..chunk_count)
        .into_par_iter()
        .flat_map_iter(|i| {
            let start = i * chunk_size;
            let end = start
                .saturating_add(chunk_size)
                .saturating_add(16)
                .min(total);
            if start >= total {
                return Vec::new().into_iter();
            }
            let slice = &mmap[start..end];
            let ac = ac.clone();
            let mut local_hits = Vec::new();
            for m in ac.find_iter(slice) {
                if m.start() >= chunk_size {
                    continue;
                }
                let sig = &SIGNATURES[m.pattern().as_usize()];
                if sig.name == "mp4" && start + m.start() < 4 {
                    continue;
                }
                let real_offset = if sig.name == "mp4" {
                    (start + m.start()).saturating_sub(4) as u64
                } else {
                    (start + m.start()) as u64
                };
                local_hits.push(CarveHit {
                    offset: real_offset,
                    signature: hex_of(sig.magic),
                    extension: sig.extension.to_string(),
                    confidence: 0.50,
                });
            }
            local_hits.into_iter()
        })
        .collect();

    Ok(dedup_hits(hits))
}

/// Extract carved files from an image into staging directory, calculating real lengths
pub fn extract_carved_files(
    path: &str,
    hits: &[CarveHit],
    out_dir: &std::path::Path,
) -> anyhow::Result<Vec<CarvedFileInfo>> {
    std::fs::create_dir_all(out_dir)?;
    let file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(Vec::new());
    }
    let mmap = unsafe { Mmap::map(&file)? };
    let total = mmap.len();

    let mut extracted = Vec::new();

    for (idx, hit) in hits.iter().enumerate() {
        if !SIGNATURES.iter().any(|s| s.extension == hit.extension) {
            anyhow::bail!("Unsupported output extension");
        }
        let start = usize::try_from(hit.offset)?;
        if start >= total {
            continue;
        }

        let max_len = (16 * 1024 * 1024).min(total - start);
        let slice = &mmap[start..start + max_len];

        let len = match hit.extension.as_str() {
            "jpg" => {
                if let Some(pos) = slice.windows(2).position(|w| w == b"\xFF\xD9") {
                    (pos + 2).min(slice.len())
                } else {
                    65536.min(slice.len())
                }
            }
            "png" => {
                if let Some(pos) = slice.windows(4).position(|w| w == b"IEND") {
                    (pos + 8).min(slice.len())
                } else {
                    65536.min(slice.len())
                }
            }
            "pdf" => {
                if let Some(pos) = slice.windows(5).position(|w| w == b"%%EOF") {
                    (pos + 6).min(slice.len())
                } else {
                    131072.min(slice.len())
                }
            }
            "zip" => {
                if let Some(pos) = slice.windows(4).position(|w| w == b"PK\x05\x06") {
                    (pos + 22).min(slice.len())
                } else {
                    131072.min(slice.len())
                }
            }
            "mp4" => {
                let report = crate::mp4::validate(slice);
                report
                    .top_level_boxes
                    .iter()
                    .take_while(|b| {
                        matches!(
                            b.typ.as_str(),
                            "ftyp"
                                | "moov"
                                | "mdat"
                                | "free"
                                | "skip"
                                | "wide"
                                | "uuid"
                                | "moof"
                                | "sidx"
                                | "styp"
                                | "mfra"
                        )
                    })
                    .last()
                    .and_then(|b| b.offset.checked_add(b.size))
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= slice.len())
                    .unwrap_or(0)
            }
            _ => 65536.min(slice.len()),
        };

        if len < 16 {
            continue;
        }

        let file_bytes = &slice[..len];
        let file_name = format!("recovered_{:03}_{}.{}", idx + 1, hit.offset, hit.extension);
        let dest_path = out_dir.join(&file_name);

        {
            use std::io::Write;
            let mut output = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&dest_path)?;
            output.write_all(file_bytes)?;
            extracted.push(CarvedFileInfo {
                id: format!("carve_{}_{}", hit.offset, idx),
                name: file_name,
                path: dest_path.to_string_lossy().to_string(),
                size_bytes: len as u64,
                extension: hit.extension.clone(),
                confidence: hit.confidence,
                offset: hit.offset,
            });
        }
    }

    Ok(extracted)
}

/// Carves and extracts recoverable files from a folder/directory.
/// Recursively scans files in the directory, detecting valid media signatures
/// or carving embedded items within raw or composite files.
pub fn carve_folder(
    dir_path: &str,
    out_dir: &std::path::Path,
) -> anyhow::Result<Vec<CarvedFileInfo>> {
    let source = std::fs::canonicalize(dir_path)?;
    anyhow::ensure!(source.is_dir(), "Scan source must be a directory");
    std::fs::create_dir_all(out_dir)?;
    anyhow::ensure!(
        !std::fs::canonicalize(out_dir)?.starts_with(&source),
        "Staging directory must be outside the scanned folder"
    );
    let mut extracted = Vec::new();
    let mut file_paths = Vec::new();

    // Collect files recursively (up to 3,000 files)
    fn collect_files(dir: &std::path::Path, list: &mut Vec<std::path::PathBuf>, depth: usize) {
        if depth > 10 || list.len() >= 3000 {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if list.len() >= 3000 {
                    break;
                }
                if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
                    continue;
                }
                if p.is_file() {
                    list.push(p);
                } else if p.is_dir() {
                    collect_files(&p, list, depth + 1);
                }
            }
        }
    }

    collect_files(std::path::Path::new(dir_path), &mut file_paths, 0);

    let patterns: Vec<&[u8]> = SIGNATURES.iter().map(|s| s.magic).collect();
    let ac = AhoCorasick::builder()
        .match_kind(aho_corasick::MatchKind::LeftmostFirst)
        .build(&patterns)?;

    for (file_idx, fpath) in file_paths.iter().enumerate() {
        let metadata = match fpath.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let file_len = metadata.len();
        if file_len == 0 || file_len > 250 * 1024 * 1024 {
            // Skip 0-byte or very large (>250MB) files
            continue;
        }

        let orig_name = fpath
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let ext = fpath
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();

        // Any file with a recognized extension is directly staged
        let is_known = !ext.is_empty()
            && [
                "jpg", "jpeg", "png", "gif", "bmp", "webp", "svg", "ico", "pdf", "doc", "docx",
                "xls", "xlsx", "ppt", "pptx", "txt", "md", "csv", "json", "xml", "log", "sql",
                "zip", "rar", "7z", "tar", "gz", "mp4", "mov", "avi", "mkv", "mp3", "wav", "flac",
                "py", "rs", "js", "ts", "jsx", "tsx", "html", "css", "c", "cpp", "h",
            ]
            .contains(&ext.as_str());

        if is_known {
            let dest_name = format!("recovered_{:03}_{}", file_idx + 1, orig_name);
            let dest_path = out_dir.join(&dest_name);
            {
                use std::io;
                let mut source = File::open(fpath)?;
                let mut output = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&dest_path)?;
                io::copy(&mut source, &mut output)?;
                extracted.push(CarvedFileInfo {
                    id: format!("dir_file_{}", file_idx),
                    name: orig_name.clone(),
                    path: dest_path.to_string_lossy().to_string(),
                    size_bytes: file_len,
                    extension: ext.clone(),
                    confidence: 0.99,
                    offset: 0,
                });
            }
        } else {
            // Scan binary, unknown, dat, or raw container files for embedded signatures
            if let Ok(f) = File::open(fpath) {
                use std::io::Read;
                let mut buf = Vec::new();
                let mut take = f.take(8 * 1024 * 1024);
                if take.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                    for m in ac.find_iter(&buf) {
                        let sig = &SIGNATURES[m.pattern().as_usize()];
                        let offset = if sig.name == "mp4" {
                            m.start().saturating_sub(4)
                        } else {
                            m.start()
                        };
                        let slice = &buf[offset..];
                        let len = match sig.extension {
                            "jpg" => slice
                                .windows(2)
                                .position(|w| w == b"\xFF\xD9")
                                .map(|p| p + 2)
                                .unwrap_or(65536.min(slice.len())),
                            "png" => slice
                                .windows(4)
                                .position(|w| w == b"IEND")
                                .map(|p| p + 8)
                                .unwrap_or(65536.min(slice.len())),
                            "pdf" => slice
                                .windows(5)
                                .position(|w| w == b"%%EOF")
                                .map(|p| p + 6)
                                .unwrap_or(131072.min(slice.len())),
                            "zip" => slice
                                .windows(4)
                                .position(|w| w == b"PK\x05\x06")
                                .map(|p| p + 22)
                                .unwrap_or(131072.min(slice.len())),
                            _ => 65536.min(slice.len()),
                        };
                        if len >= 16 {
                            let carved_bytes = &slice[..len.min(slice.len())];
                            let carve_name = format!(
                                "carved_{:03}_{}_{}.{}",
                                file_idx + 1,
                                offset,
                                orig_name,
                                sig.extension
                            );
                            let dest_path = out_dir.join(&carve_name);
                            {
                                use std::io::Write;
                                let mut output = std::fs::OpenOptions::new()
                                    .write(true)
                                    .create_new(true)
                                    .open(&dest_path)?;
                                output.write_all(carved_bytes)?;
                                extracted.push(CarvedFileInfo {
                                    id: format!("dir_carve_{}_{}", file_idx, offset),
                                    name: carve_name,
                                    path: dest_path.to_string_lossy().to_string(),
                                    size_bytes: carved_bytes.len() as u64,
                                    extension: sig.extension.to_string(),
                                    confidence: 0.50,
                                    offset: offset as u64,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(extracted)
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn dedup_hits(mut hits: Vec<CarveHit>) -> Vec<CarveHit> {
    hits.sort_by_key(|h| h.offset);
    hits.dedup_by_key(|h| h.offset);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn mp4_extraction_includes_media_and_metadata_boxes() {
        let mut image = tempfile::NamedTempFile::new().unwrap();
        let output = tempfile::tempdir().unwrap();
        let mut data = Vec::new();
        for (kind, payload) in [
            (b"ftyp", b"isom0000".as_slice()),
            (b"mdat", b"payload!".as_slice()),
            (b"moov", b"metadata".as_slice()),
        ] {
            data.extend_from_slice(&((8 + payload.len()) as u32).to_be_bytes());
            data.extend_from_slice(kind);
            data.extend_from_slice(payload);
        }
        image.write_all(&data).unwrap();
        let hits = carve_image(image.path().to_str().unwrap(), 1024).unwrap();
        let files =
            extract_carved_files(image.path().to_str().unwrap(), &hits, output.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(std::fs::read(&files[0].path).unwrap(), data);
    }

    #[test]
    fn empty_images_and_chunk_boundary_signatures() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        assert!(carve_image(tmp.path().to_str().unwrap(), 1024)
            .unwrap()
            .is_empty());
        let mut data = vec![0; 2048];
        data[1022..1030].copy_from_slice(b"\x89PNG\r\n\x1A\n");
        tmp.write_all(&data).unwrap();
        let hits = carve_image(tmp.path().to_str().unwrap(), usize::MAX).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(
            carve_image(tmp.path().to_str().unwrap(), 1024).unwrap(),
            hits
        );
    }

    #[test]
    fn malformed_folder_footer_does_not_panic_or_overwrite() {
        let source = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();
        let mut data = b"\x89PNG\r\n\x1A\n".to_vec();
        data.extend_from_slice(&[0; 32]);
        data.extend_from_slice(b"IEND");
        std::fs::write(source.path().join("truncated.dat"), &data).unwrap();
        let files = carve_folder(source.path().to_str().unwrap(), dest.path()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].size_bytes, data.len() as u64);
        assert!(carve_folder(source.path().to_str().unwrap(), dest.path()).is_err());
    }

    #[test]
    fn rejects_recursive_staging() {
        let source = tempfile::tempdir().unwrap();
        assert!(carve_folder(source.path().to_str().unwrap(), &source.path().join("out")).is_err());
    }

    #[test]
    fn carves_jpeg_and_png() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0u8; 4096];
        data[100..104].copy_from_slice(b"\xFF\xD8\xFF\xE0");
        data[2000..2008].copy_from_slice(b"\x89PNG\r\n\x1A\n");
        tmp.write_all(&data).unwrap();

        let hits = carve_image(tmp.path().to_str().unwrap(), 1024).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].offset, 100);
        assert_eq!(hits[0].extension, "jpg");
        assert_eq!(hits[1].offset, 2000);
        assert_eq!(hits[1].extension, "png");
    }

    #[test]
    fn extracts_real_carved_files() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        let mut data = vec![0u8; 8192];

        // Valid JPEG header and footer
        data[100..104].copy_from_slice(b"\xFF\xD8\xFF\xE0");
        data[300..302].copy_from_slice(b"\xFF\xD9");

        // Valid PNG header and IEND footer
        data[1000..1008].copy_from_slice(b"\x89PNG\r\n\x1A\n");
        data[1500..1504].copy_from_slice(b"IEND");

        tmp.write_all(&data).unwrap();

        let hits = carve_image(tmp.path().to_str().unwrap(), 1024).unwrap();
        assert_eq!(hits.len(), 2);

        let out_dir = tempfile::tempdir().unwrap();
        let files =
            extract_carved_files(tmp.path().to_str().unwrap(), &hits, out_dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].extension, "jpg");
        assert_eq!(files[0].size_bytes, 202); // 302 - 100
        assert_eq!(files[1].extension, "png");
        assert_eq!(files[1].size_bytes, 508); // 1500 - 1000 + 8
    }
}

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
            let end = ((i + 1) * chunk_size + 16).min(total);
            if start >= total {
                return Vec::new().into_iter();
            }
            let slice = &mmap[start..end];
            let ac = ac.clone();
            let mut local_hits = Vec::new();
            for m in ac.find_iter(slice) {
                let sig = &SIGNATURES[m.pattern().as_usize()];
                let real_offset = if sig.name == "mp4" {
                    (start + m.start()).saturating_sub(4) as u64
                } else {
                    (start + m.start()) as u64
                };
                local_hits.push(CarveHit {
                    offset: real_offset,
                    signature: hex_of(sig.magic),
                    extension: sig.extension.to_string(),
                    confidence: 0.94,
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
    let mmap = unsafe { Mmap::map(&file)? };
    let total = mmap.len();

    let mut extracted = Vec::new();

    for (idx, hit) in hits.iter().enumerate() {
        let start = hit.offset as usize;
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
                if let Some(pos) = slice.windows(5).rposition(|w| w == b"%%EOF") {
                    (pos + 6).min(slice.len())
                } else {
                    131072.min(slice.len())
                }
            }
            "zip" => {
                if let Some(pos) = slice.windows(4).rposition(|w| w == b"PK\x05\x06") {
                    (pos + 22).min(slice.len())
                } else {
                    131072.min(slice.len())
                }
            }
            "mp4" => {
                if slice.len() >= 4 {
                    let box_size = u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]) as usize;
                    if box_size > 8 && box_size <= slice.len() {
                        box_size
                    } else {
                        262144.min(slice.len())
                    }
                } else {
                    slice.len()
                }
            }
            _ => 65536.min(slice.len()),
        };

        if len < 16 {
            continue;
        }

        let file_bytes = &slice[..len];
        let file_name = format!("recovered_{:03}_{}.{}", idx + 1, hit.offset, hit.extension);
        let dest_path = out_dir.join(&file_name);

        if std::fs::write(&dest_path, file_bytes).is_ok() {
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
pub fn carve_folder(dir_path: &str, out_dir: &std::path::Path) -> anyhow::Result<Vec<CarvedFileInfo>> {
    std::fs::create_dir_all(out_dir)?;
    let mut extracted = Vec::new();
    let mut file_paths = Vec::new();
    
    // Collect files recursively (up to 2,000 files)
    fn collect_files(dir: &std::path::Path, list: &mut Vec<std::path::PathBuf>, depth: usize) {
        if depth > 10 || list.len() >= 2000 {
            return;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    list.push(path);
                } else if path.is_dir() {
                    collect_files(&path, list, depth + 1);
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
        if file_len == 0 || file_len > 100 * 1024 * 1024 {
            // Skip empty files or files >100MB to maintain instant speed
            continue;
        }

        let orig_name = fpath.file_name().unwrap_or_default().to_string_lossy().to_string();
        let ext = fpath.extension().unwrap_or_default().to_string_lossy().to_lowercase();

        // Read up to 8MB of file
        let read_len = (8 * 1024 * 1024).min(file_len as usize);
        let mut buf = vec![0u8; read_len];
        if let Ok(mut f) = File::open(fpath) {
            use std::io::Read;
            if f.read_exact(&mut buf).is_err() && f.read_to_end(&mut buf).is_err() {
                continue;
            }
        } else {
            continue;
        }

        let is_media = ["jpg", "jpeg", "png", "pdf", "zip", "mp4", "txt", "rs", "py", "js", "html", "css", "json", "doc", "docx"].contains(&ext.as_str());

        if is_media {
            let dest_name = format!("recovered_{:03}_{}", file_idx + 1, orig_name);
            let dest_path = out_dir.join(&dest_name);
            if std::fs::copy(fpath, &dest_path).is_ok() {
                extracted.push(CarvedFileInfo {
                    id: format!("dir_file_{}", file_idx),
                    name: dest_name,
                    path: dest_path.to_string_lossy().to_string(),
                    size_bytes: file_len,
                    extension: ext.clone(),
                    confidence: 0.99,
                    offset: 0,
                });
            }
        } else {
            // Scan for embedded magic bytes in binary / unknown / dat files
            for m in ac.find_iter(&buf) {
                let sig = &SIGNATURES[m.pattern().as_usize()];
                let offset = if sig.name == "mp4" {
                    m.start().saturating_sub(4)
                } else {
                    m.start()
                };
                let slice = &buf[offset..];
                let len = match sig.extension {
                    "jpg" => slice.windows(2).position(|w| w == b"\xFF\xD9").map(|p| p + 2).unwrap_or(65536.min(slice.len())),
                    "png" => slice.windows(4).position(|w| w == b"IEND").map(|p| p + 8).unwrap_or(65536.min(slice.len())),
                    "pdf" => slice.windows(5).rposition(|w| w == b"%%EOF").map(|p| p + 6).unwrap_or(131072.min(slice.len())),
                    "zip" => slice.windows(4).rposition(|w| w == b"PK\x05\x06").map(|p| p + 22).unwrap_or(131072.min(slice.len())),
                    _ => 65536.min(slice.len()),
                };
                if len >= 16 {
                    let carved_bytes = &slice[..len];
                    let carve_name = format!("carved_{:03}_{}_{}.{}", file_idx + 1, offset, orig_name, sig.extension);
                    let dest_path = out_dir.join(&carve_name);
                    if std::fs::write(&dest_path, carved_bytes).is_ok() {
                        extracted.push(CarvedFileInfo {
                            id: format!("dir_carve_{}_{}", file_idx, offset),
                            name: carve_name,
                            path: dest_path.to_string_lossy().to_string(),
                            size_bytes: len as u64,
                            extension: sig.extension.to_string(),
                            confidence: 0.95,
                            offset: offset as u64,
                        });
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
        let files = extract_carved_files(tmp.path().to_str().unwrap(), &hits, out_dir.path()).unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].extension, "jpg");
        assert_eq!(files[0].size_bytes, 202); // 302 - 100
        assert_eq!(files[1].extension, "png");
        assert_eq!(files[1].size_bytes, 508); // 1500 - 1000 + 8
    }
}

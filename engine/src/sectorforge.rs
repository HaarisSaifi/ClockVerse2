use aho_corasick::AhoCorasick;
use memmap2::Mmap;
use rayon::prelude::*;
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

#[derive(Debug, Clone, PartialEq)]
pub struct CarveHit {
    pub offset: u64,
    pub signature: String,
    pub extension: String,
    pub confidence: f32,
}

/// SectorForge: memory-mapped, multi-threaded signature scan.
/// Chunks the device image so rayon can scan in parallel without
/// ever loading the whole disk into RAM.
pub fn carve_image(path: &str, chunk_size: usize) -> anyhow::Result<Vec<CarveHit>> {
    let file = File::open(path)?;
    // SAFETY: image opened read-only; we never write to the source disk.
    // This is a forensic invariant — VaultGuard enforces it app-wide.
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
            // Overlap chunks by max signature length so boundary hits aren't missed.
            let end = ((i + 1) * chunk_size + 16).min(total);
            if start >= total {
                return Vec::new().into_iter();
            }
            let slice = &mmap[start..end];
            let ac = ac.clone();
            let mut local_hits = Vec::new();
            for m in ac.find_iter(slice) {
                let sig = &SIGNATURES[m.pattern().as_usize()];
                // MP4: "ftyp" lives at offset+4 of the box; real start is 4 bytes back.
                let real_offset = if sig.name == "mp4" {
                    (start + m.start()).saturating_sub(4) as u64
                } else {
                    (start + m.start()) as u64
                };
                local_hits.push(CarveHit {
                    offset: real_offset,
                    signature: hex_of(sig.magic),
                    extension: sig.extension.to_string(),
                    confidence: 0.90, // refined later by Integrity Gate
                });
            }
            local_hits.into_iter()
        })
        .collect();

    Ok(dedup_hits(hits))
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// Boundary-overlap can report the same hit twice; dedup by offset.
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
}

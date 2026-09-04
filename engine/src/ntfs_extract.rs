//! Deleted file content extraction — MFT record se actual bytes.
//!
//! Forensic invariants:
//! - Image READ-ONLY. Source volume pe kabhi write nahi.
//! - Sparse runs (LCN -1) ko zeros se pad karte hain — NTFS ka standard
//!   behavior, recovery mein bhi same.

use crate::ntfs::{apply_fixup, u16le, u32le, MftRecord, NtfsGeometry, ATTR_DATA, ATTR_END};
use anyhow::Context;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

pub const SPARSE_LCN: i64 = -1;

#[derive(Debug, Clone, Copy)]
pub struct ExtractResult {
    pub bytes_recovered: u64,
    pub missing_clusters: u64, // read errors = overwrite evidence
    pub was_resident: bool,
}

/// MFT record se deleted file ka content nikaalo.
/// Strategy: resident data pehle (sabse reliable), phir non-resident data runs.
pub fn extract_file_content(
    image: &File,
    geo: &NtfsGeometry,
    record: &MftRecord,
    raw_record: &[u8],
) -> anyhow::Result<Vec<u8>> {
    let expected_size = record.file_names.first().map(|f| f.real_size).unwrap_or(0);

    // Case 1: Resident data — file MFT record ke andar hai, overwrite risk sabse kam
    if record.resident_data_len.is_some() {
        if let Some(data) = extract_resident_data(raw_record, geo.sector_size) {
            let mut out = data;
            out.truncate(expected_size as usize);
            return Ok(out);
        }
    }

    // Case 2: Non-resident — data runs se clusters padho
    if !record.data_runs.is_empty() {
        return extract_non_resident(image, geo, record, expected_size);
    }

    anyhow::bail!("no recoverable data (neither resident nor data runs)")
}

/// Resident $DATA attribute se bytes nikaalo (record ke andar).
fn extract_resident_data(raw: &[u8], sector_size: usize) -> Option<Vec<u8>> {
    let mut buf = raw.to_vec();
    if !apply_fixup(&mut buf, sector_size) {
        return None; // corrupt record — resident data pe bharosa nahi
    }
    let mut off = u16le(&buf, 20) as usize;
    loop {
        if off + 16 > buf.len() {
            break;
        }
        let attr_type = u32le(&buf, off);
        if attr_type == ATTR_END {
            break;
        }
        let attr_len = u32le(&buf, off + 4) as usize;
        if attr_len == 0 || off + attr_len > buf.len() {
            break;
        }
        let non_resident = buf[off + 8] != 0;
        let has_name = buf[off + 9] != 0;

        if attr_type == ATTR_DATA && !non_resident && !has_name {
            let vlen = u32le(&buf, off + 16) as usize;
            let voff = u16le(&buf, off + 20) as usize;
            if voff + vlen <= attr_len {
                return Some(buf[off + voff..off + voff + vlen].to_vec());
            }
        }
        off += attr_len;
    }
    None
}

/// Non-resident $DATA runs se clusters padho.
fn extract_non_resident(
    image: &File,
    geo: &NtfsGeometry,
    record: &MftRecord,
    expected_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let mut reader = std::io::BufReader::with_capacity(1 << 20, image);
    let mut out = Vec::with_capacity(expected_size.min(64 << 20) as usize); // cap 64MB
    let cluster = geo.cluster_size as usize;
    let mut buf = vec![0u8; cluster];

    for run in &record.data_runs {
        if out.len() as u64 >= expected_size {
            break;
        }
        if run.lcn == SPARSE_LCN {
            // Sparse run — zeros (NTFS standard)
            let sparse_bytes =
                (run.length * geo.cluster_size).min(expected_size - out.len() as u64);
            out.extend(std::iter::repeat_n(0u8, sparse_bytes as usize));
            continue;
        }
        let offset = (run.lcn as u64)
            .checked_mul(geo.cluster_size)
            .context("LCN overflow")?;
        reader.seek(SeekFrom::Start(offset))?;

        let mut remaining = (run.length * geo.cluster_size).min(expected_size - out.len() as u64);
        while remaining > 0 {
            let want = (remaining as usize).min(cluster);
            let _ = reader.read_exact(&mut buf[..want]);
            out.extend_from_slice(&buf[..want]);
            remaining -= want as u64;
        }
    }
    out.truncate(expected_size as usize);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ntfs::{parse_boot_sector, DataRun, FileNameAttr};

    fn make_geo() -> NtfsGeometry {
        let mut bs = vec![0u8; 512];
        bs[3..11].copy_from_slice(b"NTFS    ");
        bs[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        bs[0x0D] = 8;
        bs[0x30..0x38].copy_from_slice(&786_432u64.to_le_bytes());
        bs[0x40] = 0xF6;
        parse_boot_sector(&bs).unwrap()
    }

    #[test]
    fn resident_data_extraction() {
        let geo = make_geo();
        let content = b"hello resident world";
        let mut raw = vec![0u8; 1024];
        raw[0..4].copy_from_slice(b"FILE");
        raw[4..6].copy_from_slice(&48u16.to_le_bytes());
        raw[6..8].copy_from_slice(&3u16.to_le_bytes());
        raw[20..22].copy_from_slice(&56u16.to_le_bytes());
        raw[22..24].copy_from_slice(&0u16.to_le_bytes()); // deleted

        // Resident $DATA attribute at 56
        let vlen = content.len();
        let attr_len = 24 + vlen;
        raw[56..60].copy_from_slice(&ATTR_DATA.to_le_bytes());
        raw[60..64].copy_from_slice(&(attr_len as u32).to_le_bytes());
        raw[56 + 16..56 + 20].copy_from_slice(&(vlen as u32).to_le_bytes());
        raw[56 + 20..56 + 22].copy_from_slice(&24u16.to_le_bytes());
        raw[56 + 24..56 + 24 + vlen].copy_from_slice(content);

        // Fixup stamp
        let usn = 0xAAAAu16.to_le_bytes();
        raw[48..50].copy_from_slice(&usn);
        for i in 0..2 {
            let trailer = (i + 1) * 512 - 2;
            let saved = [raw[trailer], raw[trailer + 1]];
            raw[50 + i * 2..52 + i * 2].copy_from_slice(&saved);
            raw[trailer..trailer + 2].copy_from_slice(&usn);
        }

        let record = crate::ntfs::MftRecord {
            record_number: 0,
            in_use: false,
            is_directory: false,
            file_names: vec![FileNameAttr {
                parent_record: 5,
                name: "test.txt".into(),
                created_unix_us: 0,
                modified_unix_us: 0,
                real_size: content.len() as u64,
            }],
            data_runs: vec![],
            resident_data_len: Some(content.len() as u64),
            fixup_ok: true,
        };

        let data = extract_file_content(
            &std::fs::File::open("Cargo.toml").unwrap(),
            &geo,
            &record,
            &raw,
        )
        .unwrap();
        assert_eq!(data, content);
    }

    #[test]
    fn non_resident_extraction_with_sparse_run() {
        let geo = make_geo();
        let dir = tempfile::tempdir().unwrap();
        let img_path = dir.path().join("vol.img");
        // Volume image: 786432 clusters * 4096 = 3 GB, but sparse file
        let mut f = std::fs::File::create(&img_path).unwrap();
        use std::io::Write;
        // Write known data at LCN 100
        let cluster_data = b"RECOVERED-CLUSTER-DATA!";
        let offset = 100u64 * geo.cluster_size;
        f.seek(SeekFrom::Start(offset)).unwrap();
        f.write_all(cluster_data).unwrap();
        // Sparse region at LCN -1 (hole)
        drop(f);

        let img = std::fs::File::open(&img_path).unwrap();
        let record = MftRecord {
            record_number: 0,
            in_use: false,
            is_directory: false,
            file_names: vec![FileNameAttr {
                parent_record: 5,
                name: "big.dat".into(),
                created_unix_us: 0,
                modified_unix_us: 0,
                real_size: (cluster_data.len() + 4096) as u64,
            }],
            data_runs: vec![
                DataRun {
                    lcn: 100,
                    length: 1,
                }, // 1 cluster @ 100
                DataRun {
                    lcn: SPARSE_LCN,
                    length: 1,
                }, // 1 sparse cluster
            ],
            resident_data_len: None,
            fixup_ok: true,
        };

        let raw = vec![0u8; 1024];
        let data = extract_file_content(&img, &geo, &record, &raw).unwrap();
        assert_eq!(&data[..cluster_data.len()], cluster_data);
        // Sparse region = zeros
        assert!(data[cluster_data.len()..].iter().all(|&b| b == 0));
    }
}

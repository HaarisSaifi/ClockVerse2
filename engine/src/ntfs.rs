//! NTFS $MFT parsing — deleted file recovery from the Master File Table.
//!
//! Forensic invariants (app-wide, non-negotiable):
//! - Read-only access always. Source volume pe kabhi write nahi.
//! - Corrupt record skip hota hai, kabhi fatal nahi.
//! - Fixup validation mandatory: sector trailers USN se match nahi hue toh
//!   record partially overwritten hai — parse karo, lekin fixup_ok=false
//!   flag karo taaki confidence score gir sake.

use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};

pub const ATTR_FILE_NAME: u32 = 0x30;
pub const ATTR_DATA: u32 = 0x80;
pub const ATTR_END: u32 = 0xFFFF_FFFF;
const FILETIME_EPOCH_100NS: i128 = 116_444_736_000_000_000; // 1601 → 1970

#[derive(Debug, Clone)]
pub struct MftRecord {
    pub record_number: u64,
    pub in_use: bool, // false = DELETED — yahi forensic gold hai
    pub is_directory: bool,
    pub file_names: Vec<FileNameAttr>,
    pub data_runs: Vec<DataRun>,
    pub resident_data_len: Option<u64>, // chhote files MFT record ke ANDAR rehte hain
    pub fixup_ok: bool,
}

#[derive(Debug, Clone)]
pub struct FileNameAttr {
    pub parent_record: u64,
    pub name: String,
    pub created_unix_us: i64,
    pub modified_unix_us: i64,
    pub real_size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataRun {
    pub lcn: i64,    // absolute logical cluster number
    pub length: u64, // clusters
}

#[derive(Debug, Clone, Copy)]
pub struct NtfsGeometry {
    pub sector_size: usize,
    pub cluster_size: u64,
    pub mft_lcn: u64,
    pub record_size: usize,
}

#[inline]
pub(crate) fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
pub(crate) fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
pub(crate) fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

fn filetime_to_unix_micros(ft: u64) -> i64 {
    ((ft as i128 - FILETIME_EPOCH_100NS) / 10) as i64
}

/// NTFS boot sector → geometry. Volume image ka pehla sector.
pub fn parse_boot_sector(bs: &[u8]) -> Option<NtfsGeometry> {
    if bs.len() < 512 || &bs[3..11] != b"NTFS    " {
        return None;
    }
    let sector_size = u16le(bs, 0x0B) as usize;
    let spc = bs[0x0D] as u64;
    if sector_size == 0 || spc == 0 {
        return None;
    }
    let mft_lcn = u64le(bs, 0x30);
    let rec_clusters = bs[0x40] as i8;
    // Negative value = record size is 2^|v| bytes (0xF6 → 1024)
    let record_size = if rec_clusters < 0 {
        1usize << (-rec_clusters as u32)
    } else {
        rec_clusters as usize * sector_size * spc as usize
    };
    Some(NtfsGeometry {
        sector_size,
        cluster_size: sector_size as u64 * spc,
        mft_lcn,
        record_size,
    })
}

/// Update Sequence Array fixup — multi-sector records ki corruption guard.
/// Har sector ke last 2 bytes USN hone chahiye; originals USA mein saved hote hain.
/// Returns false = record partially overwritten (trust kam, parse phir bhi).
pub fn apply_fixup(record: &mut [u8], sector_size: usize) -> bool {
    if record.len() < 48 {
        return false;
    }
    let usa_off = u16le(record, 4) as usize;
    let usa_count = u16le(record, 6) as usize;
    if usa_count < 1 || usa_off + usa_count * 2 > record.len() {
        return false;
    }
    let usn = [record[usa_off], record[usa_off + 1]];
    for i in 0..(usa_count - 1) {
        let trailer = (i + 1) * sector_size - 2;
        if trailer + 2 > record.len() {
            return false;
        }
        if record[trailer] != usn[0] || record[trailer + 1] != usn[1] {
            return false; // red flag — kisi ne is sector pe overwrite kiya
        }
        let fix = usa_off + 2 + i * 2;
        record[trailer] = record[fix];
        record[trailer + 1] = record[fix + 1];
    }
    true
}

/// Non-resident $DATA data runs → absolute cluster map.
/// Deleted file ka content recover karne ki key yahi hai: runs bataate hain
/// file ke clusters disk pe KAHAN hain (agar overwrite nahi hue).
pub fn parse_data_runs(mut data: &[u8]) -> Vec<DataRun> {
    let mut runs = Vec::new();
    let mut lcn: i64 = 0;
    while let Some(&header) = data.first() {
        if header == 0 {
            break;
        }
        let len_bytes = (header & 0x0F) as usize;
        let off_bytes = (header >> 4) as usize;
        data = &data[1..];
        if data.len() < len_bytes + off_bytes {
            break;
        }
        let mut length: u64 = 0;
        for (i, byte) in data.iter().take(len_bytes).enumerate() {
            length |= (*byte as u64) << (8 * i);
        }
        // offset is SIGNED, relative to previous run — sign-extend manually
        let mut offset: i64 = 0;
        for i in 0..off_bytes {
            offset |= (data[len_bytes + i] as i64) << (8 * i);
        }
        if off_bytes > 0 && off_bytes < 8 {
            let sign_bit = 1i64 << (off_bytes * 8 - 1);
            if offset & sign_bit != 0 {
                offset |= -1i64 << (off_bytes * 8);
            }
        }
        lcn += offset;
        runs.push(DataRun { lcn, length });
        data = &data[len_bytes + off_bytes..];
    }
    runs
}

fn parse_file_name(v: &[u8]) -> Option<FileNameAttr> {
    if v.len() < 66 {
        return None;
    }
    let parent = u64le(v, 0) & 0x0000_FFFF_FFFF_FFFF; // low 48 bits = record number
    let created = filetime_to_unix_micros(u64le(v, 8));
    let modified = filetime_to_unix_micros(u64le(v, 16));
    let real_size = u64le(v, 48);
    let name_chars = v[64] as usize;
    if v.len() < 66 + name_chars * 2 {
        return None;
    }
    let name = String::from_utf16_lossy(
        &v[66..66 + name_chars * 2]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect::<Vec<_>>(),
    );
    Some(FileNameAttr {
        parent_record: parent,
        name,
        created_unix_us: created,
        modified_unix_us: modified,
        real_size,
    })
}

/// Single MFT record parse. None = signature nahi hai (blank/overwritten slot).
pub fn parse_record(raw: &[u8], record_number: u64, sector_size: usize) -> Option<MftRecord> {
    if raw.len() < 48 || &raw[0..4] != b"FILE" {
        return None;
    }
    let mut buf = raw.to_vec();
    let fixup_ok = apply_fixup(&mut buf, sector_size);

    let flags = u16le(&buf, 22);
    let mut rec = MftRecord {
        record_number,
        in_use: flags & 0x01 != 0,
        is_directory: flags & 0x02 != 0,
        file_names: Vec::new(),
        data_runs: Vec::new(),
        resident_data_len: None,
        fixup_ok,
    };

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
            break; // corrupt chain — stop, jo mila woh valid hai
        }
        let non_resident = buf[off + 8] != 0;
        let has_name = buf[off + 9] != 0;

        match (attr_type, non_resident) {
            (ATTR_FILE_NAME, false) => {
                let vlen = u32le(&buf, off + 16) as usize;
                let voff = u16le(&buf, off + 20) as usize;
                if voff + vlen <= attr_len {
                    if let Some(fna) = parse_file_name(&buf[off + voff..off + voff + vlen]) {
                        rec.file_names.push(fna);
                    }
                }
            }
            (ATTR_DATA, true) if !has_name => {
                // default stream only (named ADS streams skip — Phase 2.5)
                let runs_off = u16le(&buf, off + 32) as usize;
                if runs_off < attr_len {
                    rec.data_runs = parse_data_runs(&buf[off + runs_off..off + attr_len]);
                }
            }
            (ATTR_DATA, false) if !has_name => {
                rec.resident_data_len = Some(u32le(&buf, off + 16) as u64);
            }
            _ => {}
        }
        off += attr_len;
    }
    Some(rec)
}

/// Scan $MFT sequentially; har parsed record callback ko milta hai.
/// Returns: deleted-record count (recovery candidates).
///
/// NOTE: yeh $MFT ko contiguous maanta hai — 95%+ real volumes pe sahi.
/// Fragmented $MFT (uske apne data runs se map karna) Phase 2.5 mein.
pub fn scan_mft<F: FnMut(&MftRecord)>(
    image: &File,
    geo: &NtfsGeometry,
    max_records: u64,
    mut on_record: F,
) -> anyhow::Result<u64> {
    let mut reader = BufReader::with_capacity(1 << 20, image);
    reader.seek(SeekFrom::Start(geo.mft_lcn * geo.cluster_size))?;
    let mut buf = vec![0u8; geo.record_size];
    let mut deleted = 0u64;

    for rn in 0..max_records {
        if reader.read_exact(&mut buf).is_err() {
            break; // $MFT ka end
        }
        if let Some(rec) = parse_record(&buf, rn, geo.sector_size) {
            if !rec.in_use {
                deleted += 1;
            }
            on_record(&rec);
        }
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic 1024-byte deleted FILE record, fixup-stamped, with $FILE_NAME.
    fn make_deleted_record() -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&48u16.to_le_bytes()); // USA offset
        r[6..8].copy_from_slice(&3u16.to_le_bytes()); // USA count (2 sectors + USN)
        r[20..22].copy_from_slice(&56u16.to_le_bytes()); // first attr
        r[22..24].copy_from_slice(&0u16.to_le_bytes()); // flags: NOT in use = deleted

        // $FILE_NAME resident attribute at 56
        let name = "photo.jpg";
        let vlen = 66 + name.len() * 2;
        let attr_len = 24 + vlen; // 108, parser alignment-agnostic hai
        r[56..60].copy_from_slice(&ATTR_FILE_NAME.to_le_bytes());
        r[60..64].copy_from_slice(&(attr_len as u32).to_le_bytes());
        r[56 + 16..56 + 20].copy_from_slice(&(vlen as u32).to_le_bytes());
        r[56 + 20..56 + 22].copy_from_slice(&24u16.to_le_bytes()); // value offset

        let v = 56 + 24;
        r[v..v + 8].copy_from_slice(&5u64.to_le_bytes()); // parent = root
        r[v + 8..v + 16].copy_from_slice(&133_000_000_000_000_000u64.to_le_bytes());
        r[v + 48..v + 56].copy_from_slice(&12345u64.to_le_bytes()); // real_size
        r[v + 64] = name.len() as u8;
        for (i, ch) in name.encode_utf16().enumerate() {
            r[v + 66 + i * 2..v + 68 + i * 2].copy_from_slice(&ch.to_le_bytes());
        }
        // END marker
        let end = 56 + attr_len;
        r[end..end + 4].copy_from_slice(&ATTR_END.to_le_bytes());

        // Protective fixup stamp (inverse of apply_fixup)
        let usn = 0xAAAAu16.to_le_bytes();
        r[48..50].copy_from_slice(&usn);
        for i in 0..2 {
            let trailer = (i + 1) * 512 - 2;
            let saved = [r[trailer], r[trailer + 1]]; // save originals first
            r[50 + i * 2..52 + i * 2].copy_from_slice(&saved);
            r[trailer..trailer + 2].copy_from_slice(&usn); // stamp
        }
        r
    }

    #[test]
    fn parses_deleted_record_with_valid_fixup() {
        let rec = parse_record(&make_deleted_record(), 42, 512).unwrap();
        assert!(rec.fixup_ok);
        assert!(!rec.in_use); // deleted — recovery candidate
        assert!(!rec.is_directory);
        assert_eq!(rec.file_names[0].name, "photo.jpg");
        assert_eq!(rec.file_names[0].real_size, 12345);
        assert_eq!(rec.file_names[0].parent_record, 5);
    }

    #[test]
    fn corrupt_fixup_is_flagged_not_fatal() {
        let mut raw = make_deleted_record();
        raw[510] ^= 0xFF; // sector trailer tod do — overwrite simulation
        let rec = parse_record(&raw, 42, 512).unwrap();
        assert!(!rec.fixup_ok); // flagged — confidence gate isko neeche karega
    }

    #[test]
    fn data_runs_decode_signed_relative_offsets() {
        // run1: 16 clusters @ LCN 256 | run2: 8 clusters @ LCN 224 (offset -32)
        let bytes = [0x21u8, 0x10, 0x00, 0x01, 0x11, 0x08, 0xE0, 0x00];
        let runs = parse_data_runs(&bytes);
        assert_eq!(
            runs,
            vec![
                DataRun {
                    lcn: 256,
                    length: 16
                },
                DataRun {
                    lcn: 224,
                    length: 8
                },
            ]
        );
    }

    #[test]
    fn boot_sector_geometry() {
        let mut bs = vec![0u8; 512];
        bs[3..11].copy_from_slice(b"NTFS    ");
        bs[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        bs[0x0D] = 8; // 4 KB clusters
        bs[0x30..0x38].copy_from_slice(&786_432u64.to_le_bytes());
        bs[0x40] = 0xF6; // -10 → 1024-byte records
        let geo = parse_boot_sector(&bs).unwrap();
        assert_eq!(geo.sector_size, 512);
        assert_eq!(geo.cluster_size, 4096);
        assert_eq!(geo.record_size, 1024);
        assert_eq!(geo.mft_lcn, 786_432);
    }
}

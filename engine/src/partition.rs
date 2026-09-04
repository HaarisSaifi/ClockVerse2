//! Partition table parsing — MBR + GPT. Whole-disk image se NTFS volume ka
//! offset+size nikaalo, taaki boot sector sahi jagah se padha ja sake.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Partition {
    pub index: u32,
    pub first_lba: u64, // sector number
    pub sector_count: u64,
    pub partition_type: PartitionType,
    pub name: Option<String>, // GPT mein UTF-16 label hota hai
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionType {
    Ntfs,
    Fat32,
    Linux,
    Efi,
    Unknown(u8),
}

fn mbr_type(byte: u8) -> PartitionType {
    match byte {
        0x07 => PartitionType::Ntfs,
        0x0B | 0x0C | 0x0E => PartitionType::Fat32,
        0x83 => PartitionType::Linux,
        0xEF => PartitionType::Efi,
        other => PartitionType::Unknown(other),
    }
}

fn gpt_type(guid: &[u8; 16]) -> PartitionType {
    // Well-known GPT type GUIDs (first 4 bytes little-endian DWORD)
    let dword = u32::from_le_bytes([guid[0], guid[1], guid[2], guid[3]]);
    match dword {
        0xA2A0D0EB => PartitionType::Ntfs,  // Basic data (Windows)
        0xC12A7328 => PartitionType::Efi,   // EFI system
        0x0FC63DAF => PartitionType::Linux, // Linux filesystem
        0xEBD0A0A2 => PartitionType::Ntfs,  // Microsoft basic data (alt)
        _ => PartitionType::Unknown(0xFF),
    }
}

#[inline]
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn u64le(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// MBR partition table (LBA 0, offset 0x1BE, 4 entries).
pub fn parse_mbr(sector0: &[u8]) -> Vec<Partition> {
    if sector0.len() < 512 || &sector0[510..512] != b"\x55\xAA" {
        return vec![];
    }
    let mut out = Vec::new();
    for i in 0..4 {
        let off = 0x1BE + i * 16;
        let part_type = sector0[off + 4];
        if part_type == 0 {
            continue; // empty slot
        }
        let first_lba = u32le(sector0, off + 8) as u64;
        let count = u32le(sector0, off + 12) as u64;
        if count == 0 {
            continue;
        }
        out.push(Partition {
            index: i as u32 + 1,
            first_lba,
            sector_count: count,
            partition_type: mbr_type(part_type),
            name: None,
        });
    }
    out
}

/// GPT partition entries. `header` = LBA 1, `entries` = raw entry array.
pub fn parse_gpt(header: &[u8], entries: &[u8]) -> Vec<Partition> {
    if header.len() < 92 || &header[0..8] != b"EFI PART" {
        return vec![];
    }
    let entry_count = u32le(header, 80) as usize;
    let entry_size = u32le(header, 84) as usize;
    if entry_size < 128 {
        return vec![];
    }
    let mut out = Vec::new();
    for i in 0..entry_count.min(entries.len() / entry_size) {
        let off = i * entry_size;
        let type_guid: [u8; 16] = entries[off..off + 16].try_into().unwrap();
        if type_guid == [0u8; 16] {
            continue; // unused entry
        }
        let first_lba = u64le(entries, off + 32);
        let last_lba = u64le(entries, off + 40);
        if last_lba < first_lba {
            continue;
        }
        // GPT name: UTF-16LE at offset 56, 72 bytes
        let name_bytes = &entries[off + 56..off + 56 + 72];
        let name: String = name_bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .map(|c| char::from_u32(c as u32).unwrap_or('?'))
            .collect();
        out.push(Partition {
            index: i as u32 + 1,
            first_lba,
            sector_count: last_lba - first_lba + 1,
            partition_type: gpt_type(&type_guid),
            name: if name.is_empty() { None } else { Some(name) },
        });
    }
    out
}

/// Auto-detect: GPT ya MBR? Dono se pehla NTFS partition do.
pub fn find_ntfs_volume(sector0: &[u8], sector1: &[u8], gpt_entries: &[u8]) -> Option<Partition> {
    // GPT pehle try karo (modern disks)
    let gpt_parts = parse_gpt(sector1, gpt_entries);
    if let Some(p) = gpt_parts
        .iter()
        .find(|p| p.partition_type == PartitionType::Ntfs)
    {
        return Some(p.clone());
    }
    // Fallback: MBR
    let mbr_parts = parse_mbr(sector0);
    mbr_parts
        .into_iter()
        .find(|p| p.partition_type == PartitionType::Ntfs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mbr_single_ntfs() {
        let mut s = vec![0u8; 512];
        s[510..512].copy_from_slice(b"\x55\xAA");
        let off = 0x1BE;
        s[off + 4] = 0x07; // NTFS
        s[off + 8..off + 12].copy_from_slice(&2048u32.to_le_bytes()); // first LBA
        s[off + 12..off + 16].copy_from_slice(&1_000_000u32.to_le_bytes()); // count
        let parts = parse_mbr(&s);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].partition_type, PartitionType::Ntfs);
        assert_eq!(parts[0].first_lba, 2048);
        assert_eq!(parts[0].sector_count, 1_000_000);
    }

    #[test]
    fn gpt_basic_data_partition() {
        let mut hdr = vec![0u8; 512];
        hdr[0..8].copy_from_slice(b"EFI PART");
        hdr[80..84].copy_from_slice(&128u32.to_le_bytes()); // 128 entries
        hdr[84..88].copy_from_slice(&128u32.to_le_bytes()); // 128 bytes each

        let mut entries = vec![0u8; 128 * 128];
        // Entry 0: Microsoft basic data
        let guid = [
            0xEB, 0xD0, 0xA0, 0xA2, 0xB9, 0xE5, 0x44, 0x33, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26,
            0x99, 0xC7,
        ];
        entries[0..16].copy_from_slice(&guid);
        entries[32..40].copy_from_slice(&2048u64.to_le_bytes()); // first LBA
        entries[40..48].copy_from_slice(&999_999u64.to_le_bytes()); // last LBA
                                                                    // Name: "Data"
        let name = "Data";
        for (i, ch) in name.encode_utf16().enumerate() {
            entries[56 + i * 2..58 + i * 2].copy_from_slice(&ch.to_le_bytes());
        }

        let parts = parse_gpt(&hdr, &entries);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].partition_type, PartitionType::Ntfs);
        assert_eq!(parts[0].first_lba, 2048);
        assert_eq!(parts[0].sector_count, 999_999 - 2048 + 1);
        assert_eq!(parts[0].name.as_deref(), Some("Data"));
    }

    #[test]
    fn auto_detect_prefers_gpt() {
        let mut s0 = vec![0u8; 512];
        s0[510..512].copy_from_slice(b"\x55\xAA");
        // MBR has a FAT32 partition
        s0[0x1BE + 4] = 0x0B;
        s0[0x1BE + 8..0x1BE + 12].copy_from_slice(&2048u32.to_le_bytes());
        s0[0x1BE + 12..0x1BE + 16].copy_from_slice(&500_000u32.to_le_bytes());

        let mut s1 = vec![0u8; 512];
        s1[0..8].copy_from_slice(b"EFI PART");
        s1[80..84].copy_from_slice(&128u32.to_le_bytes());
        s1[84..88].copy_from_slice(&128u32.to_le_bytes());

        let mut entries = vec![0u8; 128 * 128];
        let guid = [
            0xEB, 0xD0, 0xA0, 0xA2, 0xB9, 0xE5, 0x44, 0x33, 0x87, 0xC0, 0x68, 0xB6, 0xB7, 0x26,
            0x99, 0xC7,
        ];
        entries[0..16].copy_from_slice(&guid);
        entries[32..40].copy_from_slice(&1_048_576u64.to_le_bytes());
        entries[40..48].copy_from_slice(&2_097_151u64.to_le_bytes());

        let found = find_ntfs_volume(&s0, &s1, &entries).unwrap();
        assert_eq!(found.partition_type, PartitionType::Ntfs);
        assert_eq!(found.first_lba, 1_048_576); // GPT wala, MBR wala nahi
    }
}

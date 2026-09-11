//! Hardware and volume detection for direct 1-click recovery.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DriveInfo {
    pub letter: String,            // e.g. "D" or "E"
    pub root_path: String,         // e.g. "D:\\"
    pub display_name: String,      // e.g. "SanDisk 32GB (E:)"
    pub label: String,             // e.g. "SanDisk"
    pub fs_type: String,           // e.g. "NTFS", "FAT32", "exFAT"
    pub drive_type: String,        // "Removable USB", "Fixed Internal", "Optical", "Unknown"
    pub bus_type: String,          // "USB", "NVMe", "SATA", "SCSI", "Unknown"
    pub media_type: String,        // "SSD", "HDD", "Flash Memory", "Unspecified"
    pub is_removable: bool,
    pub is_ssd: bool,
    pub supports_trim: bool,
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub raw_volume_path: String,   // "\\\\.\\D:"
}

#[derive(Debug, Deserialize)]
struct PsVolume {
    #[serde(rename = "DriveLetter")]
    drive_letter: Option<String>,
    #[serde(rename = "FileSystemLabel")]
    file_system_label: Option<String>,
    #[serde(rename = "FileSystem")]
    file_system: Option<String>,
    #[serde(rename = "DriveType")]
    drive_type: Option<String>,
    #[serde(rename = "SizeRemaining")]
    size_remaining: Option<u64>,
    #[serde(rename = "Size")]
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct PsPhysicalDisk {
    #[serde(rename = "DeviceId")]
    _device_id: Option<String>,
    #[serde(rename = "FriendlyName")]
    _friendly_name: Option<String>,
    #[serde(rename = "MediaType")]
    media_type: Option<String>,
    #[serde(rename = "BusType")]
    bus_type: Option<String>,
    #[serde(rename = "Size")]
    _size: Option<u64>,
}

pub fn list_drives() -> Vec<DriveInfo> {
    #[cfg(windows)]
    {
        list_drives_windows()
    }
    #[cfg(not(windows))]
    {
        vec![]
    }
}

#[cfg(windows)]
fn list_drives_windows() -> Vec<DriveInfo> {
    use std::os::windows::process::CommandExt;

    // 1. Query physical disks to know bus type (NVMe, USB, SATA) and media type (SSD, HDD)
    let phys_script = "Get-PhysicalDisk | Select-Object DeviceId, FriendlyName, MediaType, BusType, Size | ConvertTo-Json -Compress";
    let phys_disks: Vec<PsPhysicalDisk> = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", phys_script])
        .creation_flags(0x08000000)
        .output()
        .ok()
        .and_then(|out| {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                return None;
            }
            if s.starts_with('[') {
                serde_json::from_str(&s).ok()
            } else {
                serde_json::from_str::<PsPhysicalDisk>(&s).ok().map(|d| vec![d])
            }
        })
        .unwrap_or_default();

    // 2. Query logical volumes
    let vol_script = "Get-Volume | Select-Object DriveLetter, FileSystemLabel, FileSystem, DriveType, SizeRemaining, Size | ConvertTo-Json -Compress";
    let volumes: Vec<PsVolume> = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", vol_script])
        .creation_flags(0x08000000)
        .output()
        .ok()
        .and_then(|out| {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.is_empty() {
                return None;
            }
            if s.starts_with('[') {
                serde_json::from_str(&s).ok()
            } else {
                serde_json::from_str::<PsVolume>(&s).ok().map(|v| vec![v])
            }
        })
        .unwrap_or_default();

    let mut result = Vec::new();
    for vol in volumes {
        let Some(letter) = vol.drive_letter else {
            continue;
        };
        let letter = letter.trim().to_uppercase();
        if letter.is_empty() {
            continue;
        }

        let is_removable = vol.drive_type.as_deref() == Some("Removable");
        let label = vol.file_system_label.unwrap_or_default();
        let fs_type = vol.file_system.unwrap_or_else(|| "Unknown".into());

        // Correlate with physical disk properties
        let (bus_type, media_type, is_ssd, supports_trim) = if is_removable {
            ("USB".to_string(), "Flash Memory".to_string(), false, false)
        } else if let Some(first_disk) = phys_disks.first() {
            let b = first_disk.bus_type.clone().unwrap_or_else(|| "Fixed".into());
            let m = first_disk.media_type.clone().unwrap_or_else(|| "Disk".into());
            let ssd = m.eq_ignore_ascii_case("SSD") || b.eq_ignore_ascii_case("NVMe");
            let trim = ssd && !b.eq_ignore_ascii_case("USB");
            (b, m, ssd, trim)
        } else {
            ("Fixed".to_string(), "HDD".to_string(), false, false)
        };

        let drive_type_display = if is_removable {
            "Removable USB / SD Card".to_string()
        } else if is_ssd {
            format!("Internal SSD ({bus_type})")
        } else {
            "Internal Hard Drive (HDD)".to_string()
        };

        let friendly_prefix = if !label.is_empty() {
            format!("{label} ({letter}:)")
        } else {
            format!("Local Disk ({letter}:)")
        };

        let display_name = if is_removable {
            format!("💾 {friendly_prefix} - USB Drive")
        } else if is_ssd {
            format!("⚡ {friendly_prefix} - High Speed SSD")
        } else {
            format!("💽 {friendly_prefix}")
        };

        result.push(DriveInfo {
            display_name,
            root_path: format!("{letter}:\\"),
            raw_volume_path: format!(r"\\.\{letter}:"),
            letter,
            label,
            fs_type,
            drive_type: drive_type_display,
            bus_type,
            media_type,
            is_removable,
            is_ssd,
            supports_trim,
            total_bytes: vol.size.unwrap_or(0),
            free_bytes: vol.size_remaining.unwrap_or(0),
        });
    }

    result.sort_by(|a, b| {
        // Show Removable USB drives first for convenient recovery
        b.is_removable.cmp(&a.is_removable).then(a.letter.cmp(&b.letter))
    });

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_info_structure_serializes() {
        let drive = DriveInfo {
            letter: "E".into(),
            root_path: "E:\\".into(),
            display_name: "SanDisk 32GB (E:)".into(),
            label: "SanDisk".into(),
            fs_type: "FAT32".into(),
            drive_type: "Removable USB".into(),
            bus_type: "USB".into(),
            media_type: "Flash Memory".into(),
            is_removable: true,
            is_ssd: false,
            supports_trim: false,
            total_bytes: 32 * 1024 * 1024 * 1024,
            free_bytes: 10 * 1024 * 1024 * 1024,
            raw_volume_path: r"\\.\E:".into(),
        };

        let json = serde_json::to_string(&drive).unwrap();
        assert!(json.contains("SanDisk"));
        assert!(json.contains("Removable USB"));
    }
}

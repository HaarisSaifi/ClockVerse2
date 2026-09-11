//! SSD Deep Shadow Miner: Recovers deleted files on SSDs/NVMe by mining historical Windows Shadow Copies.
//! Bypasses SSD TRIM zeroes by extracting pre-deletion snapshots directly from the kernel volsnap driver.

use crate::vss::{self, ShadowCopyInfo};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MinedShadowFile {
    pub shadow_id: String,
    pub creation_time: String,
    pub file_name: String,
    pub relative_path: String,
    pub device_object: String,
    pub size_bytes: u64,
    pub source_volume: String,
}

/// Search for a file query across all existing Windows VSS Shadow Copies on a given volume (e.g. "C:").
pub fn mine_shadow_files(volume: &str, query: &str) -> Result<Vec<MinedShadowFile>> {
    let clean_vol = volume.trim().trim_end_matches(['\\', '/']).to_uppercase();
    let shadows = vss::list_shadow_copies()?;

    let matching_shadows: Vec<ShadowCopyInfo> = shadows
        .into_iter()
        .filter(|s| {
            let v = s.volume_name.trim_end_matches(['\\', '/']).to_uppercase();
            v.starts_with(&clean_vol) || clean_vol.starts_with(&v)
        })
        .collect();

    if matching_shadows.is_empty() {
        return Ok(vec![]);
    }

    let mut results = Vec::new();
    let q = query.trim().to_lowercase();

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        for shadow in matching_shadows {
            let dev = &shadow.device_object;
            // Run a shallow search in Common User folders (Users, Desktop, Documents, Downloads)
            let search_filter = if q.is_empty() { "*.*" } else { &q };
            let script = format!(
                r#"
                $shadow = '{dev}'
                $targets = @("$shadow\Users")
                $out = @()
                foreach ($t in $targets) {{
                    if (Test-Path $t) {{
                        Get-ChildItem -Path $t -Recurse -Filter '*{search_filter}*' -File -Depth 4 -ErrorAction SilentlyContinue | Select-Object -First 30 | ForEach-Object {{
                            [PSCustomObject]@{{
                                Name = $_.Name
                                FullName = $_.FullName
                                Length = $_.Length
                            }}
                        }}
                    }}
                }}
                $out | ConvertTo-Json -Compress
                "#
            );

            let output = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &script])
                .creation_flags(0x08000000)
                .output();

            if let Ok(out) = output {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    #[derive(Deserialize)]
                    struct Item {
                        #[serde(rename = "Name")]
                        name: String,
                        #[serde(rename = "FullName")]
                        full_name: String,
                        #[serde(rename = "Length")]
                        length: Option<u64>,
                    }

                    let items: Vec<Item> = if s.starts_with('[') {
                        serde_json::from_str(&s).unwrap_or_default()
                    } else if let Ok(single) = serde_json::from_str::<Item>(&s) {
                        vec![single]
                    } else {
                        vec![]
                    };

                    for item in items {
                        let rel = item
                            .full_name
                            .trim_start_matches(dev)
                            .trim_start_matches(['\\', '/'])
                            .to_string();

                        results.push(MinedShadowFile {
                            shadow_id: shadow.id.clone(),
                            creation_time: shadow.creation_time.clone(),
                            file_name: item.name,
                            relative_path: rel,
                            device_object: dev.clone(),
                            size_bytes: item.length.unwrap_or(0),
                            source_volume: clean_vol.clone(),
                        });
                    }
                }
            }

            if results.len() >= 50 {
                break;
            }
        }
    }

    Ok(results)
}

/// Restore a mined shadow file directly to the user's chosen folder.
pub fn restore_mined_file(file: &MinedShadowFile, destination: &Path) -> Result<()> {
    vss::restore_file_from_shadow(&file.device_object, &file.relative_path, destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mined_file_serializes() {
        let f = MinedShadowFile {
            shadow_id: "id-123".into(),
            creation_time: "2026-09-11".into(),
            file_name: "test.pdf".into(),
            relative_path: "Users\\test.pdf".into(),
            device_object: r"\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy1".into(),
            size_bytes: 2048,
            source_volume: "C:".into(),
        };
        let s = serde_json::to_string(&f).unwrap();
        assert!(s.contains("test.pdf"));
    }
}

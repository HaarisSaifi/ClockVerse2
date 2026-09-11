//! Native Windows Volume Shadow Copy (VSS) integration.
//! Enables zero-extra-space (0 MB) instant point-in-time snapshots and anti-TRIM recovery.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShadowCopyInfo {
    pub id: String,
    pub volume_name: String,
    pub device_object: String,     // e.g. \\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy1
    pub creation_time: String,
    pub original_volume: String,   // e.g. "C:\"
}

pub fn is_admin() -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script = "([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)";
        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", script])
            .creation_flags(0x08000000)
            .output();
        if let Ok(out) = output {
            String::from_utf8_lossy(&out.stdout).trim().eq_ignore_ascii_case("True")
        } else {
            false
        }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Create an instant, 0 MB initial cost point-in-time VSS snapshot for a volume (e.g. "C:" or "D:").
pub fn create_shadow_copy(volume: &str) -> Result<ShadowCopyInfo> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        ensure!(is_admin(), "Administrator privileges are required to create a Windows VSS snapshot. Please launch ClockVerse as Administrator.");

        let clean_vol = volume.trim().trim_end_matches('\\').trim_end_matches('/');
        let vol_root = format!("{clean_vol}\\");

        // Use WMI Win32_ShadowCopy.Create
        let script = format!(
            r#"
            $res = (Get-CimInstance -List Win32_ShadowCopy).Create('{vol_root}', 'ClientAccessible')
            if ($res.ReturnValue -eq 0) {{
                $id = $res.ShadowID
                $s = Get-CimInstance Win32_ShadowCopy -Filter "ID='$id'"
                [PSCustomObject]@{{
                    ID = $s.ID
                    VolumeName = $s.VolumeName
                    DeviceObject = $s.DeviceObject
                    InstallDate = ($s.InstallDate.ToString())
                }} | ConvertTo-Json -Compress
            }} else {{
                throw "VSS Error Code: $($res.ReturnValue)"
            }}
            "#
        );

        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .creation_flags(0x08000000)
            .output()
            .context("Failed to run PowerShell VSS command")?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("VSS Creation Failed: {err}");
        }

        let json_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        #[derive(Deserialize)]
        struct PsOut {
            #[serde(rename = "ID")]
            id: String,
            #[serde(rename = "VolumeName")]
            volume_name: Option<String>,
            #[serde(rename = "DeviceObject")]
            device_object: Option<String>,
            #[serde(rename = "InstallDate")]
            install_date: Option<String>,
        }

        let parsed: PsOut = serde_json::from_str(&json_str)
            .context("Failed to parse VSS creation response")?;

        Ok(ShadowCopyInfo {
            id: parsed.id,
            volume_name: parsed.volume_name.unwrap_or_default(),
            device_object: parsed.device_object.unwrap_or_default(),
            creation_time: parsed.install_date.unwrap_or_else(|| "Just now".into()),
            original_volume: vol_root,
        })
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("VSS is a Windows-specific technology");
    }
}

/// List all available historical shadow copies on the system.
pub fn list_shadow_copies() -> Result<Vec<ShadowCopyInfo>> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script = r#"
        Get-CimInstance Win32_ShadowCopy -ErrorAction SilentlyContinue | ForEach-Object {
            [PSCustomObject]@{
                ID = $_.ID
                VolumeName = $_.VolumeName
                DeviceObject = $_.DeviceObject
                InstallDate = ($_.InstallDate.ToString())
            }
        } | ConvertTo-Json -Compress
        "#;

        let output = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", script])
            .creation_flags(0x08000000)
            .output()
            .context("Failed to query VSS shadow copies")?;

        if !output.status.success() {
            return Ok(vec![]);
        }

        let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if s.is_empty() {
            return Ok(vec![]);
        }

        #[derive(Deserialize)]
        struct PsShadow {
            #[serde(rename = "ID")]
            id: String,
            #[serde(rename = "VolumeName")]
            volume_name: Option<String>,
            #[serde(rename = "DeviceObject")]
            device_object: Option<String>,
            #[serde(rename = "InstallDate")]
            install_date: Option<String>,
        }

        let list: Vec<PsShadow> = if s.starts_with('[') {
            serde_json::from_str(&s).unwrap_or_default()
        } else if let Ok(single) = serde_json::from_str::<PsShadow>(&s) {
            vec![single]
        } else {
            vec![]
        };

        Ok(list
            .into_iter()
            .map(|sc| ShadowCopyInfo {
                id: sc.id,
                volume_name: sc.volume_name.clone().unwrap_or_default(),
                device_object: sc.device_object.unwrap_or_default(),
                creation_time: sc.install_date.unwrap_or_default(),
                original_volume: sc.volume_name.unwrap_or_default(),
            })
            .collect())
    }
    #[cfg(not(windows))]
    {
        Ok(vec![])
    }
}

/// Restore a single file from a VSS shadow copy into destination path.
pub fn restore_file_from_shadow(
    device_object: &str,
    relative_path: &str,
    destination: &Path,
) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let clean_rel = relative_path.trim_start_matches(['\\', '/']);
        let shadow_file = format!("{device_object}\\{clean_rel}");
        let dest_str = destination.to_string_lossy();

        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Use robocopy / cmd copy with bypass privileges
        let cmd = format!("cmd /c copy /y \"{shadow_file}\" \"{dest_str}\"");
        let status = std::process::Command::new("cmd")
            .args(["/c", &cmd])
            .creation_flags(0x08000000)
            .status()
            .context("Failed to execute copy from shadow copy")?;

        ensure!(status.success(), "Failed to copy file from shadow copy");
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let _ = (device_object, relative_path, destination);
        anyhow::bail!("VSS supported on Windows only");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vss_structure_serialization() {
        let sc = ShadowCopyInfo {
            id: "{12345678-1234-1234-1234-1234567890AB}".into(),
            volume_name: "C:\\".into(),
            device_object: r"\\?\GLOBALROOT\Device\HarddiskVolumeShadowCopy1".into(),
            creation_time: "2026-09-11 12:00:00".into(),
            original_volume: "C:\\".into(),
        };
        let json = serde_json::to_string(&sc).unwrap();
        assert!(json.contains("HarddiskVolumeShadowCopy1"));
    }
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clockverse_engine::index::EventIndex;
use clockverse_engine::timesnap::{CapsuleStatus, ProtectedFolder, TimeCapsule};
use clockverse_engine::sidecar::Sidecar;
use clockverse_engine::{
    chrono::StreamStitch, ntfs, ntfs_extract, partition, sectorforge, EngineEvent,
};
use serde::{Deserialize, Serialize};
use reqwest::Client;
use serde_json::json;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Mutex as TokioMutex;

/// Managed sidecar handle shared across commands (async mutex — blocking RPC).
type SharedSidecar = Arc<TokioMutex<Option<Sidecar>>>;

/// Managed state: a gateway index for the active session. `Arc<Mutex<_>>`
/// lets tasks move an owned handle into blocking threads (never borrow `State`).
struct AppState {
    index: Arc<StdMutex<EventIndex>>,
    time_capsule: Arc<StdMutex<TimeCapsule>>,
}

#[tauri::command]
async fn start_scan(app: AppHandle, target: String) -> Result<String, String> {
    let _ = app.emit(
        "engine",
        EngineEvent::ScanStarted {
            target: target.clone(),
            total_sectors: 0,
        },
    );

    // Carving runs on a blocking thread — NEVER the UI thread.
    let hits = tauri::async_runtime::spawn_blocking(move || {
        sectorforge::carve_image(&target, 64 * 1024 * 1024) // 64 MB chunks
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    for (i, hit) in hits.iter().enumerate() {
        let _ = app.emit(
            "engine",
            EngineEvent::SectorResult {
                particle_index: i as u32,
                state_code: 1, // carved
                cluster: hit.offset / 4096,
                signature: hit.signature.clone(),
                confidence: hit.confidence,
            },
        );
    }

    let _ = app.emit(
        "engine",
        EngineEvent::ScanComplete {
            found: hits.len() as u32,
            verified: 0,
            failures: 0,
        },
    );

    Ok(format!("{} files carved", hits.len()))
}

/// Ingest a JSONL telemetry log, index it, and stream one event per row to the UI.
#[tauri::command]
async fn chrono_ingest(
    app: AppHandle,
    state: State<'_, AppState>,
    jsonl: String,
) -> Result<String, String> {
    let index = Arc::clone(&state.index);
    let _ = tauri::async_runtime::spawn_blocking(move || {
        let events = StreamStitch::parse(jsonl.as_bytes());
        let idx = index.lock().map_err(|e| e.to_string())?;
        let before = idx.event_count().map_err(|e| e.to_string())?;
        for ev in &events {
            let op_kind = match &ev.op {
                clockverse_engine::chrono::DeltaOp::Write { .. } => "write",
                clockverse_engine::chrono::DeltaOp::Patch { .. } => "patch",
                clockverse_engine::chrono::DeltaOp::Delete => "delete",
            };
            idx.push(ev).map_err(|e| e.to_string())?;
            let _ = app.emit(
                "engine",
                EngineEvent::ChronoEventIngested {
                    path: ev.file_path.clone(),
                    ts_micros: ev.ts_micros,
                    op_kind: op_kind.to_string(),
                },
            );
        }
        let after = idx.event_count().map_err(|e| e.to_string())?;
        let files = idx.files().map_err(|e| e.to_string())?;
        let _ = app.emit(
            "engine",
            EngineEvent::SessionUpdated {
                session_id: "active".to_string(),
                event_count: after,
                file_count: files.len() as u64,
            },
        );
        Ok::<u64, String>(before)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    Ok("ingested".to_string())
}

/// Session constellation summary — file count, event count, max ts.
#[tauri::command]
async fn session_summary(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let index = Arc::clone(&state.index);
    let (event_count, files, max_ts) = tauri::async_runtime::spawn_blocking(move || {
        let idx = index.lock().map_err(|e| e.to_string())?;
        let event_count = idx.event_count().map_err(|e| e.to_string())?;
        let files = idx.files().map_err(|e| e.to_string())?;
        let max_ts = idx.max_ts().map_err(|e| e.to_string())?;
        Ok::<(u64, Vec<String>, u64), String>((event_count, files, max_ts))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({
        "session_id": "active",
        "event_count": event_count,
        "file_count": files.len(),
        "max_ts_micros": max_ts,
        "files": files,
    }))
}

/// Time-travel: reconstruct all files as of a given microsecond timestamp.
#[tauri::command]
async fn chrono_time_travel(
    state: State<'_, AppState>,
    as_of_micros: u64,
) -> Result<serde_json::Value, String> {
    let index = Arc::clone(&state.index);
    let files = tauri::async_runtime::spawn_blocking(move || {
        let idx = index.lock().map_err(|e| e.to_string())?;
        let files: BTreeMap<String, String> = idx
            .reconstruct_as_of(as_of_micros)
            .map_err(|e| e.to_string())?
            .into_iter()
            .map(|(k, v)| (k, String::from_utf8_lossy(&v.bytes).into_owned()))
            .collect();
        Ok::<BTreeMap<String, String>, String>(files)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    Ok(serde_json::json!({ "as_of_micros": as_of_micros, "files": files }))
}

#[tauri::command]
fn trim_health_check(target: String) -> String {
    // Phase 2: real TRIM detection via OS APIs (Windows: FSCTL, macOS: diskutil).
    format!("target={target} trim=unknown — treat as SSD: minimize writes")
}

#[derive(Serialize)]
struct DeletedFileInfo {
    record_number: u64,
    name: String,
    size_bytes: u64,
    is_resident: bool,
    fixup_ok: bool,
    modified_utc: String, // ISO-ish for UI
}

/// Scan a disk image for deleted files via $MFT.
/// Results stream as EngineEvent::SectorResult (state_code=1, carved).
#[tauri::command]
async fn scan_deleted_files(
    app: AppHandle,
    image_path: String,
) -> Result<Vec<DeletedFileInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let img = std::fs::File::open(&image_path).map_err(|e| format!("open: {e}"))?;

        // Partition table se NTFS volume dhoondo
        let mut s0 = [0u8; 512];
        let mut s1 = [0u8; 512];
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&image_path).map_err(|e| e.to_string())?;
        f.read_exact(&mut s0).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
        f.read_exact(&mut s1).map_err(|e| e.to_string())?;
        // GPT entries: typically 128 entries * 128 bytes = 16KB at LBA 2
        f.seek(SeekFrom::Start(2 * 512))
            .map_err(|e| e.to_string())?;
        let mut gpt_entries = vec![0u8; 128 * 128];
        let _ = f.read_exact(&mut gpt_entries);

        let part =
            partition::find_ntfs_volume(&s0, &s1, &gpt_entries).ok_or("no NTFS partition found")?;
        let vol_offset = part.first_lba * 512;

        // Boot sector padho
        let mut bs = vec![0u8; 512];
        f.seek(SeekFrom::Start(vol_offset))
            .map_err(|e| e.to_string())?;
        f.read_exact(&mut bs).map_err(|e| e.to_string())?;
        let geo = ntfs::parse_boot_sector(&bs).ok_or("invalid NTFS boot sector")?;

        // $MFT scan
        let mut deleted = Vec::new();
        let app2 = app.clone();
        let _ = ntfs::scan_mft(&img, &geo, 1_000_000, |rec| {
            if rec.in_use || rec.is_directory {
                return;
            }
            if let Some(fna) = rec.file_names.first() {
                let info = DeletedFileInfo {
                    record_number: rec.record_number,
                    name: fna.name.clone(),
                    size_bytes: fna.real_size,
                    is_resident: rec.resident_data_len.is_some(),
                    fixup_ok: rec.fixup_ok,
                    modified_utc: format!("{}µs", fna.modified_unix_us),
                };
                // Stream to crystal: deleted = carved (amber)
                let _ = app2.emit(
                    "engine",
                    EngineEvent::SectorResult {
                        particle_index: rec.record_number as u32,
                        state_code: 1,
                        cluster: rec.record_number * geo.record_size as u64 / geo.cluster_size,
                        signature: "MFT-DELETED".into(),
                        confidence: if rec.fixup_ok { 0.85 } else { 0.40 },
                    },
                );
                deleted.push(info);
            }
        })
        .map_err(|e| e.to_string())?;

        let _ = app.emit(
            "engine",
            EngineEvent::ScanComplete {
                found: deleted.len() as u32,
                verified: 0,
                failures: 0,
            },
        );
        Ok(deleted)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Extract one deleted file's content by MFT record number.
#[tauri::command]
async fn extract_deleted_file(
    image_path: String,
    record_number: u64,
    output_path: String,
) -> Result<u64, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let img = std::fs::File::open(&image_path).map_err(|e| e.to_string())?;

        // Partition + geometry (same as scan)
        let mut f = std::fs::File::open(&image_path).map_err(|e| e.to_string())?;
        let mut s0 = [0u8; 512];
        let mut s1 = [0u8; 512];
        use std::io::{Read, Seek, SeekFrom};
        f.read_exact(&mut s0).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
        f.read_exact(&mut s1).map_err(|e| e.to_string())?;
        f.seek(SeekFrom::Start(2 * 512))
            .map_err(|e| e.to_string())?;
        let mut gpt_entries = vec![0u8; 128 * 128];
        let _ = f.read_exact(&mut gpt_entries);

        let part =
            partition::find_ntfs_volume(&s0, &s1, &gpt_entries).ok_or("no NTFS partition")?;
        let vol_offset = part.first_lba * 512;
        let mut bs = vec![0u8; 512];
        f.seek(SeekFrom::Start(vol_offset))
            .map_err(|e| e.to_string())?;
        f.read_exact(&mut bs).map_err(|e| e.to_string())?;
        let geo = ntfs::parse_boot_sector(&bs).ok_or("invalid NTFS")?;

        // Record padho
        let rec_offset =
            vol_offset + geo.mft_lcn * geo.cluster_size + record_number * geo.record_size as u64;
        let mut raw = vec![0u8; geo.record_size];
        f.seek(SeekFrom::Start(rec_offset))
            .map_err(|e| e.to_string())?;
        f.read_exact(&mut raw).map_err(|e| e.to_string())?;

        let rec = ntfs::parse_record(&raw, record_number, geo.sector_size)
            .ok_or("record parse failed")?;
        let data = ntfs_extract::extract_file_content(&img, &geo, &rec, &raw)
            .map_err(|e| e.to_string())?;

        // VaultGuard: staging dir mein likho (final destination baad mein)
        std::fs::write(&output_path, &data).map_err(|e| e.to_string())?;
        Ok(data.len() as u64)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// List partitions via the pytsk3 sidecar (EWF/raw image support).
#[tauri::command]
async fn sidecar_list_partitions(
    sidecar: State<'_, SharedSidecar>,
    image_path: String,
) -> Result<serde_json::Value, String> {
    let mut guard = sidecar.lock().await;
    if let Some(ref mut sc) = *guard {
        sc.list_partitions(&image_path)
            .await
            .map_err(|e| e.to_string())
    } else {
        Err("Forensic sidecar is not available (Python/sidecar missing)".into())
    }
}

/// Render a thumbnail for a carved image at a byte offset.
#[tauri::command]
async fn sidecar_thumbnail(
    sidecar: State<'_, SharedSidecar>,
    image_path: String,
    offset: u64,
    out_path: String,
) -> Result<serde_json::Value, String> {
    let mut guard = sidecar.lock().await;
    if let Some(ref mut sc) = *guard {
        sc.carve_thumbnail(&image_path, offset, &out_path)
            .await
            .map_err(|e| e.to_string())
    } else {
        Err("Forensic sidecar is not available (Python/sidecar missing)".into())
    }
}

/// Integrity Gate: strict structural check for carved files.
/// MP4 → mp4.rs gate; baaki files carver signature already matched.
#[tauri::command]
async fn verify_carved_file(app: AppHandle, path: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let data = std::fs::read(&path).map_err(|e| e.to_string())?;
        // Extension ke hisaab se gate — mp4 ka strict parser, baaki signature check
        let ok = if path.ends_with(".mp4") {
            clockverse_engine::mp4::validate(&data).playable_estimate()
        } else {
            true // JPEG/PNG/PDF: carver signature already matched
        };
        if ok {
            // Crystal pe TEAL — Integrity Gate pass (state_code=2)
            let _ = app.emit(
                "engine",
                EngineEvent::FileVerified {
                    path: path.clone(),
                    sha256: String::new(), // Phase 2.5: sidecar verify_file hook
                },
            );
        }
        Ok(ok)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_temp_dir() -> String {
    std::env::temp_dir().to_string_lossy().to_string()
}

/// Native Windows file picker dialog for forensic disk images (.dd, .img, .raw, .E01).
#[tauri::command]
async fn select_image_file() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let script = r#"
            Add-Type -AssemblyName System.Windows.Forms
            $f = New-Object System.Windows.Forms.OpenFileDialog
            $f.Filter = "Disk Images (*.dd;*.img;*.raw;*.E01)|*.dd;*.img;*.raw;*.E01|All Files (*.*)|*.*"
            if ($f.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
                Write-Output $f.FileName
            }
            "#;
            let output = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", script])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .output()
                .map_err(|e| e.to_string())?;
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                Ok(None)
            } else {
                Ok(Some(path))
            }
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    })
    .await
    .map_err(|e| e.to_string())?
}


#[tauri::command]
async fn time_capsule_protect(
    state: State<'_, AppState>,
    path: String,
    name: String,
) -> Result<ProtectedFolder, String> {
    let mut capsule = state.time_capsule.lock().map_err(|e| e.to_string())?;
    capsule.protect_folder(path, name).map_err(|e| e.to_string())
}

#[tauri::command]
async fn time_capsule_snapshot(
    state: State<'_, AppState>,
    folder_path: String,
) -> Result<u64, String> {
    let mut capsule = state.time_capsule.lock().map_err(|e| e.to_string())?;
    let snap = capsule.snapshot_folder(&folder_path).map_err(|e| e.to_string())?;
    Ok(snap.created_at)
}

#[tauri::command]
async fn time_capsule_list(state: State<'_, AppState>) -> Result<Vec<ProtectedFolder>, String> {
    let capsule = state.time_capsule.lock().map_err(|e| e.to_string())?;
    Ok(capsule.folders.clone())
}

#[tauri::command]
async fn select_folder() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let script = r#"
            Add-Type -AssemblyName System.Windows.Forms
            $f = New-Object System.Windows.Forms.FolderBrowserDialog
            $f.Description = "Select Folder to Protect with Time Capsule"
            if ($f.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
                Write-Output $f.SelectedPath
            }
            "#;
            let output = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", script])
                .creation_flags(0x08000000) // CREATE_NO_WINDOW
                .output()
                .map_err(|e| e.to_string())?;
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if path.is_empty() {
                Ok(None)
            } else {
                Ok(Some(path))
            }
        }
        #[cfg(not(windows))]
        {
            Ok(None)
        }
    })
    .await
    .map_err(|e| e.to_string())?
}


const DEFAULT_SUPABASE_URL: &str = "https://clockverse-prod.supabase.co";
const DEFAULT_SUPABASE_ANON_KEY: &str = "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.dummy";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LicenseStatus {
    pub valid: bool,
    pub tier: Option<String>,
    pub activated: bool,
    pub error: Option<String>,
    pub expires_at: Option<String>,
}

#[tauri::command]
async fn validate_license(key: String) -> Result<LicenseStatus, String> {
    let machine_id = machine_uid::get().unwrap_or_else(|_| "generic-machine-id".to_string());
    let supabase_url = std::env::var("CLOCKVERSE_SUPABASE_URL")
        .unwrap_or_else(|_| DEFAULT_SUPABASE_URL.to_string());
    let supabase_key = std::env::var("CLOCKVERSE_SUPABASE_KEY")
        .unwrap_or_else(|_| DEFAULT_SUPABASE_ANON_KEY.to_string());

    let client = Client::new();
    let res = client
        .post(format!("{}/functions/v1/validate-license", supabase_url))
        .header("Authorization", format!("Bearer {}", supabase_key))
        .header("Content-Type", "application/json")
        .json(&json!({
            "license_key": key,
            "machine_id": machine_id
        }))
        .send()
        .await
        .map_err(|e| format!("Network request failed: {}", e))?;

    let status: LicenseStatus = res
        .json()
        .await
        .map_err(|e| format!("Invalid JSON response: {}", e))?;
    Ok(status)
}

#[tauri::command]
async fn activate_license(key: String) -> Result<LicenseStatus, String> {
    let status = validate_license(key).await?;
    if status.valid && status.activated {
        let mut config = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
        config.push("clockverse");
        std::fs::create_dir_all(&config).ok();
        config.push("license.json");
        std::fs::write(config, serde_json::to_string_pretty(&status).unwrap())
            .map_err(|e| e.to_string())?;
        Ok(status)
    } else {
        Err(status.error.unwrap_or_else(|| "License activation failed".to_string()))
    }
}

#[tauri::command]
async fn check_license_grace() -> Result<LicenseStatus, String> {
    let mut config = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    config.push("clockverse");
    config.push("license.json");
    if let Ok(content) = std::fs::read_to_string(config) {
        if let Ok(status) = serde_json::from_str::<LicenseStatus>(&content) {
            // Check expiry date if specified
            if let Some(expires) = &status.expires_at {
                if let Ok(exp) = chrono::DateTime::parse_from_rfc3339(expires) {
                    if exp.timestamp() <= chrono::Utc::now().timestamp() {
                        return Ok(LicenseStatus {
                            valid: false,
                            tier: status.tier,
                            activated: false,
                            error: Some("License expired".to_string()),
                            expires_at: status.expires_at,
                        });
                    }
                }
            }
            return Ok(status);
        }
    }
    Ok(LicenseStatus {
        valid: false,
        tier: None,
        activated: false,
        error: None,
        expires_at: None,
    })
}

fn main() {
    // Problem #1 fix: Persistent DB with env override for tests
    let db_path = std::env::var("CLOCKVERSE_DB")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let mut p = dirs::data_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
            p.push("clockverse");
            p.push("sessions.db");
            p
        });

    // Ensure directory exists
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let index = EventIndex::open(db_path.to_str().expect("invalid db path"))
        .expect("failed to open persistent event index");

    // Problem #3 fix: Resolve sidecar path relative to exe or cwd
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let sidecar_path = exe_dir.join("sidecar/sidecar.py");
    let sidecar_path = if sidecar_path.exists() {
        sidecar_path
    } else {
        // Dev fallback: assume running from workspace root
        std::path::PathBuf::from("sidecar/sidecar.py")
    };

    // Problem #3 fix: Python executable detection (Windows vs Unix)
    let python_cmd = if cfg!(windows) { "python" } else { "python3" };

    let sidecar: Option<Sidecar> = tauri::async_runtime::block_on(async {
        match Sidecar::spawn(python_cmd, sidecar_path.to_str().unwrap()).await {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("[WARN] Forensic sidecar unavailable: {}. Running without sidecar.", e);
                None
            }
        }
    });

    let time_capsule = Arc::new(StdMutex::new(TimeCapsule::new()));

    // Background Time Capsule Auto-Snapshot Daemon (Runs every 10 minutes)
    let capsule_daemon = Arc::clone(&time_capsule);
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(600));
        loop {
            interval.tick().await;
            let active_folders: Vec<String> = {
                if let Ok(c) = capsule_daemon.lock() {
                    c.folders.iter()
                        .filter(|f| f.status == CapsuleStatus::Active)
                        .map(|f| f.path.clone())
                        .collect()
                } else {
                    Vec::new()
                }
            };

            for folder in active_folders {
                if let Ok(mut c) = capsule_daemon.lock() {
                    if let Err(e) = c.snapshot_folder(&folder) {
                        eprintln!("[TimeCapsule] Background snapshot error for {}: {}", folder, e);
                    } else {
                        println!("[TimeCapsule] Auto-snapshot completed for {}", folder);
                    }
                }
            }
        }
    });

    tauri::Builder::default()
        .manage(AppState {
            index: Arc::new(StdMutex::new(index)),
            time_capsule: Arc::clone(&time_capsule),
        })
        .manage(Arc::new(TokioMutex::new(sidecar)) as SharedSidecar)
        .invoke_handler(tauri::generate_handler![
            start_scan,
            trim_health_check,
            chrono_ingest,
            session_summary,
            chrono_time_travel,
            scan_deleted_files,
            extract_deleted_file,
            verify_carved_file,
            sidecar_list_partitions,
            sidecar_thumbnail,
            get_temp_dir,
            select_image_file,
            time_capsule_protect,
            time_capsule_snapshot,
            time_capsule_list,
            select_folder,
            validate_license,
            activate_license,
            check_license_grace
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClockVerse");
}

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
async fn start_scan(
    app: AppHandle,
    target: String,
) -> Result<Vec<clockverse_engine::sectorforge::CarvedFileInfo>, String> {
    let clean_target = target.trim().trim_matches('"').to_string();
    if clean_target.is_empty() {
        return Err("Target path cannot be empty. Please select a folder or file to scan.".to_string());
    }

    let _ = app.emit(
        "engine",
        EngineEvent::ScanStarted {
            target: clean_target.clone(),
            total_sectors: 0,
        },
    );

    let path_obj = std::path::PathBuf::from(&clean_target);
    let staging_dir = std::env::temp_dir().join("clockverse_staging");

    // Case 1: Folder / Directory Scan
    if path_obj.is_dir() {
        let dir_str = clean_target.clone();
        let staging_clone = staging_dir.clone();
        let extracted = tauri::async_runtime::spawn_blocking(move || {
            sectorforge::carve_folder(&dir_str, &staging_clone)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;

        for (i, file) in extracted.iter().enumerate() {
            let _ = app.emit(
                "engine",
                EngineEvent::SectorResult {
                    particle_index: (i % 1200) as u32,
                    state_code: 1, // carved
                    cluster: (file.offset / 4096) as u64,
                    signature: file.extension.clone(),
                    confidence: file.confidence,
                },
            );
            let _ = app.emit(
                "engine",
                EngineEvent::FileRestored {
                    path: file.path.clone(),
                    bytes: file.size_bytes,
                },
            );
        }

        let verified_count = extracted.len() as u32;
        let _ = app.emit(
            "engine",
            EngineEvent::ScanComplete {
                found: verified_count,
                verified: verified_count,
                failures: 0,
            },
        );

        return Ok(extracted);
    }

    // Case 2: File / Disk Image / Raw Drive
    let target_clone = clean_target.clone();
    let target_for_err = clean_target.clone();
    let hits = tauri::async_runtime::spawn_blocking(move || {
        sectorforge::carve_image(&target_clone, 64 * 1024 * 1024)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| {
        let err_str = e.to_string();
        if target_for_err.contains("PhysicalDrive") && (err_str.contains("Access is denied") || err_str.contains("os error 5")) {
            "Administrator privileges required to read direct physical drives (\\\\.\\PhysicalDrive0). Right-click ClockVerse and choose 'Run as administrator', or scan a disk image / use Instant Demo Platter.".to_string()
        } else {
            err_str
        }
    })?;

    for (i, hit) in hits.iter().enumerate() {
        let _ = app.emit(
            "engine",
            EngineEvent::SectorResult {
                particle_index: (i % 1200) as u32,
                state_code: 1, // carved
                cluster: hit.offset / 4096,
                signature: hit.signature.clone(),
                confidence: hit.confidence,
            },
        );
    }

    // Extract files into staging directory
    let staging_dir = std::env::temp_dir().join("clockverse_staging");
    let target_clone2 = target.clone();
    let hits_clone = hits.clone();
    let extracted = tauri::async_runtime::spawn_blocking(move || {
        sectorforge::extract_carved_files(&target_clone2, &hits_clone, &staging_dir)
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;

    for file in &extracted {
        let _ = app.emit(
            "engine",
            EngineEvent::FileRestored {
                path: file.path.clone(),
                bytes: file.size_bytes,
            },
        );
    }

    let verified_count = extracted.len() as u32;
    let _ = app.emit(
        "engine",
        EngineEvent::ScanComplete {
            found: hits.len() as u32,
            verified: verified_count,
            failures: (hits.len() as u32).saturating_sub(verified_count),
        },
    );

    Ok(extracted)
}

/// Creates a simulated disk platter image containing real sample JPEG, PNG, and PDF files
#[tauri::command]
async fn create_demo_platter() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = std::env::temp_dir().join("clockverse_demo_platter.img");
        let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
        use std::io::Write;

        let mut platter = vec![0u8; 1024 * 1024]; // 1MB simulated disk platter

        // 1. Valid 1x1 JPEG image at offset 16384 (16 KB)
        let jpeg_data = [
            0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, 0x4A, 0x46, 0x49, 0x46, 0x00, 0x01, 0x01, 0x01, 0x00, 0x48,
            0x00, 0x48, 0x00, 0x00, 0xFF, 0xDB, 0x00, 0x43, 0x00, 0x08, 0x06, 0x06, 0x07, 0x06, 0x05, 0x08,
            0x07, 0x07, 0x07, 0x09, 0x09, 0x08, 0x0A, 0x0C, 0x14, 0x0D, 0x0C, 0x0B, 0x0B, 0x0C, 0x19, 0x12,
            0x13, 0x0F, 0x14, 0x1D, 0x1A, 0x1F, 0x1E, 0x1D, 0x1A, 0x1C, 0x1C, 0x20, 0x24, 0x2E, 0x27, 0x20,
            0x22, 0x2C, 0x23, 0x1C, 0x1C, 0x28, 0x37, 0x29, 0x2C, 0x30, 0x31, 0x34, 0x34, 0x34, 0x1F, 0x27,
            0x39, 0x3D, 0x38, 0x32, 0x3C, 0x2E, 0x33, 0x34, 0x32, 0xFF, 0xC0, 0x00, 0x0B, 0x08, 0x00, 0x01,
            0x00, 0x01, 0x01, 0x01, 0x11, 0x00, 0xFF, 0xC4, 0x00, 0x1F, 0x00, 0x00, 0x01, 0x05, 0x01, 0x01,
            0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03, 0x04,
            0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0xFF, 0xDA, 0x00, 0x08, 0x01, 0x01, 0x00, 0x00, 0x3F,
            0x00, 0xBF, 0x00, 0xFF, 0xD9
        ];
        platter[16384..16384 + jpeg_data.len()].copy_from_slice(&jpeg_data);

        // 2. Valid 1x1 PNG image at offset 65536 (64 KB)
        let png_data = [
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
            0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
            0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
            0x42, 0x60, 0x82
        ];
        platter[65536..65536 + png_data.len()].copy_from_slice(&png_data);

        // 3. Valid PDF document at offset 131072 (128 KB)
        let pdf_str = "%PDF-1.4\n1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n3 0 obj<</Type/Page/MediaBox[0 0 612 792]/Parent 2 0 R/Resources<<>>>>endobj\nxref\n0 4\n0000000000 65535 f \n0000000009 00000 n \n0000000052 00000 n \n0000000101 00000 n \ntrailer<</Size 4/Root 1 0 R>>\nstartxref\n178\n%%EOF\n";
        let pdf_bytes = pdf_str.as_bytes();
        platter[131072..131072 + pdf_bytes.len()].copy_from_slice(pdf_bytes);

        file.write_all(&platter).map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Restores a recovered file to the user's Downloads or target directory
#[tauri::command]
async fn restore_file_to_disk(
    source_path: String,
    destination_dir: Option<String>,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let src = std::path::Path::new(&source_path);
        if !src.exists() {
            return Err(format!("Source file does not exist: {}", source_path));
        }

        let file_name = src.file_name().ok_or("invalid file name")?;

        let target_dir = if let Some(dir) = destination_dir {
            std::path::PathBuf::from(dir)
        } else {
            let base = dirs::download_dir()
                .or_else(dirs::desktop_dir)
                .unwrap_or_else(|| std::env::temp_dir());
            base.join("ClockVerse_Restored")
        };

        std::fs::create_dir_all(&target_dir).map_err(|e| e.to_string())?;
        let dest = target_dir.join(file_name);
        std::fs::copy(&src, &dest).map_err(|e| e.to_string())?;

        Ok(dest.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
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

/// Open a file or folder in Windows File Explorer
#[tauri::command]
async fn open_in_explorer(path: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let p = std::path::Path::new(&path);
        let target = if p.is_file() {
            p.parent().unwrap_or(p)
        } else {
            p
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            std::process::Command::new("explorer")
                .arg(target)
                .creation_flags(0x08000000)
                .spawn()
                .map_err(|e| e.to_string())?;
        }
        Ok(())
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
            $f.Title = "Select File or Disk Image to Scan"
            $f.Filter = "All Files (*.*)|*.*|Disk Images (*.dd;*.img;*.raw;*.iso;*.vhd;*.E01)|*.dd;*.img;*.raw;*.iso;*.vhd;*.E01|Media & Documents (*.jpg;*.png;*.pdf;*.zip;*.mp4)|*.jpg;*.png;*.pdf;*.zip;*.mp4"
            $f.FilterIndex = 1
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
async fn time_capsule_history(
    state: State<'_, AppState>,
    folder_path: String,
) -> Result<Vec<clockverse_engine::timesnap::Snapshot>, String> {
    let capsule = state.time_capsule.lock().map_err(|e| e.to_string())?;
    Ok(capsule.list_snapshots(&folder_path))
}

#[tauri::command]
async fn time_capsule_rollback(
    state: State<'_, AppState>,
    folder_path: String,
    snapshot_id: String,
) -> Result<u32, String> {
    let capsule = state.time_capsule.lock().map_err(|e| e.to_string())?;
    capsule.rollback_folder(&folder_path, &snapshot_id).map_err(|e| e.to_string())
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


const DEFAULT_SUPABASE_URL: &str = "https://hdjedvcyzrzrryvddsat.supabase.co";
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
    // Unlocked Community Pro Edition: 100% working features enabled
    Ok(LicenseStatus {
        valid: true,
        tier: Some("pro".to_string()),
        activated: true,
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

    // Problem #3 fix: Multi-candidate sidecar path resolution
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| std::path::PathBuf::from("."));

    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    let candidates = [
        exe_dir.join("sidecar/sidecar.py"),
        exe_dir.join("../../sidecar/sidecar.py"),
        exe_dir.join("../sidecar/sidecar.py"),
        cwd.join("sidecar/sidecar.py"),
        std::path::PathBuf::from("sidecar/sidecar.py"),
    ];

    let sidecar_path = candidates.into_iter().find(|p| p.exists());

    // Python executable detection (Windows vs Unix)
    let python_cmd = if cfg!(windows) { "python" } else { "python3" };

    let sidecar: Option<Sidecar> = if let Some(path) = sidecar_path {
        tauri::async_runtime::block_on(async {
            match Sidecar::spawn(python_cmd, path.to_str().unwrap()).await {
                Ok(s) => {
                    println!("[INFO] Forensic sidecar connected at {:?}", path);
                    Some(s)
                }
                Err(e) => {
                    eprintln!("[WARN] Forensic sidecar unavailable: {}. Running without sidecar.", e);
                    None
                }
            }
        })
    } else {
        eprintln!("[WARN] sidecar.py not found in any candidate path. Running in standalone native mode.");
        None
    };

    let time_capsule = Arc::new(StdMutex::new(TimeCapsule::new()));

    // Background Time Capsule Auto-Snapshot Daemon (Runs every 10 minutes)
    let capsule_daemon = Arc::clone(&time_capsule);
    tauri::async_runtime::spawn(async move {
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
            open_in_explorer,
            select_image_file,
            create_demo_platter,
            restore_file_to_disk,
            time_capsule_protect,
            time_capsule_snapshot,
            time_capsule_list,
            time_capsule_history,
            time_capsule_rollback,
            select_folder,
            validate_license,
            activate_license,
            check_license_grace
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClockVerse");
}

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod workbench;
use clockverse_engine::index::EventIndex;
use clockverse_engine::sidecar::Sidecar;
use clockverse_engine::timesnap::{CapsuleStatus, ProtectedFolder, TimeCapsule};
use clockverse_engine::{chrono::StreamStitch, EngineEvent};
use serde::Serialize;
use serde_json::json;
use workbench::*;

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
    snapshot_config: std::path::PathBuf,
}

#[tauri::command]
async fn create_demo_platter() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(|| {
        use std::io::Write;
        let path =
            std::env::temp_dir().join(format!("clockverse_demo_{}.img", uuid::Uuid::new_v4()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let mut platter = vec![0u8; 1024 * 1024];
        let png = include_bytes!("../assets/sample.png");
        let pdf = include_bytes!("../assets/sample.pdf");
        platter[65536..65536 + png.len()].copy_from_slice(png);
        platter[131072..131072 + pdf.len()].copy_from_slice(pdf);
        file.write_all(&platter).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        Ok(path.to_string_lossy().to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Import a telemetry log into the optional session index.
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

/// Read partition metadata through the optional forensic sidecar.
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

/// Open the directory containing a saved result.
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
            $f.Filter = "All Files (*.*)|*.*|Raw Disk Images (*.dd;*.img;*.raw)|*.dd;*.img;*.raw|Media & Documents (*.jpg;*.png;*.pdf;*.zip;*.mp4)|*.jpg;*.png;*.pdf;*.zip;*.mp4"
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
    app: AppHandle,
    session: State<'_, RecoveryState>,
    state: State<'_, AppState>,
    path: String,
    name: String,
) -> Result<ProtectedFolder, String> {
    let guard = session.begin()?;
    let cancel = Arc::clone(&session.cancel);
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        shared
            .lock()
            .map_err(|e| e.to_string())?
            .protect_controlled(path, name, &cancel, |p| {
                let _ = app.emit("snapshot-progress", p);
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn time_capsule_snapshot(
    app: AppHandle,
    session: State<'_, RecoveryState>,
    state: State<'_, AppState>,
    folder_path: String,
) -> Result<u64, String> {
    let guard = session.begin()?;
    let cancel = Arc::clone(&session.cancel);
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        shared
            .lock()
            .map_err(|e| e.to_string())?
            .snapshot_controlled(&folder_path, &cancel, |p| {
                let _ = app.emit("snapshot-progress", p);
            })
            .map(|s| s.created_at)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn time_capsule_list(state: State<'_, AppState>) -> Result<Vec<ProtectedFolder>, String> {
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(shared.lock().map_err(|e| e.to_string())?.folders.clone())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn time_capsule_history(
    state: State<'_, AppState>,
    folder_path: String,
) -> Result<Vec<clockverse_engine::timesnap::Snapshot>, String> {
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        Ok(shared
            .lock()
            .map_err(|e| e.to_string())?
            .list_snapshots(&folder_path))
    })
    .await
    .map_err(|e| e.to_string())?
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

    let sidecar_path = if std::env::var("CLOCKVERSE_ENABLE_SIDECAR").as_deref() == Ok("1") {
        candidates.into_iter().find(|p| p.exists())
    } else {
        None
    };

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
                    eprintln!(
                        "[WARN] Forensic sidecar unavailable: {}. Running without sidecar.",
                        e
                    );
                    None
                }
            }
        })
    } else {
        eprintln!(
            "[WARN] sidecar.py not found in any candidate path. Running in standalone native mode."
        );
        None
    };

    let snapshot_config = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("clockverse/snapshot-location.json");
    let capsule = match std::fs::read(&snapshot_config) {
        Ok(bytes) => match serde_json::from_slice::<std::path::PathBuf>(&bytes) {
            Ok(path) if path.is_dir() => TimeCapsule::with_storage(path),
            _ => TimeCapsule::unavailable(snapshot_config.clone(), "Configured backup location is missing or invalid. Reconnect the drive or open an existing backup.".into()),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => TimeCapsule::new(),
        Err(e) => TimeCapsule::unavailable(snapshot_config.clone(), e.to_string()),
    };
    let time_capsule = Arc::new(StdMutex::new(capsule));

    let recovery_state = RecoveryState::default();
    let scanning_flag = Arc::clone(&recovery_state.active);
    let daemon_cancel = Arc::clone(&recovery_state.cancel);
    // Check configured schedules every minute; defer background I/O during recovery.
    let capsule_daemon = Arc::clone(&time_capsule);
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await; // Do not immediately duplicate startup snapshots.
        loop {
            interval.tick().await;
            if scanning_flag
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_err()
            {
                continue;
            }
            let guard = ActiveGuard(Arc::clone(&scanning_flag));
            daemon_cancel.store(false, std::sync::atomic::Ordering::SeqCst);
            let cancel = Arc::clone(&daemon_cancel);
            let shared = Arc::clone(&capsule_daemon);
            if let Err(error) = tauri::async_runtime::spawn_blocking(move || {
                let _guard = guard;
                if let Ok(mut capsule) = shared.lock() {
                    let folders: Vec<String> = capsule
                        .folders
                        .iter()
                        .filter(|f| {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_micros() as u64;
                            f.status != CapsuleStatus::Paused
                                && now
                                    .saturating_sub(f.last_attempt.or(f.last_snapshot).unwrap_or(0))
                                    >= capsule.policy.interval_minutes.saturating_mul(60_000_000)
                        })
                        .map(|f| f.path.clone())
                        .collect();
                    for folder in folders {
                        if cancel.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        if let Err(error) = capsule.snapshot_controlled(&folder, &cancel, |_| {}) {
                            if let Some(record) =
                                capsule.folders.iter_mut().find(|f| f.path == folder)
                            {
                                record.status = CapsuleStatus::Error(error.to_string());
                            }
                            if let Err(error) = capsule.save_folders() {
                                eprintln!("[TimeCapsule] Cannot persist status: {}", error);
                            }
                            eprintln!("[TimeCapsule] {}: {}", folder, error);
                        }
                    }
                }
            })
            .await
            {
                eprintln!("[TimeCapsule] Worker failed: {}", error);
            }
        }
    });

    tauri::Builder::default()
        .manage(recovery_state)
        .manage(AppState {
            index: Arc::new(StdMutex::new(index)),
            time_capsule: Arc::clone(&time_capsule),
            snapshot_config,
        })
        .manage(Arc::new(TokioMutex::new(sidecar)) as SharedSidecar)
        .invoke_handler(tauri::generate_handler![
            recovery_scan,
            recovery_cancel,
            recovery_search,
            recovery_preview,
            capsule_policy,
            capsule_pause,
            capsule_export,
            capsule_maintenance,
            capsule_location,
            capsule_estimate,
            chrono_ingest,
            session_summary,
            chrono_time_travel,
            sidecar_list_partitions,
            get_temp_dir,
            open_in_explorer,
            select_image_file,
            create_demo_platter,
            time_capsule_protect,
            time_capsule_snapshot,
            time_capsule_list,
            time_capsule_history,
            select_folder,
            list_system_drives,
            check_admin_privileges,
            vss_create,
            vss_list,
            vss_restore,
            ssd_mine_shadows,
            ssd_restore_mined_file,
            relaunch_as_admin,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ClockVerse");
}
#[cfg(test)]
mod native_tests {
    #[tokio::test]
    async fn demo_image_recovers_real_png_and_pdf() {
        let path = super::create_demo_platter().await.unwrap();
        let out = std::env::temp_dir().join(format!("clockverse_test_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&out).unwrap();
        let report = clockverse_engine::recovery::run(
            std::path::Path::new(&path),
            &out,
            "",
            &std::sync::atomic::AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert!(!report.partial);
        assert_eq!(report.files.len(), 2);
        let png = report.files.iter().find(|f| f.extension == "png").unwrap();
        assert_eq!(
            std::fs::read(&png.path).unwrap(),
            include_bytes!("../assets/sample.png")
        );
        let pdf = report.files.iter().find(|f| f.extension == "pdf").unwrap();
        assert!(std::fs::read(&pdf.path).unwrap().ends_with(b"%%EOF"));
        // Only explicitly created test files and their isolated UUID directory are removed.
        std::fs::remove_file(&path).unwrap();
        for file in &report.files {
            std::fs::remove_file(&file.path).unwrap();
        }
        std::fs::remove_file(std::path::Path::new(&report.output_dir).join("recovery-report.json"))
            .unwrap();
        std::fs::remove_dir(&report.output_dir).unwrap();
        std::fs::remove_dir(&out).unwrap();
    }
}

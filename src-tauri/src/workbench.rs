use super::*;
use clockverse_engine::{
    recovery::{self, RecoveryReport},
    timesnap::SnapshotPolicy,
};
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Default)]
pub struct RecoveryState {
    pub(crate) active: Arc<AtomicBool>,
    pub(crate) cancel: Arc<AtomicBool>,
    latest: Arc<StdMutex<Option<RecoveryReport>>>,
}
pub(crate) struct ActiveGuard(pub(crate) Arc<AtomicBool>);
impl RecoveryState {
    pub(crate) fn begin(&self) -> Result<ActiveGuard, String> {
        self.active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map_err(|_| {
                "Another disk operation is running. Stop it or wait for completion.".to_string()
            })?;
        self.cancel.store(false, Ordering::SeqCst);
        Ok(ActiveGuard(Arc::clone(&self.active)))
    }
}
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

#[tauri::command]
pub async fn recovery_scan(
    app: AppHandle,
    state: State<'_, RecoveryState>,
    target: String,
    destination: String,
    query: Option<String>,
) -> Result<RecoveryReport, String> {
    let guard = state.begin()?;
    let cancel = Arc::clone(&state.cancel);
    let latest = Arc::clone(&state.latest);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let target = target.trim().trim_matches('"');
        let result = recovery::run(
            std::path::Path::new(target),
            std::path::Path::new(destination.trim().trim_matches('"')),
            query.as_deref().unwrap_or(""),
            &cancel,
            |progress| {
                let _ = app.emit("recovery-progress", progress);
            },
        )
        .map_err(|e| e.to_string())?;
        *latest.lock().map_err(|e| e.to_string())? = Some(result.clone());
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub fn recovery_cancel(state: State<'_, RecoveryState>) {
    state.cancel.store(true, Ordering::SeqCst);
}

#[derive(Serialize)]
pub struct SearchReply {
    query: String,
    matches: Vec<recovery::RecoveryFile>,
    snapshots: Vec<SnapshotMatch>,
    checked: Vec<String>,
    guidance: String,
}
#[derive(Serialize)]
pub struct SnapshotMatch {
    snapshot_id: String,
    rel_path: String,
    folder: String,
    size: u64,
    created_at: u64,
}
#[tauri::command]
pub async fn recovery_search(
    state: State<'_, AppState>,
    session: State<'_, RecoveryState>,
    query: String,
) -> Result<SearchReply, String> {
    let capsule = Arc::clone(&state.time_capsule);
    let latest = Arc::clone(&session.latest);
    tauri::async_runtime::spawn_blocking(move || {
        let query = recovery::search_term(&query);
        if query.is_empty() || query.len() > 200 { return Err("Enter a filename, part of a name, or extension (up to 200 characters).".into()); }
        let report = latest.lock().map_err(|e| e.to_string())?;
        let matches = report.as_ref().map(|r| r.files.iter().filter(|f| recovery::matches_query(&f.name, &query)).take(100).cloned().collect()).unwrap_or_default();
        drop(report);
        let capsule = capsule.lock().map_err(|e| e.to_string())?;
        let mut snapshots = vec![];
        'folders: for folder in &capsule.folders {
            for snapshot in capsule.list_snapshots(&folder.path) {
                for entry in snapshot.entries {
                    if recovery::matches_query(&entry.rel_path, &query) {
                        snapshots.push(SnapshotMatch { snapshot_id: snapshot.id.clone(), rel_path: entry.rel_path, folder: folder.path.clone(), size: entry.size, created_at: snapshot.created_at });
                        if snapshots.len() >= 100 { break 'folders; }
                    }
                }
            }
        }
        Ok(SearchReply { query, matches, snapshots, checked: vec!["Latest completed/partial scan (up to 100 matching results)".into(), "Registered snapshot manifests (up to 100 matching versions; content verified on restore)".into()],
            guidance: "If missing, run a targeted image scan below. NTFS records can preserve original names; carving usually cannot. Use a filename with extension, such as invoice.pdf, or just .pdf. No backup is required for image recovery. A file's deletion age alone does not determine recoverability. Recycle Bin, File History and cloud versions are not searched automatically.".into() })
    }).await.map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn capsule_policy(
    state: State<'_, AppState>,
    policy: Option<SnapshotPolicy>,
) -> Result<serde_json::Value, String> {
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        let mut capsule = shared.lock().map_err(|e| e.to_string())?;
        if let Some(error) = &capsule.load_error { return Err(error.clone()); }
        if let Some(p) = policy { capsule.set_policy(p).map_err(|e| e.to_string())?; }
        Ok(json!({"policy": capsule.policy, "storage_dir": capsule.storage_dir, "free_bytes": clockverse_engine::timesnap::available_space(&capsule.storage_dir).map_err(|e| e.to_string())?, "used_bytes": capsule.storage_bytes().map_err(|e| e.to_string())?}))
    }).await.map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn capsule_pause(
    state: State<'_, AppState>,
    folder_path: String,
    paused: bool,
) -> Result<(), String> {
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        shared
            .lock()
            .map_err(|e| e.to_string())?
            .set_paused(&folder_path, paused)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn capsule_export(
    app: AppHandle,
    session: State<'_, RecoveryState>,
    state: State<'_, AppState>,
    snapshot_id: String,
    rel_path: Option<String>,
    destination: String,
) -> Result<String, String> {
    let guard = session.begin()?;
    let cancel = Arc::clone(&session.cancel);
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let capsule = shared.lock().map_err(|e| e.to_string())?;
        let dest = std::fs::canonicalize(destination).map_err(|e| e.to_string())?;
        let storage = std::fs::canonicalize(&capsule.storage_dir).map_err(|e| e.to_string())?;
        if !dest.is_dir() || dest.starts_with(storage) {
            return Err("Choose an existing destination outside snapshot storage.".into());
        }
        let out = dest.join(format!("ClockVerse_Snapshot_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&out).map_err(|e| e.to_string())?;
        if let Some(path) = rel_path {
            let name = std::path::Path::new(&path)
                .file_name()
                .ok_or("Invalid filename")?;
            capsule
                .restore_controlled(&snapshot_id, &path, &out.join(name), &cancel)
                .map_err(|e| e.to_string())?;
        } else {
            capsule
                .export_controlled(&out, &snapshot_id, &cancel, |p| {
                    let _ = app.emit("snapshot-progress", p);
                })
                .map_err(|e| e.to_string())?;
        }
        Ok(out.to_string_lossy().into())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn recovery_preview(
    state: State<'_, RecoveryState>,
    file_id: String,
) -> Result<serde_json::Value, String> {
    let latest = Arc::clone(&state.latest);
    tauri::async_runtime::spawn_blocking(move || {
        use std::io::Read;
        use base64::Engine;
        let guard = latest.lock().map_err(|e| e.to_string())?;
        let file = guard.as_ref().and_then(|r| r.files.iter().find(|f| f.id == file_id)).cloned().ok_or("File is not in the latest recovery session")?;
        drop(guard);
        if matches!(file.extension.as_str(), "jpg" | "jpeg" | "png") && file.size_bytes <= 8 * 1024 * 1024 {
            let mut bytes = Vec::new();
            std::fs::File::open(&file.path).map_err(|e| e.to_string())?.take(8 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            if bytes.len() > 8 * 1024 * 1024 { return Err("Preview is limited to 8 MiB".into()); }
            let mime = if file.extension == "png" { "image/png" } else { "image/jpeg" };
            return Ok(json!({"image": format!("data:{mime};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))}));
        }
        if matches!(file.extension.as_str(), "txt" | "md" | "csv" | "json" | "log" | "rs" | "py" | "js" | "html" | "xml" | "css") {
            let mut bytes = Vec::new();
            std::fs::File::open(&file.path).map_err(|e| e.to_string())?.take(64 * 1024).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
            return Ok(json!({"text": String::from_utf8_lossy(&bytes), "message": "First 64 KiB only"}));
        }
        Ok(json!({"message": "Inline preview supports JPEG/PNG up to 8 MiB and text up to 64 KiB. This file is saved; locate it to inspect with a trusted viewer."}))
    }).await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn capsule_estimate(
    state: State<'_, AppState>,
    session: State<'_, RecoveryState>,
    path: String,
) -> Result<serde_json::Value, String> {
    let guard = session.begin()?;
    let cancel = Arc::clone(&session.cancel);
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let capsule = shared.lock().map_err(|e| e.to_string())?;
        serde_json::to_value(
            capsule
                .estimate(std::path::Path::new(&path), &cancel)
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn capsule_maintenance(
    state: State<'_, AppState>,
    session: State<'_, RecoveryState>,
    cleanup: bool,
) -> Result<serde_json::Value, String> {
    let guard = session.begin()?;
    let cancel = Arc::clone(&session.cancel);
    let shared = Arc::clone(&state.time_capsule);
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let capsule = shared.lock().map_err(|e| e.to_string())?;
        let result = if cleanup {
            capsule.cleanup(&cancel)
        } else {
            capsule.audit(&cancel)
        }
        .map_err(|e| e.to_string())?;
        serde_json::to_value(result).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn capsule_location(
    app: AppHandle,
    state: State<'_, AppState>,
    session: State<'_, RecoveryState>,
    path: String,
    existing: bool,
) -> Result<String, String> {
    let guard = session.begin()?;
    let cancel = Arc::clone(&session.cancel);
    let shared = Arc::clone(&state.time_capsule);
    let config = state.snapshot_config.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _guard = guard;
        let mut capsule = shared.lock().map_err(|e| e.to_string())?;
        let parent = std::fs::canonicalize(path).map_err(|e| e.to_string())?;
        let target = if existing {
            if !parent.join("folders.json").is_file() {
                return Err(
                    "Select an existing ClockVerse repository containing folders.json".into(),
                );
            }
            TimeCapsule::with_storage(parent)
        } else {
            capsule
                .copy_repository(
                    &parent.join(format!("ClockVerse_Backups_{}", uuid::Uuid::new_v4())),
                    &cancel,
                    |p| {
                        let _ = app.emit("snapshot-progress", p);
                    },
                )
                .map_err(|e| e.to_string())?
        };
        if let Some(error) = &target.load_error {
            return Err(error.clone());
        }
        if let Some(parent) = config.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        clockverse_engine::timesnap::atomic_json(&config, &target.storage_dir)
            .map_err(|e| e.to_string())?;
        let result = target.storage_dir.to_string_lossy().into_owned();
        *capsule = target;
        Ok(result)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn list_system_drives() -> Result<Vec<clockverse_engine::drives::DriveInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        Ok(clockverse_engine::drives::list_drives())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn check_admin_privileges() -> bool {
    clockverse_engine::vss::is_admin()
}

#[tauri::command]
pub async fn vss_create(volume: String) -> Result<clockverse_engine::vss::ShadowCopyInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        clockverse_engine::vss::create_shadow_copy(&volume).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn vss_list() -> Result<Vec<clockverse_engine::vss::ShadowCopyInfo>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        clockverse_engine::vss::list_shadow_copies().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn vss_restore(
    device_object: String,
    relative_path: String,
    destination: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dest = std::path::Path::new(&destination);
        clockverse_engine::vss::restore_file_from_shadow(&device_object, &relative_path, dest)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn ssd_mine_shadows(
    volume: String,
    query: String,
) -> Result<Vec<clockverse_engine::shadow_miner::MinedShadowFile>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        clockverse_engine::shadow_miner::mine_shadow_files(&volume, &query)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn ssd_restore_mined_file(
    file: clockverse_engine::shadow_miner::MinedShadowFile,
    destination: String,
) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dest = std::path::Path::new(&destination);
        clockverse_engine::shadow_miner::restore_mined_file(&file, dest)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn relaunch_as_admin() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(Some(0)).collect();
        let verb_w: Vec<u16> = "runas\0".encode_utf16().collect();

        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                hwnd: isize,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show_cmd: i32,
            ) -> isize;
        }

        let res = unsafe {
            ShellExecuteW(
                0,
                verb_w.as_ptr(),
                exe_w.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1, // SW_SHOWNORMAL
            )
        };

        if res > 32 {
            std::process::exit(0);
        } else {
            Err("Elevation request was cancelled or failed.".into())
        }
    }
    #[cfg(not(windows))]
    {
        Err("Elevation is only supported on Windows.".into())
    }
}



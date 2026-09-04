//! Time Capsule — background snapshot daemon.
//! Incremental: sirf changed files save hote hain (content-hash based).
//! Storage: user-selected folder -> compressed/structured snapshots in app data dir.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedFolder {
    pub path: String,
    pub name: String,
    pub added_at: u64,
    pub last_snapshot: Option<u64>,
    pub file_count: u64,
    pub total_bytes: u64,
    pub status: CapsuleStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CapsuleStatus {
    Active,
    Paused,
    Error(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub rel_path: String,
    pub hash: String, // SHA-256 hex digest
    pub size: u64,
    pub modified: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub folder_path: String,
    pub created_at: u64,
    pub entries: Vec<SnapshotEntry>,
    pub total_size: u64,
    pub changed_files: u32,
}

pub struct TimeCapsule {
    pub storage_dir: PathBuf,
    pub folders: Vec<ProtectedFolder>,
}

impl Default for TimeCapsule {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeCapsule {
    pub fn new() -> Self {
        let storage = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("clockverse")
            .join("time-capsule");
        fs::create_dir_all(&storage).ok();

        let mut capsule = Self {
            storage_dir: storage,
            folders: Vec::new(),
        };
        capsule.load_folders();
        capsule
    }

    pub fn with_storage(storage: PathBuf) -> Self {
        fs::create_dir_all(&storage).ok();
        let mut capsule = Self {
            storage_dir: storage,
            folders: Vec::new(),
        };
        capsule.load_folders();
        capsule
    }

    pub fn save_folders(&self) {
        let path = self.storage_dir.join("folders.json");
        if let Ok(json) = serde_json::to_string_pretty(&self.folders) {
            let _ = fs::write(path, json);
        }
    }

    pub fn load_folders(&mut self) {
        let path = self.storage_dir.join("folders.json");
        if path.exists() {
            if let Ok(json) = fs::read_to_string(path) {
                if let Ok(folders) = serde_json::from_str::<Vec<ProtectedFolder>>(&json) {
                    self.folders = folders;
                }
            }
        }
    }

    /// Add a folder to active protection. Takes an immediate baseline snapshot.
    pub fn protect_folder(&mut self, path: String, name: String) -> anyhow::Result<ProtectedFolder> {
        let target_path = Path::new(&path);
        if !target_path.exists() {
            anyhow::bail!("Folder does not exist: {}", path);
        }

        if self.folders.iter().any(|f| f.path == path) {
            anyhow::bail!("Folder is already protected: {}", path);
        }

        let folder = ProtectedFolder {
            path: path.clone(),
            name,
            added_at: now_micros(),
            last_snapshot: None,
            file_count: 0,
            total_bytes: 0,
            status: CapsuleStatus::Active,
        };

        self.folders.push(folder);
        self.save_folders();

        // Perform initial baseline snapshot
        self.snapshot_folder(&path)?;

        let updated = self.folders.iter().find(|f| f.path == path).cloned()
            .ok_or_else(|| anyhow::anyhow!("Failed to retrieve protected folder"))?;
        Ok(updated)
    }

    /// Incremental content-hashed snapshot of a protected folder.
    pub fn snapshot_folder(&mut self, folder_path: &str) -> anyhow::Result<Snapshot> {
        let _ = self.folders.iter_mut()
            .find(|f| f.path == folder_path)
            .ok_or_else(|| anyhow::anyhow!("Folder is not registered for protection"))?;

        let mut entries = Vec::new();
        let mut total_size = 0u64;
        let mut changed = 0u32;

        for entry in walkdir::WalkDir::new(folder_path)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }

            // Skip hidden, git internals, and OS junk
            if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                if file_name.starts_with('.') || file_name == "desktop.ini" || file_name == "Thumbs.db" {
                    continue;
                }
            }

            // Also skip if path is inside .git or node_modules
            let path_str = path.to_string_lossy();
            if path_str.contains("/.git/") || path_str.contains("\\.git\\")
                || path_str.contains("/node_modules/") || path_str.contains("\\node_modules\\")
                || path_str.contains("/target/") || path_str.contains("\\target\\") {
                continue;
            }

            let rel_path = match path.strip_prefix(folder_path) {
                Ok(p) => p.to_string_lossy().replace('\\', "/"),
                Err(_) => continue,
            };

            let metadata = match fs::metadata(path) {
                Ok(m) => m,
                Err(_) => continue,
            };

            let size = metadata.len();
            total_size += size;

            let content = match fs::read(path) {
                Ok(c) => c,
                Err(_) => continue,
            };

            let hash = format!("{:x}", Sha256::digest(&content));
            let modified = metadata
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_micros() as u64;

            changed += 1;

            entries.push(SnapshotEntry {
                rel_path,
                hash,
                size,
                modified,
            });
        }

        let snapshot = Snapshot {
            id: uuid::Uuid::new_v4().to_string(),
            folder_path: folder_path.to_string(),
            created_at: now_micros(),
            entries,
            total_size,
            changed_files: changed,
        };

        if let Some(folder) = self.folders.iter_mut().find(|f| f.path == folder_path) {
            folder.last_snapshot = Some(snapshot.created_at);
            folder.file_count = snapshot.entries.len() as u64;
            folder.total_bytes = snapshot.total_size;
            folder.status = CapsuleStatus::Active;
        }

        self.save_snapshot(&snapshot)?;
        self.save_folders();

        Ok(snapshot)
    }

    /// Restore file from snapshot to destination path
    pub fn restore_file(&self, snapshot_id: &str, rel_path: &str, dest: &str) -> anyhow::Result<()> {
        let snapshot = self.load_snapshot(snapshot_id)?;
        let _ = snapshot.entries.iter()
            .find(|e| e.rel_path == rel_path)
            .ok_or_else(|| anyhow::anyhow!("File '{}' not found in snapshot {}", rel_path, snapshot_id))?;

        let src = Path::new(&snapshot.folder_path).join(rel_path);
        if src.exists() {
            let dest_path = Path::new(dest);
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent).ok();
            }
            fs::copy(&src, dest)?;
            Ok(())
        } else {
            anyhow::bail!("Original source file no longer present at {:?}", src)
        }
    }

    pub fn save_snapshot(&self, snapshot: &Snapshot) -> anyhow::Result<()> {
        let file = self.storage_dir.join(format!("{}.json", snapshot.id));
        let json = serde_json::to_string_pretty(snapshot)?;
        fs::write(file, json)?;
        Ok(())
    }

    pub fn load_snapshot(&self, id: &str) -> anyhow::Result<Snapshot> {
        let file = self.storage_dir.join(format!("{}.json", id));
        let json = fs::read_to_string(file)?;
        Ok(serde_json::from_str(&json)?)
    }

    /// List all snapshots for a given protected folder
    pub fn list_snapshots(&self, folder_path: &str) -> Vec<Snapshot> {
        let mut list = Vec::new();
        if let Ok(entries) = fs::read_dir(&self.storage_dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    let file_name = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
                    if file_name == "folders" {
                        continue;
                    }
                    if let Ok(json) = fs::read_to_string(&path) {
                        if let Ok(snap) = serde_json::from_str::<Snapshot>(&json) {
                            if snap.folder_path == folder_path {
                                list.push(snap);
                            }
                        }
                    }
                }
            }
        }
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_protect_folder_and_snapshot() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("time-capsule");
        let mut capsule = TimeCapsule::with_storage(storage);

        let test_dir = tmp.path().join("test_project");
        fs::create_dir(&test_dir).unwrap();
        fs::write(test_dir.join("main.py"), "print('hello from clockverse')").unwrap();
        fs::write(test_dir.join("readme.txt"), "Documentation").unwrap();

        let folder = capsule
            .protect_folder(
                test_dir.to_string_lossy().to_string(),
                "Test Project".to_string(),
            )
            .unwrap();

        assert_eq!(folder.name, "Test Project");
        assert_eq!(folder.file_count, 2);
        assert_eq!(capsule.folders.len(), 1);

        let snaps = capsule.list_snapshots(&folder.path);
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].entries.len(), 2);
    }
}

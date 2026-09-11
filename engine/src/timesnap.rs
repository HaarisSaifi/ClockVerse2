//! Versioned folder backups, with chunk deduplication and verified atomic restore.
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;
const SPACE_RESERVE: u64 = 256 * 1024 * 1024;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtectedFolder {
    pub path: String,
    pub name: String,
    pub added_at: u64,
    pub last_snapshot: Option<u64>,
    #[serde(default)]
    pub last_attempt: Option<u64>,
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
pub struct ChunkRef {
    pub hash: String,
    pub size: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub rel_path: String,
    pub hash: String,
    pub size: u64,
    pub modified: u64,
    // None reads legacy whole-file objects; Some supports chunked files including empty files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunks: Option<Vec<ChunkRef>>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    pub folder_path: String,
    pub created_at: u64,
    pub entries: Vec<SnapshotEntry>,
    pub total_size: u64,
    pub changed_files: u32,
    #[serde(default)]
    pub directories: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotPolicy {
    pub interval_minutes: u64,
    pub max_storage_bytes: u64,
    pub keep_versions: usize,
    pub prune_old_versions: bool,
}
impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self {
            interval_minutes: 30,
            max_storage_bytes: 100 * 1024 * 1024 * 1024,
            keep_versions: 50,
            prune_old_versions: false,
        }
    }
}
impl SnapshotPolicy {
    fn validate(&self) -> Result<()> {
        ensure!(
            (5..=1440).contains(&self.interval_minutes),
            "Interval must be 5–1440 minutes"
        );
        ensure!(
            (1..=1000).contains(&self.keep_versions),
            "Keep 1–1000 versions"
        );
        ensure!(
            (SPACE_RESERVE..=1024u64.pow(5)).contains(&self.max_storage_bytes),
            "Storage budget must be between 256 MiB and 1 PiB"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct SnapshotProgress {
    pub phase: String,
    pub files: u64,
    pub bytes_processed: u64,
    pub bytes_stored: u64,
}
#[derive(Debug, Serialize)]
pub struct FolderEstimate {
    pub files: u64,
    pub directories: u64,
    pub total_bytes: u64,
    pub excluded_entries: u64,
    pub free_bytes: u64,
    pub budget_remaining: u64,
    pub conservative_fits: bool,
}
#[derive(Debug, Serialize)]
pub struct MaintenanceReport {
    pub objects: u64,
    pub bytes: u64,
    pub damaged: Vec<String>,
}
pub struct TimeCapsule {
    pub storage_dir: PathBuf,
    pub folders: Vec<ProtectedFolder>,
    pub policy: SnapshotPolicy,
    pub load_error: Option<String>,
    lock_file: Option<File>,
}
impl Default for TimeCapsule {
    fn default() -> Self {
        Self::new()
    }
}
impl TimeCapsule {
    pub fn new() -> Self {
        Self::with_storage(
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("clockverse")
                .join("time-capsule"),
        )
    }
    pub fn unavailable(storage: PathBuf, error: String) -> Self {
        Self {
            storage_dir: storage,
            folders: vec![],
            policy: SnapshotPolicy::default(),
            load_error: Some(error),
            lock_file: None,
        }
    }
    pub fn with_storage(storage: PathBuf) -> Self {
        let mut value = Self {
            storage_dir: storage,
            folders: vec![],
            policy: SnapshotPolicy::default(),
            load_error: None,
            lock_file: None,
        };
        if let Err(e) = value.initialize() {
            value.load_error = Some(e.to_string());
        }
        value
    }
    fn initialize(&mut self) -> Result<()> {
        fs::create_dir_all(&self.storage_dir)?;
        self.storage_dir = fs::canonicalize(&self.storage_dir)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.storage_dir.join(".repository.lock"))?;
        lock.try_lock()
            .map_err(|e| anyhow::anyhow!("Backup repository is in use or cannot be locked: {e}"))?;
        self.lock_file = Some(lock);
        fs::create_dir_all(self.storage_dir.join("objects"))?;
        ensure!(
            fs::canonicalize(self.storage_dir.join("objects"))?.parent()
                == Some(self.storage_dir.as_path()),
            "Objects directory must remain inside repository"
        );
        self.folders = read_config(&self.storage_dir.join("folders.json"))?.unwrap_or_default();
        self.policy = read_config(&self.storage_dir.join("policy.json"))?.unwrap_or_default();
        self.policy.validate()?;
        Ok(())
    }
    fn ready(&self) -> Result<()> {
        ensure!(
            self.load_error.is_none() && self.lock_file.is_some(),
            "{}",
            self.load_error
                .as_deref()
                .unwrap_or("Repository lock unavailable")
        );
        Ok(())
    }
    pub fn save_folders(&self) -> Result<()> {
        self.ready()?;
        atomic_json(&self.storage_dir.join("folders.json"), &self.folders)
    }
    pub fn load_folders(&mut self) {
        match read_config(&self.storage_dir.join("folders.json")) {
            Ok(value) => self.folders = value.unwrap_or_default(),
            Err(e) => self.load_error = Some(e.to_string()),
        }
    }
    pub fn set_policy(&mut self, policy: SnapshotPolicy) -> Result<()> {
        self.ready()?;
        policy.validate()?;
        atomic_json(&self.storage_dir.join("policy.json"), &policy)?;
        self.policy = policy;
        Ok(())
    }
    pub fn set_paused(&mut self, path: &str, paused: bool) -> Result<()> {
        self.ready()?;
        let folder = self
            .folders
            .iter_mut()
            .find(|f| f.path == path)
            .context("Folder not registered")?;
        let old = folder.status.clone();
        folder.status = if paused {
            CapsuleStatus::Paused
        } else {
            CapsuleStatus::Active
        };
        if let Err(e) = self.save_folders() {
            self.folders
                .iter_mut()
                .find(|f| f.path == path)
                .unwrap()
                .status = old;
            return Err(e);
        }
        Ok(())
    }
    pub fn storage_bytes(&self) -> Result<u64> {
        self.ready()?;
        let mut size = 0u64;
        for e in fs::read_dir(self.storage_dir.join("objects"))? {
            let e = e?;
            if e.file_type()?.is_file() {
                size = size
                    .checked_add(e.metadata()?.len())
                    .context("Storage size overflow")?;
            }
        }
        Ok(size)
    }
    fn check_source(&self, path: &Path) -> Result<PathBuf> {
        self.ready()?;
        let source = fs::canonicalize(path).context("Protected folder is unavailable")?;
        ensure!(source.is_dir(), "Source must be a folder");
        ensure!(
            !self.storage_dir.starts_with(&source) && !source.starts_with(&self.storage_dir),
            "Protected folder and snapshot storage must not overlap"
        );
        Ok(source)
    }
    pub fn estimate(&self, path: &Path, cancel: &AtomicBool) -> Result<FolderEstimate> {
        let source = self.check_source(path)?;
        let mut total = 0u64;
        let mut files = 0;
        let mut directories = 0;
        let mut excluded = 0;
        for entry in walker(&source) {
            cancelled(cancel)?;
            let entry = entry?;
            if entry.file_type().is_file() {
                if excluded_file(entry.path()) {
                    excluded += 1;
                    continue;
                }
                total = total
                    .checked_add(entry.metadata()?.len())
                    .context("Folder size overflow")?;
                files += 1;
            } else if entry.file_type().is_dir() {
                directories += 1;
            } else {
                excluded += 1;
            }
        }
        let free = available_space(&self.storage_dir)?;
        let remaining = self
            .policy
            .max_storage_bytes
            .saturating_sub(self.storage_bytes()?);
        Ok(FolderEstimate {
            files,
            directories,
            total_bytes: total,
            excluded_entries: excluded,
            free_bytes: free,
            budget_remaining: remaining,
            conservative_fits: total <= remaining && total.saturating_add(SPACE_RESERVE) <= free,
        })
    }
    pub fn protect_folder(&mut self, path: String, name: String) -> Result<ProtectedFolder> {
        self.protect_controlled(path, name, &AtomicBool::new(false), |_| {})
    }
    pub fn protect_controlled(
        &mut self,
        path: String,
        name: String,
        cancel: &AtomicBool,
        progress: impl FnMut(SnapshotProgress),
    ) -> Result<ProtectedFolder> {
        let canonical = self.check_source(Path::new(&path))?;
        ensure!(
            !self
                .folders
                .iter()
                .any(|f| fs::canonicalize(&f.path).ok().as_ref() == Some(&canonical)),
            "Folder already protected"
        );
        self.folders.push(ProtectedFolder {
            path: path.clone(),
            name,
            added_at: now_micros(),
            last_snapshot: None,
            last_attempt: None,
            file_count: 0,
            total_bytes: 0,
            status: CapsuleStatus::Active,
        });
        if let Err(e) = self.snapshot_controlled(&path, cancel, progress) {
            self.folders.retain(|f| f.path != path);
            self.save_folders()?;
            return Err(e);
        }
        self.folders
            .iter()
            .find(|f| f.path == path)
            .cloned()
            .context("Folder registration failed")
    }
    pub fn snapshot_folder(&mut self, path: &str) -> Result<Snapshot> {
        self.snapshot_controlled(path, &AtomicBool::new(false), |_| {})
    }
    pub fn snapshot_controlled(
        &mut self,
        folder_path: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(SnapshotProgress),
    ) -> Result<Snapshot> {
        self.ready()?;
        let record = self
            .folders
            .iter_mut()
            .find(|f| f.path == folder_path)
            .context("Folder not registered for protection")?;
        let paused = record.status == CapsuleStatus::Paused;
        record.last_attempt = Some(now_micros());
        let source = self.check_source(Path::new(folder_path))?;
        let previous = self.list_snapshots(folder_path).into_iter().next();
        let mut stored = self.storage_bytes()?;
        let before = stored;
        let mut entries = vec![];
        let mut directories = vec![];
        let mut total = 0u64;
        let mut verified = HashSet::new();
        let mut buffer = vec![0u8; CHUNK_SIZE];
        let mut last_emit = Instant::now();
        progress(SnapshotProgress {
            phase: "Capturing file chunks".into(),
            files: 0,
            bytes_processed: 0,
            bytes_stored: 0,
        });
        for entry in walker(&source) {
            cancelled(cancel)?;
            let entry = entry?;
            let path = entry.path();
            let rel = path
                .strip_prefix(&source)?
                .to_str()
                .context("Filename cannot be represented as UTF-8")?
                .replace('\\', "/");
            if entry.file_type().is_dir() {
                if !rel.is_empty() {
                    directories.push(rel);
                }
                continue;
            }
            if !entry.file_type().is_file() || excluded_file(path) {
                continue;
            }
            let mut file = File::open(path)?;
            let metadata = file.metadata()?;
            let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_micros() as u64;
            let mut file_hash = Sha256::new();
            let mut chunks = vec![];
            let mut read = 0u64;
            loop {
                cancelled(cancel)?;
                let mut n = 0;
                while n < buffer.len() {
                    let got = file.read(&mut buffer[n..])?;
                    if got == 0 {
                        break;
                    }
                    n += got;
                }
                if n == 0 {
                    break;
                }
                file_hash.update(&buffer[..n]);
                let hash = format!("{:x}", Sha256::digest(&buffer[..n]));
                let obj = self.storage_dir.join("objects").join(&hash);
                if !verified.contains(&hash) {
                    if obj.exists() {
                        ensure!(
                            fs::symlink_metadata(&obj)?.file_type().is_file(),
                            "Snapshot object is not a regular file"
                        );
                        let (size, actual) = read_and_verify_object(&obj, cancel)?;
                        ensure!(size==n as u64 && actual==hash,"Stored object is corrupt: {hash}. Run integrity audit; no snapshot was published.");
                    } else {
                        let compressed = zstd::encode_all(&buffer[..n], 3)?;
                        let comp_len = compressed.len() as u64;
                        ensure!(stored.saturating_add(comp_len)<=self.policy.max_storage_bytes,"Snapshot storage budget reached. Increase budget or clean unused objects; previous restore points are safe.");
                        ensure!(
                            available_space(&self.storage_dir)? > comp_len + SPACE_RESERVE,
                            "Backup drive is low on space; previous restore points are safe"
                        );
                        let mut temp =
                            tempfile::NamedTempFile::new_in(self.storage_dir.join("objects"))?;
                        temp.write_all(&compressed)?;
                        temp.as_file().sync_all()?;
                        temp.persist_noclobber(&obj)?;
                        stored += comp_len;
                    }
                    verified.insert(hash.clone());
                }
                chunks.push(ChunkRef {
                    hash,
                    size: n as u64,
                });
                read = read.checked_add(n as u64).context("File size overflow")?;
                if last_emit.elapsed().as_millis() >= 200 {
                    progress(SnapshotProgress {
                        phase: "Capturing file chunks".into(),
                        files: entries.len() as u64,
                        bytes_processed: total + read,
                        bytes_stored: stored - before,
                    });
                    last_emit = Instant::now();
                }
            }
            let after = file.metadata()?;
            ensure!(
                read == metadata.len()
                    && after.len() == metadata.len()
                    && after.modified()? == metadata.modified()?,
                "File changed during capture: {}. Retry when the file is stable.",
                path.display()
            );
            total = total.checked_add(read).context("Folder size overflow")?;
            entries.push(SnapshotEntry {
                rel_path: rel,
                hash: format!("{:x}", file_hash.finalize()),
                size: read,
                modified,
                chunks: Some(chunks),
            });
        }
        cancelled(cancel)?;
        entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        directories.sort();
        let unchanged = previous.as_ref().is_some_and(|p| {
            p.directories == directories
                && p.entries.len() == entries.len()
                && p.entries.iter().zip(&entries).all(|(a, b)| {
                    a.rel_path == b.rel_path
                        && a.hash == b.hash
                        && a.modified == b.modified
                        && a.size == b.size
                })
        });
        let old: HashMap<&str, &str> = previous
            .as_ref()
            .map(|p| {
                p.entries
                    .iter()
                    .map(|e| (e.rel_path.as_str(), e.hash.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        let changed = entries
            .iter()
            .filter(|e| old.get(e.rel_path.as_str()).copied() != Some(e.hash.as_str()))
            .count() as u32;
        let snapshot = if unchanged {
            previous.unwrap()
        } else {
            Snapshot {
                id: uuid::Uuid::new_v4().to_string(),
                folder_path: folder_path.into(),
                created_at: now_micros(),
                entries,
                total_size: total,
                changed_files: changed,
                directories,
            }
        };
        if !unchanged {
            self.save_snapshot(&snapshot)?;
        }
        if let Some(record) = self.folders.iter_mut().find(|f| f.path == folder_path) {
            record.last_snapshot = Some(now_micros());
            record.file_count = snapshot.entries.len() as u64;
            record.total_bytes = total;
            record.status = if paused {
                CapsuleStatus::Paused
            } else {
                CapsuleStatus::Active
            };
        }
        self.save_folders()?;
        if !unchanged && self.policy.prune_old_versions {
            self.apply_retention(folder_path)?;
            self.cleanup(&AtomicBool::new(false))?;
        }
        progress(SnapshotProgress {
            phase: if unchanged {
                "Unchanged version reused".into()
            } else {
                "Snapshot saved".into()
            },
            files: snapshot.entries.len() as u64,
            bytes_processed: total,
            bytes_stored: stored - before,
        });
        Ok(snapshot)
    }
    pub fn restore_file(&self, id: &str, rel: &str, dest: &str) -> Result<u64> {
        self.restore_controlled(id, rel, Path::new(dest), &AtomicBool::new(false))
    }
    pub fn restore_controlled(
        &self,
        id: &str,
        rel: &str,
        dest: &Path,
        cancel: &AtomicBool,
    ) -> Result<u64> {
        self.ready()?;
        let snapshot = self.load_snapshot(id)?;
        let entry = snapshot
            .entries
            .iter()
            .find(|e| e.rel_path == rel)
            .context("File not in snapshot")?;
        self.restore_entry(entry, dest, cancel)
    }
    fn restore_entry(
        &self,
        entry: &SnapshotEntry,
        dest: &Path,
        cancel: &AtomicBool,
    ) -> Result<u64> {
        validate_entry(entry)?;
        let parent = dest
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        ensure!(
            !fs::canonicalize(parent)?.starts_with(&self.storage_dir),
            "Cannot restore over backup storage"
        );
        ensure!(
            available_space(parent)? > entry.size.saturating_add(SPACE_RESERVE),
            "Not enough free space to restore this file safely"
        );
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        let mut whole = Sha256::new();
        let mut total = 0u64;
        let legacy = vec![ChunkRef {
            hash: entry.hash.clone(),
            size: entry.size,
        }];
        let refs = entry.chunks.as_ref().unwrap_or(&legacy);
        for part in refs {
            cancelled(cancel)?;
            let path = self.storage_dir.join("objects").join(&part.hash);
            ensure!(
                fs::symlink_metadata(&path)?.file_type().is_file(),
                "Snapshot object is not a regular file"
            );
            let chunk_data = read_chunk_bytes(&path)?;
            ensure!(
                chunk_data.len() as u64 == part.size,
                "Snapshot object size mismatch"
            );
            ensure!(
                format!("{:x}", Sha256::digest(&chunk_data)) == part.hash,
                "Snapshot object failed integrity verification"
            );
            whole.update(&chunk_data);
            temp.write_all(&chunk_data)?;
            total += chunk_data.len() as u64;
        }
        ensure!(
            total == entry.size && format!("{:x}", whole.finalize()) == entry.hash,
            "Snapshot file failed integrity verification"
        );
        temp.as_file().sync_all()?;
        temp.persist(dest)?;
        Ok(total)
    }
    pub fn rollback_folder(&self, path: &str, id: &str) -> Result<u32> {
        self.export_controlled(Path::new(path), id, &AtomicBool::new(false), |_| {})
    }
    pub fn export_controlled(
        &self,
        path: &Path,
        id: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(SnapshotProgress),
    ) -> Result<u32> {
        self.ready()?;
        let snapshot = self.load_snapshot(id)?;
        for e in &snapshot.entries {
            validate_entry(e)?;
        }
        for d in &snapshot.directories {
            validate_relative(d)?;
        }
        fs::create_dir_all(path)?;
        let root = fs::canonicalize(path)?;
        ensure!(
            !root.starts_with(&self.storage_dir),
            "Export must be outside snapshot storage"
        );
        ensure!(
            available_space(&root)? > snapshot.total_size.saturating_add(SPACE_RESERVE),
            "Export needs more free space"
        );
        let mut count = 0;
        let mut bytes = 0;
        for dir in &snapshot.directories {
            let dest = safe_restore_path(&root, dir)?;
            fs::create_dir_all(dest)?;
        }
        for entry in &snapshot.entries {
            cancelled(cancel)?;
            let dest = safe_restore_path(&root, &entry.rel_path)?;
            bytes += self.restore_entry(entry, &dest, cancel)?;
            count += 1;
            progress(SnapshotProgress {
                phase: "Exporting verified files".into(),
                files: count as u64,
                bytes_processed: bytes,
                bytes_stored: 0,
            });
        }
        Ok(count)
    }
    pub fn save_snapshot(&self, snapshot: &Snapshot) -> Result<()> {
        self.ready()?;
        uuid::Uuid::parse_str(&snapshot.id)?;
        atomic_json(
            &self.storage_dir.join(format!("{}.json", snapshot.id)),
            snapshot,
        )
    }
    pub fn load_snapshot(&self, id: &str) -> Result<Snapshot> {
        self.ready()?;
        uuid::Uuid::parse_str(id)?;
        let snapshot: Snapshot =
            serde_json::from_reader(File::open(self.storage_dir.join(format!("{id}.json")))?)?;
        ensure!(snapshot.id == id, "Snapshot identifier mismatch");
        Ok(snapshot)
    }
    fn snapshot_ids(&self) -> Result<Vec<String>> {
        self.ready()?;
        let mut ids = vec![];
        for entry in fs::read_dir(&self.storage_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .context("Invalid manifest filename")?;
                if matches!(stem, "folders" | "policy") {
                    continue;
                }
                ensure!(
                    entry.file_type()?.is_file(),
                    "Manifest is not a regular file"
                );
                uuid::Uuid::parse_str(stem).context("Unknown manifest; maintenance stopped")?;
                ids.push(stem.into());
            }
        }
        Ok(ids)
    }
    pub fn list_snapshots(&self, path: &str) -> Vec<Snapshot> {
        let mut list: Vec<_> = self
            .snapshot_ids()
            .unwrap_or_default()
            .into_iter()
            .filter_map(|id| self.load_snapshot(&id).ok())
            .filter(|s| s.folder_path == path)
            .collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        list
    }
    fn referenced_objects(&self) -> Result<HashMap<String, u64>> {
        let mut refs = HashMap::new();
        for id in self.snapshot_ids()? {
            let snapshot = self.load_snapshot(&id)?;
            for e in snapshot.entries {
                validate_entry(&e)?;
                let parts = e.chunks.unwrap_or_else(|| {
                    vec![ChunkRef {
                        hash: e.hash,
                        size: e.size,
                    }]
                });
                for p in parts {
                    if let Some(old) = refs.insert(p.hash, p.size) {
                        ensure!(old == p.size, "Conflicting object lengths");
                    }
                }
            }
        }
        Ok(refs)
    }
    fn apply_retention(&self, path: &str) -> Result<()> {
        // Validate EVERY manifest before removing any restore point.
        self.referenced_objects()?;
        for snap in self
            .list_snapshots(path)
            .into_iter()
            .skip(self.policy.keep_versions)
        {
            fs::remove_file(self.storage_dir.join(format!("{}.json", snap.id)))?;
        }
        Ok(())
    }
    pub fn audit(&self, cancel: &AtomicBool) -> Result<MaintenanceReport> {
        let refs = self.referenced_objects()?;
        let mut result = MaintenanceReport {
            objects: 0,
            bytes: 0,
            damaged: vec![],
        };
        for (hash, size) in refs {
            cancelled(cancel)?;
            match read_and_verify_object(&self.storage_dir.join("objects").join(&hash), cancel) {
                Ok((n, h)) if n == size && h == hash => {
                    result.objects += 1;
                    result.bytes += n;
                }
                _ => result.damaged.push(hash),
            }
            cancelled(cancel)?;
        }
        Ok(result)
    }
    pub fn cleanup(&self, cancel: &AtomicBool) -> Result<MaintenanceReport> {
        let refs = self.referenced_objects()?;
        let objects = fs::canonicalize(self.storage_dir.join("objects"))?;
        ensure!(
            objects.parent() == Some(self.storage_dir.as_path()),
            "Object directory resolves outside repository"
        );
        let mut result = MaintenanceReport {
            objects: 0,
            bytes: 0,
            damaged: vec![],
        };
        for entry in fs::read_dir(&objects)? {
            cancelled(cancel)?;
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if valid_hash(&name) && entry.file_type()?.is_file() && !refs.contains_key(&name) {
                let size = entry.metadata()?.len();
                fs::remove_file(entry.path())?;
                result.objects += 1;
                result.bytes += size;
            }
        }
        Ok(result)
    }
    /// Copy, verify, then switch repositories. The original repository is never deleted.
    pub fn copy_repository(
        &self,
        dest: &Path,
        cancel: &AtomicBool,
        mut progress: impl FnMut(SnapshotProgress),
    ) -> Result<TimeCapsule> {
        self.ready()?;
        ensure!(!dest.exists(), "Choose a new backup repository directory");
        let parent = fs::canonicalize(dest.parent().context("Destination parent missing")?)?;
        ensure!(
            !parent.starts_with(&self.storage_dir),
            "Cannot nest backup storage"
        );
        for folder in &self.folders {
            if let Ok(source) = fs::canonicalize(&folder.path) {
                ensure!(
                    !parent.starts_with(&source),
                    "Backup location must be outside protected folders"
                );
            }
        }
        let audit = self.audit(cancel)?;
        ensure!(
            audit.damaged.is_empty(),
            "Fix corrupt or missing objects before moving backup storage"
        );
        ensure!(
            available_space(&parent)? > self.storage_bytes()?.saturating_add(SPACE_RESERVE),
            "New backup drive lacks space"
        );
        let mut target = TimeCapsule::with_storage(dest.into());
        target.ready()?;
        let mut bytes = 0;
        let mut count = 0;
        let refs = self.referenced_objects()?;
        for (hash, expected) in refs {
            cancelled(cancel)?;
            let source = self.storage_dir.join("objects").join(&hash);
            let (decompressed_size, decompressed_hash) = read_and_verify_object(&source, cancel)?;
            ensure!(
                decompressed_size == expected && decompressed_hash == hash,
                "Object changed during repository copy"
            );
            let mut input = File::open(&source)?;
            let mut temp = tempfile::NamedTempFile::new_in(target.storage_dir.join("objects"))?;
            let mut buf = vec![0u8; 256 * 1024];
            let mut size = 0;
            loop {
                cancelled(cancel)?;
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                temp.write_all(&buf[..n])?;
                size += n as u64;
            }
            temp.as_file().sync_all()?;
            temp.persist_noclobber(target.storage_dir.join("objects").join(hash))?;
            bytes += size;
            count += 1;
            progress(SnapshotProgress {
                phase: "Copying verified backup storage".into(),
                files: count,
                bytes_processed: bytes,
                bytes_stored: bytes,
            });
        }
        for id in self.snapshot_ids()? {
            target.save_snapshot(&self.load_snapshot(&id)?)?;
        }
        target.folders = self.folders.clone();
        target.save_folders()?;
        target.set_policy(self.policy.clone())?;
        Ok(target)
    }
}
fn walker(
    path: &Path,
) -> impl Iterator<Item = std::result::Result<walkdir::DirEntry, walkdir::Error>> {
    walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            e.depth() == 0
                || !e.file_type().is_dir()
                || !matches!(
                    e.file_name().to_str(),
                    Some(".git" | "node_modules" | "target")
                )
        })
}
fn excluded_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|s| s.to_str()),
        Some("desktop.ini" | "Thumbs.db")
    )
}
fn cancelled(cancel: &AtomicBool) -> Result<()> {
    ensure!(
        !cancel.load(Ordering::Relaxed),
        "Cancelled; completed restore points are preserved"
    );
    Ok(())
}
fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn validate_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', ':'])
            && path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != ".."),
        "Unsafe snapshot path"
    );
    Ok(())
}
fn validate_entry(entry: &SnapshotEntry) -> Result<()> {
    validate_relative(&entry.rel_path)?;
    ensure!(valid_hash(&entry.hash), "Invalid snapshot hash");
    if let Some(parts) = &entry.chunks {
        let mut total = 0u64;
        for p in parts {
            ensure!(
                valid_hash(&p.hash) && p.size > 0 && p.size <= CHUNK_SIZE as u64,
                "Invalid chunk reference"
            );
            total = total.checked_add(p.size).context("Chunk size overflow")?;
        }
        ensure!(total == entry.size, "Chunk/file size mismatch");
    }
    Ok(())
}
fn safe_restore_path(root: &Path, rel: &str) -> Result<PathBuf> {
    validate_relative(rel)?;
    let dest = root.join(rel);
    let mut ancestor = dest.parent();
    while let Some(p) = ancestor {
        if p.exists() {
            ensure!(
                fs::canonicalize(p)?.starts_with(root),
                "Restore path escapes destination"
            );
            break;
        }
        ancestor = p.parent();
    }
    Ok(dest)
}
fn read_config<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match File::open(path) {
        Ok(file) => Ok(Some(serde_json::from_reader(file).with_context(|| {
            format!("Corrupt configuration preserved at {}", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}
pub fn atomic_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("Path has no parent")?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut temp, value)?;
    temp.as_file().sync_all()?;
    temp.persist(path)?;
    Ok(())
}
pub fn read_chunk_bytes(path: &Path) -> Result<Vec<u8>> {
    let raw = fs::read(path)?;
    if raw.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Ok(zstd::decode_all(&raw[..])?)
    } else {
        Ok(raw)
    }
}

pub fn read_and_verify_object(path: &Path, cancel: &AtomicBool) -> Result<(u64, String)> {
    cancelled(cancel)?;
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "Object is not a regular file"
    );
    let decompressed = read_chunk_bytes(path)?;
    let hash = format!("{:x}", Sha256::digest(&decompressed));
    Ok((decompressed.len() as u64, hash))
}

#[allow(dead_code)]
pub fn hash_file(path: &Path, cancel: &AtomicBool) -> Result<(u64, String)> {
    ensure!(
        fs::symlink_metadata(path)?.file_type().is_file(),
        "Object is not a regular file"
    );
    let mut file = File::open(path)?;
    let mut buf = vec![0; 256 * 1024];
    let mut digest = Sha256::new();
    let mut size = 0u64;
    loop {
        cancelled(cancel)?;
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, format!("{:x}", digest.finalize())))
}
#[cfg(windows)]
pub fn available_space(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            path: *const u16,
            available: *mut u64,
            total: *mut u64,
            free: *mut u64,
        ) -> i32;
    }
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut available = 0;
    ensure!(
        unsafe {
            GetDiskFreeSpaceExW(
                path.as_ptr(),
                &mut available,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        } != 0,
        "Cannot determine available backup space: {}",
        std::io::Error::last_os_error()
    );
    Ok(available)
}
#[cfg(not(windows))]
pub fn available_space(_path: &Path) -> Result<u64> {
    anyhow::bail!("Free-space inspection currently supports Windows only")
}
fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64
}

include!("timesnap_tests.rs");

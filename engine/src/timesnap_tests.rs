#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;



    #[test]
    fn explicitly_selected_build_named_root_is_captured() {
        let tmp=TempDir::new().unwrap();let source=tmp.path().join("target");fs::create_dir(&source).unwrap();fs::write(source.join("important"),b"keep").unwrap();
        let mut c=TimeCapsule::with_storage(tmp.path().join("store"));let f=c.protect_folder(source.to_string_lossy().into(),"selected".into()).unwrap();assert_eq!(f.file_count,1);
    }
    #[test]
    fn legacy_whole_file_manifest_restores_without_conversion() {
        let tmp=TempDir::new().unwrap();let c=TimeCapsule::with_storage(tmp.path().join("store"));let data=b"legacy content";let hash=format!("{:x}",Sha256::digest(data));
        fs::write(c.storage_dir.join("objects").join(&hash),data).unwrap();let id=uuid::Uuid::new_v4().to_string();
        let json=serde_json::json!({"id":id,"folder_path":"old", "created_at":1,"entries":[{"rel_path":"old.txt","hash":hash,"size":data.len(),"modified":1}],"total_size":data.len(),"changed_files":1});
        fs::write(c.storage_dir.join(format!("{id}.json")),serde_json::to_vec(&json).unwrap()).unwrap();let out=tmp.path().join("restored");c.restore_file(&id,"old.txt",out.to_str().unwrap()).unwrap();assert_eq!(fs::read(out).unwrap(),data);
    }
    #[test]
    fn repository_lock_releases_only_after_owner_drops() {
        let tmp=TempDir::new().unwrap(); let path=tmp.path().join("store");
        let first=TimeCapsule::with_storage(path.clone()); assert!(first.load_error.is_none());
        let second=TimeCapsule::with_storage(path.clone()); assert!(second.load_error.is_some());
        drop(second); drop(first);
        assert!(TimeCapsule::with_storage(path).load_error.is_none());
    }
    #[test]
    fn changed_chunk_deduplicates_and_both_versions_restore() {
        use std::io::{Seek,SeekFrom};
        let tmp=TempDir::new().unwrap(); let source=tmp.path().join("source"); fs::create_dir(&source).unwrap();
        let file=source.join("large.bin"); let original=vec![7u8;CHUNK_SIZE*3]; fs::write(&file,&original).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        let mut c=TimeCapsule::with_storage(tmp.path().join("store"));let f=c.protect_folder(source.to_string_lossy().into(),"test".into()).unwrap();
        let old=c.list_snapshots(&f.path).remove(0);
        let first_bytes = c.storage_bytes().unwrap();
        assert!(first_bytes > 0 && first_bytes < CHUNK_SIZE as u64);
        let mut handle=fs::OpenOptions::new().write(true).open(&file).unwrap();handle.seek(SeekFrom::Start(CHUNK_SIZE as u64+10)).unwrap();handle.write_all(b"changed").unwrap();drop(handle);
        let new=c.snapshot_folder(&f.path).unwrap();assert_ne!(old.id,new.id);
        assert!(c.storage_bytes().unwrap() > first_bytes);
        fs::remove_file(&file).unwrap();
        let out=tmp.path().join("restored");c.rollback_folder(out.to_str().unwrap(),&old.id).unwrap();assert_eq!(fs::read(out.join("large.bin")).unwrap(),original);assert!(out.join("empty").is_dir());
        let out2=tmp.path().join("new.bin");c.restore_file(&new.id,"large.bin",out2.to_str().unwrap()).unwrap();let b=fs::read(out2).unwrap();assert_eq!(&b[CHUNK_SIZE+10..CHUNK_SIZE+17],b"changed");
        assert!(c.audit(&AtomicBool::new(false)).unwrap().damaged.is_empty());
    }
    #[test]
    fn cleanup_keeps_referenced_data_and_fails_closed_on_bad_manifest() {
        let tmp=TempDir::new().unwrap();let source=tmp.path().join("src");fs::create_dir(&source).unwrap();fs::write(source.join("a"),b"safe").unwrap();
        let mut c=TimeCapsule::with_storage(tmp.path().join("store"));c.protect_folder(source.to_string_lossy().into(),"test".into()).unwrap();
        let orphan=c.storage_dir.join("objects").join(format!("{:x}",Sha256::digest(b"orphan")));fs::write(&orphan,b"orphan").unwrap();
        let bad=c.storage_dir.join(format!("{}.json",uuid::Uuid::new_v4()));fs::write(&bad,b"broken").unwrap();
        assert!(c.cleanup(&AtomicBool::new(false)).is_err());assert!(orphan.exists());fs::remove_file(bad).unwrap();
        let result=c.cleanup(&AtomicBool::new(false)).unwrap();assert_eq!(result.objects,1);assert_eq!(result.bytes,6);assert!(c.storage_bytes().unwrap() > 0);
    }
    #[test]
    fn migration_preserves_original_and_new_repository_restores() {
        let tmp=TempDir::new().unwrap();let source=tmp.path().join("src");fs::create_dir(&source).unwrap();fs::write(source.join("a"),b"migration").unwrap();
        let mut c=TimeCapsule::with_storage(tmp.path().join("store"));let f=c.protect_folder(source.to_string_lossy().into(),"test".into()).unwrap();let snapshot=c.list_snapshots(&f.path).remove(0);
        let next=c.copy_repository(&tmp.path().join("new"),&AtomicBool::new(false),|_|{}).unwrap();assert!(c.storage_dir.join(format!("{}.json",snapshot.id)).exists());
        next.restore_file(&snapshot.id,"a",tmp.path().join("out").to_str().unwrap()).unwrap();assert_eq!(fs::read(tmp.path().join("out")).unwrap(),b"migration");
    }
    #[test]
    fn cancelled_baseline_does_not_register_or_publish() {
        let tmp=TempDir::new().unwrap();let source=tmp.path().join("src");fs::create_dir(&source).unwrap();fs::write(source.join("a"),b"cancel").unwrap();
        let mut c=TimeCapsule::with_storage(tmp.path().join("store"));let cancel=AtomicBool::new(false);
        assert!(c.protect_controlled(source.to_string_lossy().into(),"test".into(),&cancel, |_| {cancel.store(true,Ordering::SeqCst);}).is_err());
        assert!(c.folders.is_empty());assert!(c.snapshot_ids().unwrap().is_empty());
    }
    #[test]
    #[ignore = "Reads and restores 1 GiB; run explicitly for large-file regression"]
    fn one_gibibyte_roundtrip() {
        let tmp=TempDir::new().unwrap();let source=tmp.path().join("src");fs::create_dir(&source).unwrap();let file=File::create(source.join("1GiB.bin")).unwrap();file.set_len(1024*1024*1024).unwrap();drop(file);
        let mut c=TimeCapsule::with_storage(tmp.path().join("store"));let f=c.protect_folder(source.to_string_lossy().into(),"large".into()).unwrap();let s=c.list_snapshots(&f.path).remove(0);
        assert_eq!(s.total_size,1024*1024*1024);assert_eq!(s.entries[0].chunks.as_ref().unwrap().len(),256);assert_eq!(c.storage_bytes().unwrap(),CHUNK_SIZE as u64);
        let out=tmp.path().join("restored.bin");c.restore_file(&s.id,"1GiB.bin",out.to_str().unwrap()).unwrap();assert_eq!(hash_file(&out,&AtomicBool::new(false)).unwrap(),hash_file(&source.join("1GiB.bin"),&AtomicBool::new(false)).unwrap());
    }
    #[test]
    fn corrupt_configuration_is_preserved_and_blocks_mutation() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("store");
        fs::create_dir(&storage).unwrap();
        fs::write(storage.join("folders.json"), "broken config").unwrap();
        let mut capsule = TimeCapsule::with_storage(storage.clone());
        assert!(capsule.load_error.is_some());
        assert!(capsule.set_policy(SnapshotPolicy::default()).is_err());
        assert!(capsule.save_folders().is_err());
        assert_eq!(
            fs::read_to_string(storage.join("folders.json")).unwrap(),
            "broken config"
        );
    }
    #[test]
    fn manual_snapshot_does_not_unpause_schedule() {
        let tmp = TempDir::new().unwrap();
        let mut capsule = TimeCapsule::with_storage(tmp.path().join("store"));
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        let folder = capsule
            .protect_folder(source.to_string_lossy().into(), "test".into())
            .unwrap();
        capsule.set_paused(&folder.path, true).unwrap();
        fs::write(source.join("new.txt"), "new").unwrap();
        capsule.snapshot_folder(&folder.path).unwrap();
        assert_eq!(capsule.folders[0].status, CapsuleStatus::Paused);
    }
    #[test]
    fn retention_is_opt_in_and_keeps_shared_content() {
        let tmp = TempDir::new().unwrap();
        let mut capsule = TimeCapsule::with_storage(tmp.path().join("store"));
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "one").unwrap();
        let folder = capsule
            .protect_folder(source.to_string_lossy().into(), "test".into())
            .unwrap();
        fs::write(source.join("file.txt"), "two").unwrap();
        capsule.snapshot_folder(&folder.path).unwrap();
        assert_eq!(capsule.list_snapshots(&folder.path).len(), 2);
        capsule.policy.prune_old_versions = true;
        capsule.policy.keep_versions = 1;
        fs::write(source.join("file.txt"), "three").unwrap();
        capsule.snapshot_folder(&folder.path).unwrap();
        assert_eq!(capsule.list_snapshots(&folder.path).len(), 1);
        let latest = capsule.list_snapshots(&folder.path).remove(0);
        let dest = tmp.path().join("restored.txt");
        capsule
            .restore_file(&latest.id, "file.txt", &dest.to_string_lossy())
            .unwrap();
        assert_eq!(fs::read_to_string(dest).unwrap(), "three");
    }
    #[test]
    fn policy_persists_and_unchanged_capture_reuses_version() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("store");
        let mut capsule = TimeCapsule::with_storage(storage.clone());
        let mut policy = SnapshotPolicy::default();
        policy.interval_minutes = 5;
        capsule.set_policy(policy).unwrap();
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join(".important"), "keep hidden files").unwrap();
        let folder = capsule
            .protect_folder(source.to_string_lossy().into(), "test".into())
            .unwrap();
        assert_eq!(folder.file_count, 1);
        let original = capsule.list_snapshots(&folder.path)[0].id.clone();
        assert_eq!(capsule.snapshot_folder(&folder.path).unwrap().id, original);
        assert_eq!(capsule.list_snapshots(&folder.path).len(), 1);
        capsule.set_paused(&folder.path, true).unwrap();
        drop(capsule);
        let loaded = TimeCapsule::with_storage(storage);
        assert_eq!(loaded.policy.interval_minutes, 5);
        assert_eq!(loaded.folders[0].status, CapsuleStatus::Paused);
    }

    #[test]
    fn snapshot_quota_failure_does_not_publish_manifest() {
        let tmp = TempDir::new().unwrap();
        let mut capsule = TimeCapsule::with_storage(tmp.path().join("store"));
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        let folder = capsule
            .protect_folder(source.to_string_lossy().into(), "test".into())
            .unwrap();
        capsule.policy.max_storage_bytes = 1;
        fs::write(source.join("large.txt"), "too much").unwrap();
        assert!(capsule.snapshot_folder(&folder.path).is_err());
        assert_eq!(capsule.list_snapshots(&folder.path).len(), 1);
    }

    #[test]
    fn rejects_storage_recursion_and_invalid_ids() {
        let tmp = TempDir::new().unwrap();
        let mut capsule = TimeCapsule::with_storage(tmp.path().join("store"));
        assert!(capsule
            .protect_folder(tmp.path().to_string_lossy().into(), "bad".into())
            .is_err());
        assert!(capsule.load_snapshot("../folders").is_err());
        assert!(capsule.folders.is_empty());
    }

    #[test]
    fn corrupt_object_preserves_destination_and_reports_failure() {
        let tmp = TempDir::new().unwrap();
        let mut capsule = TimeCapsule::with_storage(tmp.path().join("store"));
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file.txt"), "original").unwrap();
        let folder = capsule
            .protect_folder(source.to_string_lossy().into(), "source".into())
            .unwrap();
        let snap = capsule.list_snapshots(&folder.path).remove(0);
        fs::write(
            capsule
                .storage_dir
                .join("objects")
                .join(&snap.entries[0].hash),
            "broken",
        )
        .unwrap();
        fs::write(source.join("file.txt"), "keep me").unwrap();
        assert!(capsule.rollback_folder(&folder.path, &snap.id).is_err());
        assert_eq!(
            fs::read_to_string(source.join("file.txt")).unwrap(),
            "keep me"
        );
    }

    #[test]
    fn rejects_traversal_before_restoring_any_files() {
        let tmp = TempDir::new().unwrap();
        let capsule = TimeCapsule::with_storage(tmp.path().join("store"));
        let snap = Snapshot {
            id: uuid::Uuid::new_v4().to_string(),
            folder_path: "unused".into(),
            created_at: 0,
            directories: vec![],
            entries: vec![SnapshotEntry {
                chunks: None,
                rel_path: "../escape.txt".into(),
                hash: "a".repeat(64),
                size: 0,
                modified: 0,
            }],
            total_size: 0,
            changed_files: 0,
        };
        capsule.save_snapshot(&snap).unwrap();
        assert!(capsule
            .rollback_folder(&tmp.path().join("dest").to_string_lossy(), &snap.id)
            .is_err());
        assert!(!tmp.path().join("escape.txt").exists());
    }

    #[test]
    fn missing_source_does_not_publish_empty_snapshot() {
        let tmp = TempDir::new().unwrap();
        let mut capsule = TimeCapsule::with_storage(tmp.path().join("store"));
        let source = tmp.path().join("source");
        fs::create_dir(&source).unwrap();
        let folder = capsule
            .protect_folder(source.to_string_lossy().into(), "source".into())
            .unwrap();
        fs::remove_dir(&source).unwrap();
        assert!(capsule.snapshot_folder(&folder.path).is_err());
        assert_eq!(capsule.list_snapshots(&folder.path).len(), 1);
    }

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

    #[test]
    fn test_accidental_deletion_and_byte_exact_rollback() {
        let tmp = TempDir::new().unwrap();
        let storage = tmp.path().join("time-capsule-storage");
        let mut capsule = TimeCapsule::with_storage(storage);

        let test_dir = tmp.path().join("my_secret_code");
        fs::create_dir(&test_dir).unwrap();

        let original_code = "def compute(): return 42 * 1337";
        let original_doc = "# Important Secret Documentation";
        fs::write(test_dir.join("app.py"), original_code).unwrap();
        fs::write(test_dir.join("secret.md"), original_doc).unwrap();

        // 1. Protect folder (takes initial snapshot)
        let folder = capsule
            .protect_folder(
                test_dir.to_string_lossy().to_string(),
                "Secret Project".to_string(),
            )
            .unwrap();

        let snaps = capsule.list_snapshots(&folder.path);
        assert_eq!(snaps.len(), 1);
        let snap_id = &snaps[0].id;

        // 2. DISASTER STRIKES: An accidental deletion or corrupted overwrite occurs!
        // Delete app.py completely
        fs::remove_file(test_dir.join("app.py")).unwrap();
        assert!(!test_dir.join("app.py").exists());

        // Corrupt secret.md
        fs::write(test_dir.join("secret.md"), "CORRUPTED DATA DESTROYED").unwrap();

        // 3. RESURRECTION & ROLLBACK: Time Capsule brings back the exact data
        let restored = capsule.rollback_folder(&folder.path, snap_id).unwrap();
        assert_eq!(restored, 2);

        // 4. VERIFY 100% BYTE ACCURACY
        assert!(test_dir.join("app.py").exists());
        let recovered_code = fs::read_to_string(test_dir.join("app.py")).unwrap();
        assert_eq!(recovered_code, original_code);

        let recovered_doc = fs::read_to_string(test_dir.join("secret.md")).unwrap();
        assert_eq!(recovered_doc, original_doc);

        println!("[PASS] Byte-exact resurrection and rollback verified!");
    }
}

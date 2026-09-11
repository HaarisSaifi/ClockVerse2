# Snapshot storage decisions

## Capacity

1 GiB, 10 GiB and 50 GiB folders use the same streaming path. No fixed folder or individual-file cap is imposed by snapshots. The configurable quota measures unique content objects, with a 100 GiB default for new repositories. Allow the full initial source size plus filesystem/manifest overhead and 256 MiB free-space reserve; allow additional space for changed chunks retained by later versions. Existing settings do not silently increase on upgrade. Recovery scans have separate limits described in PRODUCTION_READINESS.md.

Chunks are fixed at 4 MiB. Identical chunks share one stored object. A change within one chunk adds that chunk, while inserted bytes can shift following chunks and cause greater storage use. Compression and encryption at rest are not implemented. Chunk byte buffers are bounded, but manifests and reference maps grow with file/chunk count. Disk quotas exclude JSON metadata. A folder with millions of small files needs separate memory/performance testing.

## Correctness

Objects and manifests are staged, synced and atomically published. Restore checks every chunk and the full-file SHA-256 and length before publishing each destination file. Whole-folder export is not transactional: completed files remain if later work fails. Old whole-file object snapshots remain readable. Capture checks per-file size and modification time before/after reading, but cannot guarantee application consistency across files. Close editing applications before capture. NTFS permissions, alternate streams and other filesystem metadata are not preserved.

Microsoft documents the VSS requester/writer/provider coordination needed for application-consistent snapshots: https://learn.microsoft.com/en-us/windows-server/storage/file-server/volume-shadow-copy-service . This implementation is a versioned folder backup, not a VSS implementation.

## Concurrency and cleanup

The repository holds an OS exclusive file lock for its lifetime. Rust documents lock behavior and lifetime at https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock . The desktop separately serializes recovery, capture, export and maintenance and supports cancellation.

Cleanup builds references from every retained manifest and aborts on malformed metadata before deleting anything. It deletes only regular hash-named unreferenced objects within the canonical object directory. This follows the distinction between forgetting versions and pruning unreferenced data described by restic: https://restic.readthedocs.io/en/stable/060_forget.html . Automatic retention remains opt-in.

Repository relocation verifies content, copies into a fresh directory and persists the new location only after success. The original is preserved. An interrupted copy may leave a partial destination repository; the app continues using the original. Choosing a different physical drive protects against source-disk failure; same-drive backups do not.

For deleted data without a snapshot, Microsoft recommends minimizing source use and a different recovery destination: https://support.microsoft.com/en-US/Windows/Experience/Backup-Recovery/windows-file-recovery . An intact backup enables byte-verified restoration; unbacked deleted-data recovery cannot have a universal success percentage.

## Validation scope

Synthetic regressions cover multi-chunk edits, deduplication, old/new version restore, legacy manifests, empty directories, cancellation, locking, migration and fail-closed cleanup. The explicit 1 GiB regression passed, including a full restored/original SHA-256 comparison. It ran separately because it reads/writes real logical gigabytes. Repeated zero chunks intentionally test deduplication; this is not a representative 50 GiB mixed-data throughput benchmark. 10/50 GiB real workloads, battery use, low-end PCs, power cuts, signed distribution and full native UI QA remain required release qualification.

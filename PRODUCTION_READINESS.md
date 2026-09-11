# Recovery Studio: implementation and release status

Status: functional development build. Synthetic recovery cases pass; universal recovery, production certification and low-end hardware performance are not claimed.

## Implemented in this iteration

### Recovery without a prior snapshot

The active desktop route is `recovery_scan`, backed by `engine/src/recovery.rs`. It scans stable files in 4 MiB blocks with a bounded lookahead, rather than mapping an entire disk into memory. It searches deleted NTFS records and format signatures in the same sweep. NTFS record discovery does not assume a contiguous MFT.

Resident data uses the actual $DATA length rather than possibly stale $FILE_NAME size. Non-resident extraction adds the partition base to cluster offsets, follows fragmented/sparse runs, checks fixups and read lengths, and uses bounded buffers. Unreadable/incomplete NTFS extents are not silently padded with stale bytes. Encrypted/compressed streams and missing extension records are reported as unsupported.

Recovered files are atomically published in a unique session folder chosen by the user. Folder mode requires another volume and labels results as existing files. Scan cancellation preserves completed outputs. A report records skipped candidates, partial scans, warnings, output hashes and validation labels. Signature-validation reads have a separate 8 GiB budget to constrain repeated false-positive I/O; a targeted extension scan can narrow work.

### Useful lightweight interface

The previous 3D particle renderer is replaced in the active UI by a CSS/SVG Recovery Observatory tied to actual progress. It stays still while idle, pauses when hidden and respects reduced-motion preferences. No WebGL, Three.js or remote fonts are loaded.

The workflow provides source/destination selection, Stop, progress, filename/source filters, 40-row pagination, result origin and validation labels, bounded image/text previews and session-folder access. Files are already saved when listed; the old unconditional Restore All success path is retired.

The missing-file assistant runs locally. It extracts filenames from requests such as `meri invoice.pdf nahi aayi`, searches the latest completed/partial scan and registered snapshot manifests, offers snapshot export and initiates targeted image scans. It does not invent recovery results or claim to reconstruct physically missing bytes.

### Snapshot policies

The default interval is 30 minutes, configurable from 5 minutes to 24 hours. New repositories default to a 100 GiB content-object budget; existing policies are preserved. Files are split into 4 MiB SHA-256 addressed chunks. Only new chunks are written, including when a large file changes in one region. Empty directories are preserved; legacy whole-file manifests remain readable. Unchanged captures reuse the prior restore point; hashing still reads eligible files so metadata-only changes cannot silently evade capture. New content is streamed and atomically stored. Restores verify size and SHA-256 before publishing.

Pause/resume, configurable retained-version count, opt-in old restore-point pruning and new-folder exports are wired into the UI. A manual capture does not unpause a schedule. Manual capture, export, maintenance and recovery share an exclusive cancellable operation gate. Background captures reserve the same gate; failed attempts are spaced according to the configured interval. Malformed folder/policy configuration is preserved and blocks writes with an error.

## Evidence

Latest standard checks: 55 engine tests and 1 desktop integration test passed; the explicit 1 GiB snapshot/restore/full-hash regression also passed (319.36 seconds in debug mode while release compilation shared the machine; not a throughput benchmark). Three sidecar checks passed. JavaScript syntax and 58 unique HTML IDs / 52 direct DOM bindings passed. Development desktop build succeeded. Earlier UI checks apply only to the previously inspected layout.

- Engine tests cover deleted resident recovery without a snapshot, fragmented partition-relative extents, sparse tails, corrupt fixups, truncated extents, chunk-boundary signatures, cancellation, PNG CRC and malformed ZIP bounds, conversational search terms, quota failure, retention, unchanged captures and corrupt snapshot restore/configuration.
- A desktop test runs the actual demo-image command through the recovery engine and compares recovered PNG bytes with its fixture; PDF termination is checked.
- Initial browser visual inspection confirmed the new upper layout. Browser interaction checks confirmed required-field errors, folder-mode guidance and honest offline-chat failure without the desktop bridge.
- A later browser visual recheck was blocked by automatic approval review citing a usage limit. Full final browser/desktop interaction QA and responsive screenshots remain unverified. No alternative UI automation was used to bypass that rejection.

## Known limits and remaining release work

1. Direct physical-device recovery is not enabled. This build needs a stable disk/volume image for deleted-data recovery. Native physical-disk imaging, physical-disk destination mapping and privileged-device workflows remain release work.
2. A session publishes at most 10,000 files / 20 GiB. Signature candidates are bounded at 64 MiB; the extra validation-read budget is 8 GiB. NTFS extraction can exceed the signature size limit within the session budget. These limits are surfaced, not a claim of complete disk coverage.
3. GPT handling assumes 512-byte logical sectors. Volume boot geometry, NTFS compression/encryption, alternate streams, attribute-list extensions, other filesystems, damaged partition tables and general fragmented carving need broader support and labelled real-world corpora.
4. JPEG/PDF boundaries and ZIP directory bounds are not full format validation. MP4 box checks do not prove playback. PDF incremental revisions, ZIP member CRCs and richer repair remain incomplete. Output hashes describe recovered bytes; they do not establish original identity.
5. The assistant searches the latest session and registered snapshots only, with bounded matching results. It does not automatically search Recycle Bin, VSS, File History, OneDrive or Google Drive. It requires a selected image for a targeted deleted-file scan.
6. Snapshot location can be changed through a verified repository copy with persisted configuration. The previous repository remains untouched. Exclusive OS file locking prevents two app instances writing one store. Audit verifies referenced chunks; cleanup deletes only unreferenced hash-named objects after every manifest is parsed. Opt-in retention runs cleanup. Disk free-space checks reserve 256 MiB; concurrent external writes can still exhaust a drive. Change-journal scheduling, battery awareness, encryption at rest and power-loss testing remain release work.
7. Snapshot capture is not a filesystem-wide point-in-time transaction. Files can change across the capture; whole-folder export is per-file atomic and may be partial on a later error. User-selected folders on the same disk do not protect against that disk failing.
8. Test native dialogs/IPC, preview behavior, multi-hour scans, low-memory/low-disk conditions, crash/power loss and real low-end Windows devices before release. Signed installer, update/rollback/uninstall and clean-machine distribution tests are still needed.
9. The desktop has no license activation, machine-ID check, payment gateway or remote entitlement request. The legacy server remains in the source workspace for compatibility but is excluded from default build members and is not required or bundled by the desktop. No certificate, service subscription or ads were purchased.

Do not advertise 100% recovery, bug-free operation, a guaranteed SmartScreen bypass or measured recovery probabilities. SSD TRIM, overwrite, encryption and physical unreadability can make recovery impossible even when deletion was recent.

# ClockVerse Recovery Studio

A Windows desktop recovery workbench with an offline missing-file assistant and optional version snapshots. This is a development build with verified synthetic recovery cases, not a universal recovery guarantee.

## Use

1. Run `cargo run -p clockverse` or build with `cargo build -p clockverse` and launch `target/debug/clockverse.exe`.
2. Select a stable disk/volume image and an existing recovery destination, preferably on a separate healthy disk.
3. Start recovery. Review source/integrity labels, preview supported files, and locate the saved session folder. `recovery-report.json` records results and limitations.
4. If something is missing, type a filename or extension in the assistant. It searches the latest scan and registered snapshots, and can run a targeted image scan.
5. For future versions, add a snapshot folder and choose capture interval/storage budget. Snapshots only run while the app is open. Exports go to a new folder rather than overwriting current work.

The sample-image button creates a synthetic image containing a valid PNG and PDF; it is a demonstration, not a measurement of real-world recovery accuracy.

## What is supported

- Stable raw image files: read in 4 MiB blocks with cancellation, partial-result reporting and output limits.
- NTFS deleted records: resident data and ordinary fragmented/sparse unnamed data runs, with partition-relative offsets, fixup checks and strict read lengths. No prior snapshot or deletion-date cutoff is required.
- JPEG, PNG, PDF, ZIP and MP4 signature candidates. Validation depth varies by format and is shown per result. PNG chunk CRCs are checked; a recorded output hash does not prove original-file identity.
- Existing-folder copying, clearly separated from deleted-file recovery. A different destination volume is required for folder mode.
- Offline assistant: filename/extension search, simple filename extraction from conversational requests, snapshot lookup/export and targeted scans. It does not call an external AI service or search cloud accounts.
- Lightweight CSS/SVG progress visual. No Three.js/WebGL import, external font download or continuous idle animation in the active interface. Reduced-motion and hidden-window preferences stop motion.
- Snapshots: streamed hashing, deduplicated object writes, atomic publication, restore verification, unchanged-version reuse, pause/resume, configurable interval and storage budget. Older restore-point pruning is opt-in; shared object cleanup is not automatic.

## Architecture

- `engine/src/recovery.rs`: active recovery pipeline and synthetic regression tests.
- `engine/src/ntfs.rs`, `ntfs_extract.rs`: NTFS parsing and extraction primitives.
- `engine/src/timesnap.rs`: snapshot store, policies and integrity checks.
- `src-tauri/src/workbench.rs`: recovery, preview, assistant and snapshot IPC.
- `ui/index.html`, `ui/workbench.css`, `ui/js/app.js`: active Recovery Studio interface.
- Existing forensic/index/sidecar modules remain for development. The optional Python sidecar only starts when `CLOCKVERSE_ENABLE_SIDECAR=1`; active recovery does not depend on it.

## Verification

```powershell
cargo test -p clockverse-engine -p clockverse
python sidecar/test_sidecar.py
Get-Content ui/js/app.js | node --input-type=module --check
cargo build -p clockverse
```

`npm run dev` serves a browser UI preview. File operations require the desktop app; the preview never fabricates recovery results.

See [PRODUCTION_READINESS.md](PRODUCTION_READINESS.md) for limitations, validation evidence and remaining release gates.

## Large-folder snapshots, without payments

New repositories default to a configurable 100 GiB budget. There is no 1/10/50 GiB folder cap: eligible files are streamed in 4 MiB chunks, subject to quota and available storage. Budget applies to unique stored chunks across all versions, not the source-folder size. The first capture can require approximately the full source size; later captures write only new chunks but still read all eligible files. Hash references and file lists consume memory proportional to file/chunk count.

Use **Change backup location** to copy and verify the repository onto a chosen drive; the previous copy stays intact. **Verify saved data** checks chunks, **Clean unused chunks** reclaims unreferenced content, and **Stop operation** cancels foreground backup work. **Open existing backup** reconnects an existing repository after a drive move. Repository locations persist across restarts.

Desktop recovery and snapshots work without payment or license activation. The old server source is optional and excluded from default workspace builds. See [release limitations](PRODUCTION_READINESS.md) and [snapshot design and sources](docs/SNAPSHOT_DESIGN.md).

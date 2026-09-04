# CLOCKVERSE — SYSTEM WIRING MAP (v1.0)

```
LAYER 1: UI (ui/)                    LAYER 2: SHELL (src-tauri/)         LAYER 3: ENGINE (engine/)
─────────────────────────            ──────────────────────────         ──────────────────────────
app.js (orchestrator)                main.rs (Tauri commands)           sectorforge.rs ─ carver
 ├─ event-stream.js ◄──── listen('engine') ──── app.emit("engine", ────► EngineEvent enum (lib.rs)
 ├─ holo-core.js          EngineEvent) ◄──────── ALL commands stream    chrono.rs ─ replay
 ├─ constellation.js                                  results           index.rs ─ SQLite
 ├─ mode-controller.js ── body[data-mode] ◄── single mode truth        harvest.rs ─ streaming
 └─ preview-bay.js ────── invoke(cmd) ────────► command handler ──────► ntfs.rs ─ MFT parser
                                                                          ntfs_extract.rs ─ bytes
LAYER 4: SIDECAR (sidecar/)                                                 partition.rs ─ GPT/MBR
sidecar.py ◄── JSON-RPC/stdio ── sidecar.rs ◄── engine API              imager.rs ─ .dd + resume
(pytsk3, Pillow, hashlib)                                                  mp4.rs ─ Integrity Gate
```

## COMMAND SURFACE (invoke name → engine → emits)

| invoke | engine | emits |
|---|---|---|
| `start_scan` | `sectorforge::carve_image` | `ScanStarted`, `SectorResult*`, `ScanComplete` |
| `scan_deleted_files` | `partition → ntfs::scan_mft` | `SectorResult*` (amber), `ScanComplete` |
| `extract_deleted_file` | `ntfs_extract::extract` | (returns bytes written) |
| `verify_carved_file` | `mp4::validate` | `FileVerified` (teal) |
| `chrono_ingest` | `index/harvest` | `IngestProgress*`, `SessionUpdated` |
| `chrono_time_travel` | `index + chrono::StreamStitch` | (returns reconstruction) |
| `list_sessions` | `index::sessions` | (returns `Vec<SessionSummary>`) |
| `sidecar_*` | `sidecar.rs → sidecar.py` | (returns JSON) |
| `trim_health_check` | (stub, Phase 2.5) | (returns string) |

## STATE TRANSITION COLORS (crystal truth table)

```
0 lost(red) → 1 carved(amber) → 2 verified(teal) → 3 restored(violet)
SectorResult=1  FileVerified=2  FileRestored=3
```

## HARD RULES (kabhi mat todo)

- **R1:** UI ↔ engine ka SIRF ek channel: `EngineEvent` over `"engine"` topic.
  Koi command apna custom event topic nahi banati.
- **R2:** `body[data-mode]` hi mode ka single source of truth hai —
  constellation, crystal, CSS sab isi se sync hote hain.
- **R3:** Source disk/image read-only. Sirf imager OUTPUT aur VaultGuard
  staging pe write allowed.
- **R4:** Corrupt input → skip/flag, kabhi panic nahi (har parser mein).
- **R5:** Heavy work `spawn_blocking` mein; UI thread sirf rendering.

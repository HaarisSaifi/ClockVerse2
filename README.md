# ⏳ ClockVerse: Temporal Forensic Reconstruction Engine

> **$100-Tier Luxury Forensic Data Resurrection Instrument**  
> Powered by Rust (SectorForge & ChronoScan), Python JSON-RPC Forensic Sidecar, Axum 0.8 License Server, Tauri 2 Shell, and Obsidian Hologram 3D UI.

---

## 🏛 Architecture Overview

```
ClockVerse2_anti/
├── Cargo.toml                     # Cargo workspace configuration
├── package.json                   # UI development and testing scripts
├── README.md                      # Architecture & run guide
├── engine/                        # High-throughput Rust Engine (clockverse-engine)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                 # IPC protocol & event types
│       ├── sectorforge.rs         # Rayon + memmap2 + Aho-Corasick carver
│       ├── chrono.rs              # ChronoScan delta stitcher & log replayer
│       └── sidecar.rs             # Python JSON-RPC stdio bridge
├── sidecar/                       # Forensic Python Sidecar
│   ├── sidecar.py                 # JSON-RPC server (image_info, verify_file, carve_thumbnail)
│   └── test_sidecar.py            # Automated test suite for sidecar protocol
├── server/                        # Axum 0.8 License Server (clockverse-license)
│   ├── Cargo.toml
│   └── src/
│       └── main.rs                # Razorpay webhook, HMAC-SHA256 & Ed25519 key generation
├── src-tauri/                     # Tauri 2 Desktop Shell
│   ├── Cargo.toml
│   ├── tauri.conf.json            # Desktop window specs & CSP
│   └── src/
│       └── main.rs                # Tauri invoke commands emitting engine events
└── ui/                            # Obsidian Hologram Luxury UI
    ├── index.html                 # Main interface deck & canvas
    ├── tokens.css                 # Obsidian Hologram design tokens
    ├── holo.css                   # Card system with @property --angle
    ├── vendor/
    │   └── three.module.js        # Vendored Three.js r185 (100% offline CSP)
    └── js/
        ├── app.js                 # Event wiring & UI bootstrap
        ├── event-stream.js        # SSE/Tauri event bridge with dev mock fallback
        ├── mode-controller.js     # Dual-mode finite state machine (Chrono <-> Sector)
        └── holo-core.js           # 200k particle GPU data crystal
```

---

## ⚡ Key Subsystems

### 1. SectorForge Engine (`engine/src/sectorforge.rs`)
- Multi-threaded signature carver scanning disk images memory-mapped (`memmap2`) in 64MB chunks via `rayon`.
- Single-pass multi-pattern matching with `aho-corasick` for JPEG, PNG, PDF, ZIP, GZIP, and MP4.
- Forensic Invariant: Strictly read-only image access (`VaultGuard`).

### 2. ChronoScan Stitcher (`engine/src/chrono.rs`)
- Temporal Differential Stitcher replaying genesis and mutation delta events.
- Reconstructs files forward in time up to a specified temporal cutoff (Today, 2-3 Days Ago, Last Week, Deep Forensic).

### 3. Forensic Sidecar (`sidecar/sidecar.py`)
- Python process communicating with Rust over JSON-RPC (newline-delimited JSON over stdio).
- Handles file verification (1MB-chunked SHA-256 integrity hashing), image stats, and thumbnail extraction.

### 4. License Server (`server/src/main.rs`)
- Axum 0.8 service verifying Razorpay webhooks with constant-time HMAC-SHA256 (`ct_eq`).
- Generates machine-bound, cryptographically signed license keys using Ed25519 (`clockverse|pro|{order_id}|...|sig={hex}`).

### 5. Obsidian Hologram UI (`ui/`)
- GPU-instanced 200,000 particle golden-spiral data crystal in WebGL (custom GLSL shaders).
- Dual-Mode Finite State Machine (`IDLE` -> `TRANSITIONING` -> `SWAPPED` -> `IDLE`) crossfading between Quantum Chrono (cyan) and Plasma Sector (amber).
- Holographic card system featuring `@property --angle` rotating conic-gradient borders.
- Auto-quality tiering (detects slow frames and halves draw range smoothly without lag).
- Zero idle GPU burn (pauses animation loop on `visibilitychange`).

---

## 🚀 Quickstart & Verification

### 1. Run Sidecar Tests
```bash
python sidecar/test_sidecar.py
```

### 2. Run Rust Engine Tests
```bash
cargo test -p clockverse-engine
```

### 3. Run License Server Tests
```bash
cargo test -p clockverse-license
```

### 4. Run Frontend in Browser Dev Mode
```bash
npx serve ui -l 5173
```
Open `http://localhost:5173` in your browser.

### 5. Run Tauri Desktop App
```bash
cargo tauri dev
```

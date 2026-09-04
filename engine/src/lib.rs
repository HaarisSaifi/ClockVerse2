pub mod chrono;
pub mod imager;
pub mod index;
pub mod mp4;
pub mod ntfs;
pub mod ntfs_extract;
pub mod partition;
pub mod sectorforge;
pub mod sidecar;

use serde::{Deserialize, Serialize};

/// Events streamed to the UI over SSE / Tauri event bridge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EngineEvent {
    ScanStarted {
        target: String,
        total_sectors: u64,
    },
    SectorResult {
        particle_index: u32,
        state_code: u8, // 0 lost, 1 carved, 2 verified, 3 restored
        cluster: u64,
        signature: String, // e.g. "FFD8FF" (jpeg)
        confidence: f32,
    },
    Throughput {
        bytes_per_sec: u64,
        eta_secs: u64,
    },
    FileVerified {
        path: String,
        sha256: String,
    },
    FileRestored {
        path: String,
        bytes: u64,
    },
    ScanComplete {
        found: u32,
        verified: u32,
        failures: u32,
    },
    /// A telemetry event was ingested into the session index (Phase 1).
    ChronoEventIngested {
        path: String,
        ts_micros: u64,
        op_kind: String,
    },
    /// A session's event index was updated; UI refreshes the constellation.
    SessionUpdated {
        session_id: String,
        event_count: u64,
        file_count: u64,
    },
    Error {
        code: String,
        message: String,
    },
}

/// Commands from UI -> engine (Tauri command payloads).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum EngineCommand {
    StartScan {
        target: String,
        depth: ScanDepth,
    },
    SuspendScan {
        token: String,
    },
    ResumeScan {
        token: String,
    },
    RestoreFiles {
        file_ids: Vec<String>,
        destination: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ScanDepth {
    Last24h,
    Days2To3,
    LastWeek,
    DeepForensic,
}

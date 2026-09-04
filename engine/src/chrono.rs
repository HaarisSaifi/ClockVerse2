use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One telemetry event from an AI workspace session log (JSONL on disk).
/// ts_micros is the ordering authority — wall-clock strings are NOT trusted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryEvent {
    pub ts_micros: u64,
    pub file_path: String,
    pub op: DeltaOp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeltaOp {
    /// Full snapshot (session start, or checkpoint)
    Write {
        content: Vec<u8>,
    },
    /// Byte-range replacement — the common AI-edit case
    Patch {
        offset: u64,
        delete_len: u64,
        insert: Vec<u8>,
    },
    Delete,
}

/// In-memory reconstruction of one file's current state during replay.
#[derive(Debug, Default, Clone)]
pub struct FileState {
    pub bytes: Vec<u8>,
    pub last_ts: u64,
    pub patch_count: u32,
}

impl FileState {
    pub(crate) fn apply(&mut self, ev: &TelemetryEvent) {
        match &ev.op {
            DeltaOp::Write { content } => {
                self.bytes = content.clone();
            }
            DeltaOp::Patch {
                offset,
                delete_len,
                insert,
            } => {
                let start = (*offset as usize).min(self.bytes.len());
                let end = (start + *delete_len as usize).min(self.bytes.len());
                // splice: replace [start..end) with insert — O(n) memmove, no realloc churn
                self.bytes.splice(start..end, insert.iter().copied());
                self.patch_count += 1;
            }
            DeltaOp::Delete => {
                self.bytes.clear();
            }
        }
        self.last_ts = ev.ts_micros;
    }
}

/// StreamStitch: chronological delta replay with microsecond ordering.
///
/// Input: raw JSONL bytes (one TelemetryEvent per line, ANY order —
/// logs from multiple tools interleave arbitrarily).
/// Output: per-file reconstructed state at the LATEST event, or at
/// `as_of_micros` for time-travel ("give me this file as it was 3 days ago").
pub struct StreamStitch;

impl StreamStitch {
    pub fn parse(jsonl: &[u8]) -> Vec<TelemetryEvent> {
        jsonl
            .split(|&b| b == b'\n')
            .filter(|line| !line.is_empty())
            .filter_map(|line| serde_json::from_slice::<TelemetryEvent>(line).ok())
            // Malformed lines are skipped, never fatal — forensic rule:
            // a corrupt log line must not kill a 1 GB recovery session.
            .collect()
    }

    /// Sort by (ts_micros, original_index) — stable order for same-microsecond
    /// events. This is what "microsecond ordering" actually means in code.
    pub fn order(mut events: Vec<TelemetryEvent>) -> Vec<TelemetryEvent> {
        events.sort_by_key(|e| e.ts_micros); // sort_by_key is stable in Rust
        events
    }

    /// Replay events into per-file states. `as_of_micros = u64::MAX`
    /// means "reconstruct to latest known state".
    pub fn replay(events: &[TelemetryEvent], as_of_micros: u64) -> BTreeMap<String, FileState> {
        let mut files: BTreeMap<String, FileState> = BTreeMap::new();
        for ev in events.iter().filter(|e| e.ts_micros <= as_of_micros) {
            files.entry(ev.file_path.clone()).or_default().apply(ev);
        }
        files
    }

    /// Full pipeline: bytes in → reconstructed workspace out.
    pub fn reconstruct(jsonl: &[u8], as_of_micros: u64) -> BTreeMap<String, FileState> {
        Self::replay(&Self::order(Self::parse(jsonl)), as_of_micros)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: u64, path: &str, op: DeltaOp) -> TelemetryEvent {
        TelemetryEvent {
            ts_micros: ts,
            file_path: path.into(),
            op,
        }
    }

    #[test]
    fn replays_patches_in_microsecond_order_even_if_input_unordered() {
        // Logs arrive out of order (multi-tool interleave) — replay must not care.
        let events = vec![
            ev(
                3000,
                "main.py",
                DeltaOp::Patch {
                    offset: 6,
                    delete_len: 5,
                    insert: b"earth".to_vec(),
                },
            ),
            ev(
                1000,
                "main.py",
                DeltaOp::Write {
                    content: b"hello world".to_vec(),
                },
            ),
            ev(
                2000,
                "main.py",
                DeltaOp::Patch {
                    offset: 0,
                    delete_len: 5,
                    insert: b"HELLO".to_vec(),
                },
            ),
        ];
        let files = StreamStitch::replay(&StreamStitch::order(events), u64::MAX);
        assert_eq!(files["main.py"].bytes, b"HELLO earth");
        assert_eq!(files["main.py"].patch_count, 2);
    }

    #[test]
    fn time_travel_stops_at_as_of_boundary() {
        let events = vec![
            ev(
                1000,
                "a.rs",
                DeltaOp::Write {
                    content: b"v1".to_vec(),
                },
            ),
            ev(
                5000,
                "a.rs",
                DeltaOp::Write {
                    content: b"v2-broken".to_vec(),
                },
            ),
        ];
        // "File as it was at t=2000" — the whole ChronoScan pitch.
        let files = StreamStitch::replay(&StreamStitch::order(events), 2000);
        assert_eq!(files["a.rs"].bytes, b"v1");
    }

    #[test]
    fn patch_beyond_eof_clamps_instead_of_panicking() {
        // Corrupt/foreign telemetry must never crash the engine.
        let events = vec![
            ev(
                1,
                "x",
                DeltaOp::Write {
                    content: b"ab".to_vec(),
                },
            ),
            ev(
                2,
                "x",
                DeltaOp::Patch {
                    offset: 99,
                    delete_len: 50,
                    insert: b"!".to_vec(),
                },
            ),
        ];
        let files = StreamStitch::replay(&StreamStitch::order(events), u64::MAX);
        assert_eq!(files["x"].bytes, b"ab!");
    }

    #[test]
    fn delete_clears_state_but_history_survives_time_travel() {
        let events = vec![
            ev(
                1,
                "gone.txt",
                DeltaOp::Write {
                    content: b"data".to_vec(),
                },
            ),
            ev(9, "gone.txt", DeltaOp::Delete),
        ];
        let ordered = StreamStitch::order(events);
        assert!(StreamStitch::replay(&ordered, u64::MAX)["gone.txt"]
            .bytes
            .is_empty());
        assert_eq!(StreamStitch::replay(&ordered, 5)["gone.txt"].bytes, b"data");
    }

    #[test]
    fn parse_skips_corrupt_lines() {
        let log = b"{\"ts_micros\":1,\"file_path\":\"a\",\"op\":{\"kind\":\"write\",\"content\":[65]}}\n{GARBAGE\n";
        let events = StreamStitch::parse(log);
        assert_eq!(events.len(), 1); // bad line skipped, good line survives
    }
}

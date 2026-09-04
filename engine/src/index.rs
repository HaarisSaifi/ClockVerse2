use rusqlite::{params, Connection};
use std::collections::BTreeMap;
use std::path::Path;

use crate::chrono::{DeltaOp, FileState, TelemetryEvent};

/// EventIndex — SQLite-backed persistent store for telemetry events.
///
/// Design rationale (blueprint §4): a 1 GB recovery session must not live
/// in RAM. Events are streamed into an on-disk SQLite index as they arrive;
/// reconstruction pulls only the events for the requested `as_of` window
/// and replays them through StreamStitch. This keeps memory flat regardless
/// of session size.
pub struct EventIndex {
    conn: Connection,
}

impl EventIndex {
    /// Open (or create) the event index at `path`.
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS telemetry (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts_micros INTEGER NOT NULL,
                 file_path TEXT NOT NULL,
                 op_kind TEXT NOT NULL,
                 op_json BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_ts ON telemetry (ts_micros);
             CREATE INDEX IF NOT EXISTS idx_file ON telemetry (file_path);",
        )?;
        Ok(Self { conn })
    }

    /// Open an in-memory index (tests / ephemeral sessions).
    pub fn in_memory() -> anyhow::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS telemetry (
                 seq INTEGER PRIMARY KEY AUTOINCREMENT,
                 ts_micros INTEGER NOT NULL,
                 file_path TEXT NOT NULL,
                 op_kind TEXT NOT NULL,
                 op_json BLOB NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_ts ON telemetry (ts_micros);
             CREATE INDEX IF NOT EXISTS idx_file ON telemetry (file_path);",
        )?;
        Ok(Self { conn })
    }

    /// Append one event. The order of insertion is preserved via `seq`,
    /// but ordering authority during replay is always `ts_micros`.
    pub fn push(&self, ev: &TelemetryEvent) -> anyhow::Result<()> {
        let op_kind = match &ev.op {
            DeltaOp::Write { .. } => "write",
            DeltaOp::Patch { .. } => "patch",
            DeltaOp::Delete => "delete",
        };
        let op_json = serde_json::to_vec(&ev.op)?;
        self.conn.execute(
            "INSERT INTO telemetry (ts_micros, file_path, op_kind, op_json) VALUES (?1, ?2, ?3, ?4)",
            params![ev.ts_micros as i64, ev.file_path, op_kind, op_json],
        )?;
        Ok(())
    }

    /// Bulk append (used when a raw JSONL log is ingested).
    pub fn push_all(&self, events: &[TelemetryEvent]) -> anyhow::Result<()> {
        for ev in events {
            self.push(ev)?;
        }
        Ok(())
    }

    /// Reconstruct the whole workspace as of `as_of_micros` by indexing the
    /// persisted events and replaying through StreamStitch.
    /// `as_of_micros == u64::MAX` means "latest known state" (no upper bound).
    pub fn reconstruct_as_of(
        &self,
        as_of_micros: u64,
    ) -> anyhow::Result<BTreeMap<String, FileState>> {
        let bound = as_of_micros != u64::MAX;
        let sql = if bound {
            "SELECT ts_micros, file_path, op_json FROM telemetry WHERE ts_micros <= ?1 ORDER BY ts_micros"
        } else {
            "SELECT ts_micros, file_path, op_json FROM telemetry ORDER BY ts_micros"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = if bound {
            stmt.query(params![as_of_micros as i64])?
        } else {
            stmt.query([])?
        };

        let mut files: BTreeMap<String, FileState> = BTreeMap::new();
        while let Some(row) = rows.next()? {
            let ts: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            let op_json: Vec<u8> = row.get(2)?;
            let op: DeltaOp = serde_json::from_slice(&op_json)?;
            let ev = TelemetryEvent {
                ts_micros: ts as u64,
                file_path: path,
                op,
            };
            files.entry(ev.file_path.clone()).or_default().apply(&ev);
        }
        Ok(files)
    }

    /// Reconstruct a single file's state as of `as_of_micros`.
    /// `as_of_micros == u64::MAX` means "latest known state".
    pub fn file_as_of(&self, file_path: &str, as_of_micros: u64) -> anyhow::Result<FileState> {
        let bound = as_of_micros != u64::MAX;
        let sql = if bound {
            "SELECT ts_micros, op_json FROM telemetry WHERE file_path = ?1 AND ts_micros <= ?2 ORDER BY ts_micros"
        } else {
            "SELECT ts_micros, op_json FROM telemetry WHERE file_path = ?1 ORDER BY ts_micros"
        };
        let mut stmt = self.conn.prepare(sql)?;
        let mut rows = if bound {
            stmt.query(params![file_path, as_of_micros as i64])?
        } else {
            stmt.query(params![file_path])?
        };

        let mut state = FileState::default();
        while let Some(row) = rows.next()? {
            let ts: i64 = row.get(0)?;
            let op_json: Vec<u8> = row.get(1)?;
            let op: DeltaOp = serde_json::from_slice(&op_json)?;
            let ev = TelemetryEvent {
                ts_micros: ts as u64,
                file_path: file_path.to_string(),
                op,
            };
            state.apply(&ev);
        }
        Ok(state)
    }

    /// Total event count (for progress reporting).
    pub fn event_count(&self) -> anyhow::Result<u64> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM telemetry", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    /// The set of files touched in this index.
    pub fn files(&self) -> anyhow::Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT DISTINCT file_path FROM telemetry")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Latest microsecond timestamp in the index (or 0 if empty).
    pub fn max_ts(&self) -> anyhow::Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COALESCE(MAX(ts_micros), 0) FROM telemetry",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Reconstruct a `TelemetryEvent` vector (ordered) back out of the index —
    /// useful for re-ingesting into higher-level replay without re-parsing JSONL.
    pub fn drain_events(&self) -> anyhow::Result<Vec<TelemetryEvent>> {
        let mut stmt = self
            .conn
            .prepare("SELECT ts_micros, file_path, op_json FROM telemetry ORDER BY ts_micros")?;
        let rows = stmt.query_map([], |row| {
            let ts: i64 = row.get(0)?;
            let path: String = row.get(1)?;
            let op_json: Vec<u8> = row.get(2)?;
            Ok((ts, path, op_json))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (ts, path, op_json) = row?;
            let op: DeltaOp = serde_json::from_slice(&op_json)?;
            out.push(TelemetryEvent {
                ts_micros: ts as u64,
                file_path: path,
                op,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrono::StreamStitch;

    fn ev(ts: u64, path: &str, op: DeltaOp) -> TelemetryEvent {
        TelemetryEvent {
            ts_micros: ts,
            file_path: path.into(),
            op,
        }
    }

    #[test]
    fn persists_and_reconstructs_across_sessions() {
        let idx = EventIndex::in_memory().unwrap();
        idx.push_all(&[
            ev(
                1000,
                "a.rs",
                DeltaOp::Write {
                    content: b"v1".to_vec(),
                },
            ),
            ev(
                2000,
                "a.rs",
                DeltaOp::Patch {
                    offset: 0,
                    delete_len: 2,
                    insert: b"v2".to_vec(),
                },
            ),
        ])
        .unwrap();

        let files = idx.reconstruct_as_of(u64::MAX).unwrap();
        assert_eq!(files["a.rs"].bytes, b"v2");

        // Time-travel through the index
        let past = idx.reconstruct_as_of(1500).unwrap();
        assert_eq!(past["a.rs"].bytes, b"v1");
    }

    #[test]
    fn file_as_of_returns_single_file_state() {
        let idx = EventIndex::in_memory().unwrap();
        idx.push_all(&[
            ev(
                100,
                "x.txt",
                DeltaOp::Write {
                    content: b"hello".to_vec(),
                },
            ),
            ev(
                200,
                "y.txt",
                DeltaOp::Write {
                    content: b"world".to_vec(),
                },
            ),
        ])
        .unwrap();
        let s = idx.file_as_of("y.txt", u64::MAX).unwrap();
        assert_eq!(s.bytes, b"world");
    }

    #[test]
    fn count_and_files_are_accurate() {
        let idx = EventIndex::in_memory().unwrap();
        idx.push_all(&[
            ev(
                1,
                "a",
                DeltaOp::Write {
                    content: b"x".to_vec(),
                },
            ),
            ev(
                2,
                "a",
                DeltaOp::Patch {
                    offset: 0,
                    delete_len: 0,
                    insert: b"y".to_vec(),
                },
            ),
            ev(
                3,
                "b",
                DeltaOp::Write {
                    content: b"z".to_vec(),
                },
            ),
        ])
        .unwrap();
        assert_eq!(idx.event_count().unwrap(), 3);
        assert_eq!(idx.files().unwrap().len(), 2);
        assert_eq!(idx.max_ts().unwrap(), 3);
    }

    #[test]
    fn round_trips_through_stream_stitch() {
        let raw = b"{\"ts_micros\":1,\"file_path\":\"a\",\"op\":{\"kind\":\"write\",\"content\":[65]}}\n{\"ts_micros\":2,\"file_path\":\"a\",\"op\":{\"kind\":\"patch\",\"offset\":0,\"delete_len\":1,\"insert\":[66]}}\n";
        let events = StreamStitch::parse(raw);
        let idx = EventIndex::in_memory().unwrap();
        idx.push_all(&events).unwrap();
        let drained = idx.drain_events().unwrap();
        let files = StreamStitch::replay(&drained, u64::MAX);
        assert_eq!(files["a"].bytes, b"B");
    }
}

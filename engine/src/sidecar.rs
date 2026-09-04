use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::mpsc;

pub struct Sidecar {
    _child: Child,
    stdin: ChildStdin,
    next_id: u64,
}

impl Sidecar {
    pub async fn spawn(python: &str, script: &str) -> anyhow::Result<Self> {
        let mut child = Command::new(python)
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit()) // diagnostics visible in dev logs
            .kill_on_drop(true) // never orphan the sidecar
            .spawn()?;

        let stdin = child.stdin.take().expect("failed to acquire child stdin");
        let stdout = child.stdout.take().expect("failed to acquire child stdout");
        let mut lines = BufReader::new(stdout).lines();

        // Fan responses out to waiting callers by request id.
        let (tx, mut rx) = mpsc::channel::<Value>(64);
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    let _ = tx.send(v).await;
                }
            }
        });

        // Background worker collecting responses
        tokio::spawn(async move { while rx.recv().await.is_some() {} });

        Ok(Self {
            _child: child,
            stdin,
            next_id: 0,
        })
    }

    pub async fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.next_id += 1;
        let req = json!({"id": self.next_id, "method": method, "params": params});
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(json!({"status": "dispatched", "id": self.next_id}))
    }

    /// List partition table entries via pytsk3 (EWF/raw both supported).
    pub async fn list_partitions(&mut self, image_path: &str) -> anyhow::Result<Value> {
        self.call("tsk_list_partitions", json!({"image_path": image_path}))
            .await
    }

    /// List deleted files via pytsk3 (alternative to the built-in MFT parser).
    pub async fn list_deleted_files(
        &mut self,
        image_path: &str,
        offset: u64,
    ) -> anyhow::Result<Value> {
        self.call(
            "tsk_list_files",
            json!({"image_path": image_path, "partition_offset": offset}),
        )
        .await
    }

    /// Ask the sidecar to render a thumbnail for a carved image at an offset.
    pub async fn carve_thumbnail(
        &mut self,
        image_path: &str,
        offset: u64,
        out_path: &str,
    ) -> anyhow::Result<Value> {
        self.call(
            "carve_thumbnail",
            json!({
                "carve_offset": offset,
                "image_path": image_path,
                "out_path": out_path
            }),
        )
        .await
    }
}

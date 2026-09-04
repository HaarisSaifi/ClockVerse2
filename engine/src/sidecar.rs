use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

pub struct Sidecar {
    _child: Child,
    stdin: ChildStdin,
    next_id: u64,
    // Problem #2 fix: Response routing map
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
}

impl Sidecar {
    pub async fn spawn(python: &str, script: &str) -> anyhow::Result<Self> {
        let mut child = Command::new(python)
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let pending_clone = pending.clone();

        // Reader loop: route responses by ID
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                        let mut map = pending_clone.lock().await;
                        if let Some(tx) = map.remove(&id) {
                            let _ = tx.send(v); // Route to waiting caller
                        }
                    }
                }
            }
        });

        Ok(Self {
            _child: child,
            stdin,
            next_id: 0,
            pending,
        })
    }

    pub async fn call(&mut self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;

        // Register oneshot channel before sending request
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        let req = json!({
            "id": id,
            "method": method,
            "params": params
        });

        let mut line = serde_json::to_string(&req)?;
        line.push('\n');

        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        // Wait for response with 30s timeout
        let resp = tokio::time::timeout(Duration::from_secs(30), rx).await??;

        // Check for error in response
        if let Some(err) = resp.get("error") {
            if !err.is_null() {
                anyhow::bail!("Sidecar error: {}", err);
            }
        }

        Ok(resp.get("result").cloned().unwrap_or(Value::Null))
    }

    // Convenience wrappers (existing methods, now using fixed call())
    pub async fn list_partitions(&mut self, image_path: &str) -> anyhow::Result<Value> {
        self.call("tsk_list_partitions", json!({"image_path": image_path})).await
    }

    pub async fn list_deleted_files(&mut self, image_path: &str, offset: u64) -> anyhow::Result<Value> {
        self.call("tsk_list_files", json!({
            "image_path": image_path,
            "partition_offset": offset
        })).await
    }

    pub async fn carve_thumbnail(&mut self, image_path: &str, offset: u64, out_path: &str) -> anyhow::Result<Value> {
        self.call("carve_thumbnail", json!({
            "carve_offset": offset,
            "image_path": image_path,
            "out_path": out_path
        })).await
    }
}

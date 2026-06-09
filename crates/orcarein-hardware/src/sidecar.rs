//! Persistent Python sidecar over line-delimited JSON.
//!
//! A [`Sidecar`] owns a long-lived `python -u <script>` child process and
//! communicates with it via newline-terminated JSON messages on stdin/stdout.
//! The Python namespace persists for the lifetime of the process, so driver
//! objects created in an `init` block (e.g. a `ServoKit` handle) stay alive
//! across subsequent `exec`/`eval` calls.
//!
//! # Protocol
//!
//! Request (one JSON line):
//! ```json
//! {"id": 1, "kind": "init"|"exec"|"eval", "code": "<python source>"}
//! ```
//!
//! Response (one JSON line):
//! ```json
//! {"id": 1, "ok": true, "result": <value-or-null>, "error": null}
//! ```
//!
//! On failure:
//! ```json
//! {"id": 1, "ok": false, "result": null, "error": "<repr(exception)>"}
//! ```

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time;

use crate::HardwareError;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(serde::Serialize)]
struct SidecarRequest {
    id: u64,
    kind: &'static str,
    code: String,
}

#[derive(serde::Deserialize)]
struct SidecarResponse {
    id: u64,
    ok: bool,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Sidecar
// ---------------------------------------------------------------------------

/// A long-lived Python subprocess that executes arbitrary code in a persistent
/// namespace and returns results over line-delimited JSON.
///
/// Drop the `Sidecar` to kill the child process. Calls are sequential
/// (one outstanding request at a time) — suitable for turn-based closed-loop
/// control.
pub struct Sidecar {
    child: Child,
    stdin: ChildStdin,
    reader: Lines<BufReader<ChildStdout>>,
    next_id: u64,
    timeout: Duration,
}

impl Sidecar {
    /// Spawn a new sidecar process.
    ///
    /// # Arguments
    ///
    /// * `python` – Python interpreter name or path (e.g. `"python3"`).
    /// * `script` – Path to the sidecar Python script.
    /// * `init`   – Optional Python source executed once inside the persistent
    ///   namespace before the `Sidecar` is returned. Use this to import
    ///   libraries and create driver objects. Any exception here propagates
    ///   as [`HardwareError::Sidecar`].
    pub async fn spawn(
        python: &str,
        script: &Path,
        init: Option<&str>,
    ) -> Result<Self, HardwareError> {
        let mut child = tokio::process::Command::new(python)
            .arg("-u")
            .arg(script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HardwareError::Sidecar("failed to open stdin".into()))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HardwareError::Sidecar("failed to open stdout".into()))?;

        let reader = BufReader::new(stdout).lines();

        let mut sidecar = Sidecar {
            child,
            stdin,
            reader,
            next_id: 1,
            timeout: Duration::from_secs(10),
        };

        if let Some(code) = init {
            sidecar.request("init", code.to_owned()).await?;
        }

        Ok(sidecar)
    }

    /// Execute Python source in the persistent namespace; the return value is
    /// discarded (`exec` semantics).
    pub async fn exec(&mut self, code: String) -> Result<(), HardwareError> {
        self.request("exec", code).await?;
        Ok(())
    }

    /// Evaluate a Python expression in the persistent namespace and return the
    /// JSON-serialisable result.
    pub async fn eval(&mut self, code: String) -> Result<serde_json::Value, HardwareError> {
        let maybe = self.request("eval", code).await?;
        Ok(maybe.unwrap_or(serde_json::Value::Null))
    }

    /// Send one request and wait for the matching response.
    async fn request(
        &mut self,
        kind: &'static str,
        code: String,
    ) -> Result<Option<serde_json::Value>, HardwareError> {
        let id = self.next_id;
        self.next_id += 1;

        let req = SidecarRequest { id, kind, code };
        let mut line = serde_json::to_string(&req)
            .map_err(|e| HardwareError::Sidecar(format!("serialise: {e}")))?;
        line.push('\n');

        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| HardwareError::Sidecar(format!("write stdin: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| HardwareError::Sidecar(format!("flush stdin: {e}")))?;

        match time::timeout(self.timeout, self.reader.next_line()).await {
            Err(_elapsed) => {
                let _ = self.child.start_kill();
                Err(HardwareError::Sidecar("timed out".into()))
            }
            Ok(Ok(None)) => {
                let _ = self.child.start_kill();
                Err(HardwareError::Sidecar("sidecar exited".into()))
            }
            Ok(Err(e)) => {
                let _ = self.child.start_kill();
                Err(HardwareError::Io(e))
            }
            Ok(Ok(Some(resp_line))) => {
                let resp: SidecarResponse = serde_json::from_str(&resp_line)
                    .map_err(|e| HardwareError::Sidecar(format!("deserialise response: {e}")))?;

                if resp.id != id {
                    let _ = self.child.start_kill();
                    return Err(HardwareError::Sidecar(format!(
                        "id mismatch: expected {id}, got {}",
                        resp.id
                    )));
                }

                if !resp.ok {
                    return Err(HardwareError::Sidecar(
                        resp.error.unwrap_or_else(|| "(no error text)".into()),
                    ));
                }

                Ok(resp.result)
            }
        }
    }

    /// Probe the local `PATH` for a usable Python interpreter.
    ///
    /// Returns the first of `["python3", "python"]` that can be launched
    /// successfully, or `None` if neither is found. Tests use this to skip
    /// gracefully in environments where Python is not installed.
    pub fn locate_python() -> Option<String> {
        for name in &["python3", "python"] {
            if let Ok(out) = std::process::Command::new(name).arg("--version").output() {
                if out.status.success() {
                    return Some((*name).to_owned());
                }
            }
        }
        None
    }
}

impl Drop for Sidecar {
    /// Kill the child process when the `Sidecar` is dropped, preventing leaked
    /// Python processes.
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

// ---------------------------------------------------------------------------
// Unit tests (no python required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locate_python_returns_string_or_none() {
        // Just verify the function runs without panicking.
        let result = Sidecar::locate_python();
        if let Some(ref name) = result {
            assert!(!name.is_empty());
        }
    }
}

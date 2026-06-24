//! A minimal stdio MCP server for tests: answers initialize/tools/list/
//! tools/call with canned responses. Pure std (no tokio) so it's a tiny,
//! cross-platform fixture. Only built with `--features mcp`.

use std::io::{BufRead, Write};

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = v.get("id").cloned();
        if id.is_none() || id == Some(serde_json::Value::Null) {
            continue; // notification
        }
        let method = v.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let result = match method {
            "initialize" => {
                serde_json::json!({"protocolVersion":"2025-06-18","serverInfo":{"name":"mock"}})
            }
            "tools/list" => {
                serde_json::json!({"tools":[{"name":"echo","description":"echo tool","inputSchema":{"type":"object"}}]})
            }
            "tools/call" => {
                serde_json::json!({"content":[{"type":"text","text":"mock-ok"}],"isError":false})
            }
            _ => serde_json::Value::Null,
        };
        let resp = serde_json::json!({"jsonrpc":"2.0","id":id.unwrap(),"result":result});
        writeln!(stdout, "{resp}").ok();
        stdout.flush().ok();
    }
}

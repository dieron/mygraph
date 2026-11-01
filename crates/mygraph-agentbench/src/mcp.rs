//! A minimal MCP client over stdio (one JSON-RPC message per line), for agent loops that call a
//! model API directly: start the server, list its tools, call them.

use crate::agent::Result;
use serde_json::{Value as Json, json};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

pub struct Server {
    child: Child,
    /// `None` once closed.
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// The server's `instructions` from `initialize`.
    pub instructions: String,
    /// The server's tools as listed: `name`, `description`, `inputSchema`.
    pub tools: Vec<Json>,
}

impl Server {
    /// Starts an `mcpServers` entry (`command`, `args`) with its stderr going to `stderr_file`.
    pub fn start(entry: &Json, stderr_file: &Path) -> Result<Self> {
        let command = entry["command"]
            .as_str()
            .ok_or("MCP server without command")?;
        let args: Vec<&str> = entry["args"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Json::as_str)
            .collect();
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(std::fs::File::create(stderr_file)?)
            .spawn()?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        let mut s = Server {
            child,
            stdin: Some(stdin),
            stdout,
            next_id: 0,
            instructions: String::new(),
            tools: Vec::new(),
        };
        let init = s.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "mygraph-agentbench", "version": env!("CARGO_PKG_VERSION") },
            }),
        )?;
        s.instructions = init["instructions"].as_str().unwrap_or_default().to_owned();
        s.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        s.tools = s.request("tools/list", json!({}))?["tools"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(s)
    }

    fn send(&mut self, msg: &Json) -> Result<()> {
        let stdin = self.stdin.as_mut().ok_or("MCP server closed")?;
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, method: &str, params: Json) -> Result<Json> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line)? == 0 {
                return Err(format!("MCP server closed during {method}").into());
            }
            let Ok(msg) = serde_json::from_str::<Json>(&line) else {
                continue;
            };
            if msg["id"] != id {
                continue;
            }
            if let Some(e) = msg.get("error") {
                return Err(format!("MCP {method}: {e}").into());
            }
            return Ok(msg["result"].clone());
        }
    }

    /// Calls a tool; returns its text and whether it reported an error.
    pub fn call(&mut self, name: &str, arguments: Json) -> Result<(String, bool)> {
        let r = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )?;
        let text = r["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        Ok((text, r["isError"] == true))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Closing stdin ends the server; wait so its graph file is released.
        drop(self.stdin.take());
        if self.child.wait().is_err() {
            let _ = self.child.kill();
        }
    }
}

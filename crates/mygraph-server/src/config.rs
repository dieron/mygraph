//! Server configuration (`server.toml`). Every field has a default, so the file is optional.

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Folder holding `graphs/`, `blobs/`, `text/`, `snapshots/` and `trash/`
    /// (docs/03-storage-and-search.md §4).
    pub data_dir: PathBuf,
    /// Address of the HTTP listener. Local-only by default (docs/05-architecture.md §9).
    pub listen: SocketAddr,
    /// Graph used by transports that name none (MCP over stdio, later).
    pub default_graph: String,
    /// `tracing` filter, e.g. `info` or `mygraph=debug`.
    pub log: String,
    /// Append the log to this file instead of the console (for a server started at login).
    pub log_file: Option<PathBuf>,
    /// Largest accepted request body ([API-HTTP-3]).
    pub max_body_bytes: usize,
    /// How long a request waits for a graph that is loading ([API-GRAPH-2]).
    pub warmup_wait_ms: u64,
    /// Load every graph at start instead of on first use.
    pub preload: bool,
    /// Keep a tantivy full-text index per graph (else search uses SQLite FTS5).
    pub text_index: bool,
    /// How often background maintenance runs: full-text index commits (writes do not wait for
    /// them; after a crash the index catches up from the change log) and passive WAL checkpoints.
    pub maintenance_ms: u64,
    /// Requests slower than this are logged with their shape.
    pub slow_ms: u64,
    /// API keys ([API-AUTH-1]). None: every request is the local admin ([API-AUTH-4]).
    pub keys: Vec<KeyConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyConfig {
    /// The key itself, or `key_sha256`: its SHA-256 as 64 lowercase hex digits.
    pub key: Option<String>,
    pub key_sha256: Option<String>,
    pub actor: String,
    pub role: Role,
    /// Graphs this key may use; empty means all.
    #[serde(default)]
    pub graphs: Vec<String>,
}

/// [API-AUTH-1] Ordered: each role may do everything the previous one may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Role {
    AgentRead,
    AgentWrite,
    Admin,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            listen: SocketAddr::from(([127, 0, 0, 1], 7420)),
            default_graph: "default".to_owned(),
            log: "info".to_owned(),
            log_file: None,
            max_body_bytes: 16 << 20,
            warmup_wait_ms: 2000,
            preload: true,
            text_index: true,
            maintenance_ms: 1000,
            slow_ms: 500,
            keys: Vec::new(),
        }
    }
}

impl Config {
    /// Reads `path` if given; a missing file is an error only when it was named explicitly.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let config = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
                toml::from_str(&text).map_err(|e| format!("invalid {}: {e}", p.display()))?
            }
            None => Self::default(),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        use mygraph_core::ids::{validate_actor_name, validate_graph_name};
        validate_graph_name(&self.default_graph)
            .map_err(|r| format!("default_graph {:?}: {r}", self.default_graph))?;
        for (i, k) in self.keys.iter().enumerate() {
            match (&k.key, &k.key_sha256) {
                (Some(key), None) if !key.is_empty() => {}
                (None, Some(h))
                    if h.len() == 64
                        && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) => {}
                _ => {
                    return Err(format!(
                        "keys[{i}]: set exactly one of key or key_sha256 (64 lowercase hex digits)"
                    ));
                }
            }
            validate_actor_name(&k.actor).map_err(|r| format!("keys[{i}].actor: {r}"))?;
            for g in &k.graphs {
                validate_graph_name(g).map_err(|r| format!("keys[{i}].graphs {g:?}: {r}"))?;
            }
        }
        // [API-AUTH-4]
        if self.keys.is_empty() && !self.listen.ip().is_loopback() {
            return Err(format!(
                "listen {} is not a loopback address: configure [[keys]] first",
                self.listen
            ));
        }
        Ok(())
    }

    /// The configuration without key material (`GET /admin/config`).
    pub fn redacted(&self) -> Json {
        json!({
            "data_dir": self.data_dir.display().to_string(),
            "listen": self.listen.to_string(),
            "default_graph": self.default_graph,
            "log": self.log,
            "log_file": self.log_file.as_ref().map(|p| p.display().to_string()),
            "max_body_bytes": self.max_body_bytes,
            "warmup_wait_ms": self.warmup_wait_ms,
            "preload": self.preload,
            "text_index": self.text_index,
            "maintenance_ms": self.maintenance_ms,
            "slow_ms": self.slow_ms,
            "keys": self.keys.len(),
        })
    }
}

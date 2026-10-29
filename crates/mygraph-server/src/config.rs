//! Server configuration (`server.toml`). Every field has a default, so the file is optional.
//! Environment variables override the file ([`Config::apply_env`]), which is how a container is
//! configured; command-line flags override both.

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
    /// Reads `path` if given (a missing file is an error only when it was named explicitly),
    /// then the environment.
    pub fn load(path: Option<&Path>) -> Result<Self, String> {
        let mut config: Self = match path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .map_err(|e| format!("cannot read {}: {e}", p.display()))?;
                toml::from_str(&text).map_err(|e| format!("invalid {}: {e}", p.display()))?
            }
            None => Self::default(),
        };
        config.apply_env(&|name| std::env::var(name).ok())?;
        config.validate()?;
        Ok(config)
    }

    /// Settings from environment variables, read through `var`:
    ///
    /// | Variable | Setting |
    /// |---|---|
    /// | `MYGRAPH_LISTEN` | `listen`, e.g. `0.0.0.0:7420` |
    /// | `MYGRAPH_DATA_DIR` | `data_dir` |
    /// | `MYGRAPH_DEFAULT_GRAPH` | `default_graph` |
    /// | `MYGRAPH_LOG` | `log` |
    /// | `MYGRAPH_ADMIN_KEY` | adds a key for actor `admin`, role `admin` |
    /// | `MYGRAPH_AGENT_KEY` | adds a key for actor `agent`, role `agent-write` |
    /// | `MYGRAPH_READ_KEY` | adds a key for actor `reader`, role `agent-read` |
    ///
    /// Each may instead be given as `<NAME>_FILE`, the path of a file holding the value (Docker
    /// secrets). Values are trimmed; empty ones count as unset. Keys add to those of the file.
    pub fn apply_env(&mut self, var: &dyn Fn(&str) -> Option<String>) -> Result<(), String> {
        let get = |name: &str| -> Result<Option<String>, String> {
            let set = |v: Option<String>| v.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
            if let Some(v) = set(var(name)) {
                return Ok(Some(v));
            }
            match set(var(&format!("{name}_FILE"))) {
                Some(path) => std::fs::read_to_string(&path)
                    .map(|s| set(Some(s)))
                    .map_err(|e| format!("{name}_FILE {path}: {e}")),
                None => Ok(None),
            }
        };
        if let Some(v) = get("MYGRAPH_LISTEN")? {
            self.listen = v
                .parse()
                .map_err(|e| format!("MYGRAPH_LISTEN {v:?}: {e}"))?;
        }
        if let Some(v) = get("MYGRAPH_DATA_DIR")? {
            self.data_dir = PathBuf::from(v);
        }
        if let Some(v) = get("MYGRAPH_DEFAULT_GRAPH")? {
            self.default_graph = v;
        }
        if let Some(v) = get("MYGRAPH_LOG")? {
            self.log = v;
        }
        for (name, actor, role) in [
            ("MYGRAPH_ADMIN_KEY", "admin", Role::Admin),
            ("MYGRAPH_AGENT_KEY", "agent", Role::AgentWrite),
            ("MYGRAPH_READ_KEY", "reader", Role::AgentRead),
        ] {
            if let Some(key) = get(name)? {
                self.keys.push(KeyConfig {
                    key: Some(key),
                    key_sha256: None,
                    actor: actor.to_owned(),
                    role,
                    graphs: Vec::new(),
                });
            }
        }
        Ok(())
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
                "listen {} is not a loopback address: configure [[keys]] or MYGRAPH_ADMIN_KEY first",
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

#[cfg(test)]
mod tests {
    use super::{Config, Role};
    use std::collections::HashMap;

    #[test]
    fn environment_overrides_and_keys() {
        let dir = tempfile::tempdir().unwrap();
        let secret = dir.path().join("agent_key");
        std::fs::write(&secret, "agent-secret\n").unwrap();
        let env: HashMap<&str, String> = HashMap::from([
            ("MYGRAPH_LISTEN", "0.0.0.0:7500".to_owned()),
            ("MYGRAPH_DATA_DIR", "/data".to_owned()),
            ("MYGRAPH_ADMIN_KEY", " admin-secret ".to_owned()),
            ("MYGRAPH_AGENT_KEY_FILE", secret.display().to_string()),
            ("MYGRAPH_READ_KEY", "  ".to_owned()),
        ]);
        let mut c = Config::default();
        c.apply_env(&|n| env.get(n).cloned()).unwrap();
        assert_eq!(c.listen.to_string(), "0.0.0.0:7500");
        assert_eq!(c.data_dir.to_str(), Some("/data"));
        let keys: Vec<_> = c
            .keys
            .iter()
            .map(|k| (k.key.as_deref(), k.actor.as_str(), k.role))
            .collect();
        assert_eq!(
            keys,
            [
                (Some("admin-secret"), "admin", Role::Admin),
                (Some("agent-secret"), "agent", Role::AgentWrite),
            ]
        );
        c.validate().unwrap();

        // Without keys a non-loopback listener is refused ([API-AUTH-4]).
        let mut c = Config::default();
        c.apply_env(&|n| (n == "MYGRAPH_LISTEN").then(|| "0.0.0.0:7420".to_owned()))
            .unwrap();
        assert!(c.validate().unwrap_err().contains("not a loopback address"));
        let bad =
            Config::default().apply_env(&|n| (n == "MYGRAPH_LISTEN").then(|| "nope".to_owned()));
        assert!(bad.unwrap_err().starts_with("MYGRAPH_LISTEN"));
        let missing = Config::default()
            .apply_env(&|n| (n == "MYGRAPH_ADMIN_KEY_FILE").then(|| "/no/such/file".to_owned()));
        assert!(missing.unwrap_err().starts_with("MYGRAPH_ADMIN_KEY_FILE"));
    }
}

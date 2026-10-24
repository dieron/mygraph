//! Server configuration (`server.toml`). Every field has a default, so the file is optional.

use serde::Deserialize;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Folder holding `graphs/` and `blobs/` (docs/03-storage-and-search.md §4).
    pub data_dir: PathBuf,
    /// Address of the HTTP listener. Local-only by default (docs/05-architecture.md §9).
    pub listen: SocketAddr,
    /// Graph used when a request names none.
    pub default_graph: String,
    /// `tracing` filter, e.g. `info` or `mygraph=debug`.
    pub log: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            data_dir: PathBuf::from("data"),
            listen: SocketAddr::from(([127, 0, 0, 1], 7420)),
            default_graph: "default".to_owned(),
            log: "info".to_owned(),
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
        mygraph_core::ids::validate_graph_name(&config.default_graph)
            .map_err(|r| format!("default_graph {:?}: {r}", config.default_graph))?;
        Ok(config)
    }
}

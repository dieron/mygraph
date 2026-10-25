//! API keys and roles ([API-AUTH-1] .. [API-AUTH-4]).

use crate::App;
use crate::config::{Config, Role};
use crate::error::ApiError;
use axum::extract::FromRequestParts;
use axum::http::header::AUTHORIZATION;
use axum::http::request::Parts;
use mygraph_core::store::sha256_hex;
use serde_json::{Value as Json, json};
use std::collections::HashMap;
use std::sync::Arc;

/// Who is calling: the actor recorded for changes, the role, and the graphs it may use.
#[derive(Debug, Clone)]
pub struct Caller {
    pub actor: String,
    pub role: Role,
    /// `None`: every graph.
    pub graphs: Option<Vec<String>>,
}

impl Caller {
    pub fn require(&self, role: Role) -> Result<(), ApiError> {
        if self.role >= role {
            Ok(())
        } else {
            let need = serde_json::to_value(role).unwrap_or(Json::Null);
            Err(ApiError::new(
                "auth.forbidden",
                format!("this needs the role {}", need.as_str().unwrap_or("?")),
            ))
        }
    }

    pub fn may_use(&self, graph: &str) -> bool {
        self.graphs
            .as_ref()
            .is_none_or(|gs| gs.iter().any(|g| g == graph))
    }

    pub fn check_graph(&self, graph: &str) -> Result<(), ApiError> {
        if self.may_use(graph) {
            Ok(())
        } else {
            Err(ApiError::new(
                "auth.graph_forbidden",
                format!("this key may not use graph {graph}"),
            ))
        }
    }
}

/// Configured keys by the SHA-256 of the key, so keys need not be kept in memory in clear.
pub struct Keys {
    by_hash: HashMap<String, Caller>,
}

impl Keys {
    pub fn new(config: &Config) -> Self {
        let by_hash = config
            .keys
            .iter()
            .map(|k| {
                let hash = match (&k.key, &k.key_sha256) {
                    (Some(key), _) => sha256_hex(key.as_bytes()),
                    (None, Some(h)) => h.clone(),
                    (None, None) => unreachable!("validated"),
                };
                let caller = Caller {
                    actor: k.actor.clone(),
                    role: k.role,
                    graphs: (!k.graphs.is_empty()).then(|| k.graphs.clone()),
                };
                (hash, caller)
            })
            .collect();
        Self { by_hash }
    }

    pub fn authenticate(&self, header: Option<&str>) -> Result<Caller, ApiError> {
        if self.by_hash.is_empty() {
            // [API-AUTH-4]
            return Ok(Caller {
                actor: "local".to_owned(),
                role: Role::Admin,
                graphs: None,
            });
        }
        let header = header.ok_or_else(|| {
            ApiError::new("auth.missing_key", "missing Authorization: Bearer <key>")
        })?;
        let key = header
            .strip_prefix("Bearer ")
            .or_else(|| header.strip_prefix("bearer "))
            .map(str::trim)
            .ok_or_else(|| ApiError::new("auth.invalid_key", "expected Bearer <key>"))?;
        self.by_hash
            .get(&sha256_hex(key.as_bytes()))
            .cloned()
            .ok_or_else(|| ApiError::new("auth.invalid_key", "unknown API key"))
    }

    /// `GET /admin/keys`: never the keys themselves.
    pub fn redacted(&self) -> Json {
        let mut keys: Vec<Json> = self
            .by_hash
            .iter()
            .map(|(h, c)| {
                json!({ "key_id": &h[..8], "actor": c.actor, "role": c.role,
                        "graphs": c.graphs.clone().unwrap_or_default() })
            })
            .collect();
        keys.sort_by(|a, b| a["actor"].as_str().cmp(&b["actor"].as_str()));
        json!({ "keys": keys })
    }
}

impl FromRequestParts<Arc<App>> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, ApiError> {
        let header = parts
            .headers
            .get(AUTHORIZATION)
            .map(|v| v.to_str().unwrap_or(""));
        app.keys.authenticate(header)
    }
}

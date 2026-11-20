//! Who may read or write each graph ([API-ACCESS-1] .. [API-ACCESS-5]).
//!
//! A graph's access is a level for `everyone` (every user of the server: `none` makes the graph
//! private, `read` public and read-only, `write` public and writable) and levels for single
//! users (the actors of the API keys), which override `everyone` either way. Admins may always
//! read and write; a key's role and its `graphs` list still limit what it can do.

use crate::auth::Caller;
use crate::config::Role;
use crate::error::ApiError;
use crate::server_db::ServerDb;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, PoisonError, RwLock};

/// What a caller may do with a graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    None,
    Read,
    Write,
}

impl Level {
    pub fn name(self) -> &'static str {
        match self {
            Level::None => "none",
            Level::Read => "read",
            Level::Write => "write",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Level::None),
            "read" => Some(Level::Read),
            "write" => Some(Level::Write),
            _ => None,
        }
    }
}

/// The access of one graph. A graph without a stored one is public and writable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Permissions {
    pub everyone: Level,
    pub users: BTreeMap<String, Level>,
}

impl Default for Permissions {
    fn default() -> Self {
        Self {
            everyone: Level::Write,
            users: BTreeMap::new(),
        }
    }
}

impl Permissions {
    pub fn to_json(&self) -> Json {
        let users: Map<String, Json> = self
            .users
            .iter()
            .map(|(u, l)| (u.clone(), l.name().into()))
            .collect();
        json!({ "everyone": self.everyone.name(), "users": users })
    }

    /// The level of a user, before the limits of their key.
    pub fn of(&self, actor: &str) -> Level {
        self.users.get(actor).copied().unwrap_or(self.everyone)
    }

    /// [API-ACCESS-3] Applies a change: `everyone` replaces the level for everyone; each entry
    /// of `users` sets that user's level, and `null` removes it (the user then has everyone's).
    pub fn apply(&mut self, change: &Json) -> Result<(), ApiError> {
        let bad = |m: String| ApiError::new("access.invalid_field", m);
        let change = change
            .as_object()
            .ok_or_else(|| bad("access: expected an object".into()))?;
        for k in change.keys() {
            if k != "everyone" && k != "users" {
                return Err(bad(format!("access.{k}: no such field")));
            }
        }
        if let Some(v) = change.get("everyone") {
            self.everyone = v
                .as_str()
                .and_then(Level::parse)
                .ok_or_else(|| bad("access.everyone: expected none, read or write".into()))?;
        }
        if let Some(users) = change.get("users") {
            let users = users
                .as_object()
                .ok_or_else(|| bad("access.users: expected an object".into()))?;
            for (user, level) in users {
                mygraph_core::ids::validate_actor_name(user)
                    .map_err(|r| bad(format!("access.users {user:?}: {r}")))?;
                match level {
                    Json::Null => {
                        self.users.remove(user);
                    }
                    Json::String(s) => {
                        let l = Level::parse(s).ok_or_else(|| {
                            bad(format!(
                                "access.users.{user}: expected none, read, write or null"
                            ))
                        })?;
                        self.users.insert(user.clone(), l);
                    }
                    _ => {
                        return Err(bad(format!(
                            "access.users.{user}: expected none, read, write or null"
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// The access of every graph, kept in `server.db` and cached in memory.
pub struct Access {
    db: Arc<ServerDb>,
    cache: RwLock<HashMap<String, Permissions>>,
}

impl Access {
    pub fn open(db: Arc<ServerDb>) -> Result<Self, String> {
        let mut cache = HashMap::new();
        {
            let conn = db.conn();
            let mut stmt = conn
                .prepare("SELECT graph, everyone, users FROM graph_access")
                .map_err(|e| e.to_string())?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })
                .map_err(|e| e.to_string())?;
            for row in rows {
                let (graph, everyone, users) = row.map_err(|e| e.to_string())?;
                // Unreadable rows deny rather than fall back to the public default.
                let everyone = Level::parse(&everyone).unwrap_or(Level::None);
                let users: BTreeMap<String, Level> = serde_json::from_str(&users)
                    .map_err(|e| format!("server.db: access of graph {graph}: {e}"))?;
                cache.insert(graph, Permissions { everyone, users });
            }
        }
        Ok(Self {
            db,
            cache: RwLock::new(cache),
        })
    }

    pub fn get(&self, graph: &str) -> Permissions {
        self.cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(graph)
            .cloned()
            .unwrap_or_default()
    }

    pub fn set(&self, graph: &str, p: &Permissions) -> Result<(), ApiError> {
        let users = serde_json::to_string(&p.users).unwrap_or_else(|_| "{}".into());
        self.db
            .conn()
            .execute(
                "INSERT INTO graph_access (graph, everyone, users) VALUES (?1, ?2, ?3)
                 ON CONFLICT(graph) DO UPDATE SET everyone = ?2, users = ?3",
                params![graph, p.everyone.name(), users],
            )
            .map_err(|e| ApiError::new("storage.server_db", e.to_string()))?;
        self.cache
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(graph.to_owned(), p.clone());
        Ok(())
    }

    /// Forgets a deleted graph's access, so a new graph of that name starts public.
    pub fn remove(&self, graph: &str) {
        let _ = self
            .db
            .conn()
            .execute("DELETE FROM graph_access WHERE graph = ?1", params![graph]);
        self.cache
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(graph);
    }

    /// [API-ACCESS-2] What `caller` may do with `graph`.
    pub fn level(&self, caller: &Caller, graph: &str) -> Level {
        if !caller.may_use(graph) {
            return Level::None;
        }
        let level = match caller.role {
            Role::Admin => return Level::Write,
            _ => self.get(graph).of(&caller.actor),
        };
        match caller.role {
            Role::AgentRead => level.min(Level::Read),
            _ => level,
        }
    }

    /// Fails unless `caller` may read (`need` = `Read`) or write `graph`.
    pub fn check(&self, caller: &Caller, graph: &str, need: Level) -> Result<(), ApiError> {
        let has = self.level(caller, graph);
        if has >= need {
            return Ok(());
        }
        Err(if has == Level::None {
            ApiError::new(
                "auth.graph_forbidden",
                format!("{} may not use graph {graph}", caller.actor),
            )
        } else {
            ApiError::new(
                "auth.graph_read_only",
                format!("{} may read graph {graph} but not change it", caller.actor),
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Access, Level, Permissions};
    use crate::auth::Caller;
    use crate::config::Role;
    use crate::server_db::ServerDb;
    use serde_json::json;
    use std::sync::Arc;

    fn caller(actor: &str, role: Role, graphs: Option<Vec<&str>>) -> Caller {
        Caller {
            actor: actor.into(),
            role,
            graphs: graphs.map(|g| g.into_iter().map(String::from).collect()),
            create_graphs: false,
        }
    }

    #[test]
    fn levels_combine_everyone_users_roles_and_key_graphs() {
        let dir = tempfile::tempdir().unwrap();
        let access = Access::open(Arc::new(ServerDb::open(dir.path()).unwrap())).unwrap();
        let alice = caller("alice", Role::AgentWrite, None);
        let bob = caller("bob", Role::AgentWrite, None);
        let reader = caller("alice", Role::AgentRead, None);
        let admin = caller("admin", Role::Admin, None);
        let fenced = caller("alice", Role::AgentWrite, Some(vec!["other"]));

        // Public and writable until changed.
        assert_eq!(access.level(&bob, "g"), Level::Write);
        assert_eq!(access.level(&reader, "g"), Level::Read);
        assert_eq!(access.level(&fenced, "g"), Level::None);

        let mut p = Permissions::default();
        p.apply(&json!({"everyone": "none", "users": {"alice": "write", "carol": "read"}}))
            .unwrap();
        access.set("g", &p).unwrap();
        assert_eq!(access.level(&alice, "g"), Level::Write);
        assert_eq!(access.level(&bob, "g"), Level::None);
        assert_eq!(access.level(&reader, "g"), Level::Read);
        assert_eq!(access.level(&admin, "g"), Level::Write);
        assert_eq!(
            access.check(&bob, "g", Level::Read).unwrap_err().code,
            "auth.graph_forbidden"
        );

        // A user's own entry overrides everyone's, either way; null removes it.
        p.apply(&json!({"everyone": "read", "users": {"alice": null, "bob": "none"}}))
            .unwrap();
        access.set("g", &p).unwrap();
        assert_eq!(access.level(&alice, "g"), Level::Read);
        assert_eq!(access.level(&bob, "g"), Level::None);
        assert_eq!(
            access.check(&alice, "g", Level::Write).unwrap_err().code,
            "auth.graph_read_only"
        );

        // Kept across a restart; forgotten with the graph.
        let again = Access::open(Arc::new(ServerDb::open(dir.path()).unwrap())).unwrap();
        assert_eq!(again.get("g"), p);
        again.remove("g");
        assert_eq!(again.get("g"), Permissions::default());

        for bad in [
            json!({"everyone": "all"}),
            json!({"users": {"alice": "admin"}}),
            json!({"users": ["alice"]}),
            json!({"owner": "alice"}),
        ] {
            assert!(Permissions::default().apply(&bad).is_err(), "{bad}");
        }
    }
}

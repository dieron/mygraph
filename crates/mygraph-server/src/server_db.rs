//! `<data_dir>/server.db`: what the server keeps besides the graphs — who may use each graph
//! ([`crate::access`]) and the OAuth clients, grants and tokens ([`crate::oauth`]).

use rusqlite::Connection;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

pub struct ServerDb {
    conn: Mutex<Connection>,
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS graph_access (
    graph    TEXT PRIMARY KEY,
    everyone TEXT NOT NULL,
    users    TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS oauth_clients (
    id            TEXT PRIMARY KEY,
    name          TEXT NOT NULL,
    redirect_uris TEXT NOT NULL,
    created       INTEGER NOT NULL,
    last_used     INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS oauth_grants (
    id          TEXT PRIMARY KEY,
    client_id   TEXT NOT NULL,
    client_name TEXT NOT NULL,
    key_sha256  TEXT NOT NULL,
    read_only   INTEGER NOT NULL,
    scope       TEXT NOT NULL,
    created     INTEGER NOT NULL,
    last_used   INTEGER NOT NULL,
    revoked     INTEGER
);
CREATE TABLE IF NOT EXISTS oauth_tokens (
    hash     TEXT PRIMARY KEY,
    grant_id TEXT NOT NULL,
    kind     TEXT NOT NULL,
    expires  INTEGER NOT NULL,
    used     INTEGER
);
CREATE INDEX IF NOT EXISTS oauth_tokens_grant ON oauth_tokens(grant_id);
";

impl ServerDb {
    /// Opens (or creates) `server.db` in `data_dir`.
    pub fn open(data_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| format!("cannot create {}: {e}", data_dir.display()))?;
        let path = data_dir.join("server.db");
        let conn = Connection::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .and_then(|()| conn.pragma_update(None, "synchronous", "NORMAL"))
            .and_then(|()| conn.execute_batch(SCHEMA))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

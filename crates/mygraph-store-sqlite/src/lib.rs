//! Storage format v1 on SQLite — spec/storage/schema.sql and spec/storage/storage-format.md.
//!
//! Opens and creates graph files; `records.rs` implements the engine's `Store` trait.

mod blobs;
mod records;

pub use blobs::BlobDir;

use mygraph_core::Reason;
use mygraph_core::ids::validate_graph_name;
pub use mygraph_core::model::{Policies, RequireEvidence};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use std::fmt;
use std::path::{Path, PathBuf};

/// [STORE-FILE-3] "MYGR"
pub const APPLICATION_ID: i32 = 0x4D59_4752;

/// Migrations in order: (format version reached, SQL). The SQL files are the spec's own.
const MIGRATIONS: &[(u32, &str)] = &[(
    1,
    include_str!("../../../spec/storage/migrations/0001_init.sql"),
)];

#[derive(Debug)]
pub enum StoreError {
    /// A spec reason code: `graph.exists`, `graph.not_found`, `storage.not_mygraph`, …
    Reason(Reason),
    /// The file's format version is newer than this implementation ([STORE-FILE-3]).
    TooNew {
        found: u32,
        supported: u32,
    },
    Sqlite(rusqlite::Error),
    Io(std::io::Error),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reason(r) => write!(f, "{r}"),
            Self::TooNew { found, supported } => write!(
                f,
                "storage.too_new: format version {found}, this build supports up to {supported}"
            ),
            Self::Sqlite(e) => write!(f, "sqlite: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<Reason> for StoreError {
    fn from(r: Reason) -> Self {
        Self::Reason(r)
    }
}

impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Sqlite(e)
    }
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// [STORE-DIR-1] `<data_dir>/graphs/<name>.db`
pub fn graph_path(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join("graphs").join(format!("{name}.db"))
}

/// One open graph database file.
pub struct GraphFile {
    conn: Connection,
    name: String,
    blobs: BlobDir,
}

impl GraphFile {
    /// [STORE-DIR-2] Creates a new graph file with the current format version.
    pub fn create(
        data_dir: &Path,
        name: &str,
        policies: &Policies,
        now_ms: i64,
    ) -> Result<Self, StoreError> {
        validate_graph_name(name)?;
        policies.validate()?;
        let path = graph_path(data_dir, name);
        if path.exists() {
            return Err(Reason("graph.exists").into());
        }
        std::fs::create_dir_all(path.parent().expect("graphs folder"))?;
        let mut conn = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        )?;
        configure(&conn)?;
        let tx = conn.transaction()?;
        for (_, sql) in MIGRATIONS {
            tx.execute_batch(sql)?;
        }
        let meta = [
            ("graph_name", name.to_owned()),
            ("created_at", now_ms.to_string()),
            ("spec_version", mygraph_core::SPEC_VERSION.to_owned()),
            ("policies", policies.to_json().to_string()),
        ];
        for (k, v) in meta {
            tx.execute("INSERT INTO meta (k, v) VALUES (?1, ?2)", (k, v))?;
        }
        tx.commit()?;
        Ok(Self {
            conn,
            name: name.to_owned(),
            blobs: BlobDir::new(data_dir),
        })
    }

    /// [STORE-DIR-3] Opens an existing graph file, upgrading older formats.
    pub fn open(data_dir: &Path, name: &str) -> Result<Self, StoreError> {
        validate_graph_name(name)?;
        let path = graph_path(data_dir, name);
        if !path.is_file() {
            return Err(Reason("graph.not_found").into());
        }
        let mut conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        let app_id: i32 = conn.query_row("PRAGMA application_id", [], |r| r.get(0))?;
        if app_id != APPLICATION_ID {
            return Err(Reason("storage.not_mygraph").into());
        }
        let found: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let supported = mygraph_core::STORAGE_FORMAT_VERSION;
        if found > supported {
            return Err(StoreError::TooNew { found, supported });
        }
        configure(&conn)?;
        if found < supported {
            let tx = conn.transaction()?;
            for (_, sql) in MIGRATIONS.iter().filter(|(v, _)| *v > found) {
                tx.execute_batch(sql)?;
            }
            tx.commit()?;
        }
        Ok(Self {
            conn,
            name: name.to_owned(),
            blobs: BlobDir::new(data_dir),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Value of a `meta` row ([STORE-META-1]).
    pub fn meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row("SELECT v FROM meta WHERE k = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn policies(&self) -> Result<Policies, StoreError> {
        let text = self
            .meta("policies")?
            .ok_or(Reason("storage.meta_missing"))?;
        let json: serde_json::Value =
            serde_json::from_str(&text).map_err(|_| Reason("storage.meta_invalid"))?;
        Ok(Policies::from_json(&json).ok_or(Reason("storage.meta_invalid"))?)
    }

    /// Format version from the database header.
    pub fn format_version(&self) -> Result<u32, StoreError> {
        Ok(self
            .conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

/// [STORE-FILE-4]
fn configure(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "FULL")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_761_300_000_000;

    #[test]
    fn create_then_open_keeps_meta_and_policies() {
        let dir = tempfile::tempdir().unwrap();
        let policies = Policies {
            require_evidence: RequireEvidence::Warn,
            inline_text_limit: 4096,
            normalize_types: false,
        };
        GraphFile::create(dir.path(), "research", &policies, NOW).unwrap();

        let g = GraphFile::open(dir.path(), "research").unwrap();
        assert_eq!(g.meta("format").unwrap().as_deref(), Some("mygraph"));
        assert_eq!(g.meta("format_version").unwrap().as_deref(), Some("1"));
        assert_eq!(g.meta("graph_name").unwrap().as_deref(), Some("research"));
        assert_eq!(g.meta("created_at").unwrap(), Some(NOW.to_string()));
        assert_eq!(g.policies().unwrap(), policies);
        assert_eq!(g.format_version().unwrap(), 1);
        let mode: String = g
            .connection()
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn created_schema_matches_spec_schema() {
        let dir = tempfile::tempdir().unwrap();
        let g = GraphFile::create(dir.path(), "g", &Policies::default(), NOW).unwrap();
        let spec = Connection::open_in_memory().unwrap();
        spec.execute_batch(include_str!("../../../spec/storage/schema.sql"))
            .unwrap();
        let q = "SELECT type, name, sql FROM sqlite_master ORDER BY type, name";
        let rows = |c: &Connection| -> Vec<(String, String, Option<String>)> {
            let mut st = c.prepare(q).unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(rows(g.connection()), rows(&spec));
    }

    #[test]
    fn create_refuses_existing_and_invalid_names() {
        let dir = tempfile::tempdir().unwrap();
        GraphFile::create(dir.path(), "g", &Policies::default(), NOW).unwrap();
        let err = |r: Result<GraphFile, StoreError>| match r {
            Err(StoreError::Reason(r)) => r.code(),
            Err(e) => panic!("unexpected {e}"),
            Ok(_) => panic!("expected an error"),
        };
        assert_eq!(
            err(GraphFile::create(
                dir.path(),
                "g",
                &Policies::default(),
                NOW
            )),
            "graph.exists"
        );
        assert_eq!(
            err(GraphFile::create(
                dir.path(),
                "con",
                &Policies::default(),
                NOW
            )),
            "graph_name.reserved"
        );
        let bad = Policies {
            inline_text_limit: 10,
            ..Policies::default()
        };
        assert_eq!(
            err(GraphFile::create(dir.path(), "h", &bad, NOW)),
            "graph.policy_invalid"
        );
        assert_eq!(
            err(GraphFile::open(dir.path(), "missing")),
            "graph.not_found"
        );
    }

    #[test]
    fn open_refuses_foreign_and_newer_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = graph_path(dir.path(), "foreign");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        Connection::open(&path)
            .unwrap()
            .execute_batch("CREATE TABLE t (x)")
            .unwrap();
        assert!(matches!(
            GraphFile::open(dir.path(), "foreign"),
            Err(StoreError::Reason(Reason("storage.not_mygraph")))
        ));

        GraphFile::create(dir.path(), "newer", &Policies::default(), NOW).unwrap();
        Connection::open(graph_path(dir.path(), "newer"))
            .unwrap()
            .execute_batch("PRAGMA user_version = 99")
            .unwrap();
        assert!(matches!(
            GraphFile::open(dir.path(), "newer"),
            Err(StoreError::TooNew {
                found: 99,
                supported: 1
            })
        ));
    }
}

//! Storage format v2 on SQLite — spec/storage/schema.sql and spec/storage/storage-format.md.
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
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// [STORE-FILE-3] "MYGR"
pub const APPLICATION_ID: i32 = 0x4D59_4752;

/// Migrations in order: (format version reached, SQL). The SQL files are the spec's own.
const MIGRATIONS: &[(u32, &str)] = &[
    (
        1,
        include_str!("../../../spec/storage/migrations/0001_init.sql"),
    ),
    (
        2,
        include_str!("../../../spec/storage/migrations/0002_stemming.sql"),
    ),
];

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
    /// Behind a mutex so a `GraphFile` can be shared between threads (reads run in parallel on
    /// the in-memory state; only reads that reach SQLite take turns).
    conn: Mutex<Connection>,
    name: String,
    blobs: BlobDir,
    checkpointer: Arc<Checkpointer>,
}

/// Copies the write-ahead log back into the database file from a connection of its own, so
/// checkpoints run beside writes instead of inside one (see [`GraphFile::background_checkpoints`]).
pub struct Checkpointer {
    path: PathBuf,
    conn: Mutex<Option<Connection>>,
}

impl Checkpointer {
    /// A passive checkpoint: copies what it can without waiting for or blocking anyone.
    pub fn run(&self) -> Result<(), StoreError> {
        let mut conn = self.conn.lock().unwrap_or_else(PoisonError::into_inner);
        if conn.is_none() {
            *conn = Some(Connection::open_with_flags(
                &self.path,
                OpenFlags::SQLITE_OPEN_READ_WRITE,
            )?);
        }
        let c = conn.as_ref().expect("opened");
        c.query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |_| Ok(()))?;
        Ok(())
    }
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
            conn: Mutex::new(conn),
            name: name.to_owned(),
            blobs: BlobDir::new(data_dir),
            checkpointer: Arc::new(Checkpointer {
                path: path.clone(),
                conn: Mutex::new(None),
            }),
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
            conn: Mutex::new(conn),
            name: name.to_owned(),
            blobs: BlobDir::new(data_dir),
            checkpointer: Arc::new(Checkpointer {
                path: path.clone(),
                conn: Mutex::new(None),
            }),
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Value of a `meta` row ([STORE-META-1]).
    pub fn meta(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .connection()
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
            .connection()
            .query_row("PRAGMA user_version", [], |r| r.get(0))?)
    }

    /// The connection, for this thread until the guard is dropped.
    pub fn connection(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn conn_mut(&mut self) -> &mut Connection {
        self.conn.get_mut().unwrap_or_else(PoisonError::into_inner)
    }

    /// Turns off SQLite's automatic checkpoints, which run inside the commit that crosses the
    /// threshold (on a large file that write then stalls); the caller runs
    /// [`Checkpointer::run`] periodically instead.
    pub fn background_checkpoints(&self) -> Result<Arc<Checkpointer>, StoreError> {
        self.connection()
            .pragma_update(None, "wal_autocheckpoint", 0)?;
        Ok(self.checkpointer.clone())
    }

    /// Writes a consistent copy of the database to `target` (a new file) while the graph stays
    /// online (`VACUUM INTO`).
    pub fn snapshot_to(&self, target: &Path) -> Result<(), StoreError> {
        let target = target.to_str().ok_or(Reason("storage.path_invalid"))?;
        self.connection().execute("VACUUM INTO ?1", [target])?;
        Ok(())
    }

    /// SQLite's own consistency checks: `integrity_check` and `foreign_key_check`. Returns the
    /// problems found; empty when the file is sound.
    pub fn sqlite_check(&self) -> Result<Vec<String>, StoreError> {
        let c = self.connection();
        let mut problems = Vec::new();
        let mut st = c.prepare("PRAGMA integrity_check")?;
        for r in st.query_map([], |r| r.get::<_, String>(0))? {
            let r = r?;
            if r != "ok" {
                problems.push(r);
            }
        }
        let mut st = c.prepare("PRAGMA foreign_key_check")?;
        for r in st.query_map([], |r| {
            Ok(format!(
                "foreign key: {} row {} -> {}",
                r.get::<_, String>(0)?,
                r.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                r.get::<_, String>(2)?
            ))
        })? {
            problems.push(r?);
        }
        Ok(problems)
    }

    /// Sizes in bytes of the database file and its write-ahead log.
    pub fn file_sizes(data_dir: &Path, name: &str) -> (u64, u64) {
        let db = graph_path(data_dir, name);
        let size = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
        let mut wal = db.clone().into_os_string();
        wal.push("-wal");
        (size(&db), size(Path::new(&wal)))
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
            naming: mygraph_core::model::Naming::Free,
        };
        GraphFile::create(dir.path(), "research", &policies, NOW).unwrap();

        let g = GraphFile::open(dir.path(), "research").unwrap();
        assert_eq!(g.meta("format").unwrap().as_deref(), Some("mygraph"));
        assert_eq!(g.meta("format_version").unwrap().as_deref(), Some("2"));
        assert_eq!(g.meta("graph_name").unwrap().as_deref(), Some("research"));
        assert_eq!(g.meta("created_at").unwrap(), Some(NOW.to_string()));
        assert_eq!(g.policies().unwrap(), policies);
        assert_eq!(g.format_version().unwrap(), 2);
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
        // SQLite keeps each statement's text as written; line endings depend on how git checked
        // out the two files (core.autocrlf), so they are not compared.
        let rows = |c: &Connection| -> Vec<(String, String, Option<String>)> {
            let mut st = c.prepare(q).unwrap();
            st.query_map([], |r| {
                let sql: Option<String> = r.get(2)?;
                Ok((r.get(0)?, r.get(1)?, sql.map(|s| s.replace("\r\n", "\n"))))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        assert_eq!(rows(&g.connection()), rows(&spec));
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
                supported: 2
            })
        ));
    }

    /// A version-1 file opens as version 2: its full-text rows are carried over and now match
    /// other word forms ([STORE-FTS-4]).
    #[test]
    fn version_1_files_are_upgraded_to_stemmed_full_text() {
        let dir = tempfile::tempdir().unwrap();
        let path = graph_path(dir.path(), "old");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(MIGRATIONS[0].1).unwrap();
            c.execute_batch(
                "INSERT INTO meta (k, v) VALUES ('graph_name', 'old'), ('created_at', '1'),
                   ('spec_version', '0.1.0-draft'), ('policies', '{}');
                 INSERT INTO fts_node (rowid, name, aliases, summary, props_text)
                   VALUES (7, 'Gina''s clothes store', NULL, NULL, NULL);
                 INSERT INTO fts_link (rowid, type_text, props_text) VALUES (3, 'OPENED', NULL);
                 INSERT INTO actor (idx, name, kind, created_at) VALUES (1, 'test', 'agent', 1);
                 INSERT INTO evidence (idx, kind, content, content_sha256, captured_at,
                   created_at, created_by, updated_at, updated_by)
                   VALUES (5, 'text', 'My online clothes store is open!', printf('%064d', 0),
                     1, 1, 1, 1, 1);
                 INSERT INTO fts_evidence (rowid, title, content)
                   VALUES (5, NULL, 'My online clothes store is open!');",
            )
            .unwrap();
        }
        let g = GraphFile::open(dir.path(), "old").unwrap();
        assert_eq!(g.format_version().unwrap(), 2);
        assert_eq!(g.meta("format_version").unwrap().as_deref(), Some("2"));
        let hits = |sql: &str| -> Vec<i64> {
            let c = g.connection();
            let mut st = c.prepare(sql).unwrap();
            st.query_map([], |r| r.get(0))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        };
        assert_eq!(
            hits("SELECT rowid FROM fts_node WHERE fts_node MATCH 'clothing'"),
            vec![7]
        );
        assert_eq!(
            hits("SELECT rowid FROM fts_link WHERE fts_link MATCH 'opens'"),
            vec![3]
        );
        assert_eq!(
            hits("SELECT rowid FROM fts_evidence WHERE fts_evidence MATCH 'clothing opened'"),
            vec![5]
        );
    }
}

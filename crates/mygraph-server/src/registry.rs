//! Open graphs: discovery, loading in the background, warm-up waits, create and delete
//! ([API-GRAPH-1] .. [API-GRAPH-3], [ADMIN-GRAPH-2] .. [ADMIN-GRAPH-3]).

use crate::error::ApiError;
use mygraph_core::engine::Graph;
use mygraph_core::model::Policies;
use mygraph_semantic::{Embed, UsearchIndex};
use mygraph_store_sqlite::{Checkpointer, GraphFile, graph_path};
use mygraph_text_tantivy::{TantivyText, index_dir};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::watch;

pub type G = Graph<GraphFile>;

/// One open graph. `None` after the graph was deleted: requests that still hold the handle then
/// answer `graph.not_found`. Reads share the lock; writes take it alone ([API-GRAPH-3]).
pub type Handle = Arc<RwLock<Option<G>>>;

#[derive(Clone)]
pub enum SlotState {
    Unloaded,
    Loading,
    Ready(Handle),
    Failed(String),
}

impl SlotState {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Unloaded => "unloaded",
            Self::Loading => "loading",
            Self::Ready(_) => "ready",
            Self::Failed(_) => "failed",
        }
    }
}

/// Background work per graph since the server started, for `/metrics` ([ADMIN-MET-1]).
#[derive(Debug, Clone, Default)]
pub struct GraphRun {
    pub load_s: Option<f64>,
    pub text_commits: u64,
    pub text_commit_errors: u64,
    pub text_commit_s: f64,
    pub checkpoints: u64,
    pub checkpoint_errors: u64,
    pub checkpoint_s: f64,
    pub semantic_commits: u64,
    pub semantic_commit_errors: u64,
    pub semantic_commit_s: f64,
    /// When the semantic index was last committed.
    pub semantic_saved: Option<Instant>,
}

/// How often the semantic index is saved while it is ready: saving writes the whole index, and
/// it is only a cache (after a crash it catches up from the change log). While it is building,
/// every maintenance run commits, to embed the waiting documents.
const SEMANTIC_SAVE_EVERY: Duration = Duration::from_secs(30);

pub struct Registry {
    data_dir: PathBuf,
    text_index: bool,
    /// The embedding model shared by every graph's semantic index.
    model: Option<Arc<dyn Embed>>,
    warmup_wait: Duration,
    slots: Mutex<BTreeMap<String, Arc<watch::Sender<SlotState>>>>,
    /// Background WAL checkpoints of the loaded graphs.
    checkpointers: Mutex<BTreeMap<String, Arc<Checkpointer>>>,
    runs: Mutex<BTreeMap<String, GraphRun>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

impl Registry {
    /// Finds the graphs in `<data_dir>/graphs/`; none is loaded yet.
    pub fn new(
        data_dir: &Path,
        text_index: bool,
        model: Option<Arc<dyn Embed>>,
        warmup_wait: Duration,
    ) -> Self {
        let mut slots = BTreeMap::new();
        if let Ok(dir) = std::fs::read_dir(data_dir.join("graphs")) {
            for e in dir.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "db")
                    && let Some(name) = p.file_stem().and_then(|s| s.to_str())
                    && mygraph_core::ids::validate_graph_name(name).is_ok()
                {
                    slots.insert(
                        name.to_owned(),
                        Arc::new(watch::Sender::new(SlotState::Unloaded)),
                    );
                }
            }
        }
        Self {
            data_dir: data_dir.to_owned(),
            text_index,
            model,
            warmup_wait,
            slots: Mutex::new(slots),
            checkpointers: Mutex::new(BTreeMap::new()),
            runs: Mutex::new(BTreeMap::new()),
        }
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Every graph with its state, by name.
    pub fn list(&self) -> Vec<(String, SlotState)> {
        lock(&self.slots)
            .iter()
            .map(|(n, s)| (n.clone(), s.borrow().clone()))
            .collect()
    }

    /// Background work of `name` since the server started.
    pub fn run(&self, name: &str) -> GraphRun {
        lock(&self.runs).get(name).cloned().unwrap_or_default()
    }

    fn update_run(&self, name: &str, f: impl FnOnce(&mut GraphRun)) {
        f(lock(&self.runs).entry(name.to_owned()).or_default());
    }

    pub fn state(&self, name: &str) -> Option<SlotState> {
        lock(&self.slots).get(name).map(|s| s.borrow().clone())
    }

    /// Starts loading every unloaded graph.
    pub fn preload(self: &Arc<Self>) {
        for (name, _) in self.list() {
            if let Some(slot) = lock(&self.slots).get(&name).cloned() {
                self.start_load(&name, &slot);
            }
        }
    }

    fn start_load(self: &Arc<Self>, name: &str, slot: &Arc<watch::Sender<SlotState>>) {
        let start = slot.send_if_modified(|s| {
            if matches!(s, SlotState::Unloaded) {
                *s = SlotState::Loading;
                true
            } else {
                false
            }
        });
        if !start {
            return;
        }
        let (me, name, slot) = (self.clone(), name.to_owned(), slot.clone());
        tokio::task::spawn_blocking(move || {
            let t = std::time::Instant::now();
            let state = match me.load(&name) {
                Ok(g) => {
                    tracing::info!(graph = %name, ms = t.elapsed().as_millis() as u64, "graph ready");
                    SlotState::Ready(Arc::new(RwLock::new(Some(g))))
                }
                Err(e) => {
                    tracing::error!(graph = %name, "graph failed to load: {e}");
                    SlotState::Failed(e)
                }
            };
            slot.send_replace(state);
        });
    }

    fn load(&self, name: &str) -> Result<G, String> {
        let start = std::time::Instant::now();
        let g = self.open_graph(name)?;
        let took = start.elapsed().as_secs_f64();
        self.update_run(name, |r| r.load_s = Some(took));
        Ok(g)
    }

    fn open_graph(&self, name: &str) -> Result<G, String> {
        let file = GraphFile::open(&self.data_dir, name).map_err(|e| e.to_string())?;
        let policies = file.policies().map_err(|e| e.to_string())?;
        let mut g = Graph::open(file, name, policies).map_err(|e| e.to_string())?;
        self.manage_checkpoints(name, &g);
        if self.text_index {
            // Without the index, search falls back to the store's own full text.
            match TantivyText::open(&self.data_dir, name)
                .and_then(|t| g.attach_text_index(Box::new(t)))
            {
                Ok(()) => {}
                Err(e) => tracing::warn!(graph = %name, "full-text index unavailable: {e}"),
            }
        }
        self.attach_semantic(name, &mut g);
        Ok(g)
    }

    /// Attaches the graph's semantic index when there is a model; without it, search matches
    /// words only.
    fn attach_semantic(&self, name: &str, g: &mut G) {
        if let Some(model) = &self.model
            && let Err(e) = UsearchIndex::open(&self.data_dir, name, model.clone())
                .and_then(|t| g.attach_semantic_index(Box::new(t)))
        {
            tracing::warn!(graph = %name, "semantic index unavailable: {e}");
        }
    }

    /// The graph, loading it if needed and waiting up to the warm-up time ([API-GRAPH-2]).
    pub async fn get(self: &Arc<Self>, name: &str) -> Result<Handle, ApiError> {
        let slot = lock(&self.slots).get(name).cloned();
        let slot = match slot {
            Some(s) => s,
            // A graph file created by another process since the start.
            None if mygraph_core::ids::validate_graph_name(name).is_ok()
                && graph_path(&self.data_dir, name).is_file() =>
            {
                lock(&self.slots)
                    .entry(name.to_owned())
                    .or_insert_with(|| Arc::new(watch::Sender::new(SlotState::Unloaded)))
                    .clone()
            }
            None => return Err(not_found(name)),
        };
        self.start_load(name, &slot);
        let mut rx = slot.subscribe();
        let ready = tokio::time::timeout(
            self.warmup_wait,
            rx.wait_for(|s| !matches!(s, SlotState::Loading)),
        )
        .await;
        let state = match ready {
            Ok(Ok(s)) => s.clone(),
            Ok(Err(_)) => return Err(not_found(name)),
            Err(_) => {
                let mut e = ApiError::new("graph.warming_up", format!("graph {name} is loading"));
                e.retry_after = Some(5);
                return Err(e);
            }
        };
        match state {
            SlotState::Ready(h) => Ok(h),
            SlotState::Failed(msg) => Err(ApiError::new(
                "graph.load_failed",
                format!("graph {name} failed to load: {msg}"),
            )),
            SlotState::Unloaded | SlotState::Loading => Err(not_found(name)),
        }
    }

    /// Creates a graph and loads it ([ADMIN-GRAPH-2]).
    pub fn create(&self, name: &str, policies: &Policies) -> Result<(), ApiError> {
        let mut slots = lock(&self.slots);
        if slots.contains_key(name) {
            return Err(ApiError::new(
                "graph.exists",
                format!("graph {name} exists"),
            ));
        }
        let start = std::time::Instant::now();
        let file =
            GraphFile::create(&self.data_dir, name, policies, now_ms()).map_err(store_err)?;
        let mut g =
            Graph::open(file, name, policies.clone()).map_err(|e| internal(&e.to_string()))?;
        self.manage_checkpoints(name, &g);
        if self.text_index {
            // A stale cache from an earlier graph of the same name is rebuilt by attaching.
            let _ = std::fs::remove_dir_all(index_dir(&self.data_dir, name));
            if let Err(e) = TantivyText::open(&self.data_dir, name)
                .and_then(|t| g.attach_text_index(Box::new(t)))
            {
                tracing::warn!(graph = %name, "full-text index unavailable: {e}");
            }
        }
        let _ = std::fs::remove_dir_all(mygraph_semantic::index_dir(&self.data_dir, name));
        self.attach_semantic(name, &mut g);
        slots.insert(
            name.to_owned(),
            Arc::new(watch::Sender::new(SlotState::Ready(Arc::new(RwLock::new(
                Some(g),
            ))))),
        );
        let took = start.elapsed().as_secs_f64();
        self.update_run(name, |r| {
            *r = GraphRun {
                load_s: Some(took),
                ..GraphRun::default()
            }
        });
        Ok(())
    }

    /// Closes a graph and moves its files to the trash folder ([ADMIN-GRAPH-3]).
    pub fn delete(&self, name: &str) -> Result<PathBuf, ApiError> {
        let slot = lock(&self.slots)
            .remove(name)
            .ok_or_else(|| not_found(name))?;
        if let SlotState::Ready(h) = &*slot.borrow() {
            // Waits for running requests, then closes the database and the text index.
            h.write().unwrap_or_else(PoisonError::into_inner).take();
        }
        slot.send_replace(SlotState::Unloaded);
        // Its connection keeps the file open, which would stop the move.
        lock(&self.checkpointers).remove(name);
        let trash = self.data_dir.join("trash");
        std::fs::create_dir_all(&trash).map_err(|e| internal(&e.to_string()))?;
        let stamp = crate::admin::stamp(now_ms());
        let db = graph_path(&self.data_dir, name);
        let target = trash.join(format!("{name}-{stamp}.db"));
        std::fs::rename(&db, &target).map_err(|e| internal(&e.to_string()))?;
        for suffix in ["-wal", "-shm"] {
            let mut from = db.clone().into_os_string();
            from.push(suffix);
            let mut to = target.clone().into_os_string();
            to.push(suffix);
            if Path::new(&from).exists() {
                let _ = std::fs::rename(&from, &to);
            }
        }
        let _ = std::fs::remove_dir_all(index_dir(&self.data_dir, name));
        let _ = std::fs::remove_dir_all(mygraph_semantic::index_dir(&self.data_dir, name));
        Ok(target)
    }
}

pub fn not_found(name: &str) -> ApiError {
    ApiError::new("graph.not_found", format!("no graph named {name}"))
}

fn internal(msg: &str) -> ApiError {
    ApiError::new("internal", msg)
}

fn store_err(e: mygraph_store_sqlite::StoreError) -> ApiError {
    match e {
        mygraph_store_sqlite::StoreError::Reason(r) => ApiError::new(r.code(), r.code()),
        other => ApiError::new("storage.failure", other.to_string()),
    }
}

impl Registry {
    /// Commits the full-text index of every loaded graph that has pending updates, holding no
    /// graph lock: reads and writes go on meanwhile.
    pub fn commit_text_all(&self) {
        for (name, state) in self.list() {
            let SlotState::Ready(h) = state else { continue };
            // The handle is taken under the lock, the commit runs without it. A graph busy with a
            // long write (an import) is skipped rather than waited for, so checkpoints still run.
            let (text, semantic) = match h.try_read() {
                Ok(g) => handles(g.as_ref()),
                Err(std::sync::TryLockError::Poisoned(p)) => handles(p.into_inner().as_ref()),
                Err(std::sync::TryLockError::WouldBlock) => continue,
            };
            self.commit_semantic(&name, &h, semantic);
            let Some(t) = text.filter(|t| t.pending() > 0) else {
                continue;
            };
            let start = std::time::Instant::now();
            let result = t.commit();
            let took = start.elapsed().as_secs_f64();
            self.update_run(&name, |r| {
                r.text_commit_s += took;
                match &result {
                    Ok(()) => r.text_commits += 1,
                    Err(_) => r.text_commit_errors += 1,
                }
            });
            if let Err(e) = result {
                tracing::warn!(graph = %name, "full-text index commit failed, search falls back to FTS5: {e}");
                if let Some(g) = h.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
                    g.mark_text_stale();
                }
            }
        }
    }
}

/// The graph's full-text and semantic index handles.
type Handles = (
    Option<Arc<dyn mygraph_core::store::TextIndex>>,
    Option<Arc<dyn mygraph_core::store::SemanticIndex>>,
);

fn handles(g: Option<&G>) -> Handles {
    (
        g.and_then(|g| g.text_handle()),
        g.and_then(|g| g.semantic_handle()),
    )
}

impl Registry {
    /// Embeds waiting documents and saves the semantic index: every run while it is building,
    /// else at most every [`SEMANTIC_SAVE_EVERY`].
    fn commit_semantic(
        &self,
        name: &str,
        h: &Handle,
        index: Option<Arc<dyn mygraph_core::store::SemanticIndex>>,
    ) {
        let Some(t) = index.filter(|t| t.pending() > 0) else {
            return;
        };
        let due = !t.ready()
            || self
                .run(name)
                .semantic_saved
                .is_none_or(|at| at.elapsed() >= SEMANTIC_SAVE_EVERY);
        if !due {
            return;
        }
        let start = Instant::now();
        let result = t.commit();
        let took = start.elapsed().as_secs_f64();
        self.update_run(name, |r| {
            r.semantic_commit_s += took;
            r.semantic_saved = Some(Instant::now());
            match &result {
                Ok(()) => r.semantic_commits += 1,
                Err(_) => r.semantic_commit_errors += 1,
            }
        });
        if let Err(e) = result {
            tracing::warn!(graph = %name, "semantic index commit failed, search matches words only: {e}");
            if let Some(g) = h.read().unwrap_or_else(PoisonError::into_inner).as_ref() {
                g.mark_semantic_stale();
            }
        }
    }

    /// Saves every semantic index with waiting changes now (at shutdown).
    pub fn flush_semantic(&self) {
        for (name, state) in self.list() {
            let SlotState::Ready(h) = state else { continue };
            let index = h
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .as_ref()
                .and_then(|g| g.semantic_handle());
            self.update_run(&name, |r| r.semantic_saved = None);
            self.commit_semantic(&name, &h, index);
        }
    }

    fn manage_checkpoints(&self, name: &str, g: &G) {
        match g.store().background_checkpoints() {
            Ok(c) => {
                lock(&self.checkpointers).insert(name.to_owned(), c);
            }
            Err(e) => tracing::warn!(graph = %name, "background checkpoints unavailable: {e}"),
        }
    }

    /// Periodic maintenance, holding no graph lock: commits full-text indexes and runs passive
    /// WAL checkpoints.
    pub fn maintain(&self) {
        self.commit_text_all();
        let all: Vec<(String, Arc<Checkpointer>)> = lock(&self.checkpointers)
            .iter()
            .map(|(n, c)| (n.clone(), c.clone()))
            .collect();
        for (name, c) in all {
            let start = std::time::Instant::now();
            let result = c.run();
            let took = start.elapsed().as_secs_f64();
            self.update_run(&name, |r| {
                r.checkpoint_s += took;
                match &result {
                    Ok(()) => r.checkpoints += 1,
                    Err(_) => r.checkpoint_errors += 1,
                }
            });
            if let Err(e) = result {
                tracing::warn!(graph = %name, "checkpoint failed: {e}");
            }
        }
    }
}

impl Registry {
    /// A file name inside one of the data folder's subfolders; nothing outside it can be named.
    pub fn data_file(&self, folder: &str, name: &str) -> Result<PathBuf, ApiError> {
        let plain =
            !name.is_empty() && !name.contains(['/', '\\', ':']) && name != "." && name != "..";
        let path = self.data_dir.join(folder).join(name);
        if plain && path.is_file() {
            Ok(path)
        } else {
            let code = if folder == "snapshots" {
                "admin.snapshot_not_found"
            } else {
                "admin.file_not_found"
            };
            Err(ApiError::new(code, format!("no file {name} in {folder}/")))
        }
    }

    /// Makes a snapshot the graph `name` ([ADMIN-GRAPH-6]); it loads on first use.
    pub fn restore(&self, name: &str, snapshot: &Path, replace: bool) -> Result<(), ApiError> {
        mygraph_core::ids::validate_graph_name(name)
            .map_err(|r| ApiError::new(r.code(), format!("name {name:?}: {r}")))?;
        if lock(&self.slots).contains_key(name) {
            if !replace {
                return Err(ApiError::new(
                    "graph.exists",
                    format!("graph {name} exists; pass replace: true to replace it"),
                ));
            }
            self.delete(name)?;
        }
        let target = graph_path(&self.data_dir, name);
        std::fs::create_dir_all(target.parent().expect("graphs folder"))
            .map_err(|e| internal(&e.to_string()))?;
        std::fs::copy(snapshot, &target).map_err(|e| internal(&e.to_string()))?;
        let _ = std::fs::remove_dir_all(index_dir(&self.data_dir, name));
        lock(&self.slots).insert(
            name.to_owned(),
            Arc::new(watch::Sender::new(SlotState::Unloaded)),
        );
        Ok(())
    }
}

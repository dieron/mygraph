//! Semantic index behind the engine's `SemanticIndex` seam (docs/05 §2, [RANK-8]): the texts of
//! nodes and evidence as embedding vectors from a local model ([`model`]), in an HNSW index
//! (usearch).
//!
//! Like the full-text index it is a cache derived from the graph's records ([STORE-VEC-1]): it
//! lives in `<data_dir>/vectors/<graph>/`, can be deleted at any time, and records with each save
//! the model it was built with and the change-log sequence number it is complete up to.
//! Embedding is slow (tens of passages a second on a CPU), so indexing only queues documents;
//! a query embeds the few that wait before answering, and commits embed the rest for a few
//! seconds at a time, so a rebuild goes on in the background while search matches words only.

pub mod model;

pub use model::{Embed, MODEL, MODEL_FILES, Model, Role, default_model_dir};

use mygraph_core::store::{
    Closeness, DerivedIndex, MAX_PASSAGES, SemanticHit, SemanticIndex, StoreFailure, TextDoc,
    TextTarget, passages,
};
use serde_json::{Value as Json, json};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant};
use usearch::{Index, IndexOptions, MetricKind, ScalarKind};

/// `<data_dir>/vectors/<graph>/`.
pub fn index_dir(data_dir: &Path, graph: &str) -> PathBuf {
    data_dir.join("vectors").join(graph)
}

const INDEX_FILE: &str = "index.usearch";
const META_FILE: &str = "meta.json";
/// A query first embeds up to this many waiting documents itself; with more waiting (a rebuild,
/// an import) the index is not ready and search matches words only.
const INLINE: usize = 64;
/// Passages per model run: short runs, so a query waits little behind indexing.
const BATCH: usize = 16;
/// How long one commit embeds before saving what it has.
const COMMIT_BUDGET: Duration = Duration::from_secs(5);
/// Vector keys: object kind in the top 8 bits, passage number in the next 8, internal number in
/// the low 48.
const IDX_BITS: u32 = 48;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Nodes and evidence; links are not embedded ([RANK-8]).
fn kind_code(t: TextTarget) -> Option<u64> {
    match t {
        TextTarget::Nodes => Some(0),
        TextTarget::Links => None,
        TextTarget::Evidence => Some(2),
    }
}

fn doc_key(code: u64, idx: i64) -> u64 {
    (code << 56) | (idx as u64 & ((1 << IDX_BITS) - 1))
}

fn passage_key(doc: u64, passage: usize) -> u64 {
    doc | ((passage as u64) << IDX_BITS)
}

fn new_index(dims: usize) -> Result<Index, StoreFailure> {
    let options = IndexOptions {
        dimensions: dims,
        metric: MetricKind::Cos,
        quantization: ScalarKind::F16,
        connectivity: 0,
        expansion_add: 0,
        expansion_search: 0,
        multi: false,
    };
    Ok(Index::new(&options)?)
}

fn path_str(p: &Path) -> Result<&str, StoreFailure> {
    p.to_str()
        .ok_or_else(|| format!("path is not UTF-8: {}", p.display()).into())
}

pub struct UsearchIndex {
    dir: PathBuf,
    model: Arc<dyn Embed>,
    vectors: RwLock<Index>,
    state: Mutex<Queue>,
    /// One embedder of waiting documents at a time (a commit or a query).
    embedding: Mutex<()>,
    /// One commit at a time.
    committing: Mutex<()>,
}

/// Documents indexed but not embedded yet, and the sequence number of the latest indexed change
/// not saved yet.
#[derive(Default)]
struct Queue {
    applied: Option<i64>,
    pending: HashMap<u64, Pending>,
    pending_seq: Option<i64>,
    /// Bumped on every queued document, so embedding removes only the versions it embedded.
    generation: u64,
}

struct Pending {
    text: String,
    generation: u64,
}

impl UsearchIndex {
    /// Opens the index of a graph, starting an empty one when there is none, or when it was
    /// built by another model or cannot be read (it is only a cache).
    pub fn open(data_dir: &Path, graph: &str, model: Arc<dyn Embed>) -> Result<Self, StoreFailure> {
        let dir = index_dir(data_dir, graph);
        std::fs::create_dir_all(&dir)?;
        let meta: Option<Json> = std::fs::read_to_string(dir.join(META_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok());
        let mut index = new_index(model.dims())?;
        let applied = match meta {
            Some(m)
                if m["model"] == model.id()
                    && m["dims"] == model.dims()
                    && index.load(path_str(&dir.join(INDEX_FILE))?).is_ok() =>
            {
                m["applied"].as_i64()
            }
            _ => {
                index = new_index(model.dims())?;
                let _ = std::fs::remove_file(dir.join(INDEX_FILE));
                let _ = std::fs::remove_file(dir.join(META_FILE));
                None
            }
        };
        Ok(Self {
            dir,
            model,
            vectors: RwLock::new(index),
            state: Mutex::new(Queue {
                applied,
                ..Queue::default()
            }),
            embedding: Mutex::new(()),
            committing: Mutex::new(()),
        })
    }

    /// Embeds waiting documents into the index: all of them, or until `budget` has passed.
    fn embed_pending(&self, budget: Option<Duration>) -> Result<(), StoreFailure> {
        let _one = lock(&self.embedding);
        let start = Instant::now();
        loop {
            if budget.is_some_and(|b| start.elapsed() >= b) {
                return Ok(());
            }
            let batch: Vec<(u64, u64, String)> = lock(&self.state)
                .pending
                .iter()
                .take(BATCH)
                .map(|(k, p)| (*k, p.generation, p.text.clone()))
                .collect();
            if batch.is_empty() {
                return Ok(());
            }
            // Passages of every document, embedded a few at a time.
            let mut owners = Vec::new();
            let mut texts = Vec::new();
            for (i, (_, _, text)) in batch.iter().enumerate() {
                for p in passages(text) {
                    owners.push(i);
                    texts.push(p);
                }
            }
            let mut vectors = Vec::with_capacity(texts.len());
            for chunk in texts.chunks(BATCH) {
                vectors.extend(self.model.embed(chunk, Role::Passage)?);
            }
            {
                let index = self.vectors.write().unwrap_or_else(PoisonError::into_inner);
                if index.size() + vectors.len() > index.capacity() {
                    index
                        .reserve((index.capacity() * 2).max(index.size() + vectors.len() + 1024))?;
                }
                for (key, _, _) in &batch {
                    for p in 0..MAX_PASSAGES {
                        if index.remove(passage_key(*key, p))? == 0 {
                            break;
                        }
                    }
                }
                let mut next = vec![0usize; batch.len()];
                for (v, &i) in vectors.iter().zip(&owners) {
                    index.add(passage_key(batch[i].0, next[i]), v)?;
                    next[i] += 1;
                }
            }
            let mut st = lock(&self.state);
            for (key, generation, _) in batch {
                if st
                    .pending
                    .get(&key)
                    .is_some_and(|p| p.generation == generation)
                {
                    st.pending.remove(&key);
                }
            }
        }
    }

    /// Writes the index and then its metadata, each through a temporary file. A crash between
    /// the two leaves a newer index with older metadata, which catching up from the change log
    /// repairs.
    fn save(&self, applied: Option<i64>) -> Result<(), StoreFailure> {
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        self.vectors
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .save(path_str(&tmp)?)?;
        std::fs::rename(&tmp, self.dir.join(INDEX_FILE))?;
        let meta =
            json!({ "model": self.model.id(), "dims": self.model.dims(), "applied": applied });
        let tmp = self.dir.join(format!("{META_FILE}.tmp"));
        std::fs::write(&tmp, meta.to_string())?;
        std::fs::rename(&tmp, self.dir.join(META_FILE))?;
        Ok(())
    }
}

impl DerivedIndex for UsearchIndex {
    fn applied_seq(&self) -> Option<i64> {
        lock(&self.state).applied
    }

    fn index(&self, docs: Vec<TextDoc>, seq: i64) -> Result<(), StoreFailure> {
        let mut st = lock(&self.state);
        for d in docs {
            let Some(code) = kind_code(d.target) else {
                continue;
            };
            st.generation += 1;
            let generation = st.generation;
            let text = d.meaning().to_owned();
            st.pending
                .insert(doc_key(code, d.idx), Pending { text, generation });
        }
        st.pending_seq = Some(seq);
        Ok(())
    }

    fn commit(&self) -> Result<(), StoreFailure> {
        let _one = lock(&self.committing);
        self.embed_pending(Some(COMMIT_BUDGET))?;
        let (seq, done, applied) = {
            let st = lock(&self.state);
            let Some(seq) = st.pending_seq else {
                return Ok(());
            };
            (seq, st.pending.is_empty(), st.applied)
        };
        // The sequence number is recorded only once everything up to it is embedded.
        self.save(if done { Some(seq) } else { applied })?;
        if done {
            let mut st = lock(&self.state);
            st.applied = Some(seq);
            if st.pending_seq == Some(seq) {
                st.pending_seq = None;
            }
        }
        Ok(())
    }

    fn pending(&self) -> usize {
        let st = lock(&self.state);
        // Embedded by a query but not saved yet, or a clear: a sequence number, no documents.
        st.pending.len() + usize::from(st.pending.is_empty() && st.pending_seq.is_some())
    }

    fn clear(&self) -> Result<(), StoreFailure> {
        {
            let _one = lock(&self.embedding);
            *self.vectors.write().unwrap_or_else(PoisonError::into_inner) =
                new_index(self.model.dims())?;
            let mut st = lock(&self.state);
            st.pending.clear();
            st.pending_seq = Some(-1);
        }
        self.commit()
    }
}

impl SemanticIndex for UsearchIndex {
    fn nearest(
        &self,
        target: TextTarget,
        query: &str,
        limit: usize,
        closeness: Closeness,
    ) -> Result<Option<Vec<SemanticHit>>, StoreFailure> {
        let Some(code) = kind_code(target) else {
            return Ok(Some(Vec::new()));
        };
        let waiting = lock(&self.state).pending.len();
        if waiting > INLINE {
            return Ok(None);
        }
        if waiting > 0 {
            self.embed_pending(None)?;
        }
        let q = self
            .model
            .embed(&[query.to_owned()], Role::Query)?
            .pop()
            .ok_or("the model returned no vector")?;
        let floor = self.model.floor(closeness);
        let index = self.vectors.read().unwrap_or_else(PoisonError::into_inner);
        if index.size() == 0 {
            return Ok(Some(Vec::new()));
        }
        // Several passages of one object may come first; ask for more than `limit`.
        let found = index.filtered_search(&q, limit.max(1) * 4, |k| k >> 56 == code)?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (key, distance) in found.keys.iter().zip(&found.distances) {
            let similarity = 1.0 - distance;
            if similarity < floor || out.len() == limit {
                break;
            }
            let idx = (key & ((1 << IDX_BITS) - 1)) as i64;
            if seen.insert(idx) {
                out.push(SemanticHit {
                    idx,
                    similarity,
                    passage: ((key >> IDX_BITS) & 0xff) as usize,
                });
            }
        }
        Ok(Some(out))
    }

    fn ready(&self) -> bool {
        lock(&self.state).pending.len() <= INLINE
    }
}

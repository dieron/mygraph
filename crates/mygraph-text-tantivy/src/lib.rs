//! Full-text index on tantivy behind the engine's `TextIndex` seam (docs/05 §2).
//!
//! The index is a cache derived from the graph's records ([STORE-TEXT-1]): it lives in
//! `<data_dir>/text/<graph>/`, can be deleted at any time, and records in each commit's payload
//! the change-log sequence number it is complete up to. Documents are searchable as soon as they
//! are indexed; commits (slow: segment files and fsync) are batched by the caller, and until then
//! recent documents are matched from memory. Tokenizing approximates the FTS5
//! `unicode61 remove_diacritics` tables: words of letters and digits, lowercased, diacritics
//! folded.

use mygraph_core::store::{StoreFailure, TextDoc, TextHit, TextIndex, TextTarget};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, Query, RegexQuery, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, NumericOptions, Schema, TextFieldIndexing, TextOptions,
};
use tantivy::tokenizer::{
    AsciiFoldingFilter, LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer,
};
use tantivy::{DocAddress, Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, Term};

const TOKENIZER: &str = "mygraph";
/// Bytes of RAM the writer may buffer before flushing a segment.
const WRITER_BUFFER: usize = 128 * 1024 * 1024;
/// Indexing commits by itself when this many documents are pending.
const AUTO_COMMIT: usize = 10_000;

/// `<data_dir>/text/<graph>/`.
pub fn index_dir(data_dir: &Path, graph: &str) -> PathBuf {
    data_dir.join("text").join(graph)
}

pub struct TantivyText {
    reader: IndexReader,
    writer: Mutex<IndexWriter<TantivyDocument>>,
    /// `kind << 56 | number`: unique per object, used to replace documents.
    key: Field,
    /// One text field per object kind, so a one-word query is a single term query, which
    /// tantivy answers with block-max skipping instead of scoring every match.
    texts: [Field; 3],
    state: Mutex<Commits>,
}

/// Documents indexed but not committed, by key, and the sequence number they cover. Indexing only
/// queues them here (searches match them from memory); a commit moves them into tantivy, so a
/// write never waits for tantivy.
#[derive(Default)]
struct Commits {
    applied: Option<i64>,
    pending: HashMap<u64, Pending>,
    pending_seq: Option<i64>,
    /// Bumped on every queued document, so a commit removes only the versions it wrote.
    generation: u64,
    /// `delete_all_documents` is due with the next commit.
    clear: bool,
}

struct Pending {
    text: String,
    terms: Vec<String>,
    generation: u64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn kind_code(t: TextTarget) -> u64 {
    match t {
        TextTarget::Nodes => 0,
        TextTarget::Links => 1,
        TextTarget::Evidence => 2,
    }
}

fn doc_key(t: TextTarget, idx: i64) -> u64 {
    (kind_code(t) << 56) | idx as u64
}

fn key_idx(k: u64) -> i64 {
    (k & ((1 << 56) - 1)) as i64
}

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(64))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
}

/// Terms of a text, as the index tokenizes it.
fn terms_of(text: &str) -> Vec<String> {
    let mut a = analyzer();
    let mut out = Vec::new();
    let mut stream = a.token_stream(text);
    while stream.advance() {
        out.push(stream.token().text.clone());
    }
    out
}

impl TantivyText {
    /// Opens the index of a graph, creating an empty one when there is none (or the existing
    /// one cannot be read, since it is only a cache).
    pub fn open(data_dir: &Path, graph: &str) -> Result<Self, StoreFailure> {
        let dir = index_dir(data_dir, graph);
        std::fs::create_dir_all(&dir)?;
        let mut sb = Schema::builder();
        let key = sb.add_u64_field("key", NumericOptions::default().set_indexed().set_fast());
        let options = TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqs),
        );
        let texts = ["text_nodes", "text_links", "text_evidence"]
            .map(|name| sb.add_text_field(name, options.clone()));
        let schema = sb.build();
        let index = match Index::open_in_dir(&dir) {
            Ok(i) if i.schema() == schema => i,
            _ => {
                std::fs::remove_dir_all(&dir)?;
                std::fs::create_dir_all(&dir)?;
                Index::create_in_dir(&dir, schema)?
            }
        };
        index.tokenizers().register(TOKENIZER, analyzer());
        let applied = index
            .load_metas()?
            .payload
            .and_then(|p| p.parse::<i64>().ok());
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let writer = index.writer(WRITER_BUFFER)?;
        Ok(Self {
            reader,
            writer: Mutex::new(writer),
            key,
            texts,
            state: Mutex::new(Commits {
                applied,
                ..Commits::default()
            }),
        })
    }

    /// Appends the top `limit` documents of `query` not seen yet and not pending (a pending
    /// document's committed version may be outdated).
    fn collect(
        &self,
        searcher: &tantivy::Searcher,
        query: &dyn Query,
        limit: usize,
        skip: &HashSet<u64>,
        out: &mut Vec<TextHit>,
        seen: &mut HashSet<u64>,
    ) -> Result<(), StoreFailure> {
        let top: Vec<(f32, DocAddress)> = searcher.search(
            query,
            &TopDocs::with_limit(limit + skip.len()).order_by_score(),
        )?;
        for (_, addr) in top {
            let keys = searcher
                .segment_reader(addr.segment_ord)
                .fast_fields()
                .u64("key")?;
            if let Some(k) = keys.first(addr.doc_id)
                && !skip.contains(&k)
                && seen.insert(k)
            {
                out.push(TextHit {
                    idx: key_idx(k),
                    snippet: None,
                });
            }
        }
        Ok(())
    }
}

impl TextIndex for TantivyText {
    fn applied_seq(&self) -> Option<i64> {
        lock(&self.state).applied
    }

    fn index(&self, docs: Vec<TextDoc>, seq: i64) -> Result<(), StoreFailure> {
        let pending = {
            let mut st = lock(&self.state);
            for d in docs {
                st.generation += 1;
                let p = Pending {
                    terms: terms_of(&d.text),
                    text: d.text,
                    generation: st.generation,
                };
                st.pending.insert(doc_key(d.target, d.idx), p);
            }
            st.pending_seq = Some(seq);
            st.pending.len()
        };
        // Bulk indexing (a rebuild) commits as it goes, so memory stays bounded.
        if pending >= AUTO_COMMIT {
            self.commit()?;
        }
        Ok(())
    }

    fn commit(&self) -> Result<(), StoreFailure> {
        // One commit at a time; indexing goes on meanwhile.
        let mut writer = lock(&self.writer);
        let (seq, clear, batch) = {
            let mut st = lock(&self.state);
            let Some(seq) = st.pending_seq else {
                return Ok(());
            };
            let batch: Vec<(u64, String, u64)> = st
                .pending
                .iter()
                .map(|(k, p)| (*k, p.text.clone(), p.generation))
                .collect();
            (seq, std::mem::take(&mut st.clear), batch)
        };
        if clear {
            writer.delete_all_documents()?;
        }
        for (key, text, _) in &batch {
            writer.delete_term(Term::from_field_u64(self.key, *key));
            let mut doc = TantivyDocument::default();
            doc.add_u64(self.key, *key);
            doc.add_text(self.texts[(key >> 56) as usize], text);
            writer.add_document(doc)?;
        }
        let mut prepared = writer.prepare_commit()?;
        prepared.set_payload(&seq.to_string());
        prepared.commit()?;
        self.reader.reload()?;
        // Only now do searches see the documents in tantivy, so only now do they leave memory;
        // versions queued during the commit stay.
        let mut st = lock(&self.state);
        for (key, _, generation) in batch {
            if st
                .pending
                .get(&key)
                .is_some_and(|p| p.generation == generation)
            {
                st.pending.remove(&key);
            }
        }
        if st.pending_seq == Some(seq) {
            st.pending_seq = None;
        }
        st.applied = Some(seq);
        Ok(())
    }

    fn pending(&self) -> usize {
        let st = lock(&self.state);
        // A clear or an empty rebuild chunk has a sequence number but no documents.
        st.pending.len() + usize::from(st.pending.is_empty() && st.pending_seq.is_some())
    }

    fn clear(&self) -> Result<(), StoreFailure> {
        {
            let mut st = lock(&self.state);
            st.pending.clear();
            st.pending_seq = Some(-1);
            st.clear = true;
        }
        self.commit()
    }

    fn search(
        &self,
        target: TextTarget,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<TextHit>, StoreFailure> {
        let terms = terms_of(&tokens.join(" "));
        let Some((last, rest)) = terms.split_last() else {
            return Ok(Vec::new());
        };
        let limit = limit.max(1);
        let mut out = Vec::new();
        let mut seen = HashSet::new();

        // Pending documents first (newest first): the same matching as below, from memory.
        let skip: HashSet<u64> = {
            let st = lock(&self.state);
            let code = kind_code(target);
            let mut matches: Vec<(bool, u64)> = st
                .pending
                .iter()
                .filter(|(k, _)| *k >> 56 == code)
                .filter_map(|(k, p)| {
                    let words = &p.terms;
                    let has = |t: &String| words.iter().any(|w| w == t);
                    if !rest.iter().all(has) {
                        return None;
                    }
                    if has(last) {
                        Some((true, *k))
                    } else if words.iter().any(|w| w.starts_with(last.as_str())) {
                        Some((false, *k))
                    } else {
                        None
                    }
                })
                .collect();
            matches.sort_unstable_by(|a, b| b.cmp(a));
            for (_, k) in matches {
                if seen.insert(k) {
                    out.push(TextHit {
                        idx: key_idx(k),
                        snippet: None,
                    });
                }
            }
            st.pending
                .keys()
                .filter(|k| *k >> 56 == code)
                .copied()
                .collect()
        };
        if out.len() >= limit {
            out.truncate(limit);
            return Ok(out);
        }

        let field = self.texts[kind_code(target) as usize];
        let term = |t: &str| -> Box<dyn Query> {
            Box::new(TermQuery::new(
                Term::from_field_text(field, t),
                IndexRecordOption::WithFreqs,
            ))
        };
        // Every token but the last.
        let base = || -> Vec<(Occur, Box<dyn Query>)> {
            rest.iter().map(|t| (Occur::Must, term(t))).collect()
        };
        // [RANK-6] the last token also matches as a word prefix. Documents with the whole word
        // come first, ranked by BM25 with block skipping; only when they do not fill `limit`
        // are documents with a longer word starting with it added (expanding a prefix touches
        // many terms and cannot skip, so it is the slow part).
        let searcher = self.reader.searcher();
        let mut whole = base();
        whole.push((Occur::Must, term(last)));
        // A single word runs as a bare term query, the case block-max skipping covers.
        let first: Box<dyn Query> = if whole.len() == 1 {
            whole.pop().expect("one clause").1
        } else {
            Box::new(BooleanQuery::new(whole))
        };
        let want = limit - out.len();
        self.collect(&searcher, first.as_ref(), want, &skip, &mut out, &mut seen)?;
        if out.len() < limit {
            let prefix: Box<dyn Query> = Box::new(RegexQuery::from_pattern(
                &format!("{}.+", regex_escape(last)),
                field,
            )?);
            let mut with_prefix = base();
            with_prefix.push((Occur::Must, prefix));
            self.collect(
                &searcher,
                &BooleanQuery::new(with_prefix),
                want,
                &skip,
                &mut out,
                &mut seen,
            )?;
        }
        out.truncate(limit);
        Ok(out)
    }
}

/// Escapes regex syntax (terms are letters and digits, so this is defensive).
fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else {
            out.push('\\');
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(t: TextTarget, idx: i64, text: &str) -> TextDoc {
        TextDoc {
            target: t,
            idx,
            text: text.into(),
        }
    }

    fn ids(t: &TantivyText, target: TextTarget, q: &[&str]) -> Vec<i64> {
        let q: Vec<String> = q.iter().map(|s| s.to_string()).collect();
        t.search(target, &q, 10)
            .unwrap()
            .iter()
            .map(|h| h.idx)
            .collect()
    }

    #[test]
    fn indexes_replaces_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let t = TantivyText::open(dir.path(), "g").unwrap();
        assert_eq!(t.applied_seq(), None);
        t.index(
            vec![
                doc(TextTarget::Nodes, 1, "Charles Babbage\nmathematician"),
                doc(TextTarget::Nodes, 2, "Ada Lovelace"),
                doc(TextTarget::Evidence, 1, "Notes on the Analytical Engine"),
            ],
            7,
        )
        .unwrap();
        // Searchable before and after the commit, with the same answers.
        for commit in [false, true] {
            if commit {
                assert_eq!(t.pending(), 3);
                t.commit().unwrap();
                assert_eq!((t.pending(), t.applied_seq()), (0, Some(7)));
            }
            assert_eq!(ids(&t, TextTarget::Nodes, &["babbage"]), vec![1]);
            assert_eq!(
                ids(&t, TextTarget::Nodes, &["MATHEM"]),
                vec![1],
                "prefix, case"
            );
            assert_eq!(
                ids(&t, TextTarget::Nodes, &["analytical"]),
                Vec::<i64>::new()
            );
            assert_eq!(
                ids(&t, TextTarget::Evidence, &["analytical", "eng"]),
                vec![1]
            );
        }
        // Replacing a document: the outdated committed version is not found meanwhile.
        t.index(vec![doc(TextTarget::Nodes, 1, "Charles Darwin")], 8)
            .unwrap();
        assert_eq!(ids(&t, TextTarget::Nodes, &["babbage"]), Vec::<i64>::new());
        assert_eq!(ids(&t, TextTarget::Nodes, &["charles"]), vec![1]);
        t.commit().unwrap();
        assert_eq!(ids(&t, TextTarget::Nodes, &["babbage"]), Vec::<i64>::new());
        drop(t);
        let t = TantivyText::open(dir.path(), "g").unwrap();
        assert_eq!(t.applied_seq(), Some(8));
        assert_eq!(ids(&t, TextTarget::Nodes, &["darwin"]), vec![1]);
    }

    #[test]
    fn uncommitted_documents_are_not_durable() {
        let dir = tempfile::tempdir().unwrap();
        let t = TantivyText::open(dir.path(), "g").unwrap();
        t.index(vec![doc(TextTarget::Nodes, 1, "first")], 1)
            .unwrap();
        t.commit().unwrap();
        t.index(vec![doc(TextTarget::Nodes, 2, "second")], 2)
            .unwrap();
        assert_eq!(ids(&t, TextTarget::Nodes, &["second"]), vec![2]);
        drop(t);
        // The caller catches up from the change log after seq 1.
        let t = TantivyText::open(dir.path(), "g").unwrap();
        assert_eq!(t.applied_seq(), Some(1));
        assert_eq!(ids(&t, TextTarget::Nodes, &["second"]), Vec::<i64>::new());
    }
}

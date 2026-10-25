//! Full-text index on tantivy behind the engine's `TextIndex` seam (docs/05 §2).
//!
//! The index is a cache derived from the graph's records ([STORE-TEXT-1]): it lives in
//! `<data_dir>/text/<graph>/`, can be deleted at any time, and records in each commit's payload
//! the change-log sequence number it is complete up to. Tokenizing approximates the FTS5
//! `unicode61 remove_diacritics` tables: words of letters and digits, lowercased, diacritics
//! folded.

use mygraph_core::store::{StoreFailure, TextDoc, TextHit, TextIndex, TextTarget};
use std::path::{Path, PathBuf};
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

/// `<data_dir>/text/<graph>/`.
pub fn index_dir(data_dir: &Path, graph: &str) -> PathBuf {
    data_dir.join("text").join(graph)
}

pub struct TantivyText {
    index: Index,
    reader: IndexReader,
    writer: IndexWriter<TantivyDocument>,
    /// `kind << 56 | number`: unique per object, used to replace documents.
    key: Field,
    /// One text field per object kind, so a one-word query is a single term query, which
    /// tantivy answers with block-max skipping instead of scoring every match.
    texts: [Field; 3],
    applied: Option<i64>,
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

fn analyzer() -> TextAnalyzer {
    TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(64))
        .filter(LowerCaser)
        .filter(AsciiFoldingFilter)
        .build()
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
            index,
            reader,
            writer,
            key,
            texts,
            applied,
        })
    }

    fn commit(&mut self, seq: i64) -> Result<(), StoreFailure> {
        let mut prepared = self.writer.prepare_commit()?;
        prepared.set_payload(&seq.to_string());
        prepared.commit()?;
        self.reader.reload()?;
        self.applied = Some(seq);
        Ok(())
    }

    /// Appends the top `limit` documents of `query` not seen yet.
    fn collect(
        &self,
        searcher: &tantivy::Searcher,
        query: &dyn Query,
        limit: usize,
        out: &mut Vec<TextHit>,
        seen: &mut std::collections::HashSet<u64>,
    ) -> Result<(), StoreFailure> {
        let top: Vec<(f32, DocAddress)> =
            searcher.search(query, &TopDocs::with_limit(limit).order_by_score())?;
        for (_, addr) in top {
            let keys = searcher
                .segment_reader(addr.segment_ord)
                .fast_fields()
                .u64("key")?;
            if let Some(k) = keys.first(addr.doc_id)
                && seen.insert(k)
            {
                out.push(TextHit {
                    idx: (k & ((1 << 56) - 1)) as i64,
                    snippet: None,
                });
            }
        }
        Ok(())
    }

    /// Terms of the query tokens, as the index tokenized them.
    fn terms(&self, tokens: &[String]) -> Vec<String> {
        let mut a = analyzer();
        let mut out = Vec::new();
        let joined = tokens.join(" ");
        let mut stream = a.token_stream(&joined);
        while stream.advance() {
            out.push(stream.token().text.clone());
        }
        out
    }
}

impl TextIndex for TantivyText {
    fn applied_seq(&self) -> Option<i64> {
        self.applied
    }

    fn index(&mut self, docs: Vec<TextDoc>, seq: i64) -> Result<(), StoreFailure> {
        for d in docs {
            let key = doc_key(d.target, d.idx);
            self.writer.delete_term(Term::from_field_u64(self.key, key));
            let mut doc = TantivyDocument::default();
            doc.add_u64(self.key, key);
            doc.add_text(self.texts[kind_code(d.target) as usize], &d.text);
            self.writer.add_document(doc)?;
        }
        self.commit(seq)
    }

    fn clear(&mut self) -> Result<(), StoreFailure> {
        self.writer.delete_all_documents()?;
        self.commit(-1)
    }

    fn search(
        &self,
        target: TextTarget,
        tokens: &[String],
        limit: usize,
    ) -> Result<Vec<TextHit>, StoreFailure> {
        let terms = self.terms(tokens);
        let Some((last, rest)) = terms.split_last() else {
            return Ok(Vec::new());
        };
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
        let limit = limit.max(1);
        let mut whole = base();
        whole.push((Occur::Must, term(last)));
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        // A single word runs as a bare term query, the case block-max skipping covers.
        let first: Box<dyn Query> = if whole.len() == 1 {
            whole.pop().expect("one clause").1
        } else {
            Box::new(BooleanQuery::new(whole))
        };
        self.collect(&searcher, first.as_ref(), limit, &mut out, &mut seen)?;
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
                limit,
                &mut out,
                &mut seen,
            )?;
        }
        out.truncate(limit);
        let _ = &self.index;
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

    #[test]
    fn indexes_replaces_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = TantivyText::open(dir.path(), "g").unwrap();
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
        let ids = |t: &TantivyText, target, q: &[&str]| -> Vec<i64> {
            let q: Vec<String> = q.iter().map(|s| s.to_string()).collect();
            t.search(target, &q, 10)
                .unwrap()
                .iter()
                .map(|h| h.idx)
                .collect()
        };
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
        // Replacing a document.
        t.index(vec![doc(TextTarget::Nodes, 1, "Charles Darwin")], 8)
            .unwrap();
        assert_eq!(ids(&t, TextTarget::Nodes, &["babbage"]), Vec::<i64>::new());
        drop(t);
        let t = TantivyText::open(dir.path(), "g").unwrap();
        assert_eq!(t.applied_seq(), Some(8));
        assert_eq!(ids(&t, TextTarget::Nodes, &["darwin"]), vec![1]);
    }
}

//! Multi-hop question answering over given passages: HotpotQA (distractor setting) and MuSiQue
//! (docs/11-benchmarks.md §3). Two ways to answer each question:
//!
//! - `graph`: one agent records the passages in a fresh MyGraph graph without seeing the
//!   question; a second agent, with a new context, gets only the question and answers from the
//!   graph, citing evidence IDs. Each cited evidence's text is mapped back to the passages'
//!   sentences, which are graded against the supporting facts.
//! - `direct`: the same model reads the passages in its prompt and names the supporting sentences
//!   (the usual reader setting, as a reference).
//!
//! Supporting facts are graded where the dataset marks them: sentences for HotpotQA, paragraphs
//! for MuSiQue. Passage-level scores are reported for both.

use crate::agent::{Agent, Result, Run, last_json_with, read_transcript};
use crate::grade::{Fact, Sentences, best_answer_scores, set_scores, split_sentences};
use crate::graph;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy)]
pub enum Dataset {
    Hotpot,
    Musique,
}

pub struct Passage {
    pub title: String,
    /// Joined, the passage's text.
    pub sentences: Vec<String>,
}

pub struct Question {
    pub id: String,
    pub question: String,
    /// The answer, then accepted aliases.
    pub answers: Vec<String>,
    /// HotpotQA: `bridge` or `comparison`; MuSiQue: `2hop`, `3hop` or `4hop`.
    pub kind: String,
    pub passages: Vec<Passage>,
    /// Supporting sentences, where the dataset marks them (HotpotQA).
    pub supporting_sentences: Option<BTreeSet<Fact>>,
    pub supporting_passages: BTreeSet<usize>,
}

impl Question {
    /// The supporting facts graded as `sp_*`: sentences where known, else passages (as sentence
    /// sets are compared, a passage is written `(passage, 0)`).
    fn supporting(&self) -> BTreeSet<Fact> {
        self.supporting_sentences
            .clone()
            .unwrap_or_else(|| self.supporting_passages.iter().map(|p| (*p, 0)).collect())
    }

    /// `facts` at the granularity of [`Question::supporting`].
    fn graded(&self, facts: &BTreeSet<Fact>) -> BTreeSet<Fact> {
        if self.supporting_sentences.is_some() {
            facts.clone()
        } else {
            facts.iter().map(|(p, _)| (*p, 0)).collect()
        }
    }
}

pub struct Options {
    pub dataset: Dataset,
    pub data: PathBuf,
    pub out: PathBuf,
    pub questions: usize,
    pub seed: u64,
    pub modes: Vec<String>,
    pub parallel: usize,
    pub mygraph: PathBuf,
    pub agent: Agent,
    /// Grade the transcripts and graphs of an earlier run again instead of running agents.
    pub regrade: bool,
    /// Keep the graphs of an earlier run and run only the answering agents again.
    pub reanswer: bool,
}

impl Options {
    /// Runs an agent, or with `regrade` reads the transcript it left.
    pub(crate) fn agent_run(
        &self,
        dir: &Path,
        name: &str,
        system: &str,
        prompt: &str,
        mcp: Option<&Json>,
    ) -> Result<Run> {
        if self.regrade {
            read_transcript(&dir.join(format!("{name}.jsonl")))
        } else {
            self.agent.run(dir, name, system, prompt, mcp)
        }
    }
}

fn strs(v: &Json) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .map(|s| s.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The rows of the Hugging Face `hotpotqa/hotpot_qa` parquet file (config `distractor`).
pub fn load_hotpot(path: &Path) -> Result<Vec<Question>> {
    let reader = SerializedFileReader::new(fs::File::open(path)?)?;
    let mut out = Vec::new();
    for row in reader.get_row_iter(None)? {
        let row = to_json(&Field::Group(row?));
        let ctx = &row["context"];
        let passages: Vec<Passage> = strs(&ctx["title"])
            .into_iter()
            .zip(ctx["sentences"].as_array().into_iter().flatten())
            .map(|(title, ss)| Passage {
                title,
                sentences: strs(ss),
            })
            .collect();
        // Titles are unique within a HotpotQA question.
        let number = |t: &str| passages.iter().position(|p| p.title == t);
        let sf = &row["supporting_facts"];
        let sentences: BTreeSet<Fact> = strs(&sf["title"])
            .iter()
            .zip(sf["sent_id"].as_array().into_iter().flatten())
            .filter_map(|(t, i)| Some((number(t)?, i.as_u64()? as usize)))
            .collect();
        out.push(Question {
            id: row["id"].as_str().unwrap_or_default().to_owned(),
            question: row["question"].as_str().unwrap_or_default().to_owned(),
            answers: vec![row["answer"].as_str().unwrap_or_default().to_owned()],
            kind: row["type"].as_str().unwrap_or_default().to_owned(),
            supporting_passages: sentences.iter().map(|(p, _)| *p).collect(),
            supporting_sentences: Some(sentences),
            passages,
        });
    }
    Ok(out)
}

/// The lines of MuSiQue's `musique_ans_v1.0_dev.jsonl` (answerable questions; paragraphs are split
/// into sentences here, as MuSiQue marks only supporting paragraphs).
pub fn load_musique(path: &Path) -> Result<Vec<Question>> {
    let mut out = Vec::new();
    for line in std::io::BufReader::new(fs::File::open(path)?).lines() {
        let row: Json = serde_json::from_str(&line?)?;
        if row["answerable"] == false {
            continue;
        }
        let paragraphs = row["paragraphs"].as_array().cloned().unwrap_or_default();
        let id = row["id"].as_str().unwrap_or_default().to_owned();
        let mut answers = vec![row["answer"].as_str().unwrap_or_default().to_owned()];
        answers.extend(strs(&row["answer_aliases"]));
        out.push(Question {
            kind: format!("{}hop", id.chars().next().unwrap_or('?')),
            id,
            question: row["question"].as_str().unwrap_or_default().to_owned(),
            answers,
            supporting_sentences: None,
            supporting_passages: paragraphs
                .iter()
                .enumerate()
                .filter(|(_, p)| p["is_supporting"] == true)
                .map(|(i, _)| i)
                .collect(),
            passages: paragraphs
                .iter()
                .map(|p| Passage {
                    title: p["title"].as_str().unwrap_or_default().to_owned(),
                    sentences: split_sentences(p["paragraph_text"].as_str().unwrap_or_default()),
                })
                .collect(),
        });
    }
    Ok(out)
}

fn to_json(f: &Field) -> Json {
    match f {
        Field::Null => Json::Null,
        Field::Bool(b) => (*b).into(),
        Field::Int(i) => (*i).into(),
        Field::Long(i) => (*i).into(),
        Field::Str(s) => s.as_str().into(),
        Field::Group(r) => Json::Object(
            r.get_column_iter()
                .map(|(k, v)| (k.clone(), to_json(v)))
                .collect(),
        ),
        Field::ListInternal(l) => Json::Array(l.elements().iter().map(to_json).collect()),
        other => other.to_string().into(),
    }
}

/// `n` distinct items, the same for the same seed (splitmix64, partial Fisher–Yates).
pub(crate) fn sample<T>(all: Vec<T>, n: usize, seed: u64) -> Vec<T> {
    let mut s = seed;
    let mut next = move || {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut all: Vec<Option<T>> = all.into_iter().map(Some).collect();
    let n = n.min(all.len());
    for i in 0..n {
        let j = i + (next() % (all.len() - i) as u64) as usize;
        all.swap(i, j);
    }
    all.into_iter().take(n).flatten().collect()
}

/// The page URL a passage is presented under: its Wikipedia address.
fn page_url(title: &str) -> String {
    let mut u = String::from("https://en.wikipedia.org/wiki/");
    for b in title.replace(' ', "_").bytes() {
        if b.is_ascii_alphanumeric() || b"-._~()',!*:@".contains(&b) {
            u.push(b as char);
        } else {
            u.push_str(&format!("%{b:02X}"));
        }
    }
    u
}

fn passage_text(p: &Passage) -> String {
    p.sentences.concat().trim().to_owned()
}

fn ingest_prompt(q: &Question) -> String {
    let mut p = format!(
        "Below are {} Wikipedia passages. Record what they say in MyGraph, so that later \
         questions about any of them, including questions that combine facts from several \
         passages, can be answered from the graph alone and checked against the exact sentences. \
         Treat each passage as a web page at the URL given. When you are done, reply with one \
         line: done.\n",
        q.passages.len()
    );
    for passage in &q.passages {
        p += &format!(
            "\n## {}\nURL: {}\n\n{}\n",
            passage.title,
            page_url(&passage.title),
            passage_text(passage)
        );
    }
    p
}

/// The same in both modes: the answer's form, and when to say `unknown`.
pub(crate) const ANSWER_RULES: &str = "The answer is as short as possible: a name, a date, a number, a short \
     phrase, or yes or no. When the sources point to an answer without settling every detail, give \
     the best-supported answer; answer \"unknown\" only when they hold nothing that answers the \
     question.";

fn answer_prompt(q: &Question) -> String {
    format!(
        "Answer the question below from what is recorded in MyGraph only, not from your own \
         knowledge, and cite the evidence behind the answer.\n\nQuestion: {}\n\nEnd your reply \
         with one line of JSON: {{\"answer\": \"...\", \"evidence\": [\"e_...\"]}}. \
         \"evidence\" lists the IDs of the evidence (the text fragments) that support each step \
         of the reasoning. {ANSWER_RULES}",
        q.question
    )
}

const SYSTEM_DIRECT: &str = "You answer questions from the passages you are given.";

fn direct_prompt(q: &Question) -> String {
    let mut p = String::from("Passages:\n");
    for (n, passage) in q.passages.iter().enumerate() {
        p += &format!("\n## Passage {n}: {}\n", passage.title);
        for (i, s) in passage.sentences.iter().enumerate() {
            p += &format!("[{i}] {}\n", s.trim());
        }
    }
    p += &format!(
        "\nQuestion: {}\n\nAnswer from the passages only, not from your own knowledge. End your \
         reply with one line of JSON: {{\"answer\": \"...\", \"facts\": [[0, 0]]}}. \"facts\" \
         lists the sentences (passage number and sentence number) that support each step of the \
         reasoning. {ANSWER_RULES}",
        q.question
    );
    p
}

/// Per-question scores for one mode, from the answer and the sentences it cites.
fn scores(q: &Question, answer: &str, facts: &BTreeSet<Fact>, run: &Run) -> Map<String, Json> {
    let (em, f1, prec, rec) = best_answer_scores(answer, &q.answers);
    let (sp_em, sp_f1, sp_prec, sp_rec) = set_scores(&q.graded(facts), &q.supporting());
    let passages: BTreeSet<usize> = facts.iter().map(|(p, _)| *p).collect();
    let (_, passage_f1, passage_prec, passage_rec) = set_scores(&passages, &q.supporting_passages);
    let (jp, jr) = (prec * sp_prec, rec * sp_rec);
    let joint_f1 = if jp + jr > 0.0 {
        2.0 * jp * jr / (jp + jr)
    } else {
        0.0
    };
    let m = json!({
        "answer": answer, "em": em, "f1": f1,
        "sp_em": sp_em, "sp_f1": sp_f1, "sp_prec": sp_prec, "sp_recall": sp_rec,
        "passage_f1": passage_f1, "passage_prec": passage_prec, "passage_recall": passage_rec,
        "joint_em": em * sp_em, "joint_f1": joint_f1,
        "facts": facts.iter().map(|(p, i)| json!([p, i])).collect::<Vec<_>>(),
        "unknown": answer.trim().eq_ignore_ascii_case("unknown"),
        "run": run.to_json(),
    });
    m.as_object().cloned().unwrap_or_default()
}

fn run_direct(o: &Options, q: &Question, dir: &Path) -> Result<Map<String, Json>> {
    let run = o.agent_run(dir, "direct", SYSTEM_DIRECT, &direct_prompt(q), None)?;
    let reply = last_json_with(&run.text, "answer").unwrap_or(Json::Null);
    let facts = reply["facts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|f| {
            // Passage numbers; runs before they were numbered named the passage by title.
            let p = f[0].as_u64().map(|p| p as usize).or_else(|| {
                let t = f[0].as_str()?;
                q.passages.iter().position(|p| p.title == t)
            })?;
            Some((p, f[1].as_u64()? as usize))
        })
        .collect();
    Ok(scores(
        q,
        reply["answer"].as_str().unwrap_or(""),
        &facts,
        &run,
    ))
}

fn run_graph(o: &Options, q: &Question, dir: &Path) -> Result<Map<String, Json>> {
    // A graph left by an interrupted run is started over.
    let data = dir.join("data");
    let keep_graph = o.regrade || o.reanswer;
    if !keep_graph {
        graph::create(&data)?;
    }
    let server = |actor: &str| graph::server(&o.mygraph, &data, actor);
    let system = graph::system();
    let ingest = if keep_graph {
        read_transcript(&dir.join("ingest.jsonl"))?
    } else {
        o.agent.run(
            dir,
            "ingest",
            &system,
            &ingest_prompt(q),
            Some(&server("ingest-agent")),
        )?
    };
    let ans = o.agent_run(
        dir,
        "answer",
        &system,
        &answer_prompt(q),
        Some(&server("answer-agent")),
    )?;
    let reply = last_json_with(&ans.text, "answer").unwrap_or(Json::Null);
    let cited: Vec<String> = reply["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str().map(str::to_owned))
        .collect();

    // Every evidence in the graph and the sentences it quotes.
    let (texts, size) = graph::contents(&data)?;
    let sentences = Sentences::new(q.passages.iter().map(|p| p.sentences.as_slice()));
    let evidence: BTreeMap<String, BTreeSet<Fact>> = texts
        .into_iter()
        .map(|(id, e)| {
            let facts = sentences.matching(&e.content);
            (id, facts)
        })
        .collect();
    let stored: BTreeSet<Fact> = evidence.values().flatten().cloned().collect();
    let mut facts = BTreeSet::new();
    let mut unknown_refs = Vec::new();
    for c in &cited {
        match evidence.get(c) {
            Some(f) => facts.extend(f.iter().cloned()),
            None => unknown_refs.push(c.clone()),
        }
    }
    let mut m = scores(q, reply["answer"].as_str().unwrap_or(""), &facts, &ans);
    let support = q.supporting();
    let stored_graded = q.graded(&stored);
    let total: usize = q.passages.iter().map(|p| p.sentences.len()).sum();
    m.insert("cited".into(), json!(cited));
    m.insert("cited_unknown".into(), json!(unknown_refs));
    m.insert(
        "supporting_stored".into(),
        (support.intersection(&stored_graded).count() as f64 / support.len().max(1) as f64).into(),
    );
    m.insert(
        "sentences_stored".into(),
        (stored.len() as f64 / total.max(1) as f64).into(),
    );
    m.insert("graph".into(), size);
    m.insert("ingest".into(), ingest.to_json());
    Ok(m)
}

pub(crate) fn mean(rs: &[&Json], f: impl Fn(&Json) -> Option<f64>) -> Json {
    let v: Vec<f64> = rs.iter().filter_map(|r| f(r)).collect();
    if v.is_empty() {
        Json::Null
    } else {
        ((v.iter().sum::<f64>() / v.len() as f64 * 1000.0).round() / 1000.0).into()
    }
}

fn summarize(results: &[Json], mode: &str) -> Json {
    let rs: Vec<&Json> = results
        .iter()
        .filter(|r| r[mode].is_object())
        .map(|r| &r[mode])
        .collect();
    let metric = |k: &'static str| mean(&rs, move |r| r[k].as_f64());
    let mut s = json!({
        "questions": rs.len(),
        "em": metric("em"), "f1": metric("f1"),
        "sp_em": metric("sp_em"), "sp_f1": metric("sp_f1"),
        "sp_prec": metric("sp_prec"), "sp_recall": metric("sp_recall"),
        "passage_f1": metric("passage_f1"),
        "joint_em": metric("joint_em"), "joint_f1": metric("joint_f1"),
        "unknown": rs.iter().filter(|r| r["unknown"] == true).count(),
        "errors": rs.iter().filter(|r| !r["run"]["error"].is_null()).count(),
        "cost_usd": mean(&rs, |r| r["run"]["cost_usd"].as_f64()),
        "tool_calls": mean(&rs, |r| r["run"]["tool_calls"].as_f64()),
        "seconds": mean(&rs, |r| r["run"]["seconds"].as_f64()),
    });
    if mode == "graph" {
        let ingest = |k: &'static str| mean(&rs, move |r| r["ingest"][k].as_f64());
        let graph = |k: &'static str| mean(&rs, move |r| r["graph"][k].as_f64());
        s["supporting_stored"] = metric("supporting_stored");
        s["sentences_stored"] = metric("sentences_stored");
        s["ingest"] = json!({
            "cost_usd": ingest("cost_usd"), "tool_calls": ingest("tool_calls"),
            "seconds": ingest("seconds"),
            "errors": rs.iter().filter(|r| !r["ingest"]["error"].is_null()).count(),
        });
        s["graph"] = json!({
            "nodes": graph("nodes"), "links": graph("links"),
            "evidence": graph("evidence"), "citations": graph("citations"),
        });
    }
    let kinds: BTreeSet<&str> = results.iter().filter_map(|r| r["type"].as_str()).collect();
    let mut by_kind = Map::new();
    for kind in kinds {
        let k: Vec<&Json> = results
            .iter()
            .filter(|r| r["type"] == kind && r[mode].is_object())
            .map(|r| &r[mode])
            .collect();
        by_kind.insert(
            kind.into(),
            json!({
                "questions": k.len(),
                "em": mean(&k, |r| r["em"].as_f64()),
                "f1": mean(&k, |r| r["f1"].as_f64()),
                "sp_f1": mean(&k, |r| r["sp_f1"].as_f64()),
            }),
        );
    }
    s["by_type"] = Json::Object(by_kind);
    s
}

pub fn run(o: &Options) -> Result<Json> {
    let all = match o.dataset {
        Dataset::Hotpot => load_hotpot(&o.data)?,
        Dataset::Musique => load_musique(&o.data)?,
    };
    let total = all.len();
    let qs = sample(all, o.questions, o.seed);
    fs::create_dir_all(&o.out)?;
    let queue = Mutex::new(qs.iter().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..o.parallel.max(1) {
            s.spawn(|| {
                loop {
                    let Some(q) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let r = run_question(o, q).unwrap_or_else(
                        |e| json!({ "id": q.id, "type": q.kind, "error": e.to_string() }),
                    );
                    if let Some(e) = r.get("error") {
                        eprintln!("{} error: {e}", q.id);
                    }
                    eprintln!(
                        "{} {:<10} {}",
                        q.id,
                        q.kind,
                        o.modes
                            .iter()
                            .map(|m| format!(
                                "{m}: em {} sp_f1 {:.2}",
                                r[m]["em"],
                                r[m]["sp_f1"].as_f64().unwrap_or(0.0)
                            ))
                            .collect::<Vec<_>>()
                            .join("  ")
                    );
                    results.lock().unwrap().push(r);
                }
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut summary = json!({
        "dataset": { "file": o.data.file_name().map(|f| f.to_string_lossy()), "questions": total },
        "sample": { "questions": qs.len(), "seed": o.seed },
        "provider": match &o.agent.backend {
            crate::agent::Backend::ClaudeCode(_) => "claude-code",
            crate::agent::Backend::OpenAi(_) => "openai",
        },
        "model": o.agent.model,
        "failed": results.iter().filter(|r| r.get("error").is_some()).count(),
    });
    for m in &o.modes {
        summary[m] = summarize(&results, m);
    }
    fs::write(
        o.out.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    Ok(summary)
}

/// Runs the modes for one question, or reads the result of an earlier run from its folder.
fn run_question(o: &Options, q: &Question) -> Result<Json> {
    let dir = o.out.join(format!("q-{}", q.id));
    let file = dir.join("result.json");
    let facts = |s: &BTreeSet<Fact>| s.iter().map(|(p, i)| json!([p, i])).collect::<Vec<_>>();
    let mut r = match fs::read_to_string(&file) {
        Ok(s) => serde_json::from_str(&s)?,
        Err(_) => json!({
            "id": q.id, "type": q.kind, "question": q.question, "gold": q.answers,
            "supporting": facts(&q.supporting()),
        }),
    };
    // An error saved by an earlier run is about that run; the modes it left out run again.
    if let Some(m) = r.as_object_mut() {
        m.remove("error");
    }
    fs::create_dir_all(&dir)?;
    // The passages, for reading transcripts and checking the grading.
    let passages: Vec<Json> = q
        .passages
        .iter()
        .map(|p| json!({ "title": p.title, "url": page_url(&p.title), "sentences": p.sentences }))
        .collect();
    fs::write(
        dir.join("question.json"),
        serde_json::to_string_pretty(&json!({
            "id": q.id, "question": q.question, "answers": q.answers, "type": q.kind,
            "supporting_passages": q.supporting_passages,
            "supporting_sentences": q.supporting_sentences.as_ref().map(facts),
            "passages": passages,
        }))?,
    )?;
    for m in &o.modes {
        if r[m].is_object() && !o.regrade && !o.reanswer {
            continue;
        }
        let scored = match m.as_str() {
            "graph" => run_graph(o, q, &dir),
            "direct" => run_direct(o, q, &dir),
            other => Err(format!("unknown mode {other}").into()),
        };
        // A failed mode, or one whose agent run ended in an error (such as a usage limit), is
        // reported and left out of the file, so a rerun tries it again.
        let failed_run = |s: &Map<String, Json>| {
            ["run", "ingest"]
                .iter()
                .find_map(|k| s.get(*k)?.get("error")?.as_str().map(str::to_owned))
        };
        match scored {
            Ok(scored) if failed_run(&scored).is_some() => {
                r["error"] = format!("{m}: {}", failed_run(&scored).unwrap_or_default()).into();
            }
            Ok(scored) => {
                r[m] = Json::Object(scored);
                fs::write(&file, serde_json::to_string_pretty(&r)?)?;
            }
            Err(e) => r["error"] = format!("{m}: {e}").into(),
        }
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_urls_are_wikipedia_addresses() {
        assert_eq!(
            page_url("Ed Wood (film)"),
            "https://en.wikipedia.org/wiki/Ed_Wood_(film)"
        );
        assert_eq!(
            page_url("Café #1?"),
            "https://en.wikipedia.org/wiki/Caf%C3%A9_%231%3F"
        );
    }

    #[test]
    fn musique_support_is_graded_by_passage() {
        let q = Question {
            id: "2hop__1_2".into(),
            question: "?".into(),
            answers: vec!["x".into()],
            kind: "2hop".into(),
            passages: Vec::new(),
            supporting_sentences: None,
            supporting_passages: [1, 4].into(),
        };
        let cited: BTreeSet<Fact> = [(1, 0), (1, 2), (4, 1), (7, 0)].into();
        let m = scores(&q, "x", &cited, &Run::default());
        assert_eq!(m["sp_recall"], 1.0);
        assert!((m["sp_prec"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(m["passage_recall"], 1.0);
        assert_eq!(m["em"], 1.0);
    }
}

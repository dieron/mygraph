//! HotpotQA, distractor setting (docs/11-benchmarks.md §3). Two ways to answer each question:
//!
//! - `graph`: one agent records the ten passages in a fresh MyGraph graph without seeing the
//!   question; a second agent, with a new context, gets only the question and answers from the
//!   graph, citing evidence IDs. Each cited evidence's text is mapped back to the passages'
//!   sentences, which are graded against the supporting facts.
//! - `direct`: the same model reads the passages in its prompt and names the supporting sentences
//!   (the usual reader setting, as a reference).

use crate::agent::{Agent, Result, Run, last_json_with, read_transcript};
use crate::grade::{Fact, Sentences, answer_scores, set_scores};
use mygraph_core::engine::Graph;
use mygraph_core::ids::ObjectKind;
use mygraph_core::model::{Policies, Status, id_string};
use mygraph_store_sqlite::GraphFile;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SKILL: &str = include_str!("../../../skills/mygraph/SKILL.md");
const SKILL_WRITE: &str = include_str!("../../../skills/mygraph/references/write.md");
const SKILL_QUERY: &str = include_str!("../../../skills/mygraph/references/query.md");
const GRAPH: &str = "default";

pub struct Question {
    pub id: String,
    pub question: String,
    pub answer: String,
    /// `bridge` or `comparison`.
    pub kind: String,
    pub supporting: BTreeSet<Fact>,
    /// Passages: title and sentences.
    pub context: Vec<(String, Vec<String>)>,
}

pub struct Options {
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
}

impl Options {
    /// Runs an agent, or with `regrade` reads the transcript it left.
    fn agent_run(
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

/// The rows of the Hugging Face `hotpotqa/hotpot_qa` parquet file (config `distractor`).
pub fn load(path: &Path) -> Result<Vec<Question>> {
    let reader = SerializedFileReader::new(fs::File::open(path)?)?;
    let mut out = Vec::new();
    for row in reader.get_row_iter(None)? {
        let row = to_json(&Field::Group(row?));
        let strs = |v: &Json| -> Vec<String> {
            v.as_array()
                .into_iter()
                .flatten()
                .map(|s| s.as_str().unwrap_or_default().to_owned())
                .collect()
        };
        let sf = &row["supporting_facts"];
        let supporting = strs(&sf["title"])
            .into_iter()
            .zip(sf["sent_id"].as_array().into_iter().flatten())
            .map(|(t, i)| (t, i.as_u64().unwrap_or(0) as usize))
            .collect();
        let ctx = &row["context"];
        let context = strs(&ctx["title"])
            .into_iter()
            .zip(ctx["sentences"].as_array().into_iter().flatten())
            .map(|(t, ss)| (t, strs(ss)))
            .collect();
        out.push(Question {
            id: row["id"].as_str().unwrap_or_default().to_owned(),
            question: row["question"].as_str().unwrap_or_default().to_owned(),
            answer: row["answer"].as_str().unwrap_or_default().to_owned(),
            kind: row["type"].as_str().unwrap_or_default().to_owned(),
            supporting,
            context,
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

/// `n` distinct questions, the same for the same seed (splitmix64, partial Fisher–Yates).
fn sample(all: Vec<Question>, n: usize, seed: u64) -> Vec<Question> {
    let mut s = seed;
    let mut next = move || {
        s = s.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut all: Vec<Option<Question>> = all.into_iter().map(Some).collect();
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

fn passage_text(sentences: &[String]) -> String {
    sentences.concat().trim().to_owned()
}

fn system_graph() -> String {
    let body = |s: &'static str| s.splitn(3, "---\n").nth(2).unwrap_or(s);
    format!(
        "You are an assistant that keeps knowledge in MyGraph, a knowledge graph where every fact \
         cites its evidence, through the `mygraph` MCP tools. This is your guide to it, followed \
         by its references for writing and querying.\n\n{}\n\n{SKILL_WRITE}\n\n{SKILL_QUERY}",
        body(SKILL)
    )
}

fn ingest_prompt(q: &Question) -> String {
    let mut p = format!(
        "Below are {} Wikipedia passages. Record what they say in MyGraph, so that later \
         questions about any of them, including questions that combine facts from several \
         passages, can be answered from the graph alone and checked against the exact sentences. \
         Treat each passage as a web page at the URL given. When you are done, reply with one \
         line: done.\n",
        q.context.len()
    );
    for (t, ss) in &q.context {
        p += &format!("\n## {t}\nURL: {}\n\n{}\n", page_url(t), passage_text(ss));
    }
    p
}

fn answer_prompt(q: &Question) -> String {
    format!(
        "Answer the question below from what is recorded in MyGraph only, not from your own \
         knowledge, and cite the evidence behind the answer.\n\nQuestion: {}\n\nEnd your reply \
         with one line of JSON: {{\"answer\": \"...\", \"evidence\": [\"e_...\"]}}. The answer is \
         as short as possible: a name, a date, a number, a short phrase, or yes or no. \
         \"evidence\" lists the IDs of the evidence (the text fragments) that support each step \
         of the reasoning. If the graph does not hold the answer, answer \"unknown\".",
        q.question
    )
}

const SYSTEM_DIRECT: &str = "You answer questions from the passages you are given.";

fn direct_prompt(q: &Question) -> String {
    let mut p = String::from("Passages:\n");
    for (t, ss) in &q.context {
        p += &format!("\n## {t}\n");
        for (i, s) in ss.iter().enumerate() {
            p += &format!("[{i}] {}\n", s.trim());
        }
    }
    p += &format!(
        "\nQuestion: {}\n\nEnd your reply with one line of JSON: {{\"answer\": \"...\", \
         \"facts\": [[\"Passage title\", 0]]}}. The answer is as short as possible: a name, a \
         date, a number, a short phrase, or yes or no. \"facts\" lists the sentences (passage \
         title and sentence number) that support each step of the reasoning.",
        q.question
    );
    p
}

/// Per-question scores for one mode.
fn scores(q: &Question, answer: &str, facts: &BTreeSet<Fact>, run: &Run) -> Map<String, Json> {
    let (em, f1, prec, rec) = answer_scores(answer, &q.answer);
    let (sp_em, sp_f1, sp_prec, sp_rec) = set_scores(facts, &q.supporting);
    let titles = |s: &BTreeSet<Fact>| s.iter().map(|(t, _)| t.clone()).collect::<BTreeSet<_>>();
    let (_, title_f1, _, _) = set_scores(&titles(facts), &titles(&q.supporting));
    let (jp, jr) = (prec * sp_prec, rec * sp_rec);
    let joint_f1 = if jp + jr > 0.0 {
        2.0 * jp * jr / (jp + jr)
    } else {
        0.0
    };
    let m = json!({
        "answer": answer, "em": em, "f1": f1,
        "sp_em": sp_em, "sp_f1": sp_f1, "sp_prec": sp_prec, "sp_recall": sp_rec,
        "title_f1": title_f1, "joint_em": em * sp_em, "joint_f1": joint_f1,
        "facts": facts.iter().map(|(t, i)| json!([t, i])).collect::<Vec<_>>(),
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
        .filter_map(|f| Some((f[0].as_str()?.to_owned(), f[1].as_u64()? as usize)))
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
    if !o.regrade {
        let _ = fs::remove_dir_all(&data);
        fs::create_dir_all(&data)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as i64;
        drop(GraphFile::create(&data, GRAPH, &Policies::default(), now)?);
    }
    let server = |actor: &str| {
        json!({ "mygraph": { "command": o.mygraph, "args": [
            "mcp", "--data-dir", data, "--graph", GRAPH, "--actor", actor
        ] } })
    };
    let system = system_graph();
    let ingest = o.agent_run(
        dir,
        "ingest",
        &system,
        &ingest_prompt(q),
        Some(&server("ingest-agent")),
    )?;
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

    let file = GraphFile::open(&data, GRAPH)?;
    let policies = file.policies()?;
    let g = Graph::open(file, GRAPH, policies)?;
    let sentences = Sentences::new(&q.context);
    let urls: BTreeMap<String, String> = q
        .context
        .iter()
        .map(|(t, _)| (page_url(t).to_lowercase(), t.clone()))
        .collect();
    // Every evidence in the graph: what it quotes, and the page it belongs to.
    let st = g.state();
    let mut stored = BTreeSet::new();
    let mut evidence: BTreeMap<String, (BTreeSet<Fact>, Option<String>)> = BTreeMap::new();
    let ids: Vec<i64> = st
        .evidence
        .iter()
        .filter(|(_, e)| e.status == Status::Active)
        .map(|(i, _)| i)
        .collect();
    for chunk in ids.chunks(100) {
        let refs: Vec<String> = chunk
            .iter()
            .map(|i| id_string(ObjectKind::Evidence, *i))
            .collect();
        let r = g
            .get(&json!({ "refs": refs, "detail": "brief", "include": {"content": true} }))
            .map_err(|e| e.to_json().to_string())?;
        for (idx, item) in chunk
            .iter()
            .zip(r["items"].as_array().into_iter().flatten())
        {
            let facts = item["content"]
                .as_str()
                .map(|c| sentences.matching(c))
                .unwrap_or_default();
            stored.extend(facts.iter().cloned());
            let e = &st.evidence[idx];
            let page = [Some(*idx), e.parent]
                .into_iter()
                .flatten()
                .filter_map(|i| st.evidence.get(&i)?.uri.as_ref())
                .find_map(|u| urls.get(&u.to_lowercase()).cloned());
            evidence.insert(id_string(ObjectKind::Evidence, *idx), (facts, page));
        }
    }
    let mut facts = BTreeSet::new();
    let mut pages = BTreeSet::new();
    let mut unknown_refs = Vec::new();
    for c in &cited {
        match evidence.get(c) {
            Some((f, p)) => {
                facts.extend(f.iter().cloned());
                pages.extend(p.iter().cloned());
            }
            None => unknown_refs.push(c.clone()),
        }
    }
    let mut m = scores(q, reply["answer"].as_str().unwrap_or(""), &facts, &ans);
    let gold_titles: BTreeSet<String> = q.supporting.iter().map(|(t, _)| t.clone()).collect();
    let (_, page_f1, _, _) = set_scores(&pages, &gold_titles);
    let total: usize = q.context.iter().map(|(_, ss)| ss.len()).sum();
    m.insert("cited".into(), json!(cited));
    m.insert("cited_unknown".into(), json!(unknown_refs));
    m.insert("cited_page_f1".into(), page_f1.into());
    m.insert(
        "supporting_stored".into(),
        (q.supporting.intersection(&stored).count() as f64 / q.supporting.len() as f64).into(),
    );
    m.insert(
        "sentences_stored".into(),
        (stored.len() as f64 / total.max(1) as f64).into(),
    );
    m.insert(
        "graph".into(),
        json!({
            "nodes": st.nodes.iter().filter(|(_, n)| n.status == Status::Active).count(),
            "links": st.links.iter().filter(|(_, l)| l.status == Status::Active).count(),
            "evidence": ids.len(),
            "citations": st.citations.iter().filter(|(_, c)| c.status == Status::Active).count(),
        }),
    );
    m.insert("ingest".into(), ingest.to_json());
    Ok(m)
}

fn mean(rs: &[&Json], f: impl Fn(&Json) -> Option<f64>) -> Json {
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
        "title_f1": metric("title_f1"),
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
        s["cited_page_f1"] = metric("cited_page_f1");
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
    let mut by_kind = Map::new();
    for kind in ["bridge", "comparison"] {
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
    let all = load(&o.data)?;
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
    let mut r = match fs::read_to_string(&file) {
        Ok(s) => serde_json::from_str(&s)?,
        Err(_) => json!({
            "id": q.id, "type": q.kind, "question": q.question, "gold": q.answer,
            "supporting": q.supporting.iter().map(|(t, i)| json!([t, i])).collect::<Vec<_>>(),
        }),
    };
    fs::create_dir_all(&dir)?;
    // The passages, for reading transcripts and checking the grading.
    let passages: Vec<Json> = q
        .context
        .iter()
        .map(|(t, ss)| json!({ "title": t, "url": page_url(t), "sentences": ss }))
        .collect();
    fs::write(
        dir.join("question.json"),
        serde_json::to_string_pretty(&json!({
            "id": q.id, "question": q.question, "answer": q.answer, "type": q.kind,
            "supporting": r["supporting"], "passages": passages,
        }))?,
    )?;
    for m in &o.modes {
        if r[m].is_object() && !o.regrade {
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
}

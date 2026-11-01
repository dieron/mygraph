//! LoCoMo (Maharana et al., 2024): ten long conversations between two people, each over up to 35
//! sessions weeks apart, with about 200 questions each (docs/11-benchmarks.md §4). Two ways to
//! answer:
//!
//! - `graph`: an agent records each conversation in a fresh graph one session at a time, in
//!   order, each session in a new context, without seeing the questions. Then each question gets a
//!   new agent that answers from a copy of that graph and cites evidence IDs; each cited
//!   evidence's text is mapped back to the messages it quotes.
//! - `direct`: the model reads the whole conversation in its prompt and cites message IDs.
//!
//! Answers are graded by a model, correct or wrong (the "LLM judge" score that published memory
//! systems report), and by token F1; citations against the messages LoCoMo marks as evidence.
//! Category 5 (adversarial) questions ask about something the conversation does not say: the right
//! answer is `unknown`. They are reported apart, as most published results leave them out.

use crate::agent::{Agent, Result, Run, last_json_with, read_transcript};
use crate::grade::{Sentences, answer_scores, set_scores};
use crate::graph;
use crate::multihop::{ANSWER_RULES, mean, sample};
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Options {
    pub data: PathBuf,
    pub out: PathBuf,
    /// Conversation IDs (`conv-26`); empty for all.
    pub conversations: Vec<String>,
    /// Questions per conversation, sampled; `None` for all.
    pub questions: Option<usize>,
    pub seed: u64,
    pub modes: Vec<String>,
    pub parallel: usize,
    pub mygraph: PathBuf,
    pub agent: Agent,
    /// Grades answers against the gold answer (categories 1–4).
    pub judge: Agent,
    /// Grade the transcripts of an earlier run again instead of running agents.
    pub regrade: bool,
    /// Keep the graphs of an earlier run and run only the answering agents again.
    pub reanswer: bool,
}

impl Options {
    /// Runs an agent, or with `regrade` reads the transcript it left.
    fn agent_run(
        &self,
        agent: &Agent,
        dir: &Path,
        name: &str,
        system: &str,
        prompt: &str,
        mcp: Option<&Json>,
    ) -> Result<Run> {
        if self.regrade {
            read_transcript(&dir.join(format!("{name}.jsonl")))
        } else {
            agent.run(dir, name, system, prompt, mcp)
        }
    }
}

struct Turn {
    /// `D3:12`: session 3, message 12.
    id: String,
    speaker: String,
    /// The message, with the caption of a photo shared in it.
    text: String,
}

struct Session {
    date_time: String,
    turns: Range<usize>,
}

struct Question {
    /// Position in the conversation's list, from 1.
    n: usize,
    question: String,
    /// None for category 5 (adversarial).
    answer: Option<String>,
    category: u64,
    /// The messages LoCoMo marks as evidence, by position in the conversation.
    evidence: BTreeSet<usize>,
}

struct Conversation {
    id: String,
    a: String,
    b: String,
    turns: Vec<Turn>,
    sessions: Vec<Session>,
    questions: Vec<Question>,
}

impl Conversation {
    fn turn(&self, id: &str) -> Option<usize> {
        self.turns.iter().position(|t| t.id == id)
    }
}

fn category_name(c: u64) -> &'static str {
    match c {
        1 => "multi-hop",
        2 => "temporal",
        3 => "open-domain",
        4 => "single-hop",
        5 => "adversarial",
        _ => "other",
    }
}

/// `locomo10.json` of the LoCoMo repository.
fn load(path: &Path) -> Result<Vec<Conversation>> {
    let all: Json = serde_json::from_str(&fs::read_to_string(path)?)?;
    let text = |v: &Json| match v {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    };
    let mut out = Vec::new();
    for c in all.as_array().ok_or("expected a list of conversations")? {
        let conv = &c["conversation"];
        let mut turns = Vec::new();
        let mut sessions = Vec::new();
        for n in 1.. {
            let Some(ts) = conv[format!("session_{n}").as_str()].as_array() else {
                break;
            };
            let start = turns.len();
            for t in ts {
                let mut message = text(&t["text"]);
                if let Some(caption) = t["blip_caption"].as_str() {
                    message += &format!(" [shares a photo: {caption}]");
                }
                turns.push(Turn {
                    id: text(&t["dia_id"]),
                    speaker: text(&t["speaker"]),
                    text: message,
                });
            }
            sessions.push(Session {
                date_time: text(&conv[format!("session_{n}_date_time").as_str()]),
                turns: start..turns.len(),
            });
        }
        let index: HashMap<&str, usize> = turns
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect();
        let questions = c["qa"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(i, q)| Question {
                n: i + 1,
                question: text(&q["question"]),
                answer: (!q["answer"].is_null()).then(|| text(&q["answer"])),
                category: q["category"].as_u64().unwrap_or(0),
                // A few entries hold several IDs in one string.
                evidence: q["evidence"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .flat_map(|e| {
                        text(e)
                            .split([';', ',', ' '])
                            .filter_map(|id| index.get(id.trim()).copied())
                            .collect::<Vec<_>>()
                    })
                    .collect(),
            })
            .collect();
        out.push(Conversation {
            id: text(&c["sample_id"]),
            a: text(&conv["speaker_a"]),
            b: text(&conv["speaker_b"]),
            turns,
            sessions,
            questions,
        });
    }
    Ok(out)
}

fn session_lines(c: &Conversation, s: &Session, ids: bool) -> String {
    c.turns[s.turns.clone()]
        .iter()
        .map(|t| {
            if ids {
                format!("[{}] {}: {}\n", t.id, t.speaker, t.text)
            } else {
                format!("{}: {}\n", t.speaker, t.text)
            }
        })
        .collect()
}

fn ingest_prompt(c: &Conversation, n: usize) -> String {
    let s = &c.sessions[n];
    format!(
        "Below is session {} of a conversation between {} and {}, held at {}. Record what it says \
         in MyGraph, so that later questions about these two people (their lives, plans and \
         opinions, what happened when, and how things changed) can be answered from the graph \
         alone and checked against the exact messages. {}Treat the session as a conversation \
         source titled \"{} and {}, session {}\", dated with the session's date; times such as \
         \"yesterday\" or \"last week\" count from that date. When you are done, reply with one \
         line: done.\n\n{}",
        n + 1,
        c.a,
        c.b,
        s.date_time,
        if n > 0 {
            "The graph already holds the earlier sessions: reuse the people, places and events \
             recorded there. "
        } else {
            ""
        },
        c.a,
        c.b,
        n + 1,
        session_lines(c, s, false)
    )
}

/// Both modes: times as dates, as LoCoMo's answers give them.
const DATE_RULE: &str = "Give a time as a date or period (\"7 May 2023\", \"the week before 9 June \
     2023\"), worked out from the session's date where the conversation says \"yesterday\" or \
     \"last week\".";

fn answer_prompt(c: &Conversation, q: &Question) -> String {
    format!(
        "Answer the question below about {} and {} from what is recorded in MyGraph only, not from \
         your own knowledge, and cite the evidence behind the answer. MyGraph holds their \
         conversations.\n\nQuestion: {}\n\nEnd your reply with one line of JSON: {{\"answer\": \
         \"...\", \"evidence\": [\"e_...\"]}}. \"evidence\" lists the IDs of the evidence (the \
         quoted messages) that support each step of the reasoning. {ANSWER_RULES} {DATE_RULE}",
        c.a, c.b, q.question
    )
}

const SYSTEM_DIRECT: &str = "You answer questions about the conversation you are given.";

fn direct_prompt(c: &Conversation, q: &Question) -> String {
    // The conversation first and the question last, so the prefix is cached across questions.
    let mut p = format!(
        "Below is a conversation between {} and {}, in {} sessions.\n",
        c.a,
        c.b,
        c.sessions.len()
    );
    for (n, s) in c.sessions.iter().enumerate() {
        p += &format!(
            "\n## Session {}, {}\n{}",
            n + 1,
            s.date_time,
            session_lines(c, s, true)
        );
    }
    p += &format!(
        "\nQuestion: {}\n\nAnswer from the conversation only, not from your own knowledge. End \
         your reply with one line of JSON: {{\"answer\": \"...\", \"evidence\": [\"D1:1\"]}}. \
         \"evidence\" lists the IDs of the messages that support each step of the reasoning. \
         {ANSWER_RULES} {DATE_RULE}",
        q.question
    );
    p
}

const SYSTEM_JUDGE: &str = "You grade answers to questions about a conversation.";

fn judge_prompt(q: &Question, gold: &str, answer: &str) -> String {
    format!(
        "Question: {}\nGold answer: {gold}\nGenerated answer: {answer}\n\nIs the generated answer \
         correct? Be generous: it is CORRECT if it gives the same information as the gold answer \
         or names the same thing, even when it is longer, worded differently or adds detail. A \
         date or time is CORRECT if it refers to the same date or period, in any format. It is \
         WRONG if it misses the point of the gold answer, contradicts it, or does not answer. \
         Reply with one line of JSON: {{\"label\": \"CORRECT\"}} or {{\"label\": \"WRONG\"}}.",
        q.question
    )
}

/// Whether an answer says the conversation does not tell (LoCoMo's own check for category 5,
/// plus `unknown`).
fn abstains(answer: &str) -> bool {
    let a = answer.trim().trim_end_matches('.').to_lowercase();
    a == "unknown" || a.contains("not mentioned") || a.contains("no information")
}

/// Token F1 as LoCoMo grades it: multi-hop answers part by part (comma-separated), open-domain
/// against the first of the gold alternatives, category 5 as abstaining or not. (Words are not
/// stemmed, unlike LoCoMo's script.)
fn f1(q: &Question, answer: &str) -> f64 {
    let Some(gold) = &q.answer else {
        return f64::from(abstains(answer));
    };
    match q.category {
        1 => {
            let parts: Vec<&str> = answer.split(',').collect();
            let golds: Vec<&str> = gold.split(',').collect();
            golds
                .iter()
                .map(|g| {
                    parts
                        .iter()
                        .map(|p| answer_scores(p, g).1)
                        .fold(0.0, f64::max)
                })
                .sum::<f64>()
                / golds.len() as f64
        }
        3 => answer_scores(answer, gold.split(';').next().unwrap_or(gold)).1,
        _ => answer_scores(answer, gold).1,
    }
}

/// Whether the answer is right, and the judge's run: category 5 must abstain; the others go to the
/// judge (its transcript is `judge-<mode>.jsonl`).
fn judge(o: &Options, dir: &Path, mode: &str, q: &Question, answer: &str) -> Result<(bool, Json)> {
    let Some(gold) = &q.answer else {
        return Ok((abstains(answer), Json::Null));
    };
    if answer.trim().is_empty() {
        return Ok((false, Json::Null));
    }
    let j = o.agent_run(
        &o.judge,
        dir,
        &format!("judge-{mode}"),
        SYSTEM_JUDGE,
        &judge_prompt(q, gold, answer),
        None,
    )?;
    if let Some(e) = &j.error {
        return Err(format!("judge: {e}").into());
    }
    let label = last_json_with(&j.text, "label").unwrap_or(Json::Null);
    Ok((label["label"] == "CORRECT", j.to_json()))
}

/// Per-question scores for one mode: the judge's verdict, F1, and the cited messages.
fn scores(
    c: &Conversation,
    q: &Question,
    answer: &str,
    cited: &BTreeSet<usize>,
    run: &Run,
    (correct, judge): (bool, Json),
) -> Map<String, Json> {
    let (ev_em, ev_f1, ev_prec, ev_rec) = set_scores(cited, &q.evidence);
    let m = json!({
        "answer": answer, "correct": f64::from(correct), "f1": f1(q, answer),
        "ev_em": ev_em, "ev_f1": ev_f1, "ev_prec": ev_prec, "ev_recall": ev_rec,
        "cited_turns": cited.iter().map(|i| &c.turns[*i].id).collect::<Vec<_>>(),
        "unknown": abstains(answer),
        "run": run.to_json(), "judge": judge,
    });
    m.as_object().cloned().unwrap_or_default()
}

fn run_direct(
    o: &Options,
    c: &Conversation,
    q: &Question,
    dir: &Path,
) -> Result<Map<String, Json>> {
    let run = o.agent_run(
        &o.agent,
        dir,
        "direct",
        SYSTEM_DIRECT,
        &direct_prompt(c, q),
        None,
    )?;
    let reply = last_json_with(&run.text, "answer").unwrap_or(Json::Null);
    let cited = reply["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| c.turn(e.as_str()?.trim()))
        .collect();
    let answer = reply["answer"].as_str().unwrap_or("");
    let verdict = judge(o, dir, "direct", q, answer)?;
    Ok(scores(c, q, answer, &cited, &run, verdict))
}

/// A conversation's graph: which messages each evidence quotes, and what building it took.
struct Stored {
    data: PathBuf,
    evidence: BTreeMap<String, BTreeSet<usize>>,
    turns: BTreeSet<usize>,
    size: Json,
    ingest: Json,
}

/// Records the conversation one session at a time, resuming after the last session recorded.
fn ingest(o: &Options, c: &Conversation, dir: &Path) -> Result<Stored> {
    let data = dir.join("data");
    let log = dir.join("ingest.json");
    let mut runs: Vec<Json> = fs::read_to_string(&log)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if o.regrade || o.reanswer {
        if runs.len() < c.sessions.len() {
            return Err(format!(
                "{}: {} of {} sessions recorded; run without --regrade or --reanswer first",
                c.id,
                runs.len(),
                c.sessions.len()
            )
            .into());
        }
    } else {
        if runs.is_empty() || !data.exists() {
            graph::create(&data)?;
            runs.clear();
        }
        let system = graph::system();
        for n in runs.len()..c.sessions.len() {
            let run = o.agent.run(
                dir,
                &format!("session-{:02}", n + 1),
                &system,
                &ingest_prompt(c, n),
                Some(&graph::server(&o.mygraph, &data, "ingest-agent")),
            )?;
            if let Some(e) = &run.error {
                return Err(format!("{} session {}: {e}", c.id, n + 1).into());
            }
            eprintln!(
                "{} session {}/{}: {} tool calls, {:.0} s, ${:.3}",
                c.id,
                n + 1,
                c.sessions.len(),
                run.tool_calls,
                run.seconds,
                run.cost_usd
            );
            runs.push(run.to_json());
            fs::write(&log, serde_json::to_string_pretty(&runs)?)?;
        }
    }
    let (texts, size) = graph::contents(&data)?;
    let messages =
        Sentences::new(c.turns.iter().map(|t| std::slice::from_ref(&t.text))).with_min_inside(4);
    // A `conversation` evidence is the session as a source: it may hold the whole transcript, and
    // citing it (for the session's date, say) does not point at any one message. Only quotes
    // (`text`) count as citing or storing messages.
    let evidence: BTreeMap<String, BTreeSet<usize>> = texts
        .into_iter()
        .map(|(id, e)| {
            let turns = if e.kind == "text" {
                messages
                    .matching(&e.content)
                    .into_iter()
                    .map(|(t, _)| t)
                    .collect()
            } else {
                BTreeSet::new()
            };
            (id, turns)
        })
        .collect();
    let turns = evidence.values().flatten().copied().collect();
    let sum = |k: &str| runs.iter().filter_map(|r| r[k].as_f64()).sum::<f64>();
    let ingest = json!({
        "sessions": runs.len(), "cost_usd": sum("cost_usd"), "tool_calls": sum("tool_calls"),
        "seconds": sum("seconds"),
    });
    Ok(Stored {
        data,
        evidence,
        turns,
        size,
        ingest,
    })
}

fn run_graph(
    o: &Options,
    c: &Conversation,
    stored: &Stored,
    q: &Question,
    dir: &Path,
) -> Result<Map<String, Json>> {
    // Each answering agent gets its own copy, so agents running at once do not share a data
    // folder and none sees another's changes.
    let data = dir.join("data");
    if !o.regrade {
        graph::copy(&stored.data, &data)?;
    }
    let ans = o.agent_run(
        &o.agent,
        dir,
        "answer",
        &graph::system(),
        &answer_prompt(c, q),
        Some(&graph::server(&o.mygraph, &data, "answer-agent")),
    );
    let _ = fs::remove_dir_all(&data);
    let ans = ans?;
    let reply = last_json_with(&ans.text, "answer").unwrap_or(Json::Null);
    let cited: Vec<String> = reply["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e.as_str().map(str::to_owned))
        .collect();
    let mut turns = BTreeSet::new();
    let mut unknown_refs = Vec::new();
    for e in &cited {
        match stored.evidence.get(e) {
            Some(t) => turns.extend(t.iter().copied()),
            None => unknown_refs.push(e.clone()),
        }
    }
    let answer = reply["answer"].as_str().unwrap_or("");
    let verdict = judge(o, dir, "graph", q, answer)?;
    let mut m = scores(c, q, answer, &turns, &ans, verdict);
    m.insert("cited".into(), json!(cited));
    m.insert("cited_unknown".into(), json!(unknown_refs));
    if !q.evidence.is_empty() {
        let kept = q.evidence.intersection(&stored.turns).count();
        m.insert(
            "evidence_stored".into(),
            (kept as f64 / q.evidence.len() as f64).into(),
        );
    }
    Ok(m)
}

fn summarize(results: &[Json], mode: &str) -> Json {
    let of = |keep: &dyn Fn(&Json) -> bool| -> Vec<&Json> {
        results
            .iter()
            .filter(|r| keep(r) && r[mode].is_object())
            .map(|r| &r[mode])
            .collect()
    };
    let main = of(&|r| r["category"] != 5);
    let adversarial = of(&|r| r["category"] == 5);
    let all = of(&|_| true);
    let metric = |rs: &[&Json], k: &'static str| mean(rs, move |r| r[k].as_f64());
    let mut by_category = Map::new();
    for cat in 1..=5 {
        let rs = of(&|r| r["category"] == cat);
        if rs.is_empty() {
            continue;
        }
        by_category.insert(
            format!("{cat} {}", category_name(cat)),
            json!({
                "questions": rs.len(), "correct": metric(&rs, "correct"), "f1": metric(&rs, "f1"),
                "ev_f1": metric(&rs, "ev_f1"),
            }),
        );
    }
    let mut s = json!({
        "questions": main.len(),
        "correct": metric(&main, "correct"), "f1": metric(&main, "f1"),
        "ev_prec": metric(&main, "ev_prec"), "ev_recall": metric(&main, "ev_recall"),
        "ev_f1": metric(&main, "ev_f1"),
        "unknown": main.iter().filter(|r| r["unknown"] == true).count(),
        "adversarial": {
            "questions": adversarial.len(), "correct": metric(&adversarial, "correct"),
        },
        "by_category": by_category,
        "errors": all.iter().filter(|r| !r["run"]["error"].is_null()).count(),
        "cost_usd": mean(&all, |r| r["run"]["cost_usd"].as_f64()),
        "judge_cost_usd": mean(&all, |r| r["judge"]["cost_usd"].as_f64()),
        "tool_calls": mean(&all, |r| r["run"]["tool_calls"].as_f64()),
        "seconds": mean(&all, |r| r["run"]["seconds"].as_f64()),
    });
    if mode == "graph" {
        s["evidence_stored"] = metric(&all, "evidence_stored");
        s["cited_unknown"] = all
            .iter()
            .filter_map(|r| r["cited_unknown"].as_array())
            .map(Vec::len)
            .sum::<usize>()
            .into();
    }
    s
}

pub fn run(o: &Options) -> Result<Json> {
    let all = load(&o.data)?;
    let total: usize = all.iter().map(|c| c.questions.len()).sum();
    let mut convs: Vec<Conversation> = all
        .into_iter()
        .filter(|c| o.conversations.is_empty() || o.conversations.contains(&c.id))
        .collect();
    if convs.is_empty() {
        return Err("no conversation matches --conversations".into());
    }
    for c in &mut convs {
        if let Some(n) = o.questions {
            let qs = std::mem::take(&mut c.questions);
            c.questions = sample(qs, n, o.seed);
            c.questions.sort_by_key(|q| q.n);
        }
    }
    fs::create_dir_all(&o.out)?;
    for c in &convs {
        write_conversation(c, &o.out.join(format!("c-{}", c.id)))?;
    }

    // The graphs: conversations at the same time, each one's sessions in order.
    let stored: Mutex<HashMap<String, std::result::Result<Stored, String>>> =
        Mutex::new(HashMap::new());
    if o.modes.iter().any(|m| m == "graph") {
        let queue = Mutex::new(convs.iter().collect::<VecDeque<_>>());
        std::thread::scope(|s| {
            for _ in 0..o.parallel.clamp(1, convs.len()) {
                s.spawn(|| {
                    loop {
                        let Some(c) = queue.lock().unwrap().pop_front() else {
                            break;
                        };
                        let r = ingest(o, c, &o.out.join(format!("c-{}", c.id)))
                            .map_err(|e| e.to_string());
                        if let Err(e) = &r {
                            eprintln!("{} ingest error: {e}", c.id);
                        }
                        stored.lock().unwrap().insert(c.id.clone(), r);
                    }
                });
            }
        });
    }
    let stored = stored.into_inner().unwrap();

    let queue = Mutex::new(
        convs
            .iter()
            .flat_map(|c| c.questions.iter().map(move |q| (c, q)))
            .collect::<VecDeque<_>>(),
    );
    let results = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..o.parallel.max(1) {
            s.spawn(|| {
                loop {
                    let Some((c, q)) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let r = run_question(o, c, stored.get(&c.id), q).unwrap_or_else(|e| {
                        json!({ "conversation": c.id, "n": q.n, "category": q.category,
                                "error": e.to_string() })
                    });
                    if let Some(e) = r.get("error") {
                        eprintln!("{} q{} error: {e}", c.id, q.n);
                    }
                    eprintln!(
                        "{} q{:<3} {:<11} {}",
                        c.id,
                        q.n,
                        category_name(q.category),
                        o.modes
                            .iter()
                            .map(|m| format!(
                                "{m}: {}",
                                match r[m]["correct"].as_f64() {
                                    Some(x) if x > 0.0 => "correct",
                                    Some(_) => "wrong",
                                    None => "-",
                                }
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
    results.sort_by(|a, b| {
        (a["conversation"].as_str(), a["n"].as_u64())
            .cmp(&(b["conversation"].as_str(), b["n"].as_u64()))
    });
    let mut summary = json!({
        "dataset": { "file": o.data.file_name().map(|f| f.to_string_lossy()), "questions": total },
        "sample": {
            "conversations": convs.iter().map(|c| &c.id).collect::<Vec<_>>(),
            "questions": results.len(), "per_conversation": o.questions, "seed": o.seed,
        },
        "provider": match &o.agent.backend {
            crate::agent::Backend::ClaudeCode(_) => "claude-code",
            crate::agent::Backend::OpenAi(_) => "openai",
        },
        "model": o.agent.model,
        "judge_model": o.judge.model,
        "failed": results.iter().filter(|r| r.get("error").is_some()).count(),
    });
    for m in &o.modes {
        summary[m] = summarize(&results, m);
    }
    if o.modes.iter().any(|m| m == "graph") {
        summary["graph"]["conversations"] = convs
            .iter()
            .map(|c| match stored.get(&c.id) {
                Some(Ok(s)) => json!({ "id": c.id, "ingest": s.ingest, "graph": s.size }),
                Some(Err(e)) => json!({ "id": c.id, "error": e }),
                None => json!({ "id": c.id }),
            })
            .collect();
    }
    fs::write(
        o.out.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    Ok(summary)
}

/// The conversation as the runner reads it, for reading transcripts and checking the grading.
fn write_conversation(c: &Conversation, dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    let sessions: Vec<Json> = c
        .sessions
        .iter()
        .map(|s| {
            json!({
                "date_time": s.date_time,
                "turns": c.turns[s.turns.clone()].iter()
                    .map(|t| json!({ "id": t.id, "speaker": t.speaker, "text": t.text }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    fs::write(
        dir.join("conversation.json"),
        serde_json::to_string_pretty(&json!({
            "id": c.id, "speakers": [c.a, c.b], "sessions": sessions,
        }))?,
    )?;
    Ok(())
}

/// Runs the modes for one question, or reads the result of an earlier run from its folder.
fn run_question(
    o: &Options,
    c: &Conversation,
    stored: Option<&std::result::Result<Stored, String>>,
    q: &Question,
) -> Result<Json> {
    let dir = o
        .out
        .join(format!("c-{}", c.id))
        .join(format!("q-{:03}", q.n));
    let file = dir.join("result.json");
    let mut r = match fs::read_to_string(&file) {
        Ok(s) => serde_json::from_str(&s)?,
        Err(_) => json!({
            "conversation": c.id, "n": q.n, "category": q.category, "question": q.question,
            "gold": q.answer,
            "evidence": q.evidence.iter().map(|i| &c.turns[*i].id).collect::<Vec<_>>(),
        }),
    };
    fs::create_dir_all(&dir)?;
    for m in &o.modes {
        if r[m].is_object() && !o.regrade && !o.reanswer {
            continue;
        }
        let scored = match (m.as_str(), stored) {
            ("graph", Some(Ok(s))) => run_graph(o, c, s, q, &dir),
            ("graph", Some(Err(e))) => Err(format!("no graph: {e}").into()),
            ("graph", None) => Err("no graph".into()),
            ("direct", _) => run_direct(o, c, q, &dir),
            (other, _) => Err(format!("unknown mode {other}").into()),
        };
        // A failed mode, or one whose agent run ended in an error, is reported and left out of
        // the file, so a rerun tries it again.
        match scored {
            Ok(s) if s["run"]["error"].is_string() => {
                r["error"] = format!("{m}: {}", s["run"]["error"]).into();
            }
            Ok(s) => {
                r[m] = Json::Object(s);
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

    fn question(category: u64, answer: Option<&str>) -> Question {
        Question {
            n: 1,
            question: "?".into(),
            answer: answer.map(str::to_owned),
            category,
            evidence: BTreeSet::new(),
        }
    }

    #[test]
    fn f1_follows_locomo_per_category() {
        let multi = question(1, Some("Pottery, camping"));
        assert_eq!(f1(&multi, "camping, pottery"), 1.0);
        assert_eq!(f1(&multi, "pottery"), 0.5);
        let open = question(3, Some("Likely no; she loves it"));
        assert_eq!(f1(&open, "likely no"), 1.0);
        let adversarial = question(5, None);
        assert_eq!(f1(&adversarial, "unknown"), 1.0);
        assert_eq!(f1(&adversarial, "Not mentioned in the conversation."), 1.0);
        assert_eq!(f1(&adversarial, "self-care"), 0.0);
    }

    #[test]
    fn loads_sessions_turns_and_evidence() {
        let dir = std::env::temp_dir().join(format!("locomo-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("locomo.json");
        let data = json!([{
            "sample_id": "conv-1",
            "conversation": {
                "speaker_a": "Ann", "speaker_b": "Bo",
                "session_1_date_time": "1:56 pm on 8 May, 2023",
                "session_1": [
                    { "speaker": "Ann", "dia_id": "D1:1", "text": "Hi!" },
                    { "speaker": "Bo", "dia_id": "D1:2", "text": "Look.", "blip_caption": "a dog" }
                ],
                "session_2_date_time": "2:00 pm on 9 May, 2023",
                "session_2": [{ "speaker": "Ann", "dia_id": "D2:1", "text": "Again." }]
            },
            "qa": [
                { "question": "When?", "answer": 2022, "evidence": ["D1:2; D2:1"], "category": 2 },
                { "question": "Who?", "adversarial_answer": "x", "evidence": ["D9:9"],
                  "category": 5 }
            ]
        }]);
        fs::write(&file, data.to_string()).unwrap();
        let c = load(&file).unwrap().remove(0);
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(c.sessions.len(), 2);
        assert_eq!(c.sessions[1].turns, 2..3);
        assert_eq!(c.turns[1].text, "Look. [shares a photo: a dog]");
        assert_eq!(c.questions[0].answer.as_deref(), Some("2022"));
        assert_eq!(c.questions[0].evidence, [1, 2].into());
        assert_eq!(c.questions[1].answer, None);
        assert!(c.questions[1].evidence.is_empty());
        assert!(direct_prompt(&c, &c.questions[0]).contains("[D1:2] Bo: Look."));
        assert!(ingest_prompt(&c, 1).contains("session 2 of a conversation"));
    }

    #[test]
    fn short_messages_do_not_match_inside_longer_quotes() {
        let turns = [
            "Thanks!".to_owned(),
            "I ran a charity race for mental health. Thanks!".to_owned(),
        ];
        let m = Sentences::new(turns.iter().map(std::slice::from_ref)).with_min_inside(4);
        assert_eq!(
            m.matching("I ran a charity race for mental health. Thanks!"),
            [(1, 0)].into()
        );
        assert_eq!(m.matching("Thanks!"), [(0, 0)].into());
    }
}

//! LongMemEval (Wu et al., 2025), the `_s` set: 500 questions, each with its own history of about
//! 50 chat sessions between a user and an assistant (about 115 000 tokens), a few of which hold
//! the answer (docs/11-benchmarks.md §4). Two ways to answer:
//!
//! - `graph`: a script, not an agent, stores the history in a fresh graph: each session as a
//!   `conversation` evidence and a `ChatSession` node with its date, each message as a `text`
//!   evidence of the session. No model reads the history while storing it, so this tests finding
//!   things in a long history rather than extracting facts from it. Then an agent answers from the
//!   graph and cites evidence IDs, which map back to messages.
//! - `direct`: the model reads the whole history in its prompt and cites message IDs.
//!
//! Answers are graded by a model, correct or wrong, with a rule per question type after
//! LongMemEval's own grading prompts; citations against the messages and sessions it marks as
//! holding the answer. Questions whose ID ends in `_abs` cannot be answered from the history: the
//! right answer says so.

use crate::agent::{Agent, Result, Run, last_json_with, read_transcript};
use crate::grade::set_scores;
use crate::graph;
use crate::multihop::{mean, sample};
use mygraph_core::model::Policies;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub struct Options {
    pub data: PathBuf,
    pub out: PathBuf,
    /// Question types (`multi-session`); empty for all.
    pub types: Vec<String>,
    /// Questions, sampled evenly over the question types; `None` for all.
    pub questions: Option<usize>,
    pub seed: u64,
    pub modes: Vec<String>,
    pub parallel: usize,
    pub mygraph: PathBuf,
    pub agent: Agent,
    pub judge: Agent,
    /// Grade the transcripts of an earlier run again instead of running agents.
    pub regrade: bool,
    /// Run the answering agents (and `direct`) again on questions that have results.
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
    /// `S12:3`: session 12, message 3, both from 1.
    id: String,
    role: String,
    content: String,
    has_answer: bool,
}

struct Session {
    id: String,
    /// As LongMemEval gives it: `2023/05/20 (Sat) 02:21`.
    date: String,
    turns: Vec<Turn>,
}

struct Question {
    id: String,
    kind: String,
    question: String,
    /// The gold answer; for a preference question, a description of the answer wanted; for an
    /// abstention question, why it cannot be answered.
    answer: String,
    date: String,
    sessions: Vec<Session>,
    /// Positions in `sessions` of the sessions that hold the answer.
    answer_sessions: BTreeSet<usize>,
}

impl Question {
    fn abstention(&self) -> bool {
        self.id.ends_with("_abs")
    }

    /// The messages that hold the answer, as (session, message) positions.
    fn evidence(&self) -> BTreeSet<(usize, usize)> {
        let mut e = BTreeSet::new();
        for (s, session) in self.sessions.iter().enumerate() {
            for (t, turn) in session.turns.iter().enumerate() {
                if turn.has_answer {
                    e.insert((s, t));
                }
            }
        }
        e
    }

    fn turn(&self, id: &str) -> Option<(usize, usize)> {
        let (s, t) = id.strip_prefix('S')?.split_once(':')?;
        let (s, t) = (s.parse::<usize>().ok()?, t.parse::<usize>().ok()?);
        let session = self.sessions.get(s.checked_sub(1)?)?;
        session.turns.get(t.checked_sub(1)?)?;
        Some((s - 1, t - 1))
    }
}

/// The question types, in the order a sample draws from them.
const KINDS: [&str; 6] = [
    "single-session-user",
    "single-session-assistant",
    "single-session-preference",
    "multi-session",
    "temporal-reasoning",
    "knowledge-update",
];

/// `longmemeval_s_cleaned.json` (or `_oracle`, `_m`) of `xiaowu0162/longmemeval-cleaned`.
fn load(path: &Path) -> Result<Vec<Question>> {
    let all: Json = serde_json::from_str(&fs::read_to_string(path)?)?;
    let text = |v: &Json| match v {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    };
    let mut out = Vec::new();
    for q in all.as_array().ok_or("expected a list of questions")? {
        let ids: Vec<String> = q["haystack_session_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .map(text)
            .collect();
        let dates: Vec<String> = q["haystack_dates"]
            .as_array()
            .into_iter()
            .flatten()
            .map(text)
            .collect();
        let sessions: Vec<Session> = q["haystack_sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(s, turns)| Session {
                id: ids.get(s).cloned().unwrap_or_default(),
                date: dates.get(s).cloned().unwrap_or_default(),
                turns: turns
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(t, turn)| Turn {
                        id: format!("S{}:{}", s + 1, t + 1),
                        role: text(&turn["role"]),
                        content: text(&turn["content"]),
                        has_answer: turn["has_answer"] == true,
                    })
                    .collect(),
            })
            .collect();
        let answer_ids: BTreeSet<String> = q["answer_session_ids"]
            .as_array()
            .into_iter()
            .flatten()
            .map(text)
            .collect();
        out.push(Question {
            id: text(&q["question_id"]),
            kind: text(&q["question_type"]),
            question: text(&q["question"]),
            answer: text(&q["answer"]),
            date: text(&q["question_date"]),
            answer_sessions: sessions
                .iter()
                .enumerate()
                .filter(|(_, s)| answer_ids.contains(&s.id))
                .map(|(i, _)| i)
                .collect(),
            sessions,
        });
    }
    Ok(out)
}

/// `n` questions spread evenly over the question types: each type shuffled with `seed`, then one
/// from each in turn.
fn sample_by_kind(all: Vec<Question>, n: usize, seed: u64) -> Vec<Question> {
    let mut groups: Vec<VecDeque<Question>> = KINDS.iter().map(|_| VecDeque::new()).collect();
    let mut other = VecDeque::new();
    for q in all {
        match KINDS.iter().position(|k| *k == q.kind) {
            Some(i) => groups[i].push_back(q),
            None => other.push_back(q),
        }
    }
    groups.push(other);
    let mut groups: Vec<VecDeque<Question>> = groups
        .into_iter()
        .map(|g| {
            let len = g.len();
            sample(g.into(), len, seed).into()
        })
        .collect();
    let mut out = Vec::new();
    while out.len() < n && groups.iter().any(|g| !g.is_empty()) {
        for g in &mut groups {
            if out.len() < n
                && let Some(q) = g.pop_front()
            {
                out.push(q);
            }
        }
    }
    out
}

/// `2023/05/20 (Sat) 02:21` as `2023-05-20T02:21:00Z`; the dates carry no time zone.
fn datetime(date: &str) -> Option<String> {
    let mut parts = date.split_whitespace();
    let day = parts.next()?.replace('/', "-");
    let time = parts.last()?;
    (day.len() == 10 && time.len() == 5).then(|| format!("{day}T{time}:00Z"))
}

/// The policies of a stored history: messages up to 256 KiB (the longest is 75 KiB).
fn policies() -> Policies {
    Policies {
        inline_text_limit: 262_144,
        ..Policies::default()
    }
}

/// A question's history in a graph, and which message each evidence is.
struct Stored {
    evidence: BTreeMap<String, (usize, usize)>,
    size: Json,
}

/// Stores the history in a fresh graph in `data`, one write per session.
fn store(q: &Question, data: &Path) -> Result<Stored> {
    let started = std::time::Instant::now();
    let mut g = graph::create_with(data, &policies())?;
    let semantic = graph::attach_semantic(&mut g, data)?;
    let mut evidence = BTreeMap::new();
    let mut messages = 0;
    for (s, session) in q.sessions.iter().enumerate() {
        let n = s + 1;
        let date = datetime(&session.date).map(|d| json!({ "$datetime": d }));
        let mut items = vec![json!({
            "ref": "@s", "kind": "conversation", "title": format!("Chat session {n}"),
            "content": format!(
                "Chat session {n} between the user and the assistant, {}, {} messages.",
                session.date, session.turns.len()
            ),
            "source_date": date, "locator": { "session": n },
        })];
        for (t, turn) in session.turns.iter().enumerate() {
            items.push(json!({
                "ref": format!("@m{}", t + 1), "kind": "text", "parent": "@s",
                "title": format!("{}, session {n}, message {}, {}", session.date, t + 1, turn.role),
                "content": turn.content, "author": turn.role, "source_date": date,
                "locator": { "session": n, "message": t + 1 },
            }));
        }
        let mut cite = vec![json!("@s")];
        cite.extend((1..=session.turns.len()).map(|t| json!(format!("@m{t}"))));
        let mut props = json!({ "messages": session.turns.len() });
        if let Some(d) = &date {
            props["date"] = d.clone();
        }
        let request = json!({
            "evidence": items,
            "nodes": [{ "type": "ChatSession", "name": format!("Session {n}"), "props": props,
                        "summary": format!("Chat session {n}, {}", session.date), "cite": cite }],
        });
        let r = g
            .write("history-script", &request, graph::now_ms()?)
            .map_err(|e| format!("session {n}: {}", e.to_json()))?;
        for t in 0..session.turns.len() {
            if let Some(id) = r["refs"][format!("@m{}", t + 1)].as_str() {
                evidence.insert(id.to_owned(), (s, t));
            }
        }
        messages += session.turns.len();
    }
    let written = started.elapsed().as_secs_f64();
    // Embedded now, so the agent's server finds the semantic index complete.
    g.commit_semantic()?;
    drop(g);
    Ok(Stored {
        evidence,
        size: json!({
            "sessions": q.sessions.len(), "messages": messages,
            "seconds": started.elapsed().as_secs_f64(), "write_seconds": written,
            "semantic": semantic,
        }),
    })
}

/// Both modes: how short to answer, when to abstain, and what "now" is.
fn answer_rules(q: &Question) -> String {
    format!(
        "Today is {}; times such as \"last week\" count from the date of the session they were \
         said in. The answer is short: a name, a number, a date, a phrase, or yes or no; for a \
         request for advice or suggestions, one or two sentences that use what the user has told \
         you about themselves. When the history points to an answer without settling every \
         detail, give the best-supported answer; when it holds nothing that answers the question, \
         answer \"unknown\".",
        q.date
    )
}

fn answer_prompt(q: &Question) -> String {
    format!(
        "You are the user's assistant. MyGraph holds the history of your past chats with the user: \
         each chat as a `ChatSession` node with its date, each message as a `text` evidence whose \
         title gives the date, session, message number and speaker. Answer the user's question \
         below from that history only, not from your own knowledge, and cite the evidence behind \
         the answer.\n\nQuestion: {}\n\nEnd your reply with one line of JSON: {{\"answer\": \
         \"...\", \"evidence\": [\"e_...\"]}}. \"evidence\" lists the IDs of the messages that \
         support each step of the reasoning. {}",
        q.question,
        answer_rules(q)
    )
}

const SYSTEM_DIRECT: &str = "You are an assistant that remembers its past chats with the user.";

fn direct_prompt(q: &Question) -> String {
    let mut p = format!(
        "Below is the history of your past chats with the user, in {} sessions.\n",
        q.sessions.len()
    );
    for (n, s) in q.sessions.iter().enumerate() {
        p += &format!("\n## Session {}, {}\n", n + 1, s.date);
        for t in &s.turns {
            p += &format!("[{}] {}: {}\n", t.id, t.role, t.content);
        }
    }
    p += &format!(
        "\nQuestion: {}\n\nAnswer from the history only, not from your own knowledge. End your \
         reply with one line of JSON: {{\"answer\": \"...\", \"evidence\": [\"S1:1\"]}}. \
         \"evidence\" lists the IDs of the messages that support each step of the reasoning. {}",
        q.question,
        answer_rules(q)
    );
    p
}

const SYSTEM_JUDGE: &str = "You grade an assistant's answers to questions about its past chats \
     with the user.";

/// The grading rule for the question's type, after LongMemEval's prompts.
fn judge_prompt(q: &Question, answer: &str) -> String {
    let (gold, rule) = if q.abstention() {
        (
            "Why the question cannot be answered",
            "The question cannot be answered from the chat history. The response is CORRECT if \
             it says the information is not available, unknown or was never mentioned; it is \
             WRONG if it gives an answer as if it were known.",
        )
    } else {
        match q.kind.as_str() {
            "single-session-preference" => (
                "What a good response does",
                "The description says what a response that uses the user's preferences looks \
                 like. The response is CORRECT if it recalls and uses the user's personal \
                 information in the way described; it need not cover every point. It is WRONG if \
                 it ignores or misremembers that information.",
            ),
            "temporal-reasoning" => (
                "Correct answer",
                "The response is CORRECT if it gives the correct answer or an equivalent one, \
                 also when it adds the steps that lead there. Do not count an off-by-one error \
                 in a number of days, weeks or months as wrong (18 days for 19 is CORRECT). It \
                 is WRONG if it gives another answer or only part of what is asked.",
            ),
            "knowledge-update" => (
                "Correct answer",
                "The correct answer is the most recent information. The response is CORRECT if \
                 it gives that answer, also when it mentions earlier information as well. It is \
                 WRONG if it gives the earlier information as the answer, or another answer.",
            ),
            _ => (
                "Correct answer",
                "The response is CORRECT if it gives the correct answer or an equivalent one, \
                 also when it adds the steps that lead there. It is WRONG if it gives another \
                 answer or only part of the information asked for.",
            ),
        }
    };
    format!(
        "Question: {}\n{gold}: {}\nResponse: {answer}\n\n{rule} Reply with one line of JSON: \
         {{\"label\": \"CORRECT\"}} or {{\"label\": \"WRONG\"}}.",
        q.question, q.answer
    )
}

fn abstains(answer: &str) -> bool {
    let a = answer.trim().trim_end_matches('.').to_lowercase();
    a == "unknown"
        || a.starts_with("unknown")
        || a.contains("not mentioned")
        || a.contains("no information")
}

/// Whether the answer is right, by the judge (its transcript is `judge-<mode>.jsonl`).
fn judge(o: &Options, dir: &Path, mode: &str, q: &Question, answer: &str) -> Result<(bool, Json)> {
    if answer.trim().is_empty() {
        return Ok((false, Json::Null));
    }
    let j = o.agent_run(
        &o.judge,
        dir,
        &format!("judge-{mode}"),
        SYSTEM_JUDGE,
        &judge_prompt(q, answer),
        None,
    )?;
    if let Some(e) = &j.error {
        return Err(format!("judge: {e}").into());
    }
    let label = last_json_with(&j.text, "label").unwrap_or(Json::Null);
    Ok((label["label"] == "CORRECT", j.to_json()))
}

/// Per-question scores for one mode: the verdict and the cited messages and sessions. Citations
/// are not graded on abstention questions, or where LongMemEval marks no message.
fn scores(
    q: &Question,
    answer: &str,
    cited: &BTreeSet<(usize, usize)>,
    run: &Run,
    (correct, judge): (bool, Json),
) -> Map<String, Json> {
    let mut m = json!({
        "answer": answer, "correct": f64::from(correct), "unknown": abstains(answer),
        "cited_turns": cited.iter().map(|(s, t)| &q.sessions[*s].turns[*t].id).collect::<Vec<_>>(),
        "run": run.to_json(), "judge": judge,
    });
    let gold = q.evidence();
    if !q.abstention() && !gold.is_empty() {
        let (_, ev_f1, ev_prec, ev_rec) = set_scores(cited, &gold);
        let sessions: BTreeSet<usize> = cited.iter().map(|(s, _)| *s).collect();
        let (_, _, _, session_rec) = set_scores(&sessions, &q.answer_sessions);
        m["ev_f1"] = ev_f1.into();
        m["ev_prec"] = ev_prec.into();
        m["ev_recall"] = ev_rec.into();
        m["session_recall"] = session_rec.into();
    }
    m.as_object().cloned().unwrap_or_default()
}

fn run_direct(o: &Options, q: &Question, dir: &Path) -> Result<Map<String, Json>> {
    let run = o.agent_run(
        &o.agent,
        dir,
        "direct",
        SYSTEM_DIRECT,
        &direct_prompt(q),
        None,
    )?;
    let reply = last_json_with(&run.text, "answer").unwrap_or(Json::Null);
    let cited = reply["evidence"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| q.turn(e.as_str()?.trim()))
        .collect();
    let answer = reply["answer"].as_str().unwrap_or("");
    let verdict = judge(o, dir, "direct", q, answer)?;
    Ok(scores(q, answer, &cited, &run, verdict))
}

fn run_graph(o: &Options, q: &Question, dir: &Path) -> Result<Map<String, Json>> {
    // The graph is rebuilt for every answer (it takes seconds and no model) and removed after,
    // keeping only which evidence is which message, for grading again.
    let map_file = dir.join("graph.json");
    let data = dir.join("data");
    let (evidence, size) = if o.regrade {
        let m: Json = serde_json::from_str(&fs::read_to_string(&map_file)?)?;
        let evidence = m["evidence"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(id, st)| {
                Some((
                    id.clone(),
                    (st[0].as_u64()? as usize, st[1].as_u64()? as usize),
                ))
            })
            .collect();
        (evidence, m["size"].clone())
    } else {
        let s = store(q, &data)?;
        fs::write(
            &map_file,
            serde_json::to_string(&json!({ "evidence": s.evidence, "size": s.size }))?,
        )?;
        (s.evidence, s.size)
    };
    let ans = o.agent_run(
        &o.agent,
        dir,
        "answer",
        &graph::system(),
        &answer_prompt(q),
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
        match evidence.get(e) {
            Some(st) => {
                turns.insert(*st);
            }
            // A session's evidence or node: not a message.
            None => unknown_refs.push(e.clone()),
        }
    }
    let answer = reply["answer"].as_str().unwrap_or("");
    let verdict = judge(o, dir, "graph", q, answer)?;
    let mut m = scores(q, answer, &turns, &ans, verdict);
    m.insert("cited".into(), json!(cited));
    m.insert("cited_other".into(), json!(unknown_refs));
    m.insert("stored".into(), size);
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
    let all = of(&|_| true);
    let metric = |rs: &[&Json], k: &'static str| mean(rs, move |r| r[k].as_f64());
    let mut by_type = Map::new();
    for kind in KINDS {
        let rs = of(&|r| r["type"] == kind && r["abstention"] == false);
        if !rs.is_empty() {
            by_type.insert(
                kind.into(),
                json!({ "questions": rs.len(), "correct": metric(&rs, "correct") }),
            );
        }
    }
    let abstention = of(&|r| r["abstention"] == true);
    if !abstention.is_empty() {
        by_type.insert(
            "abstention".into(),
            json!({ "questions": abstention.len(), "correct": metric(&abstention, "correct") }),
        );
    }
    json!({
        "questions": all.len(),
        "correct": metric(&all, "correct"),
        "ev_prec": metric(&all, "ev_prec"), "ev_recall": metric(&all, "ev_recall"),
        "ev_f1": metric(&all, "ev_f1"), "session_recall": metric(&all, "session_recall"),
        "unknown": all.iter().filter(|r| r["unknown"] == true).count(),
        "by_type": by_type,
        "errors": all.iter().filter(|r| !r["run"]["error"].is_null()).count(),
        "cost_usd": mean(&all, |r| r["run"]["cost_usd"].as_f64()),
        "judge_cost_usd": mean(&all, |r| r["judge"]["cost_usd"].as_f64()),
        "input_tokens": mean(&all, |r| r["run"]["input_tokens"].as_f64()),
        "tool_calls": mean(&all, |r| r["run"]["tool_calls"].as_f64()),
        "seconds": mean(&all, |r| r["run"]["seconds"].as_f64()),
    })
}

pub fn run(o: &Options) -> Result<Json> {
    let all = load(&o.data)?;
    let total = all.len();
    if let Some(t) = o.types.iter().find(|t| !KINDS.contains(&t.as_str())) {
        return Err(format!(
            "unknown question type {t}; the types are {}",
            KINDS.join(", ")
        )
        .into());
    }
    let all: Vec<Question> = all
        .into_iter()
        .filter(|q| o.types.is_empty() || o.types.contains(&q.kind))
        .collect();
    let questions = match o.questions {
        Some(n) => sample_by_kind(all, n, o.seed),
        None => all,
    };
    fs::create_dir_all(&o.out)?;
    let queue = Mutex::new(questions.iter().collect::<VecDeque<_>>());
    let results = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..o.parallel.max(1) {
            s.spawn(|| {
                loop {
                    let Some(q) = queue.lock().unwrap().pop_front() else {
                        break;
                    };
                    let r = run_question(o, q).unwrap_or_else(|e| {
                        json!({ "id": q.id, "type": q.kind, "abstention": q.abstention(),
                                "error": e.to_string() })
                    });
                    if let Some(e) = r.get("error") {
                        eprintln!("{} error: {e}", q.id);
                    }
                    eprintln!(
                        "{:<14} {:<25} {}",
                        q.id,
                        q.kind,
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
    results.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    let mut summary = json!({
        "dataset": { "file": o.data.file_name().map(|f| f.to_string_lossy()), "questions": total },
        "sample": { "questions": results.len(), "types": o.types, "seed": o.seed },
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
            "id": q.id, "type": q.kind, "abstention": q.abstention(), "question": q.question,
            "date": q.date, "gold": q.answer,
            "evidence": q.evidence().iter()
                .map(|(s, t)| &q.sessions[*s].turns[*t].id).collect::<Vec<_>>(),
            "answer_sessions": q.answer_sessions.iter().map(|s| s + 1).collect::<Vec<_>>(),
        }),
    };
    // An error saved by an earlier run is about that run; the modes it left out run again.
    if let Some(m) = r.as_object_mut() {
        m.remove("error");
    }
    fs::create_dir_all(&dir)?;
    for m in &o.modes {
        if r[m].is_object() && !o.regrade && !o.reanswer {
            continue;
        }
        let scored = match m.as_str() {
            "graph" => run_graph(o, q, &dir),
            "direct" => run_direct(o, q, &dir),
            other => Err(format!("unknown mode {other}").into()),
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

    fn history() -> Json {
        json!([{
            "question_id": "q1", "question_type": "single-session-user",
            "question": "What degree did I graduate with?", "answer": "Business Administration",
            "question_date": "2023/05/30 (Tue) 23:40",
            "haystack_session_ids": ["a", "b"],
            "haystack_dates": ["2023/05/20 (Sat) 02:21", "2023/05/21 (Sun) 10:00"],
            "haystack_sessions": [
                [{ "role": "user", "content": "Hi there." },
                 { "role": "assistant", "content": "Hello!" }],
                [{ "role": "user", "content": "I graduated in Business Administration.",
                   "has_answer": true },
                 { "role": "assistant", "content": "Congratulations!" }]
            ],
            "answer_session_ids": ["b"]
        }, {
            "question_id": "q2_abs", "question_type": "temporal-reasoning",
            "question": "When did I adopt my hamster?", "answer": 3,
            "question_date": "2023/05/30 (Tue) 23:40",
            "haystack_session_ids": [], "haystack_dates": [], "haystack_sessions": [],
            "answer_session_ids": []
        }])
    }

    /// Tests run in parallel, so each call gets its own directory: a shared one is removed by
    /// one test while another is still writing to it.
    fn loaded() -> Vec<Question> {
        static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("lme-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("lme.json");
        fs::write(&file, history().to_string()).unwrap();
        let qs = load(&file).unwrap();
        let _ = fs::remove_dir_all(&dir);
        qs
    }

    #[test]
    fn loads_sessions_turns_and_evidence() {
        let qs = loaded();
        let q = &qs[0];
        assert_eq!(q.sessions.len(), 2);
        assert_eq!(q.sessions[1].turns[0].id, "S2:1");
        assert_eq!(q.evidence(), [(1, 0)].into());
        assert_eq!(q.answer_sessions, [1].into());
        assert_eq!(q.turn("S2:1"), Some((1, 0)));
        assert_eq!(q.turn("S3:1"), None);
        assert_eq!(q.turn("S2:0"), None);
        assert!(direct_prompt(q).contains("[S2:1] user: I graduated"));
        assert!(!q.abstention());
        assert!(qs[1].abstention());
        assert_eq!(qs[1].answer, "3");
    }

    #[test]
    fn dates_become_datetimes() {
        assert_eq!(
            datetime("2023/05/20 (Sat) 02:21").as_deref(),
            Some("2023-05-20T02:21:00Z")
        );
        assert_eq!(datetime("someday"), None);
    }

    #[test]
    fn samples_spread_over_types() {
        let mut all = Vec::new();
        for (i, kind) in KINDS.iter().enumerate() {
            for j in 0..5 {
                all.push(Question {
                    id: format!("{i}-{j}"),
                    kind: (*kind).into(),
                    question: String::new(),
                    answer: String::new(),
                    date: String::new(),
                    sessions: Vec::new(),
                    answer_sessions: BTreeSet::new(),
                });
            }
        }
        let s = sample_by_kind(all, 8, 1);
        assert_eq!(s.len(), 8);
        let kinds: BTreeSet<&str> = s.iter().map(|q| q.kind.as_str()).collect();
        assert_eq!(kinds.len(), 6);
    }

    #[test]
    fn stores_each_message_as_evidence_of_its_session() {
        let qs = loaded();
        let dir = std::env::temp_dir().join(format!("lme-store-{}", std::process::id()));
        let stored = store(&qs[0], &dir.join("data")).unwrap();
        assert_eq!(stored.evidence.len(), 4);
        assert!(stored.evidence.values().any(|st| *st == (1, 0)));
        let (texts, size) = graph::contents(&dir.join("data")).unwrap();
        let _ = fs::remove_dir_all(&dir);
        assert_eq!(size["nodes"], 2);
        assert_eq!(size["evidence"], 6);
        let message = texts
            .iter()
            .find(|(_, e)| e.content.starts_with("I graduated"))
            .map(|(id, _)| id)
            .unwrap();
        assert_eq!(stored.evidence[message], (1, 0));
    }
}

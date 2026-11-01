//! An agent loop on the OpenAI Chat Completions API (or a compatible endpoint): the MCP servers'
//! tools are offered as functions, calls go to the servers, until the model replies without a
//! tool call. The transcript is written in the same shape as Claude Code's `stream-json`, so the
//! same readers grade and display both.

use crate::agent::{Agent, Result, Run};
use crate::mcp::Server;
use serde_json::{Map, Value as Json, json};
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, Instant};

/// Model calls per agent run, at most.
const MAX_TURNS: u64 = 60;
/// Attempts per model call when the API is busy (HTTP 429 or 5xx) or unreachable.
const ATTEMPTS: u32 = 6;

pub struct OpenAi {
    /// E.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub key: String,
    /// `reasoning_effort` for reasoning models (`low`, `medium`, `high`).
    pub reasoning_effort: Option<String>,
    /// USD per million tokens, for `cost_usd` and the budget; without them cost is 0.
    pub prices: Option<Prices>,
}

#[derive(Clone, Copy)]
pub struct Prices {
    pub input: f64,
    pub cached_input: f64,
    pub output: f64,
}

#[derive(Default)]
struct Usage {
    input: u64,
    cached: u64,
    output: u64,
}

impl Usage {
    fn cost(&self, p: Option<Prices>) -> f64 {
        p.map_or(0.0, |p| {
            (self.input.saturating_sub(self.cached) as f64 * p.input
                + self.cached as f64 * p.cached_input
                + self.output as f64 * p.output)
                / 1e6
        })
    }
}

/// A tool as offered to the model: which server has it, and its name there.
struct Tool {
    server: usize,
    name: String,
}

pub fn run(
    cfg: &OpenAi,
    agent: &Agent,
    dir: &Path,
    name: &str,
    system: &str,
    prompt: &str,
    mcp: Option<&Json>,
) -> Result<Run> {
    fs::write(dir.join(format!("{name}.system.md")), system)?;
    let mut log = File::create(dir.join(format!("{name}.jsonl")))?;
    let mut emit = |v: Json| writeln!(log, "{v}");
    let start = Instant::now();

    // Servers and their tools; names get a server prefix only when there are several servers.
    let entries: Vec<(&String, &Json)> = mcp
        .and_then(Json::as_object)
        .into_iter()
        .flatten()
        .collect();
    let mut servers = Vec::new();
    let mut tools: Vec<(String, Tool)> = Vec::new();
    let mut functions = Vec::new();
    let mut system = system.to_owned();
    for (i, (server_name, entry)) in entries.iter().enumerate() {
        let s = Server::start(entry, &dir.join(format!("{name}.{server_name}.stderr")))?;
        for t in &s.tools {
            let tool = t["name"].as_str().unwrap_or_default().to_owned();
            let offered = if entries.len() == 1 {
                tool.clone()
            } else {
                format!("{server_name}__{tool}")
            };
            functions.push(json!({ "type": "function", "function": {
                "name": offered, "description": t["description"],
                "parameters": t["inputSchema"],
            } }));
            tools.push((
                offered,
                Tool {
                    server: i,
                    name: tool,
                },
            ));
        }
        // Claude Code puts servers' instructions in the system prompt; so does this loop.
        if !s.instructions.is_empty() {
            system += &format!(
                "\n\n# MCP server instructions\n\n## {server_name}\n{}",
                s.instructions
            );
        }
        servers.push(s);
    }
    emit(json!({
        "type": "system", "subtype": "init", "model": agent.model,
        "tools": tools.iter().map(|(n, _)| n).collect::<Vec<_>>(),
    }))?;

    let http = ureq::AgentBuilder::new()
        .timeout_read(Duration::from_secs(600))
        .build();
    let mut messages = vec![
        json!({ "role": "system", "content": system }),
        json!({ "role": "user", "content": prompt }),
    ];
    let mut usage = Usage::default();
    let mut run = Run::default();
    let mut error = None;
    let mut done = false;
    while !done {
        if run.turns >= MAX_TURNS {
            error = Some(format!("stopped after {MAX_TURNS} model calls"));
            break;
        }
        if start.elapsed() > agent.timeout {
            error = Some(format!("timed out after {} s", agent.timeout.as_secs()));
            break;
        }
        if usage.cost(cfg.prices) > agent.max_budget_usd {
            error = Some(format!("over the budget of {} USD", agent.max_budget_usd));
            break;
        }
        let mut body = json!({ "model": agent.model, "messages": messages });
        if !functions.is_empty() {
            body["tools"] = Json::Array(functions.clone());
        }
        if let Some(e) = &cfg.reasoning_effort {
            body["reasoning_effort"] = e.as_str().into();
        }
        let resp = match complete(&http, cfg, &body) {
            Ok(r) => r,
            Err(e) => {
                error = Some(e.to_string());
                break;
            }
        };
        run.turns += 1;
        let u = &resp["usage"];
        usage.input += u["prompt_tokens"].as_u64().unwrap_or(0);
        usage.cached += u["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap_or(0);
        usage.output += u["completion_tokens"].as_u64().unwrap_or(0);
        let msg = &resp["choices"][0]["message"];
        let text = msg["content"].as_str().unwrap_or_default().to_owned();
        let calls = msg["tool_calls"].as_array().cloned().unwrap_or_default();

        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        let mut assistant = Map::new();
        assistant.insert("role".into(), "assistant".into());
        assistant.insert("content".into(), msg["content"].clone());
        if !calls.is_empty() {
            assistant.insert("tool_calls".into(), Json::Array(calls.clone()));
        }
        messages.push(Json::Object(assistant));
        if calls.is_empty() {
            run.text = text;
            done = true;
        }
        let mut results = Vec::new();
        for c in &calls {
            let (id, f) = (&c["id"], &c["function"]);
            let fname = f["name"].as_str().unwrap_or_default();
            let raw = f["arguments"].as_str().unwrap_or("{}");
            let args: std::result::Result<Json, _> = serde_json::from_str(raw);
            content.push(json!({
                "type": "tool_use", "id": id, "name": fname,
                "input": args.as_ref().cloned().unwrap_or_else(|_| json!({ "__unparsed": raw })),
            }));
            let (out, is_error) = match (args, tools.iter().find(|(n, _)| n == fname)) {
                (Err(e), _) => (format!("the arguments are not valid JSON: {e}"), true),
                (_, None) => (format!("no such tool: {fname}"), true),
                (Ok(a), Some((_, t))) => servers[t.server]
                    .call(&t.name, a)
                    .unwrap_or_else(|e| (e.to_string(), true)),
            };
            run.tool_calls += 1;
            results.push(json!({
                "type": "tool_result", "tool_use_id": id, "content": out, "is_error": is_error,
            }));
            messages.push(json!({ "role": "tool", "tool_call_id": id, "content": out }));
        }
        emit(json!({ "type": "assistant", "message": { "content": content } }))?;
        if !results.is_empty() {
            emit(json!({ "type": "user", "message": { "content": results } }))?;
        }
    }
    drop(servers);

    run.cost_usd = usage.cost(cfg.prices);
    run.input_tokens = usage.input;
    run.output_tokens = usage.output;
    run.seconds = start.elapsed().as_secs_f64();
    emit(json!({
        "type": "result",
        "subtype": if error.is_some() { "error" } else { "success" },
        "is_error": error.is_some(),
        "result": error.clone().unwrap_or_else(|| run.text.clone()),
        "total_cost_usd": run.cost_usd, "num_turns": run.turns,
        "duration_ms": (run.seconds * 1000.0) as u64,
        "usage": {
            "input_tokens": usage.input.saturating_sub(usage.cached),
            "cache_read_input_tokens": usage.cached,
            "output_tokens": usage.output,
        },
    }))?;
    run.error = error;
    Ok(run)
}

/// One Chat Completions call, retried with backoff while the API is busy.
fn complete(http: &ureq::Agent, cfg: &OpenAi, body: &Json) -> Result<Json> {
    let url = format!("{}/chat/completions", cfg.base_url.trim_end_matches('/'));
    let mut wait = Duration::from_secs(2);
    for attempt in 1..=ATTEMPTS {
        let r = http
            .post(&url)
            .set("Authorization", &format!("Bearer {}", cfg.key))
            .send_json(body.clone());
        match r {
            Ok(resp) => return Ok(resp.into_json()?),
            Err(ureq::Error::Status(code, resp)) => {
                let after = resp
                    .header("retry-after")
                    .and_then(|s| s.parse::<u64>().ok())
                    .map(Duration::from_secs);
                let text = resp.into_string().unwrap_or_default();
                let busy = code == 429 && !text.contains("insufficient_quota") || code >= 500;
                if !busy || attempt == ATTEMPTS {
                    return Err(format!("HTTP {code}: {text}").into());
                }
                std::thread::sleep(after.unwrap_or(wait));
            }
            Err(_) if attempt < ATTEMPTS => std::thread::sleep(wait),
            Err(e) => return Err(e.into()),
        }
        wait *= 2;
    }
    Err("no attempts".into())
}

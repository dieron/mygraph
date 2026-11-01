//! Runs one agent turn: with Claude Code in print mode (`claude -p`: no settings files, hooks,
//! plugins or built-in tools, only the MCP servers given), or with this crate's own loop on the
//! OpenAI API ([`crate::openai`]). The transcript is kept as JSON lines.

use serde_json::{Value as Json, json};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub enum Backend {
    /// The `claude` executable.
    ClaudeCode(String),
    OpenAi(crate::openai::OpenAi),
}

pub struct Agent {
    pub backend: Backend,
    pub model: String,
    /// Spending limit per run, at list prices.
    pub max_budget_usd: f64,
    pub timeout: Duration,
}

/// What one run cost and produced.
#[derive(Default)]
pub struct Run {
    /// The agent's final reply.
    pub text: String,
    pub cost_usd: f64,
    pub turns: u64,
    pub tool_calls: u64,
    /// Input tokens, cached or not, and output tokens.
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub seconds: f64,
    pub error: Option<String>,
}

impl Run {
    pub fn to_json(&self) -> Json {
        json!({
            "cost_usd": self.cost_usd, "turns": self.turns, "tool_calls": self.tool_calls,
            "input_tokens": self.input_tokens, "output_tokens": self.output_tokens,
            "seconds": self.seconds, "error": self.error,
        })
    }
}

impl Agent {
    /// Runs `prompt` in `dir`, writing `<name>.system.md`, `<name>.mcp.json` and the transcript
    /// `<name>.jsonl` there. `mcp` is an `mcpServers` map; its tools are allowed without asking.
    pub fn run(
        &self,
        dir: &Path,
        name: &str,
        system: &str,
        prompt: &str,
        mcp: Option<&Json>,
    ) -> Result<Run> {
        let claude = match &self.backend {
            Backend::ClaudeCode(claude) => claude,
            Backend::OpenAi(cfg) => {
                return crate::openai::run(cfg, self, dir, name, system, prompt, mcp);
            }
        };
        let system_file = dir.join(format!("{name}.system.md"));
        fs::write(&system_file, system)?;
        let transcript = dir.join(format!("{name}.jsonl"));
        let mut cmd = Command::new(claude);
        cmd.current_dir(dir)
            .args(["-p", "--model", &self.model, "--setting-sources", ""])
            .args([
                "--strict-mcp-config",
                "--tools",
                "",
                "--no-session-persistence",
            ])
            .args(["--output-format", "stream-json", "--verbose"])
            .arg("--system-prompt-file")
            .arg(&system_file)
            .args(["--max-budget-usd", &self.max_budget_usd.to_string()]);
        if let Some(servers) = mcp {
            let mcp_file = dir.join(format!("{name}.mcp.json"));
            fs::write(&mcp_file, json!({ "mcpServers": servers }).to_string())?;
            cmd.arg("--mcp-config").arg(&mcp_file);
            let allowed: Vec<String> = servers
                .as_object()
                .into_iter()
                .flat_map(|m| m.keys())
                .map(|k| format!("mcp__{k}"))
                .collect();
            cmd.arg("--allowedTools").args(allowed);
        }
        cmd.stdin(Stdio::piped())
            .stdout(File::create(&transcript)?)
            .stderr(File::create(dir.join(format!("{name}.stderr")))?);
        let start = Instant::now();
        let mut child = cmd.spawn()?;
        child
            .stdin
            .take()
            .ok_or("no stdin")?
            .write_all(prompt.as_bytes())?;
        let mut timed_out = false;
        while child.try_wait()?.is_none() {
            if start.elapsed() > self.timeout {
                child.kill()?;
                timed_out = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        child.wait()?;
        let mut run = read_transcript(&transcript)?;
        run.seconds = start.elapsed().as_secs_f64();
        if timed_out {
            run.error = Some(format!("timed out after {} s", self.timeout.as_secs()));
        }
        Ok(run)
    }
}

/// Totals from a `stream-json` transcript: tool calls from the assistant messages, the rest from
/// the final `result` event.
pub fn read_transcript(path: &Path) -> Result<Run> {
    let mut run = Run {
        error: Some("no result event".into()),
        ..Run::default()
    };
    for line in BufReader::new(File::open(path)?).lines() {
        let Ok(ev) = serde_json::from_str::<Json>(&line?) else {
            continue;
        };
        match ev["type"].as_str() {
            Some("assistant") => {
                run.tool_calls += ev["message"]["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["type"] == "tool_use")
                    .count() as u64;
            }
            Some("result") => {
                let u = &ev["usage"];
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                run.text = ev["result"].as_str().unwrap_or_default().to_owned();
                run.cost_usd = ev["total_cost_usd"].as_f64().unwrap_or(0.0);
                run.turns = ev["num_turns"].as_u64().unwrap_or(0);
                run.seconds = ev["duration_ms"].as_f64().unwrap_or(0.0) / 1000.0;
                run.input_tokens = n("input_tokens")
                    + n("cache_creation_input_tokens")
                    + n("cache_read_input_tokens");
                run.output_tokens = n("output_tokens");
                run.error = (ev["is_error"] == true).then(|| {
                    // The subtype says "success" for API errors such as a usage limit; the
                    // result text says what happened.
                    ev["result"]
                        .as_str()
                        .filter(|t| !t.is_empty())
                        .or(ev["subtype"].as_str())
                        .unwrap_or("error")
                        .to_owned()
                });
            }
            _ => {}
        }
    }
    Ok(run)
}

/// The last JSON object in `text` that has key `key`, e.g. the `{"answer": …}` line of a reply.
pub fn last_json_with(text: &str, key: &str) -> Option<Json> {
    let pat = format!("{{\"{key}\"");
    text.rmatch_indices('{')
        .filter(|(i, _)| text[*i..].replace(' ', "").starts_with(&pat))
        .find_map(|(i, _)| {
            serde_json::Deserializer::from_str(&text[i..])
                .into_iter::<Json>()
                .next()?
                .ok()
                .filter(|v| v.get(key).is_some())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_answer_line() {
        let t = "Reasoning {x}.\n```json\n{ \"answer\": \"Paris\", \"evidence\": [\"e_1\"] }\n```";
        let v = last_json_with(t, "answer").unwrap();
        assert_eq!(v["answer"], "Paris");
        assert_eq!(v["evidence"][0], "e_1");
        assert!(last_json_with("no json", "answer").is_none());
    }
}

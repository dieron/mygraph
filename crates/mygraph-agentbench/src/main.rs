//! `mygraph-agentbench`: agent benchmarks with MyGraph as the agent's memory
//! (docs/11-benchmarks.md §3). The agent is Claude Code in print mode with only the MyGraph MCP
//! server; each question gets its own graph in a scratch folder.

mod agent;
mod grade;
mod hotpot;
mod mcp;
mod openai;

use agent::{Agent, Backend};
use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "mygraph-agentbench", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum)]
enum Provider {
    ClaudeCode,
    Openai,
}

#[derive(Subcommand)]
enum Cmd {
    /// HotpotQA, distractor setting: answers graded on exact match / F1 and citations graded on
    /// the supporting facts
    Hotpot {
        /// The `distractor/validation-00000-of-00001.parquet` file of `hotpotqa/hotpot_qa`
        #[arg(long)]
        data: PathBuf,
        /// Folder for transcripts, graphs and results; an interrupted run resumes from it
        #[arg(long)]
        out: PathBuf,
        #[arg(long, default_value_t = 50)]
        questions: usize,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// `graph` (store, then answer from the graph) and / or `direct` (passages in the prompt)
        #[arg(long, value_delimiter = ',', default_value = "graph,direct")]
        modes: Vec<String>,
        /// Questions run at the same time
        #[arg(long, default_value_t = 4)]
        parallel: usize,
        /// The `mygraph` executable that serves MCP
        #[arg(long)]
        mygraph: PathBuf,
        /// Who runs the agents: Claude Code (`claude -p`, your Claude login) or this crate's loop on
        /// the OpenAI API (key in `OPENAI_API_KEY`)
        #[arg(long, value_enum, default_value_t = Provider::ClaudeCode)]
        provider: Provider,
        #[arg(long, default_value = "claude")]
        claude: String,
        /// Default: `sonnet` with Claude Code; required with OpenAI
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value = "https://api.openai.com/v1")]
        openai_base_url: String,
        /// `reasoning_effort` for OpenAI reasoning models
        #[arg(long)]
        reasoning_effort: Option<String>,
        /// OpenAI prices in USD per million tokens, input / cached input / output (e.g.
        /// `1.25,0.125,10`), for costs and `--max-budget-usd`
        #[arg(long, value_delimiter = ',')]
        prices: Option<Vec<f64>>,
        /// Spending limit per agent run, at list prices
        #[arg(long, default_value_t = 3.0)]
        max_budget_usd: f64,
        /// Time limit per agent run, in minutes
        #[arg(long, default_value_t = 20)]
        timeout_minutes: u64,
        /// Grade the transcripts and graphs already in `--out` again, without running agents
        #[arg(long)]
        regrade: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Hotpot {
            data,
            out,
            questions,
            seed,
            modes,
            parallel,
            mygraph,
            provider,
            claude,
            model,
            openai_base_url,
            reasoning_effort,
            prices,
            max_budget_usd,
            timeout_minutes,
            regrade,
        } => (|| {
            let backend = match provider {
                Provider::ClaudeCode => Backend::ClaudeCode(claude),
                Provider::Openai => Backend::OpenAi(openai::OpenAi {
                    base_url: openai_base_url,
                    // A regrade reads transcripts and needs no key.
                    key: std::env::var("OPENAI_API_KEY")
                        .or_else(|e| if regrade { Ok(String::new()) } else { Err(e) })
                        .map_err(|_| "set OPENAI_API_KEY to use --provider openai")?,
                    reasoning_effort,
                    prices: match prices.as_deref() {
                        None => None,
                        Some(&[input, cached_input, output]) => Some(openai::Prices {
                            input,
                            cached_input,
                            output,
                        }),
                        Some(_) => return Err("--prices takes input,cached_input,output".into()),
                    },
                }),
            };
            let model = match (model, provider) {
                (Some(m), _) => m,
                (None, Provider::ClaudeCode) => "sonnet".to_owned(),
                (None, Provider::Openai) => return Err("--provider openai needs --model".into()),
            };
            hotpot::run(&hotpot::Options {
                data,
                out: std::path::absolute(&out)?,
                questions,
                seed,
                modes,
                parallel,
                mygraph: std::path::absolute(&mygraph)?,
                regrade,
                agent: Agent {
                    backend,
                    model,
                    max_budget_usd,
                    timeout: Duration::from_secs(timeout_minutes * 60),
                },
            })
        })(),
    };
    match result {
        Ok(summary) => println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        ),
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

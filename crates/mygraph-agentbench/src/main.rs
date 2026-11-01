//! `mygraph-agentbench`: agent benchmarks with MyGraph as the agent's memory
//! (docs/11-benchmarks.md §3). The agents are Claude Code in print mode, or this crate's own loop
//! on the OpenAI API, with only the MyGraph MCP server; each question gets its own graph in a
//! scratch folder.

mod agent;
mod grade;
mod mcp;
mod multihop;
mod openai;

use agent::{Agent, Backend};
use clap::{Args, Parser, Subcommand, ValueEnum};
use multihop::Dataset;
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
    /// HotpotQA, distractor setting: answers graded on exact match / F1, citations on the
    /// supporting sentences. `--data`: `distractor/validation-00000-of-00001.parquet` of
    /// `hotpotqa/hotpot_qa`
    Hotpot(RunArgs),
    /// MuSiQue-Ans: 2–4 hop questions over 20 paragraphs, citations graded on the supporting
    /// paragraphs. `--data`: `musique_ans_v1.0_dev.jsonl`
    Musique(RunArgs),
}

#[derive(Args)]
struct RunArgs {
    /// The dataset file
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
    /// Keep the graphs already in `--out` and run only the answering agents (and `direct`) again
    #[arg(long, conflicts_with = "regrade")]
    reanswer: bool,
}

fn options(dataset: Dataset, a: RunArgs) -> agent::Result<multihop::Options> {
    let backend = match a.provider {
        Provider::ClaudeCode => Backend::ClaudeCode(a.claude),
        Provider::Openai => Backend::OpenAi(openai::OpenAi {
            base_url: a.openai_base_url,
            // A regrade reads transcripts and needs no key.
            key: std::env::var("OPENAI_API_KEY")
                .or_else(|e| if a.regrade { Ok(String::new()) } else { Err(e) })
                .map_err(|_| "set OPENAI_API_KEY to use --provider openai")?,
            reasoning_effort: a.reasoning_effort,
            prices: match a.prices.as_deref() {
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
    let model = match (a.model, a.provider) {
        (Some(m), _) => m,
        (None, Provider::ClaudeCode) => "sonnet".to_owned(),
        (None, Provider::Openai) => return Err("--provider openai needs --model".into()),
    };
    Ok(multihop::Options {
        dataset,
        data: a.data,
        out: std::path::absolute(&a.out)?,
        questions: a.questions,
        seed: a.seed,
        modes: a.modes,
        parallel: a.parallel,
        mygraph: std::path::absolute(&a.mygraph)?,
        regrade: a.regrade,
        reanswer: a.reanswer,
        agent: Agent {
            backend,
            model,
            max_budget_usd: a.max_budget_usd,
            timeout: Duration::from_secs(a.timeout_minutes * 60),
        },
    })
}

fn main() {
    let (dataset, args) = match Cli::parse().cmd {
        Cmd::Hotpot(a) => (Dataset::Hotpot, a),
        Cmd::Musique(a) => (Dataset::Musique, a),
    };
    match options(dataset, args).and_then(|o| multihop::run(&o)) {
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

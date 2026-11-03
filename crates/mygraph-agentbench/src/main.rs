//! `mygraph-agentbench`: agent benchmarks with MyGraph as the agent's memory
//! (docs/11-benchmarks.md §3–4). The agents are Claude Code in print mode, or this crate's own loop
//! on the OpenAI API, with only the MyGraph MCP server; graphs live in the run folder.

mod agent;
mod grade;
mod graph;
mod locomo;
mod longmemeval;
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
    /// LoCoMo: long conversations recorded session by session, then questions about them, graded
    /// by a model and against the evidence messages. `--data`: `locomo10.json`
    Locomo(LocomoArgs),
    /// LongMemEval: a long chat history per question, stored by a script, then the question,
    /// graded by a model and against the messages that hold the answer. `--data`:
    /// `longmemeval_s_cleaned.json`
    Longmemeval(LongmemevalArgs),
}

#[derive(Args)]
struct LongmemevalArgs {
    #[command(flatten)]
    run: RunArgs,
    /// Question types to run (`multi-session,temporal-reasoning`); default all
    #[arg(long, value_delimiter = ',')]
    types: Vec<String>,
    /// The model that grades answers; default the agents' model
    #[arg(long)]
    judge_model: Option<String>,
    /// `reasoning_effort` of the grading model (OpenAI)
    #[arg(long, default_value = "low")]
    judge_reasoning_effort: String,
}

#[derive(Args)]
struct LocomoArgs {
    #[command(flatten)]
    run: RunArgs,
    /// Conversations to run (`conv-26,conv-30`); default all
    #[arg(long, value_delimiter = ',')]
    conversations: Vec<String>,
    /// The model that grades answers; default the agents' model
    #[arg(long)]
    judge_model: Option<String>,
    /// `reasoning_effort` of the grading model (OpenAI)
    #[arg(long, default_value = "low")]
    judge_reasoning_effort: String,
}

#[derive(Args)]
struct RunArgs {
    /// The dataset file
    #[arg(long)]
    data: PathBuf,
    /// Folder for transcripts, graphs and results; an interrupted run resumes from it
    #[arg(long)]
    out: PathBuf,
    /// Questions, sampled (default 50); LoCoMo: per conversation (default all); LongMemEval:
    /// spread over the question types (default all)
    #[arg(long)]
    questions: Option<usize>,
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

fn prices(a: &RunArgs) -> agent::Result<Option<openai::Prices>> {
    match a.prices.as_deref() {
        None => Ok(None),
        Some(&[input, cached_input, output]) => Ok(Some(openai::Prices {
            input,
            cached_input,
            output,
        })),
        Some(_) => Err("--prices takes input,cached_input,output".into()),
    }
}

/// An agent on `model` with the provider, limits and prices of `a`.
fn agent(a: &RunArgs, model: Option<&str>, effort: Option<String>) -> agent::Result<Agent> {
    let backend = match a.provider {
        Provider::ClaudeCode => Backend::ClaudeCode(a.claude.clone()),
        Provider::Openai => Backend::OpenAi(openai::OpenAi {
            base_url: a.openai_base_url.clone(),
            // A regrade reads transcripts and needs no key.
            key: std::env::var("OPENAI_API_KEY")
                .or_else(|e| if a.regrade { Ok(String::new()) } else { Err(e) })
                .map_err(|_| "set OPENAI_API_KEY to use --provider openai")?,
            reasoning_effort: effort,
            prices: prices(a)?,
        }),
    };
    let model = match (model, a.provider) {
        (Some(m), _) => m.to_owned(),
        (None, Provider::ClaudeCode) => "sonnet".to_owned(),
        (None, Provider::Openai) => return Err("--provider openai needs --model".into()),
    };
    Ok(Agent {
        backend,
        model,
        max_budget_usd: a.max_budget_usd,
        timeout: Duration::from_secs(a.timeout_minutes * 60),
    })
}

fn options(dataset: Dataset, a: RunArgs) -> agent::Result<multihop::Options> {
    Ok(multihop::Options {
        agent: agent(&a, a.model.as_deref(), a.reasoning_effort.clone())?,
        dataset,
        data: a.data,
        out: std::path::absolute(&a.out)?,
        questions: a.questions.unwrap_or(50),
        seed: a.seed,
        modes: a.modes,
        parallel: a.parallel,
        mygraph: std::path::absolute(&a.mygraph)?,
        regrade: a.regrade,
        reanswer: a.reanswer,
    })
}

/// The answering agent and the judge: the judge runs on `judge_model` (default the agents' model)
/// at `judge_effort`.
fn agent_and_judge(
    a: &RunArgs,
    judge_model: Option<String>,
    judge_effort: String,
) -> agent::Result<(Agent, Agent)> {
    let judge_model = judge_model.or_else(|| a.model.clone());
    let judge_effort = match a.provider {
        Provider::Openai => Some(judge_effort),
        Provider::ClaudeCode => None,
    };
    Ok((
        agent(a, a.model.as_deref(), a.reasoning_effort.clone())?,
        agent(a, judge_model.as_deref(), judge_effort)?,
    ))
}

fn locomo_options(l: LocomoArgs) -> agent::Result<locomo::Options> {
    let a = l.run;
    let (agent, judge) = agent_and_judge(&a, l.judge_model, l.judge_reasoning_effort)?;
    Ok(locomo::Options {
        agent,
        judge,
        data: a.data,
        out: std::path::absolute(&a.out)?,
        conversations: l.conversations,
        questions: a.questions,
        seed: a.seed,
        modes: a.modes,
        parallel: a.parallel,
        mygraph: std::path::absolute(&a.mygraph)?,
        regrade: a.regrade,
        reanswer: a.reanswer,
    })
}

fn longmemeval_options(l: LongmemevalArgs) -> agent::Result<longmemeval::Options> {
    let a = l.run;
    let (agent, judge) = agent_and_judge(&a, l.judge_model, l.judge_reasoning_effort)?;
    Ok(longmemeval::Options {
        agent,
        judge,
        data: a.data,
        out: std::path::absolute(&a.out)?,
        types: l.types,
        questions: a.questions,
        seed: a.seed,
        modes: a.modes,
        parallel: a.parallel,
        mygraph: std::path::absolute(&a.mygraph)?,
        regrade: a.regrade,
        reanswer: a.reanswer,
    })
}

fn main() {
    let result = match Cli::parse().cmd {
        Cmd::Hotpot(a) => options(Dataset::Hotpot, a).and_then(|o| multihop::run(&o)),
        Cmd::Musique(a) => options(Dataset::Musique, a).and_then(|o| multihop::run(&o)),
        Cmd::Locomo(l) => locomo_options(l).and_then(|o| locomo::run(&o)),
        Cmd::Longmemeval(l) => longmemeval_options(l).and_then(|o| longmemeval::run(&o)),
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

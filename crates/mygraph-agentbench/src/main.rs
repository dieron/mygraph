//! `mygraph-agentbench`: agent benchmarks with MyGraph as the agent's memory
//! (docs/11-benchmarks.md §3). The agent is Claude Code in print mode with only the MyGraph MCP
//! server; each question gets its own graph in a scratch folder.

mod agent;
mod grade;
mod hotpot;

use agent::Agent;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "mygraph-agentbench", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
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
        #[arg(long, default_value = "claude")]
        claude: String,
        #[arg(long, default_value = "sonnet")]
        model: String,
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
            claude,
            model,
            max_budget_usd,
            timeout_minutes,
            regrade,
        } => std::path::absolute(&mygraph)
            .and_then(|m| Ok((m, std::path::absolute(&out)?)))
            .map_err(Into::into)
            .and_then(|(mygraph, out)| {
                hotpot::run(&hotpot::Options {
                    data,
                    out,
                    questions,
                    seed,
                    modes,
                    parallel,
                    mygraph,
                    regrade,
                    agent: Agent {
                        claude,
                        model,
                        max_budget_usd,
                        timeout: Duration::from_secs(timeout_minutes * 60),
                    },
                })
            }),
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

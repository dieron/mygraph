//! The `mygraph` binary: `serve` runs the HTTP server (REST and admin API).

use clap::{Parser, Subcommand};
use mygraph_server::config::Config;
use mygraph_server::{App, serve, versions};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "mygraph",
    version,
    about = "Graph database with evidence, for AI agents"
)]
struct Cli {
    /// Configuration file (TOML). Defaults apply when omitted.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server.
    Serve {
        /// Override `listen` from the configuration, e.g. 127.0.0.1:7420.
        #[arg(long)]
        listen: Option<std::net::SocketAddr>,
        /// Override `data_dir` from the configuration.
        #[arg(long)]
        data_dir: Option<PathBuf>,
    },
    /// Print the SHA-256 of an API key, for `key_sha256` in the configuration.
    HashKey { key: String },
    /// Print the implemented spec and storage format versions.
    Versions,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut config = match Config::load(cli.config.as_deref()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("mygraph: {e}");
            return ExitCode::FAILURE;
        }
    };
    match cli.command {
        Command::Versions => {
            println!("{}", versions());
            ExitCode::SUCCESS
        }
        Command::HashKey { key } => {
            println!("{}", mygraph_core::store::sha256_hex(key.as_bytes()));
            ExitCode::SUCCESS
        }
        Command::Serve { listen, data_dir } => {
            if let Some(addr) = listen {
                config.listen = addr;
            }
            if let Some(dir) = data_dir {
                config.data_dir = dir;
            }
            if let Err(e) = config.validate() {
                eprintln!("mygraph: {e}");
                return ExitCode::FAILURE;
            }
            tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new(&config.log))
                .init();
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            match runtime.block_on(async { serve(App::new(config)).await }) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

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
    /// Serve MCP over stdio (for Claude Code and other MCP clients).
    Mcp {
        /// Running MyGraph server to forward tool calls to.
        #[arg(long, default_value = "http://127.0.0.1:7420")]
        url: String,
        /// API key for the server (default: the MYGRAPH_KEY environment variable).
        #[arg(long)]
        key: Option<String>,
        /// Open this data folder in this process instead of forwarding (only when no server
        /// uses it).
        #[arg(long)]
        data_dir: Option<PathBuf>,
        /// Graph used when a tool call names none.
        #[arg(long)]
        graph: Option<String>,
        /// Actor recorded for changes, with --data-dir.
        #[arg(long, default_value = "mcp")]
        actor: String,
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
        Command::Mcp {
            url,
            key,
            data_dir,
            graph,
            actor,
        } => {
            // stdout carries the protocol; logs go to stderr.
            tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(tracing_subscriber::EnvFilter::new(&config.log))
                .init();
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            match runtime.block_on(mcp_stdio(config, url, key, data_dir, graph, actor)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("mygraph mcp: {e}");
                    ExitCode::FAILURE
                }
            }
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

/// [MCP-STDIO-1] One JSON-RPC message per line on stdin and stdout.
async fn mcp_stdio(
    mut config: Config,
    url: String,
    key: Option<String>,
    data_dir: Option<PathBuf>,
    graph: Option<String>,
    actor: String,
) -> Result<(), String> {
    use mygraph_server::auth::Caller;
    use mygraph_server::config::Role;
    use mygraph_server::mcp::{Backend, handle};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut local = None;
    let backend = match data_dir {
        Some(dir) => {
            mygraph_core::ids::validate_actor_name(&actor).map_err(|r| format!("--actor: {r}"))?;
            config.data_dir = dir;
            if let Some(g) = graph {
                config.default_graph = g;
            }
            config.validate()?;
            let app = App::new(config);
            app.preload();
            mygraph_server::spawn_maintenance(&app);
            local = Some(app.clone());
            Backend::Local {
                app,
                caller: Caller {
                    actor,
                    role: Role::Admin,
                    graphs: None,
                },
            }
        }
        None => Backend::Remote {
            url,
            key: key.or_else(|| std::env::var("MYGRAPH_KEY").ok()),
            graph,
        },
    };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str(&line) {
            Ok(msg) => handle(&backend, msg).await,
            Err(_) => Some(serde_json::json!({
                "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" }
            })),
        };
        if let Some(r) = reply {
            let mut text = r.to_string();
            text.push('\n');
            out.write_all(text.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            out.flush().await.map_err(|e| e.to_string())?;
        }
    }
    if let Some(app) = local {
        let reg = app.registry.clone();
        let _ = tokio::task::spawn_blocking(move || reg.maintain()).await;
    }
    Ok(())
}

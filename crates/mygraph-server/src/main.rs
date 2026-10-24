//! The `mygraph` binary. M0: `serve` starts the HTTP listener with `/health`.

mod config;

use axum::{Json, Router, routing::get};
use clap::{Parser, Subcommand};
use config::Config;
use serde_json::{Value, json};
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
    },
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
        Command::Serve { listen } => {
            if let Some(addr) = listen {
                config.listen = addr;
            }
            init_tracing(&config.log);
            let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
            match runtime.block_on(serve(config)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    tracing::error!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}

fn init_tracing(filter: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(filter))
        .init();
}

fn versions() -> Value {
    json!({
        "server_version": env!("CARGO_PKG_VERSION"),
        "spec_version": mygraph_core::SPEC_VERSION,
        "storage_format_version": mygraph_core::STORAGE_FORMAT_VERSION,
    })
}

fn router() -> Router {
    Router::new().route("/health", get(health))
}

/// Liveness (docs/07-admin-api.md §5). No authentication.
async fn health() -> Json<Value> {
    let mut body = versions();
    body["status"] = json!("ok");
    Json(body)
}

async fn serve(config: Config) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    tracing::info!(
        listen = %listener.local_addr()?,
        data_dir = %config.data_dir.display(),
        "mygraph serving"
    );
    axum::serve(listener, router())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn health_reports_versions() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, router()).await.unwrap() });

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        stream
            .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();

        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let body = response.split("\r\n\r\n").nth(1).unwrap();
        let json: Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["spec_version"], mygraph_core::SPEC_VERSION);
        assert_eq!(json["storage_format_version"], 1);
    }

    #[test]
    fn default_config_is_local_only() {
        let c = Config::load(None).unwrap();
        assert!(c.listen.ip().is_loopback());
    }
}

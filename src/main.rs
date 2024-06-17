use anyhow::Result;
use axum::{routing::get, Router};
use clap::Parser;
use std::process::ExitCode;
use tokio::net::TcpListener;

#[cfg(feature = "cloudflare")]
pub mod cloudflare;

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct CommandLine {}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args = CommandLine::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let app = Router::new();
    // .route("/", get(crate::api::index));

    let listener = TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;
    Ok(ExitCode::SUCCESS)
}

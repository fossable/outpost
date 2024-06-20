use anyhow::{Context, Result};
use axum::{routing::get, Router};
use clap::Parser;
use config::ServiceConfig;
use outpost::PortMapping;
use std::{collections::HashMap, process::ExitCode};
use tokio::net::TcpListener;

pub mod config;

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

    // Load config
    let config: HashMap<String, ServiceConfig> = serde_json::from_str(
        &std::env::var("OUTPOST_CONFIG").context("get config from environment")?,
    )?;

    for (fqdn, service_config) in config.into_iter() {
        match service_config {
            #[cfg(feature = "cloudflare")]
            ServiceConfig::Cloudflare {
                service,
                cert_path,
                ports,
            } => {
                let ports: Vec<PortMapping> = PortMapping::from_vec(ports)?;
                tokio::spawn(async {
                    crate::cloudflare::CloudflareProxy::new(service, fqdn, ports)
                });
            }
        }
    }

    let app = Router::new();
    // .route("/", get(crate::api::index));

    let listener = TcpListener::bind("0.0.0.0:3000").await?;
    axum::serve(listener, app).await?;
    Ok(ExitCode::SUCCESS)
}

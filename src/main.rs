use anyhow::{bail, Result};
use clap::Parser;
use config::{CommandLine, ServiceConfig};
use std::process::ExitCode;
use tokio::net::TcpListener;

#[cfg(feature = "aws")]
use tokio::signal;
#[cfg(feature = "aws")]
use tracing::info;

pub mod api;
pub mod config;
pub mod wireguard;

#[cfg(feature = "cloudflare")]
pub mod cloudflare;

#[cfg(feature = "aws")]
pub mod aws;

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args = CommandLine::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    // Get config from command line or environment
    let service_config = match args.command {
        Some(config) => config,
        None => bail!("No configuration provided. Use a subcommand (cloudflare/aws) or set environment variables."),
    };

    match &service_config {
        #[cfg(feature = "cloudflare")]
        ServiceConfig::Cloudflare { origin_cert, .. } => {
            let ingress = service_config.ingress()?;
            let origin = service_config.origin()?;
            let origin_cert = origin_cert.clone();

            tokio::spawn(async move {
                crate::cloudflare::CloudflareProxy::new(
                    ingress.host,
                    origin.host,
                    origin.port,
                    origin_cert,
                )
                .await
                .unwrap()
                .process
                .wait()
                .await
                .unwrap();
            });
        }
        #[cfg(feature = "aws")]
        ServiceConfig::Aws { hosted_zone_id, instance_type, .. } => {
            let ingress = service_config.ingress()?;
            let origin = service_config.origin()?;
            let regions = service_config.aws_regions().unwrap_or_default();
            let hosted_zone_id = hosted_zone_id.clone();
            let instance_type = instance_type.clone();
            let region = regions.first().cloned().unwrap_or_default();

            // Generate WireGuard keys for both sides
            let wg_keys = crate::wireguard::WireGuardPair::generate().await?;

            // Get the public IP of this machine (origin)
            info!("Detecting origin IP address...");
            let origin_ip = reqwest::get("https://api.ipify.org").await?.text().await?;

            info!("Origin IP: {}", origin_ip);

            // Deploy the AWS proxy
            let proxy = crate::aws::AwsProxy::deploy(
                ingress.host.clone(),
                ingress.port,
                ingress.protocol.clone(),
                origin.host,
                origin.port,
                origin_ip,
                regions,
                instance_type.clone(),
                wg_keys.proxy.private_key,
                wg_keys.proxy.public_key.clone(),
                wg_keys.origin.public_key.clone(),
                wg_keys.origin.preshared_key.clone(),
                hosted_zone_id,
            )
            .await?;

            // Wait for stack to be completely deployed and get the proxy IP
            info!("Waiting for CloudFormation stack to complete deployment...");
            let proxy_ip = proxy.wait_for_completion().await?;
            let proxy_endpoint = format!("{}:51820", proxy_ip);

            info!("Setting up WireGuard tunnel to proxy at {}", proxy_endpoint);

            // Set up WireGuard tunnel on the origin side using boringtun
            let tunnel = crate::wireguard::OriginTunnel::setup(
                wg_keys.origin,
                wg_keys.proxy.public_key,
                proxy_endpoint,
            )
            .await?;

            info!("AWS proxy deployment complete");

            // Set up app state for UI
            let state = crate::api::AppState::default();

            // Store proxy info in state
            {
                let mut proxy_info = state.proxy_info.write().await;
                *proxy_info = Some(crate::api::ProxyInfo::Aws {
                    instance_id: "".to_string(), // Will be fetched later
                    instance_type,
                    region,
                    public_ip: proxy_ip.clone(),
                    private_ip: "172.17.0.1".to_string(),
                    state: "running".to_string(),
                    launch_time: "".to_string(), // Will be fetched later
                });
            }

            // Mark tunnel as up
            {
                let mut stats = state.stats.write().await;
                stats.tunnel_up = true;
            }

            // Set up graceful shutdown handler
            let shutdown_signal = async {
                let ctrl_c = async {
                    signal::ctrl_c()
                        .await
                        .expect("failed to install Ctrl+C handler");
                };

                #[cfg(unix)]
                let terminate = async {
                    signal::unix::signal(signal::unix::SignalKind::terminate())
                        .expect("failed to install SIGTERM handler")
                        .recv()
                        .await;
                };

                #[cfg(not(unix))]
                let terminate = std::future::pending::<()>();

                tokio::select! {
                    _ = ctrl_c => {
                        info!("Received Ctrl+C signal");
                    }
                    _ = terminate => {
                        info!("Received SIGTERM signal");
                    }
                }
            };

            let app = crate::api::router(state);

            let listener = TcpListener::bind("0.0.0.0:3000").await?;

            // Run server with graceful shutdown
            info!("Dashboard available at http://0.0.0.0:3000");
            axum::serve(listener, app)
                .with_graceful_shutdown(shutdown_signal)
                .await?;

            // Clean up AWS resources on graceful shutdown
            info!("Shutting down gracefully...");
            if let Err(e) = proxy.cleanup().await {
                tracing::warn!("Failed to cleanup AWS proxy: {}", e);
            }

            // Tunnel will be dropped here automatically
            drop(tunnel);

            return Ok(ExitCode::SUCCESS);
        }
    }

    // Fallback for non-AWS services (e.g., Cloudflare) - just serve the UI
    #[allow(unreachable_code)]
    {
        let state = crate::api::AppState::default();
        let app = crate::api::router(state);

        let listener = TcpListener::bind("0.0.0.0:3000").await?;
        axum::serve(listener, app).await?;
    }
    Ok(ExitCode::SUCCESS)
}

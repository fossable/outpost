#![recursion_limit = "512"]

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
        ServiceConfig::Aws {
            hosted_zone_id,
            instance_type,
            debug,
            use_cloudfront,
            ..
        } => {
            // Validate CloudFront configuration
            service_config.validate_cloudfront()?;

            let ingress = service_config.ingress()?;
            let origin = service_config.origin()?;
            let regions = service_config.aws_regions().unwrap_or_default();
            let hosted_zone_id = hosted_zone_id.clone();
            let instance_type = instance_type.clone();
            let debug = *debug;
            let use_cloudfront = *use_cloudfront;

            // Set up graceful shutdown handler early using a broadcast channel
            let (shutdown_tx, mut shutdown_rx) = tokio::sync::broadcast::channel::<()>(1);

            // Spawn a task to listen for shutdown signals
            tokio::spawn(async move {
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

                // Broadcast shutdown to all subscribers
                let _ = shutdown_tx.send(());
            });

            // Generate WireGuard keys for both sides
            let wg_keys = crate::wireguard::WireGuardPair::generate().await?;

            // Determine available WireGuard tunnel IPs before deployment
            info!("Finding available IP range for WireGuard tunnel...");
            let (wg_proxy_ip, wg_origin_ip) = crate::wireguard::find_available_subnet().await?;

            // Get the public IP of this machine (origin)
            info!("Detecting origin IP address...");
            let origin_ip = tokio::select! {
                result = async {
                    reqwest::get("https://api.ipify.org").await?.text().await
                } => result?,
                _ = shutdown_rx.recv() => {
                    info!("Shutdown signal received during IP detection");
                    return Ok(ExitCode::SUCCESS);
                }
            };

            info!("Origin IP: {}", origin_ip);

            // Deploy the AWS proxy
            let mut shutdown_rx2 = shutdown_rx.resubscribe();
            let mut proxy = tokio::select! {
                result = crate::aws::AwsProxy::deploy(
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
                    debug,
                    use_cloudfront,
                    wg_proxy_ip.clone(),
                    wg_origin_ip.clone(),
                ) => result?,
                _ = shutdown_rx2.recv() => {
                    info!("Shutdown signal received during deployment");
                    return Ok(ExitCode::SUCCESS);
                }
            };

            // Wait for stack to be completely deployed and get the proxy IP
            info!("Waiting for CloudFormation stack to complete deployment...");
            let mut shutdown_rx3 = shutdown_rx.resubscribe();
            let proxy_ip = tokio::select! {
                result = proxy.wait_for_completion() => result?,
                _ = shutdown_rx3.recv() => {
                    info!("Shutdown signal received during stack creation");
                    info!("Cleaning up AWS resources...");
                    if let Err(e) = proxy.cleanup().await {
                        tracing::warn!("Failed to cleanup AWS proxy: {}", e);
                    }
                    return Ok(ExitCode::SUCCESS);
                }
            };
            let proxy_endpoint = format!("{}:51820", proxy_ip);

            info!("Setting up WireGuard tunnel to proxy at {}", proxy_endpoint);

            // Set up WireGuard tunnel on the origin side using boringtun
            let mut shutdown_rx4 = shutdown_rx.resubscribe();
            let tunnel = tokio::select! {
                result = crate::wireguard::OriginTunnel::setup(
                    wg_keys.origin,
                    wg_keys.proxy.public_key,
                    proxy_endpoint,
                    wg_proxy_ip,
                    wg_origin_ip,
                ) => result?,
                _ = shutdown_rx4.recv() => {
                    info!("Shutdown signal received during tunnel setup");
                    info!("Cleaning up AWS resources...");
                    if let Err(e) = proxy.cleanup().await {
                        tracing::warn!("Failed to cleanup AWS proxy: {}", e);
                    }
                    return Ok(ExitCode::SUCCESS);
                }
            };

            info!("AWS proxy deployment complete");

            // Set up app state for UI
            let state = crate::api::AppState::default();

            // Store proxy info in state
            {
                let mut proxy_info = state.proxy_info.write().await;
                *proxy_info = Some(crate::api::ProxyInfo::Aws {
                    instance_id: proxy.instance_id.clone(),
                    instance_type,
                    region: proxy.region.clone(),
                    public_ip: proxy_ip.clone(),
                    private_ip: tunnel.proxy_ip.clone(),
                    state: "running".to_string(),
                    launch_time: proxy.launch_time.clone(),
                    uptime: String::new(), // Will be calculated dynamically in the dashboard
                });
            }

            // CloudFront info will be available in CloudFormation outputs if enabled
            // The distribution is managed by CloudFormation, not directly
            if use_cloudfront {
                info!("CloudFront distribution is being created by CloudFormation");
                info!("Distribution will be ready in approximately 15-20 minutes");
            }

            // Mark tunnel as up
            {
                let mut stats = state.stats.write().await;
                stats.tunnel_up = true;
            }

            let app = crate::api::router(state);

            let listener = TcpListener::bind("0.0.0.0:3000").await?;

            // Run server with graceful shutdown
            info!("Dashboard available at http://0.0.0.0:3000");
            let mut shutdown_rx5 = shutdown_rx.resubscribe();
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_rx5.recv().await;
                })
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

#![recursion_limit = "512"]

use anyhow::{bail, Context, Result};
use clap::Parser;
use config::{CommandLine, ServiceConfig};
use std::process::ExitCode;
use tokio::process::Command;
use tracing::level_filters::LevelFilter;

#[cfg(feature = "dashboard")]
use tokio::net::TcpListener;

#[cfg(feature = "aws")]
use tokio::signal;
#[cfg(feature = "aws")]
use tracing::info;

/// Check if the process has NET_ADMIN capability (required for WireGuard and iptables)
async fn check_net_admin() -> Result<bool> {
    // Try to check capabilities by reading /proc/self/status
    if let Ok(status) = tokio::fs::read_to_string("/proc/self/status").await {
        for line in status.lines() {
            if line.starts_with("CapEff:") {
                // Extract the effective capabilities hex value
                if let Some(cap_hex) = line.split_whitespace().nth(1) {
                    if let Ok(caps) = u64::from_str_radix(cap_hex, 16) {
                        // NET_ADMIN is bit 12 (0x1000)
                        const CAP_NET_ADMIN: u64 = 1 << 12;
                        return Ok((caps & CAP_NET_ADMIN) != 0);
                    }
                }
            }
        }
    }

    // Fallback: Try to execute a harmless iptables command to check if we have permission
    match Command::new("iptables").arg("-L").arg("-n").output().await {
        Ok(output) => Ok(output.status.success()),
        Err(_) => Ok(false),
    }
}

/// Check if a kernel module is loaded by reading /proc/modules
async fn is_kernel_module_loaded(module_name: &str) -> Result<bool> {
    let modules_content = tokio::fs::read_to_string("/proc/modules")
        .await
        .context("Failed to read /proc/modules")?;

    Ok(modules_content.lines().any(|line| line.starts_with(module_name)))
}

/// Check for required kernel modules when bandwidth limits are enabled
async fn check_bandwidth_limit_requirements(
    upload_limit: Option<u32>,
    download_limit: Option<u32>,
) -> Result<()> {
    use tracing::{info, error};

    let mut missing_modules = Vec::new();

    // HTB scheduler is required for both upload and download limits
    if !is_kernel_module_loaded("sch_htb").await? {
        missing_modules.push("sch_htb");
    }

    // IFB and act_mirred are required for download limiting (ingress shaping)
    if download_limit.is_some() {
        if !is_kernel_module_loaded("ifb").await? {
            missing_modules.push("ifb");
        }
        if !is_kernel_module_loaded("act_mirred").await? {
            missing_modules.push("act_mirred");
        }
    }

    if !missing_modules.is_empty() {
        error!("Bandwidth limiting is enabled, but required kernel modules are not loaded:");
        for module in &missing_modules {
            error!("  - {}", module);
        }
        error!("");
        error!("Please load the required modules:");
        error!("  sudo modprobe {}", missing_modules.join(" "));
        error!("");
        error!("To load them automatically on boot, add to /etc/modules-load.d/outpost.conf:");
        for module in &missing_modules {
            error!("  {}", module);
        }

        bail!(
            "Required kernel modules are not loaded: {}. \
            Bandwidth limiting requires these modules to be loaded before starting.",
            missing_modules.join(", ")
        );
    }

    if upload_limit.is_some() || download_limit.is_some() {
        info!("Bandwidth limiting kernel modules verified:");
        if upload_limit.is_some() {
            info!("  - Upload limit: {} Mbps", upload_limit.unwrap());
        }
        if download_limit.is_some() {
            info!("  - Download limit: {} Mbps", download_limit.unwrap());
        }
    }

    Ok(())
}

#[cfg(feature = "dashboard")]
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
        .with_env_filter(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(LevelFilter::INFO.into())
                .from_env_lossy(),
        )
        .init();

    // Validate bandwidth limits
    use validator::Validate;
    args.validate()
        .map_err(|e| anyhow::anyhow!("Validation failed: {}", e))?;

    // Get config from command line or environment
    let service_config = match args.command {
        Some(config) => config,
        None => bail!("No configuration provided. Use a subcommand (cloudflare/aws) or set environment variables."),
    };

    // Extract bandwidth limits
    let upload_limit = args.upload_limit;
    let download_limit = args.download_limit;

    // Check for required kernel modules if bandwidth limits are enabled
    if upload_limit.is_some() || download_limit.is_some() {
        check_bandwidth_limit_requirements(upload_limit, download_limit).await?;
    }

    match &service_config {
        #[cfg(feature = "cloudflare")]
        ServiceConfig::Cloudflare { origin_cert, .. } => {
            let ingress = service_config.ingress()?;
            let origin = service_config.origin()?;
            let origin_cert = origin_cert.clone();
            let origin_host = origin.host.clone();
            let origin_port = origin.port().expect("Origin must have a port for Cloudflare");

            tokio::spawn(async move {
                crate::cloudflare::CloudflareProxy::new(
                    ingress.host,
                    origin_host,
                    origin_port,
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
            enable_tls,
            acme_email,
            ..
        } => {
            // Validate all configurations using validator crate
            service_config.validate_all()?;

            // Check for NET_ADMIN capability (required for WireGuard and iptables)
            info!("Checking for NET_ADMIN capability...");
            match check_net_admin().await {
                Ok(true) => {
                    info!("NET_ADMIN capability detected");
                }
                Ok(false) => {
                    bail!(
                        "NET_ADMIN capability is required but not available.\n\
                        If running in Docker, add: --cap-add=NET_ADMIN\n\
                        If running in docker-compose, add to your service:\n\
                          cap_add:\n\
                            - NET_ADMIN"
                    );
                }
                Err(e) => {
                    tracing::warn!("Could not verify NET_ADMIN capability: {}", e);
                    tracing::warn!("Proceeding anyway, but WireGuard setup may fail");
                }
            }

            let ingresses = service_config.ingresses()?;
            let origin = service_config.origin()?;

            // Build port mappings: (port, protocol)
            let port_mappings: Vec<(u16, String)> = ingresses
                .iter()
                .map(|ing| {
                    let port = if let Some(origin_port) = origin.port {
                        // Single ingress: use origin's specified port
                        origin_port
                    } else {
                        // Multiple ingress: use ingress port
                        ing.port().expect("Ingress must have a port")
                    };
                    (port, ing.protocol.as_str().to_string())
                })
                .collect();

            info!("Port mappings: {:?}", port_mappings);

            // Use first ingress for deployment (backwards compat)
            let ingress = &ingresses[0];
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
                    ingress.port()?,
                    ingress.protocol.as_str().to_string(),
                    origin.host.clone(),
                    origin.port.unwrap_or_else(|| ingress.port().expect("Ingress must have a port")),
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
                    port_mappings.clone(),
                    *enable_tls,
                    acme_email.clone(),
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

            // Clone origin info before moving into tunnel setup
            let origin_host = origin.host.clone();

            // Set up WireGuard tunnel on the origin side with port mappings
            let mut shutdown_rx4 = shutdown_rx.resubscribe();
            #[cfg_attr(not(feature = "dashboard"), allow(unused_variables))]
            let tunnel = tokio::select! {
                result = crate::wireguard::OriginTunnel::setup(
                    wg_keys.origin,
                    wg_keys.proxy.public_key,
                    proxy_endpoint,
                    wg_proxy_ip,
                    wg_origin_ip,
                    origin_host,
                    port_mappings,
                    upload_limit,
                    download_limit,
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

            #[cfg(feature = "dashboard")]
            {
                // Set up app state for UI
                let state = crate::api::AppState {
                    upload_limit,
                    download_limit,
                    ..Default::default()
                };

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

                // Store tunnel in Arc for sharing with API handlers
                let tunnel = std::sync::Arc::new(tunnel);
                {
                    let mut tunnel_ref = state.tunnel.write().await;
                    *tunnel_ref = Some(tunnel.clone());
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

                // Tunnel will be dropped here automatically when Arc refcount reaches 0
                drop(tunnel);
            }

            #[cfg(not(feature = "dashboard"))]
            {
                // Without dashboard, just wait for shutdown signal
                info!("Tunnel is running. Press Ctrl+C to shutdown.");
                let mut shutdown_rx5 = shutdown_rx.resubscribe();
                let _ = shutdown_rx5.recv().await;

                // Clean up AWS resources on graceful shutdown
                info!("Shutting down gracefully...");
                if let Err(e) = proxy.cleanup().await {
                    tracing::warn!("Failed to cleanup AWS proxy: {}", e);
                }
            }

            return Ok(ExitCode::SUCCESS);
        }
    }

    // Fallback for non-AWS services (e.g., Cloudflare) - just serve the UI
    #[cfg(feature = "dashboard")]
    #[allow(unreachable_code)]
    {
        let state = crate::api::AppState {
            upload_limit,
            download_limit,
            ..Default::default()
        };
        let app = crate::api::router(state);

        let listener = TcpListener::bind("0.0.0.0:3000").await?;
        axum::serve(listener, app).await?;
    }

    #[cfg(not(feature = "dashboard"))]
    #[allow(unreachable_code)]
    {
        // Without dashboard, just wait forever (Cloudflare proxy is already running in background)
        std::future::pending::<()>().await;
    }

    Ok(ExitCode::SUCCESS)
}

use anyhow::Result;
use outpost::PortMapping;
use serde::Serialize;
use std::{
    process::{Child, Command},
    time::Duration,
};
use tempfile::TempDir;
use tracing::{debug, info, instrument};
use uuid::Uuid;

#[derive(Debug, Serialize)]
pub struct CloudflareConfigIngress {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub service: String,
}

#[derive(Debug, Serialize)]
pub struct CloudflareConfig {
    pub tunnel: String,
    #[serde(rename = "credentials-file")]
    pub credentials_file: String,

    pub ingress: Vec<CloudflareConfigIngress>,
}

#[derive(Debug)]
pub struct CloudflareProxy {
    temp: TempDir,
    /// The cloudflared child process which actually handles the routing
    pub process: Child,
}

impl CloudflareProxy {
    #[instrument(ret)]
    pub fn new(service: String, fqdn: String, ports: Vec<PortMapping>) -> Result<Self> {
        let temp = TempDir::new()?;

        // Generate config
        let mut config = CloudflareConfig {
            tunnel: Uuid::new_v4().to_string(),
            credentials_file: temp
                .path()
                .join(format!("{}.json", &fqdn))
                .to_string_lossy()
                .to_string(),
            ingress: vec![
                // This one is always required to be last
                CloudflareConfigIngress {
                    hostname: None,
                    service: "http_status:404".into(),
                },
            ],
        };

        for port in ports {
            config.ingress.insert(
                0,
                CloudflareConfigIngress {
                    hostname: Some(fqdn.clone()),
                    service: format!("http://{}:{}", &service, port.local),
                },
            )
        }

        debug!(config = ?config, "Generated cloudflared config");

        // Write config
        let config_path = temp.path().join("config.yml");
        std::fs::write(&config_path, serde_yaml::to_string(&config)?)?;

        // Make sure the tunnel doesn't already exist
        if Command::new("cloudflared")
            .arg("tunnel")
            .arg("--config")
            .arg(config_path.to_string_lossy().to_string())
            .arg("delete")
            .arg(&service)
            .spawn()?
            .wait()?
            .success()
        {
            debug!("Deleted existing tunnel successfully");
        }

        // Create tunnel
        assert!(Command::new("cloudflared")
            .arg("tunnel")
            .arg("--config")
            .arg(config_path.to_string_lossy().to_string())
            .arg("create")
            .arg(&service)
            .spawn()?
            .wait()?
            .success());

        // Update DNS record
        assert!(Command::new("cloudflared")
            .arg("tunnel")
            .arg("--config")
            .arg(config_path.to_string_lossy().to_string())
            .arg("route")
            .arg("dns")
            .arg("--overwrite-dns")
            .arg(&service)
            .arg(fqdn)
            .spawn()?
            .wait()?
            .success());

        info!("Starting cloudflare tunnel");
        Ok(Self {
            temp,
            process: Command::new("cloudflared")
                // Try to not cede any more control to cloudflare
                .arg("--no-autoupdate")
                // .arg("--management-diagnostics")
                // .arg("false")
                .arg("tunnel")
                .arg("--config")
                .arg(config_path.to_string_lossy().to_string())
                .arg("run")
                .arg(&service)
                .spawn()?,
        })
    }
}

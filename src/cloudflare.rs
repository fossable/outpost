use anyhow::Result;
use outpost::PortMapping;
use serde::Serialize;
use std::process::{Child, Command};
use tracing::{debug, instrument};

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
    /// The cloudflared child process which actually handles the routing
    pub process: Child,
}

impl CloudflareProxy {
    #[instrument(ret)]
    pub fn new(hostname: String, ports: Vec<PortMapping>) -> Result<Self> {
        let temp = tempfile::TempDir::new()?;

        // Generate config
        let config = CloudflareConfig {
            tunnel: hostname.clone(),
            credentials_file: temp
                .path()
                .join(format!("{}.json", &hostname))
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

        debug!(config = ?config, "Generated cloudflared config");

        // Write config
        let config_path = temp.path().join("config.yml");
        std::fs::write(&config_path, serde_yaml::to_string(&config)?)?;

        Ok(Self {
            process: Command::new("cloudflared")
                // Try to not cede any more control to cloudflare
                .arg("--no-autoupdate")
                .arg("--management-diagnostics")
                .arg("false")
                .arg("tunnel")
                .arg("--config")
                .arg(config_path.to_string_lossy().to_string())
                .arg("run")
                .arg(&hostname)
                .spawn()?,
        })
    }
}

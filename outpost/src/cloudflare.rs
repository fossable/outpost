use anyhow::Result;
use serde::Serialize;
use std::process::{Child, Command};
use tracing::instrument;

#[derive(Serialize)]
pub struct CloudflareConfigIngress {
    pub hostname: String,
    pub service: String,
}

#[derive(Serialize)]
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
    pub fn new() -> Result<Self> {
        let temp = tempfile::TempDir::new()?;

        // Generate config
        let config = CloudflareConfig {
            tunnel: "generated".into(),
            credentials_file: temp
                .path()
                .join("tunnel.json")
                .to_string_lossy()
                .to_string(),
            ingress: vec![],
        };

        // Write config
        let config_path = temp.path().join("config.yml");
        std::fs::write(&config_path, serde_yaml::to_string(&config)?)?;

        Ok(Self {
            process: Command::new("cloudflared")
                .arg("tunnel")
                .arg("run")
                .arg("--config")
                .arg(config_path.to_string_lossy().to_string())
                // Try to not cede any more control to cloudflare
                .arg("--no-autoupdate")
                .arg("--management-diagnostics")
                .arg("false")
                .spawn()?,
        })
    }
}

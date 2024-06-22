use anyhow::Result;
use outpost::PortMapping;
use serde::Serialize;
use tempfile::TempDir;
use tokio::process::{Child, Command};
use tracing::{debug, info, instrument};

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
    _temp: TempDir,
    /// The cloudflared child process which actually handles the routing
    pub process: Child,
}

impl Drop for CloudflareProxy {
    fn drop(&mut self) {
        debug!("Stopping cloudflare tunnel");
        futures::executor::block_on(self.process.kill()).unwrap();
    }
}

impl CloudflareProxy {
    #[instrument(ret)]
    pub async fn new(service: String, fqdn: String, ports: Vec<PortMapping>) -> Result<Self> {
        let temp = TempDir::new()?;

        // Make sure the tunnel doesn't already exist
        if Command::new("cloudflared")
            .arg("tunnel")
            .arg("delete")
            .arg(&service)
            .spawn()?
            .wait()
            .await?
            .success()
        {
            debug!("Deleted existing tunnel successfully");
        }

        // Create tunnel
        assert!(Command::new("cloudflared")
            .arg("tunnel")
            .arg("create")
            .arg(&service)
            .spawn()?
            .wait()
            .await?
            .success());

        // Update DNS record
        assert!(Command::new("cloudflared")
            .arg("tunnel")
            .arg("route")
            .arg("dns")
            .arg("--overwrite-dns")
            .arg(&service)
            .arg(&fqdn)
            .spawn()?
            .wait()
            .await?
            .success());

        // Generate config
        let mut config = CloudflareConfig {
            tunnel: "".to_string(),
            credentials_file: "".to_string(),
            ingress: vec![
                // This one is always required to be last
                CloudflareConfigIngress {
                    hostname: None,
                    service: "http_status:404".into(),
                },
            ],
        };

        // Find tunnel secret file rather than parsing command output
        for entry in std::fs::read_dir("/root/.cloudflared")? {
            let entry = entry?;

            if entry
                .file_name()
                .to_string_lossy()
                .to_owned()
                .ends_with(".json")
            {
                config.tunnel = entry
                    .path()
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .to_string();
                config.credentials_file = entry.path().to_string_lossy().to_string();
            }
        }

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

        info!("Starting cloudflare tunnel");
        Ok(Self {
            _temp: temp,
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

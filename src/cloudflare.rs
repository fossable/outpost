use anyhow::Result;
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
        // Note: We can't use async in Drop, so we use blocking. This is not ideal but necessary.
        // Ignore errors during cleanup - process may have already exited.
        let _ = futures::executor::block_on(self.process.kill());
    }
}

impl CloudflareProxy {
    #[instrument(ret)]
    pub async fn new(
        fqdn: String,
        origin_host: String,
        origin_port: u16,
        origin_cert: String,
    ) -> Result<Self> {
        let temp = TempDir::new()?;

        // Write origin cert
        std::fs::write(temp.path().join("cert.pem"), &origin_cert)?;

        // Use the FQDN as the tunnel name
        let tunnel_name = &fqdn;

        // Make sure the tunnel doesn't already exist
        if Command::new("cloudflared")
            .arg("tunnel")
            .arg("--origincert")
            .arg(temp.path().join("cert.pem"))
            .arg("delete")
            .arg(tunnel_name)
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
            .arg("--origincert")
            .arg(temp.path().join("cert.pem"))
            .arg("create")
            .arg(tunnel_name)
            .spawn()?
            .wait()
            .await?
            .success());

        // Update DNS record
        assert!(Command::new("cloudflared")
            .arg("tunnel")
            .arg("--origincert")
            .arg(temp.path().join("cert.pem"))
            .arg("route")
            .arg("dns")
            .arg("--overwrite-dns")
            .arg(tunnel_name)
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
                CloudflareConfigIngress {
                    hostname: Some(fqdn.clone()),
                    service: format!("http://{}:{}", origin_host, origin_port),
                },
                // This one is always required to be last
                CloudflareConfigIngress {
                    hostname: None,
                    service: "http_status:404".into(),
                },
            ],
        };

        // Find tunnel secret file rather than parsing command output
        for entry in std::fs::read_dir(&temp)? {
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

        debug!(config = ?config, "Generated cloudflared config");

        // Write config
        let config_path = temp.path().join("config.yml");
        std::fs::write(&config_path, serde_yaml::to_string(&config)?)?;

        info!("Starting cloudflare tunnel");
        Ok(Self {
            process: Command::new("cloudflared")
                // Try to not cede any more control to cloudflare
                .arg("--no-autoupdate")
                // .arg("--management-diagnostics")
                // .arg("false")
                .arg("tunnel")
                .arg("--origincert")
                .arg(temp.path().join("cert.pem"))
                .arg("--config")
                .arg(config_path.to_string_lossy().to_string())
                .arg("run")
                .arg(tunnel_name)
                .spawn()?,
            _temp: temp,
        })
    }
}

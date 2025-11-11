use anyhow::{bail, Context, Result};
use std::fs;
use tempfile::TempDir;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

#[derive(Debug, Clone)]
pub struct WireGuardKeys {
    pub private_key: String,
    pub public_key: String,
    pub preshared_key: String,
}

impl WireGuardKeys {
    /// Generate a new set of WireGuard keys using the wg command-line tool
    pub async fn generate() -> Result<Self> {
        // Generate private key using wg genkey
        let private_key_output = Command::new("wg")
            .arg("genkey")
            .output()
            .await
            .context("Failed to run 'wg genkey'. Make sure wireguard-tools is installed.")?;

        if !private_key_output.status.success() {
            bail!("wg genkey failed");
        }

        let private_key = String::from_utf8(private_key_output.stdout)
            .context("Invalid UTF-8 from wg genkey")?
            .trim()
            .to_string();

        // Derive public key from private key using wg pubkey
        let public_key_output = Command::new("sh")
            .arg("-c")
            .arg(format!("echo '{}' | wg pubkey", private_key))
            .output()
            .await
            .context("Failed to derive public key")?;

        if !public_key_output.status.success() {
            bail!("wg pubkey failed");
        }

        let public_key = String::from_utf8(public_key_output.stdout)
            .context("Invalid UTF-8 from wg pubkey")?
            .trim()
            .to_string();

        // Generate preshared key using wg genpsk
        let preshared_output = Command::new("wg")
            .arg("genpsk")
            .output()
            .await
            .context("Failed to run 'wg genpsk'")?;

        if !preshared_output.status.success() {
            bail!("wg genpsk failed");
        }

        let preshared_key = String::from_utf8(preshared_output.stdout)
            .context("Invalid UTF-8 from wg genpsk")?
            .trim()
            .to_string();

        Ok(Self {
            private_key,
            public_key,
            preshared_key,
        })
    }
}

#[derive(Debug)]
pub struct WireGuardPair {
    pub origin: WireGuardKeys,
    pub proxy: WireGuardKeys,
}

impl WireGuardPair {
    /// Generate a pair of WireGuard keys (one for origin, one for proxy)
    pub async fn generate() -> Result<Self> {
        Ok(Self {
            origin: WireGuardKeys::generate().await?,
            proxy: WireGuardKeys::generate().await?,
        })
    }
}

pub struct OriginTunnel {
    _temp: TempDir,
    config_path: std::path::PathBuf,
    interface_up: bool,
}

impl OriginTunnel {
    /// Set up WireGuard tunnel on the origin side using wg-quick
    ///
    /// Requirements:
    /// - wireguard-tools must be installed (provides wg-quick)
    /// - Must be run with root privileges or appropriate capabilities
    ///
    /// Note: While boringtun is used for key generation, the tunnel setup still
    /// requires wg-quick because:
    /// - Creating TUN devices requires root/CAP_NET_ADMIN privileges
    /// - Network configuration requires elevated permissions
    /// - This ensures compatibility with the kernel WireGuard implementation on the proxy
    pub async fn setup(
        origin_keys: WireGuardKeys,
        proxy_public_key: String,
        proxy_endpoint: String,
    ) -> Result<Self> {
        info!("Setting up WireGuard tunnel on origin");

        let temp = TempDir::new()?;
        let config_path = temp.path().join("wg0.conf");

        // Create WireGuard configuration
        let config = format!(
            r#"[Interface]
Address = 172.17.0.2/24
PrivateKey = {}

[Peer]
PublicKey = {}
PresharedKey = {}
Endpoint = {}
AllowedIPs = 172.17.0.1/32
PersistentKeepalive = 25
"#,
            origin_keys.private_key, proxy_public_key, origin_keys.preshared_key, proxy_endpoint
        );

        fs::write(&config_path, config).context("Failed to write WireGuard configuration")?;

        debug!("WireGuard configuration written to {:?}", config_path);

        // Check if wg-quick is available
        match Command::new("which")
            .arg("wg-quick")
            .output()
            .await
        {
            Ok(output) if output.status.success() => {
                debug!("wg-quick found, attempting to bring up tunnel");
            }
            _ => {
                error!("wg-quick not found in PATH");
                error!("Please install wireguard-tools:");
                error!("  - Debian/Ubuntu: sudo apt install wireguard-tools");
                error!("  - Fedora/RHEL: sudo dnf install wireguard-tools");
                error!("  - macOS: brew install wireguard-tools");
                error!("  - Nix: nix-shell -p wireguard-tools");
                bail!("wg-quick is required but not found");
            }
        }

        // Bring up the interface using wg-quick (requires root)
        let status = Command::new("wg-quick")
            .arg("up")
            .arg(&config_path)
            .status()
            .await
            .context("Failed to execute wg-quick")?;

        if !status.success() {
            error!("wg-quick failed to bring up the tunnel (exit code: {})", status);
            error!("This usually means:");
            error!("  1. The application is not running with root privileges");
            error!("  2. Another WireGuard interface is already active");
            error!("  3. Network configuration conflicts exist");
            error!("");
            error!("To manually activate, run:");
            error!("  sudo wg-quick up {}", config_path.display());
            bail!("Failed to activate WireGuard tunnel");
        }

        info!("WireGuard tunnel activated successfully");

        Ok(Self {
            config_path: config_path.to_path_buf(),
            _temp: temp,
            interface_up: true,
        })
    }
}

impl Drop for OriginTunnel {
    fn drop(&mut self) {
        if self.interface_up {
            info!("Bringing down WireGuard tunnel");
            if let Err(e) = std::process::Command::new("wg-quick")
                .arg("down")
                .arg(&self.config_path)
                .status()
            {
                warn!("Failed to bring down WireGuard interface: {}", e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_key_generation() {
        let keys = WireGuardKeys::generate().await.expect("Failed to generate keys");

        // Keys should be base64 encoded and not empty
        assert!(!keys.private_key.is_empty());
        assert!(!keys.public_key.is_empty());
        assert!(!keys.preshared_key.is_empty());
    }

    #[tokio::test]
    async fn test_key_pair_generation() {
        let pair = WireGuardPair::generate().await.expect("Failed to generate key pair");

        // Both sides should have valid keys
        assert!(!pair.origin.private_key.is_empty());
        assert!(!pair.origin.public_key.is_empty());
        assert!(!pair.proxy.private_key.is_empty());
        assert!(!pair.proxy.public_key.is_empty());

        // Keys should be different
        assert_ne!(pair.origin.private_key, pair.proxy.private_key);
        assert_ne!(pair.origin.public_key, pair.proxy.public_key);
    }
}

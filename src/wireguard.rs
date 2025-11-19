use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::net::Ipv4Addr;
use tempfile::TempDir;
use tokio::process::Command;
use tracing::{debug, error, info, warn};

/// Check if the process has NET_ADMIN capability (required for WireGuard and iptables)
pub async fn check_net_admin_capability() -> Result<bool> {
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

/// Get all existing IP subnets on the system to avoid collisions
async fn get_existing_subnets() -> Result<HashSet<String>> {
    use nix::ifaddrs::getifaddrs;

    let mut subnets = HashSet::new();

    // Use getifaddrs to get all network interface addresses
    let ifaddrs = getifaddrs().context("Failed to get network interface addresses")?;

    for ifaddr in ifaddrs {
        if let Some(address) = ifaddr.address {
            if let Some(sock_addr) = address.as_sockaddr_in() {
                let ip_addr = Ipv4Addr::from(sock_addr.ip());
                let octets = ip_addr.octets();
                // Store the first two octets as the network identifier
                subnets.insert(format!("{}.{}", octets[0], octets[1]));
            }
        }
    }

    debug!("Found existing subnets: {:?}", subnets);
    Ok(subnets)
}

/// Find an available /24 subnet for WireGuard that doesn't conflict with existing networks
/// Returns a tuple of (proxy_ip, origin_ip)
pub async fn find_available_subnet() -> Result<(String, String)> {
    let existing = get_existing_subnets().await?;

    // Try common private IP ranges in order of preference
    // Format: (network_prefix, proxy_ip, origin_ip)
    let candidates = vec![
        ("172.17", "172.17.0.1", "172.17.0.2"), // Docker default
        ("172.18", "172.18.0.1", "172.18.0.2"),
        ("172.19", "172.19.0.1", "172.19.0.2"),
        ("172.20", "172.20.0.1", "172.20.0.2"),
        ("172.21", "172.21.0.1", "172.21.0.2"),
        ("172.22", "172.22.0.1", "172.22.0.2"),
        ("172.23", "172.23.0.1", "172.23.0.2"),
        ("172.24", "172.24.0.1", "172.24.0.2"),
        ("172.25", "172.25.0.1", "172.25.0.2"),
        ("172.26", "172.26.0.1", "172.26.0.2"),
        ("172.27", "172.27.0.1", "172.27.0.2"),
        ("172.28", "172.28.0.1", "172.28.0.2"),
        ("172.29", "172.29.0.1", "172.29.0.2"),
        ("172.30", "172.30.0.1", "172.30.0.2"),
        ("172.31", "172.31.0.1", "172.31.0.2"),
        ("10.99", "10.99.0.1", "10.99.0.2"),
        ("10.98", "10.98.0.1", "10.98.0.2"),
        ("10.97", "10.97.0.1", "10.97.0.2"),
        ("192.168.99", "192.168.99.1", "192.168.99.2"),
    ];

    for (prefix, proxy_ip, origin_ip) in candidates {
        if !existing.contains(prefix) {
            info!(
                "Selected WireGuard subnet: {}.0.0/24 (proxy: {}, origin: {})",
                prefix, proxy_ip, origin_ip
            );
            return Ok((proxy_ip.to_string(), origin_ip.to_string()));
        }
    }

    bail!("Could not find an available IP subnet for WireGuard. All candidate ranges are in use.");
}

pub struct OriginTunnel {
    _temp: TempDir,
    config_path: std::path::PathBuf,
    interface_up: bool,
    pub proxy_ip: String,
    pub origin_ip: String,
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
        proxy_ip: String,
        origin_ip: String,
        origin_host: String,
        origin_port: u16,
    ) -> Result<Self> {
        info!("Setting up WireGuard tunnel on origin");

        let temp = TempDir::new()?;
        let config_path = temp.path().join("wg0.conf");

        // Create WireGuard configuration with dynamically selected IPs
        // The iptables rules implement tight security:
        // 1. Only accept packets from the specific proxy IP on wg0
        // 2. Only forward TCP traffic from proxy IP
        // 3. Only MASQUERADE traffic destined for the origin service
        let config = format!(
            r#"[Interface]
Address = {origin_ip}/24
PrivateKey = {private_key}
PostUp = iptables -A INPUT -i wg0 -s {proxy_ip} -j ACCEPT
PostUp = iptables -A FORWARD -i wg0 -s {proxy_ip} -p tcp -j ACCEPT
PostUp = iptables -A FORWARD -o wg0 -d {proxy_ip} -j ACCEPT
PostUp = iptables -t nat -A PREROUTING -i wg0 -s {proxy_ip} -p tcp -j DNAT --to-destination {origin_host}:{origin_port}
PostUp = iptables -t nat -A POSTROUTING -d {origin_host} -p tcp --dport {origin_port} -j MASQUERADE
PreDown = iptables -D INPUT -i wg0 -s {proxy_ip} -j ACCEPT || true
PreDown = iptables -D FORWARD -i wg0 -s {proxy_ip} -p tcp -j ACCEPT || true
PreDown = iptables -D FORWARD -o wg0 -d {proxy_ip} -j ACCEPT || true
PreDown = iptables -D PREROUTING -t nat -i wg0 -s {proxy_ip} -p tcp -j DNAT --to-destination {origin_host}:{origin_port} || true
PreDown = iptables -D POSTROUTING -t nat -d {origin_host} -p tcp --dport {origin_port} -j MASQUERADE || true

[Peer]
PublicKey = {peer_public_key}
PresharedKey = {preshared_key}
Endpoint = {proxy_endpoint}
AllowedIPs = {proxy_ip}/32
PersistentKeepalive = 25
"#,
            origin_ip = origin_ip,
            private_key = origin_keys.private_key,
            proxy_ip = proxy_ip,
            origin_host = origin_host,
            origin_port = origin_port,
            peer_public_key = proxy_public_key,
            preshared_key = origin_keys.preshared_key,
            proxy_endpoint = proxy_endpoint,
        );

        fs::write(&config_path, config).context("Failed to write WireGuard configuration")?;

        // Set restrictive permissions on the config file (0600 - owner read/write only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&config_path, perms)
                .context("Failed to set permissions on WireGuard configuration")?;
        }

        debug!("WireGuard configuration written to {:?}", config_path);

        // Check if wg-quick is available
        match Command::new("which").arg("wg-quick").output().await {
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
            error!(
                "wg-quick failed to bring up the tunnel (exit code: {})",
                status
            );
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
            proxy_ip,
            origin_ip,
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
        let keys = WireGuardKeys::generate()
            .await
            .expect("Failed to generate keys");

        // Keys should be base64 encoded and not empty
        assert!(!keys.private_key.is_empty());
        assert!(!keys.public_key.is_empty());
        assert!(!keys.preshared_key.is_empty());
    }

    #[tokio::test]
    async fn test_key_pair_generation() {
        let pair = WireGuardPair::generate()
            .await
            .expect("Failed to generate key pair");

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

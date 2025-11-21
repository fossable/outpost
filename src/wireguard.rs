use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::net::Ipv4Addr;
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
        port_mappings: Vec<(u16, String)>, // (port, protocol)
        upload_limit: Option<u32>,          // Upload limit in Mbps (origin -> proxy)
        download_limit: Option<u32>,        // Download limit in Mbps (proxy -> origin)
    ) -> Result<Self> {
        info!("Setting up WireGuard tunnel on origin");

        let temp = TempDir::new()?;
        let config_path = temp.path().join("wg0.conf");

        // Create WireGuard configuration with dynamically selected IPs
        // The iptables rules implement tight security:
        // 1. Only accept packets from the specific proxy IP on wg0
        // 2. Only forward traffic from proxy IP (per protocol)
        // 3. Only MASQUERADE traffic destined for the origin service

        if port_mappings.is_empty() {
            bail!("At least one port mapping is required for WireGuard tunnel");
        }

        // Build iptables rules for each port mapping
        let mut post_up_rules = Vec::new();
        let mut pre_down_rules = Vec::new();

        // Allow WireGuard handshake and keepalive packets (UDP on the WireGuard interface itself)
        post_up_rules.push(format!("iptables -A INPUT -i wg0 -s {} -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT", proxy_ip));
        pre_down_rules.push(format!("iptables -D INPUT -i wg0 -s {} -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT || true", proxy_ip));

        // Common rules for general FORWARD (outbound to proxy)
        post_up_rules.push(format!("iptables -A FORWARD -o wg0 -d {} -j ACCEPT", proxy_ip));
        pre_down_rules.push(format!("iptables -D FORWARD -o wg0 -d {} -j ACCEPT || true", proxy_ip));

        // Bandwidth limiting with tc (traffic control)
        // Required kernel modules: sch_htb (for HTB qdisc)
        // Upload limit: traffic going from origin to proxy (egress on wg0)
        if let Some(limit) = upload_limit {
            let rate_kbps = limit * 1000; // Convert Mbps to Kbps
            info!("Setting upload bandwidth limit to {} Mbps ({} Kbps)", limit, rate_kbps);

            // Create root qdisc with HTB (Hierarchical Token Bucket)
            post_up_rules.push(format!("tc qdisc add dev wg0 root handle 1: htb default 10"));

            // Create class with rate limit
            post_up_rules.push(format!(
                "tc class add dev wg0 parent 1: classid 1:10 htb rate {}kbit ceil {}kbit",
                rate_kbps, rate_kbps
            ));

            // Cleanup
            pre_down_rules.push(format!("tc qdisc del dev wg0 root || true"));
        }

        // Download limit: traffic coming from proxy to origin (ingress on wg0)
        // Note: tc doesn't directly support ingress shaping, so we use ifb (intermediate functional block)
        // Required kernel modules: ifb, act_mirred (for traffic redirection), sch_htb (for HTB qdisc)
        if let Some(limit) = download_limit {
            let rate_kbps = limit * 1000; // Convert Mbps to Kbps
            info!("Setting download bandwidth limit to {} Mbps ({} Kbps)", limit, rate_kbps);

            // Load ifb module and create ifb0 device
            post_up_rules.push(format!("modprobe ifb numifbs=1"));
            post_up_rules.push(format!("ip link set dev ifb0 up"));

            // Redirect ingress traffic from wg0 to ifb0
            post_up_rules.push(format!("tc qdisc add dev wg0 handle ffff: ingress"));
            post_up_rules.push(format!(
                "tc filter add dev wg0 parent ffff: protocol all u32 match u32 0 0 action mirred egress redirect dev ifb0"
            ));

            // Apply rate limit on ifb0 (which represents wg0's ingress)
            post_up_rules.push(format!("tc qdisc add dev ifb0 root handle 1: htb default 10"));
            post_up_rules.push(format!(
                "tc class add dev ifb0 parent 1: classid 1:10 htb rate {}kbit ceil {}kbit",
                rate_kbps, rate_kbps
            ));

            // Cleanup
            pre_down_rules.push(format!("tc qdisc del dev wg0 ingress || true"));
            pre_down_rules.push(format!("tc qdisc del dev ifb0 root || true"));
            pre_down_rules.push(format!("ip link set dev ifb0 down || true"));
        }

        // Create custom chain for traffic accounting (excludes WireGuard overhead)
        post_up_rules.push(format!("iptables -N OUTPOST_ACCOUNTING || true"));
        pre_down_rules.push(format!("iptables -F OUTPOST_ACCOUNTING || true"));
        pre_down_rules.push(format!("iptables -X OUTPOST_ACCOUNTING || true"));

        // Per-port rules
        for (port, protocol) in &port_mappings {
            let proto_lower = protocol.to_lowercase();

            // Validate protocol
            if proto_lower != "tcp" && proto_lower != "udp" {
                bail!("Unsupported protocol '{}' for port {}. Only 'tcp' and 'udp' are supported.", protocol, port);
            }

            // INPUT rules for specific protocol/port (only accept traffic on ports we're proxying)
            post_up_rules.push(format!(
                "iptables -A INPUT -i wg0 -s {} -p {} --dport {} -j ACCEPT",
                proxy_ip, proto_lower, port
            ));
            pre_down_rules.push(format!(
                "iptables -D INPUT -i wg0 -s {} -p {} --dport {} -j ACCEPT || true",
                proxy_ip, proto_lower, port
            ));

            // FORWARD rules for specific protocol/port
            post_up_rules.push(format!(
                "iptables -A FORWARD -i wg0 -s {} -p {} -j ACCEPT",
                proxy_ip, proto_lower
            ));
            pre_down_rules.push(format!(
                "iptables -D FORWARD -i wg0 -s {} -p {} -j ACCEPT || true",
                proxy_ip, proto_lower
            ));

            // DNAT rule to forward traffic to origin
            post_up_rules.push(format!(
                "iptables -t nat -A PREROUTING -i wg0 -s {} -p {} --dport {} -j DNAT --to-destination {}:{}",
                proxy_ip, proto_lower, port, origin_host, port
            ));
            pre_down_rules.push(format!(
                "iptables -t nat -D PREROUTING -i wg0 -s {} -p {} --dport {} -j DNAT --to-destination {}:{} || true",
                proxy_ip, proto_lower, port, origin_host, port
            ));

            // MASQUERADE rule for return traffic
            post_up_rules.push(format!(
                "iptables -t nat -A POSTROUTING -d {} -p {} --dport {} -j MASQUERADE",
                origin_host, proto_lower, port
            ));
            pre_down_rules.push(format!(
                "iptables -t nat -D POSTROUTING -d {} -p {} --dport {} -j MASQUERADE || true",
                origin_host, proto_lower, port
            ));

            // Accounting rules to track actual application traffic (not WireGuard overhead)
            // Track traffic going TO origin (download from user perspective)
            post_up_rules.push(format!(
                "iptables -A OUTPOST_ACCOUNTING -d {} -p {} --dport {} -j RETURN",
                origin_host, proto_lower, port
            ));

            // Track traffic coming FROM origin (upload from user perspective)
            post_up_rules.push(format!(
                "iptables -A OUTPOST_ACCOUNTING -s {} -p {} --sport {} -j RETURN",
                origin_host, proto_lower, port
            ));
        }

        // Jump to accounting chain from FORWARD to count packets/bytes
        post_up_rules.push(format!("iptables -A FORWARD -j OUTPOST_ACCOUNTING"));
        pre_down_rules.push(format!("iptables -D FORWARD -j OUTPOST_ACCOUNTING || true"));

        let config = format!(
            r#"[Interface]
Address = {origin_ip}/24
PrivateKey = {private_key}
{post_up}
{pre_down}

[Peer]
PublicKey = {peer_public_key}
PresharedKey = {preshared_key}
Endpoint = {proxy_endpoint}
AllowedIPs = {proxy_ip}/32
PersistentKeepalive = 25
"#,
            origin_ip = origin_ip,
            private_key = origin_keys.private_key,
            post_up = post_up_rules.iter().map(|r| format!("PostUp = {}", r)).collect::<Vec<_>>().join("\n"),
            pre_down = pre_down_rules.iter().map(|r| format!("PreDown = {}", r)).collect::<Vec<_>>().join("\n"),
            peer_public_key = proxy_public_key,
            preshared_key = origin_keys.preshared_key,
            proxy_endpoint = proxy_endpoint,
            proxy_ip = proxy_ip,
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

    /// Get traffic statistics from iptables counters
    /// Returns (bytes_uploaded, bytes_downloaded)
    pub async fn get_traffic_stats(&self) -> Result<(u64, u64)> {
        let output = Command::new("iptables")
            .args(["-L", "OUTPOST_ACCOUNTING", "-v", "-n", "-x"])
            .output()
            .await
            .context("Failed to run iptables to get traffic stats")?;

        if !output.status.success() {
            bail!("iptables command failed");
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut bytes_to_origin = 0u64;
        let mut bytes_from_origin = 0u64;

        // Parse iptables output
        // Format: pkts bytes target prot opt in out source destination
        for line in stdout.lines().skip(2) {
            // Skip header lines
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 9 {
                continue;
            }

            // Extract byte count (second column)
            if let Ok(bytes) = parts[1].parse::<u64>() {
                // Check if this is traffic TO origin (destination match)
                if parts.len() >= 9 && parts[8].starts_with("0.0.0.0/0") && parts[7] != "0.0.0.0/0" {
                    bytes_to_origin += bytes;
                }
                // Check if this is traffic FROM origin (source match)
                else if parts.len() >= 9 && parts[7].starts_with("0.0.0.0/0") && parts[8] != "0.0.0.0/0" {
                    bytes_from_origin += bytes;
                }
            }
        }

        // From user's perspective:
        // - Upload = traffic going TO origin (download from proxy)
        // - Download = traffic FROM origin (upload to proxy)
        Ok((bytes_to_origin, bytes_from_origin))
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

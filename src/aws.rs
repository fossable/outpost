use anyhow::Result;
use aws_config::{meta::region::RegionProviderChain, Region};
use aws_sdk_ec2::{types::InstanceType, Client};

pub async fn start_instance(
    wg_private_key: String,
    wg_peer_public_key: String,
    wg_shared_key: String,
) -> Result<()> {
    let region_provider = RegionProviderChain::first_try(Region::new("us-east-2"))
        .or_default_provider()
        .or_else(Region::new("us-west-2"));
    let config = aws_config::from_env().region(region_provider).load().await;
    let client = Client::new(&config);

    client
        .run_instances()
        .image_id("123")
        .instance_type(InstanceType::T1Micro)
        .user_data(format!(
            r"#
                #!/bin/bash
                sudo apt update -y
                sudo apt install -y wireguard
                sudo modprobe wireguard

                cat <<-EOF >/etc/wireguard/wg0.conf
                    [Interface]
                    Address = 172.17.0.1/24
                    ListenPort = 51820
                    PrivateKey = {}

                    # IP forwarding
                    PreUp = sysctl -w net.ipv4.ip_forward=1
                    # IP masquerading
                    PreUp = iptables -t mangle -A PREROUTING -i wg0 -j MARK --set-mark 0x30
                    PreUp = iptables -t nat -A POSTROUTING ! -o wg0 -m mark --mark 0x30 -j MASQUERADE

                    [Peer]
                    PublicKey = {}
                    PresharedKey = {}
                    AllowedIPs = 172.17.0.2/32,10.0.0.0/8
                EOF

                sudo systemctl start wg-quick@wg0.service
            #",
            wg_private_key, wg_peer_public_key, wg_shared_key,
        ))
        .min_count(1)
        .max_count(1)
        .send()
        .await?;

    Ok(())
}

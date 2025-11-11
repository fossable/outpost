use anyhow::Result;
use serde_json::json;

pub struct CloudFormationTemplate {
    pub stack_name: String,
    pub region: String,
    pub ingress_host: String,
    pub ingress_port: u16,
    pub ingress_protocol: String,
    pub origin_host: String,
    pub origin_port: u16,
    pub origin_ip: String,
    pub instance_type: String,
    pub proxy_wg_private_key: String,
    pub proxy_wg_public_key: String,
    pub origin_wg_public_key: String,
    pub preshared_key: String,
}

impl CloudFormationTemplate {
    pub fn generate(&self) -> Result<String> {
        let template = json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Description": "Outpost AWS Proxy with VPC, WireGuard, and self-destruct",

            "Parameters": {
                "HostedZoneId": {
                    "Type": "String",
                    "Description": "Route53 Hosted Zone ID for DNS record"
                },
                "LatestUbuntuAMI": {
                    "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
                    "Default": self.get_ami_ssm_parameter(),
                    "Description": "Latest Ubuntu 22.04 LTS AMI from SSM Parameter Store"
                }
            },

            "Resources": {
                // VPC
                "VPC": {
                    "Type": "AWS::EC2::VPC",
                    "Properties": {
                        "CidrBlock": "10.0.0.0/16",
                        "EnableDnsHostnames": true,
                        "EnableDnsSupport": true,
                        "Tags": [{
                            "Key": "Name",
                            "Value": format!("outpost-{}", self.stack_name)
                        }]
                    }
                },

                // Internet Gateway
                "InternetGateway": {
                    "Type": "AWS::EC2::InternetGateway",
                    "Properties": {
                        "Tags": [{
                            "Key": "Name",
                            "Value": format!("outpost-{}-igw", self.stack_name)
                        }]
                    }
                },

                "AttachGateway": {
                    "Type": "AWS::EC2::VPCGatewayAttachment",
                    "Properties": {
                        "VpcId": {"Ref": "VPC"},
                        "InternetGatewayId": {"Ref": "InternetGateway"}
                    }
                },

                // Public Subnet
                "PublicSubnet": {
                    "Type": "AWS::EC2::Subnet",
                    "Properties": {
                        "VpcId": {"Ref": "VPC"},
                        "CidrBlock": "10.0.1.0/24",
                        "MapPublicIpOnLaunch": true,
                        "Tags": [{
                            "Key": "Name",
                            "Value": format!("outpost-{}-public", self.stack_name)
                        }]
                    }
                },

                // Route Table
                "PublicRouteTable": {
                    "Type": "AWS::EC2::RouteTable",
                    "Properties": {
                        "VpcId": {"Ref": "VPC"},
                        "Tags": [{
                            "Key": "Name",
                            "Value": format!("outpost-{}-public-rt", self.stack_name)
                        }]
                    }
                },

                "PublicRoute": {
                    "Type": "AWS::EC2::Route",
                    "DependsOn": "AttachGateway",
                    "Properties": {
                        "RouteTableId": {"Ref": "PublicRouteTable"},
                        "DestinationCidrBlock": "0.0.0.0/0",
                        "GatewayId": {"Ref": "InternetGateway"}
                    }
                },

                "SubnetRouteTableAssociation": {
                    "Type": "AWS::EC2::SubnetRouteTableAssociation",
                    "Properties": {
                        "SubnetId": {"Ref": "PublicSubnet"},
                        "RouteTableId": {"Ref": "PublicRouteTable"}
                    }
                },

                // Security Group
                "SecurityGroup": {
                    "Type": "AWS::EC2::SecurityGroup",
                    "Properties": {
                        "GroupDescription": "Allow WireGuard and ingress traffic only",
                        "VpcId": {"Ref": "VPC"},
                        "SecurityGroupIngress": [
                            {
                                "IpProtocol": "udp",
                                "FromPort": 51820,
                                "ToPort": 51820,
                                "CidrIp": format!("{}/32", self.origin_ip),
                                "Description": "WireGuard from origin"
                            },
                            {
                                "IpProtocol": self.ingress_protocol.as_str(),
                                "FromPort": self.ingress_port,
                                "ToPort": self.ingress_port,
                                "CidrIp": "0.0.0.0/0",
                                "Description": "Ingress traffic"
                            }
                        ],
                        "SecurityGroupEgress": [{
                            "IpProtocol": "-1",
                            "CidrIp": "0.0.0.0/0",
                            "Description": "Allow all outbound"
                        }],
                        "Tags": [{
                            "Key": "Name",
                            "Value": format!("outpost-{}-sg", self.stack_name)
                        }]
                    }
                },

                // IAM Role for EC2 to delete its own stack
                "EC2Role": {
                    "Type": "AWS::IAM::Role",
                    "Properties": {
                        "AssumeRolePolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Principal": {"Service": "ec2.amazonaws.com"},
                                "Action": "sts:AssumeRole"
                            }]
                        },
                        "Policies": [{
                            "PolicyName": "SelfDestructPolicy",
                            "PolicyDocument": {
                                "Version": "2012-10-17",
                                "Statement": [{
                                    "Effect": "Allow",
                                    "Action": [
                                        "cloudformation:DeleteStack",
                                        "cloudformation:DescribeStacks"
                                    ],
                                    "Resource": {
                                        "Fn::Sub": "arn:aws:cloudformation:${AWS::Region}:${AWS::AccountId}:stack/${AWS::StackName}/*"
                                    }
                                }]
                            }
                        }]
                    }
                },

                "InstanceProfile": {
                    "Type": "AWS::IAM::InstanceProfile",
                    "Properties": {
                        "Roles": [{"Ref": "EC2Role"}]
                    }
                },

                // EC2 Instance
                "ProxyInstance": {
                    "Type": "AWS::EC2::Instance",
                    "DependsOn": "AttachGateway",
                    "Properties": {
                        "InstanceType": self.instance_type.clone(),
                        "ImageId": {"Ref": "LatestUbuntuAMI"},
                        "SubnetId": {"Ref": "PublicSubnet"},
                        "SecurityGroupIds": [{"Ref": "SecurityGroup"}],
                        "IamInstanceProfile": {"Ref": "InstanceProfile"},
                        "UserData": {
                            "Fn::Base64": {
                                "Fn::Sub": self.generate_userdata()
                            }
                        },
                        "Tags": [{
                            "Key": "Name",
                            "Value": format!("outpost-{}-proxy", self.stack_name)
                        }]
                    }
                },

                // Wait Condition Handle for instance initialization
                "WaitHandle": {
                    "Type": "AWS::CloudFormation::WaitConditionHandle"
                },

                // Wait Condition - waits for instance to signal completion
                "WaitCondition": {
                    "Type": "AWS::CloudFormation::WaitCondition",
                    "DependsOn": "ProxyInstance",
                    "Properties": {
                        "Handle": {"Ref": "WaitHandle"},
                        "Timeout": "600",
                        "Count": 1
                    }
                },

                // Route53 DNS Record
                "DNSRecord": {
                    "Type": "AWS::Route53::RecordSet",
                    "DependsOn": "WaitCondition",
                    "Properties": {
                        "HostedZoneId": {"Ref": "HostedZoneId"},
                        "Name": format!("{}.", self.ingress_host),
                        "Type": "A",
                        "TTL": "60",
                        "ResourceRecords": [{"Fn::GetAtt": ["ProxyInstance", "PublicIp"]}]
                    }
                }
            },

            "Outputs": {
                "ProxyPublicIP": {
                    "Description": "Public IP of the proxy instance",
                    "Value": {"Fn::GetAtt": ["ProxyInstance", "PublicIp"]}
                },
                "DNSName": {
                    "Description": "DNS name for the proxy",
                    "Value": self.ingress_host.clone()
                }
            }
        });

        Ok(serde_json::to_string_pretty(&template)?)
    }

    fn get_ami_ssm_parameter(&self) -> String {
        // Determine architecture based on instance type
        let architecture = if self.instance_type.starts_with("t4g.")
            || self.instance_type.starts_with("a1.")
            || self.instance_type.starts_with("m6g.")
            || self.instance_type.starts_with("m7g.")
            || self.instance_type.starts_with("c6g.")
            || self.instance_type.starts_with("c7g.")
            || self.instance_type.starts_with("r6g.")
            || self.instance_type.starts_with("r7g.")
            || self.instance_type.starts_with("g5g.")
        {
            "arm64"
        } else {
            "amd64"
        };

        // Return SSM parameter path for latest Ubuntu 22.04 LTS AMI
        format!(
            "/aws/service/canonical/ubuntu/server/22.04/stable/current/{}/hvm/ebs-gp2/ami-id",
            architecture
        )
    }

    fn generate_userdata(&self) -> String {
        let protocol = self.ingress_protocol.as_str();

        format!(
            r#"#!/bin/bash
set -e

# Update system
apt-get update -y
apt-get upgrade -y

# Install WireGuard and required tools
apt-get install -y wireguard iptables socat curl jq awscli

# Enable IP forwarding
echo "net.ipv4.ip_forward=1" >> /etc/sysctl.conf
sysctl -p

# Configure WireGuard
cat > /etc/wireguard/wg0.conf <<'EOF'
[Interface]
Address = 172.17.0.1/24
ListenPort = 51820
PrivateKey = {}

# IP forwarding rules
PostUp = iptables -t nat -A PREROUTING -p {} --dport {} -j DNAT --to-destination 172.17.0.2:{}
PostUp = iptables -t nat -A POSTROUTING -s 172.17.0.0/24 -j MASQUERADE
PostDown = iptables -t nat -D PREROUTING -p {} --dport {} -j DNAT --to-destination 172.17.0.2:{}
PostDown = iptables -t nat -D POSTROUTING -s 172.17.0.0/24 -j MASQUERADE

[Peer]
PublicKey = {}
PresharedKey = {}
AllowedIPs = 172.17.0.2/32
PersistentKeepalive = 25
EOF

# Start WireGuard
systemctl enable wg-quick@wg0
systemctl start wg-quick@wg0

# Create self-destruct monitoring service
cat > /usr/local/bin/outpost-monitor.sh <<'MONITOR_EOF'
#!/bin/bash

ORIGIN_IP="{}"
STACK_NAME="{}"
REGION="{}"
FAIL_COUNT=0
MAX_FAILS=60  # 5 minutes with 5-second checks

while true; do
    # Try to ping the origin through the WireGuard tunnel
    if ping -c 1 -W 2 172.17.0.2 > /dev/null 2>&1; then
        FAIL_COUNT=0
    else
        FAIL_COUNT=$((FAIL_COUNT + 1))
        echo "$(date): Origin unreachable. Fail count: $FAIL_COUNT/$MAX_FAILS"

        if [ $FAIL_COUNT -ge $MAX_FAILS ]; then
            echo "$(date): Origin unreachable for 5 minutes. Self-destructing..."
            aws cloudformation delete-stack --stack-name "$STACK_NAME" --region "$REGION"
            exit 0
        fi
    fi

    sleep 5
done
MONITOR_EOF

chmod +x /usr/local/bin/outpost-monitor.sh

# Create systemd service for monitoring
cat > /etc/systemd/system/outpost-monitor.service <<'SERVICE_EOF'
[Unit]
Description=Outpost Origin Monitor
After=network.target wg-quick@wg0.service
Requires=wg-quick@wg0.service

[Service]
Type=simple
ExecStart=/usr/local/bin/outpost-monitor.sh
Restart=always
RestartSec=10

[Install]
WantedBy=multi-user.target
SERVICE_EOF

# Start monitoring service
systemctl daemon-reload
systemctl enable outpost-monitor
systemctl start outpost-monitor

echo "Outpost proxy setup complete"

# Signal CloudFormation that initialization is complete
curl -X PUT -H 'Content-Type: application/json' --data '{{"Status":"SUCCESS","Reason":"Instance initialized","UniqueId":"ProxyInstance","Data":"Ready"}}' "${{WaitHandle}}"
"#,
            self.proxy_wg_private_key,
            protocol,
            self.ingress_port,
            self.origin_port,
            protocol,
            self.ingress_port,
            self.origin_port,
            self.origin_wg_public_key,
            self.preshared_key,
            self.origin_ip,
            self.stack_name,
            self.region
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ami_ssm_parameter_x86() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 80,
            ingress_protocol: "tcp".to_string(),
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t3.micro".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
        };

        let param = template.get_ami_ssm_parameter();
        assert!(param.contains("amd64"));
    }

    #[test]
    fn test_ami_ssm_parameter_arm() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 80,
            ingress_protocol: "tcp".to_string(),
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
        };

        let param = template.get_ami_ssm_parameter();
        assert!(param.contains("arm64"));
    }

    #[test]
    fn test_tcp_protocol_in_userdata() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 80,
            ingress_protocol: "tcp".to_string(),
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
        };

        let userdata = template.generate_userdata();
        assert!(userdata.contains("-p tcp --dport 80"));
    }

    #[test]
    fn test_udp_protocol_in_userdata() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 53,
            ingress_protocol: "udp".to_string(),
            origin_host: "localhost".to_string(),
            origin_port: 53,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
        };

        let userdata = template.generate_userdata();
        assert!(userdata.contains("-p udp --dport 53"));
    }
}

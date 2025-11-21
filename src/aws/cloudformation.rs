use anyhow::Result;
use serde_json::json;

pub struct CloudFormationTemplate {
    pub stack_name: String,
    pub region: String,
    pub ingress_host: String,
    pub ingress_port: u16, // Primary port (first ingress) for backwards compat
    pub ingress_protocol: String,
    pub port_mappings: Vec<(u16, String)>, // All port mappings (port, protocol)
    pub origin_host: String,
    pub origin_port: u16,
    pub origin_ip: String,
    pub instance_type: String,
    pub proxy_wg_private_key: String,
    pub proxy_wg_public_key: String,
    pub origin_wg_public_key: String,
    pub preshared_key: String,
    pub debug: bool,
    pub use_cloudfront: bool,
    pub wg_proxy_ip: String,
    pub wg_origin_ip: String,
}

impl CloudFormationTemplate {
    pub fn generate(&self) -> Result<String> {
        let template_obj = json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Description": "Outpost AWS Proxy with VPC, WireGuard, and self-destruct",

            "Parameters": {
                "HostedZoneId": {
                    "Type": "String",
                    "Description": "Route53 Hosted Zone ID for DNS record"
                },
                "NixOSAMI": {
                    "Type": "AWS::EC2::Image::Id",
                    "Description": "NixOS AMI ID"
                }
            },

            "Conditions": {
                "UseCloudFront": {
                    "Fn::Equals": [self.use_cloudfront, true]
                },
                "NotUseCloudFront": {
                    "Fn::Not": [{
                        "Fn::Equals": [self.use_cloudfront, true]
                    }]
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
                        "SecurityGroupIngress": self.generate_security_group_rules(),
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
                                        "cloudformation:DescribeStacks",
                                        "cloudformation:DescribeStackResource"
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
                        "ImageId": {"Ref": "NixOSAMI"},
                        "SubnetId": {"Ref": "PublicSubnet"},
                        "SecurityGroupIds": [{"Ref": "SecurityGroup"}],
                        "IamInstanceProfile": {"Ref": "InstanceProfile"},
                        "UserData": {
                            "Fn::Base64": self.generate_userdata()
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

                // Route53 DNS Record (direct to EC2, no CloudFront)
                "DirectDNSRecord": {
                    "Type": "AWS::Route53::RecordSet",
                    "Condition": "NotUseCloudFront",
                    "DependsOn": "WaitCondition",
                    "Properties": {
                        "HostedZoneId": {"Ref": "HostedZoneId"},
                        "Name": format!("{}.", self.ingress_host),
                        "Type": "A",
                        "TTL": "60",
                        "ResourceRecords": [{"Fn::GetAtt": ["ProxyInstance", "PublicIp"]}]
                    }
                },

                // CloudFront Distribution (optional)
                "CloudFrontDistribution": {
                    "Type": "AWS::CloudFront::Distribution",
                    "Condition": "UseCloudFront",
                    "DependsOn": "WaitCondition",
                    "Properties": {
                        "DistributionConfig": {
                            "Comment": "Outpost CloudFront distribution",
                            "Enabled": true,
                            "HttpVersion": "http2",
                            "Origins": [{
                                "Id": "outpost-ec2-origin",
                                "DomainName": {"Fn::GetAtt": ["ProxyInstance", "PublicIp"]},
                                "CustomOriginConfig": {
                                    "HTTPPort": 80,
                                    "HTTPSPort": 443,
                                    "OriginProtocolPolicy": "https-only",
                                    "OriginSSLProtocols": ["TLSv1.2"],
                                    "OriginReadTimeout": 30,
                                    "OriginKeepaliveTimeout": 5
                                }
                            }],
                            "DefaultCacheBehavior": {
                                "TargetOriginId": "outpost-ec2-origin",
                                "ViewerProtocolPolicy": "https-only",
                                "AllowedMethods": ["GET", "HEAD", "OPTIONS", "PUT", "POST", "PATCH", "DELETE"],
                                "CachedMethods": ["GET", "HEAD"],
                                "Compress": true,
                                "ForwardedValues": {
                                    "QueryString": true,
                                    "Headers": ["*"],
                                    "Cookies": {
                                        "Forward": "all"
                                    }
                                },
                                "MinTTL": 0,
                                "DefaultTTL": 0,
                                "MaxTTL": 0
                            }
                        }
                    }
                },

                // Route53 DNS Record (with CloudFront)
                "CloudFrontDNSRecord": {
                    "Type": "AWS::Route53::RecordSet",
                    "Condition": "UseCloudFront",
                    "DependsOn": "CloudFrontDistribution",
                    "Properties": {
                        "HostedZoneId": {"Ref": "HostedZoneId"},
                        "Name": format!("{}.", self.ingress_host),
                        "Type": "A",
                        "AliasTarget": {
                            "HostedZoneId": "Z2FDTNDATAQYW2",
                            "DNSName": {"Fn::GetAtt": ["CloudFrontDistribution", "DomainName"]},
                            "EvaluateTargetHealth": false
                        }
                    }
                }
            },

            "Outputs": {
                "ProxyPublicIP": {
                    "Description": "Public IP of the proxy instance",
                    "Value": {"Fn::GetAtt": ["ProxyInstance", "PublicIp"]}
                },
                "ProxyInstanceId": {
                    "Description": "Instance ID of the proxy",
                    "Value": {"Ref": "ProxyInstance"}
                },
                "DNSName": {
                    "Description": "DNS name for the proxy",
                    "Value": self.ingress_host.clone()
                },
                "CloudFrontDistributionId": {
                    "Condition": "UseCloudFront",
                    "Description": "CloudFront distribution ID",
                    "Value": {"Ref": "CloudFrontDistribution"}
                },
                "CloudFrontDomain": {
                    "Condition": "UseCloudFront",
                    "Description": "CloudFront distribution domain name",
                    "Value": {"Fn::GetAtt": ["CloudFrontDistribution", "DomainName"]}
                }
            }
        });

        Ok(serde_json::to_string_pretty(&template_obj)?)
    }

    fn generate_security_group_rules(&self) -> serde_json::Value {
        let mut rules = vec![
            json!({
                "IpProtocol": "udp",
                "FromPort": 51820,
                "ToPort": 51820,
                "CidrIp": format!("{}/32", self.origin_ip),
                "Description": "WireGuard from origin"
            }),
        ];

        // Add rules for each port mapping
        for (port, protocol) in &self.port_mappings {
            rules.push(json!({
                "IpProtocol": protocol.to_lowercase(),
                "FromPort": port,
                "ToPort": port,
                "CidrIp": "0.0.0.0/0",
                "Description": format!("Ingress {} traffic on port {}", protocol.to_uppercase(), port)
            }));
        }

        if self.debug {
            rules.push(json!({
                "IpProtocol": "tcp",
                "FromPort": 22,
                "ToPort": 22,
                "CidrIp": format!("{}/32", self.origin_ip),
                "Description": "Debug SSH access from origin"
            }))
        }

        serde_json::Value::Array(rules)
    }

    pub fn get_architecture(&self) -> &str {
        // Determine architecture based on instance type
        if self.instance_type.starts_with("t4g.")
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
            "x86_64"
        }
    }

    fn generate_userdata(&self) -> serde_json::Value {
        // Load the Nix configuration template at compile time
        const NIX_TEMPLATE: &str = include_str!("../../templates/proxy.nix");

        // Extract subnet from proxy IP (e.g., "172.17.0.1" -> "172.17.0.0")
        let subnet = self
            .wg_proxy_ip
            .rsplitn(2, '.')
            .nth(1)
            .map(|s| format!("{}.0", s))
            .unwrap_or_else(|| "172.17.0.0".to_string());

        // Generate Nix list expression for port mappings
        // Format: [ { port = 80; protocol = "tcp"; } { port = 443; protocol = "tcp"; } ]
        let port_mappings_nix = if self.port_mappings.is_empty() {
            "[ ]".to_string()
        } else {
            let mappings: Vec<String> = self.port_mappings
                .iter()
                .map(|(port, protocol)| {
                    format!(
                        "{{ port = {}; protocol = \"{}\"; }}",
                        port,
                        protocol.to_lowercase()
                    )
                })
                .collect();
            format!("[\n    {}\n  ]", mappings.join("\n    "))
        };

        // Replace placeholders in the Nix template
        let nix_config = NIX_TEMPLATE
            .replace(
                "debug = false",
                &format!("debug = {}", if self.debug { "true" } else { "false" }),
            )
            .replace("{PROXY_WG_PRIVATE_KEY}", &self.proxy_wg_private_key)
            .replace("{PORT_MAPPINGS}", &port_mappings_nix)
            .replace("{ORIGIN_WG_PUBLIC_KEY}", &self.origin_wg_public_key)
            .replace("{PRESHARED_KEY}", &self.preshared_key)
            .replace("{ORIGIN_IP}", &self.wg_origin_ip)
            .replace("{PROXY_IP}", &self.wg_proxy_ip)
            .replace("{SUBNET}", &subnet)
            .replace("{STACK_NAME}", &self.stack_name)
            .replace("{REGION}", &self.region);

        json!(nix_config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_architecture_detection_x86() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 80,
            ingress_protocol: "tcp".to_string(),
            port_mappings: vec![(80, "tcp".to_string())],
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t3.micro".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: false,
            use_cloudfront: false,
            wg_proxy_ip: "172.17.0.1".to_string(),
            wg_origin_ip: "172.17.0.2".to_string(),
        };

        assert_eq!(template.get_architecture(), "x86_64");
    }

    #[test]
    fn test_architecture_detection_arm() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 80,
            ingress_protocol: "tcp".to_string(),
            port_mappings: vec![(80, "tcp".to_string())],
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: false,
            use_cloudfront: false,
            wg_proxy_ip: "172.17.0.1".to_string(),
            wg_origin_ip: "172.17.0.2".to_string(),
        };

        assert_eq!(template.get_architecture(), "arm64");
    }

    #[test]
    fn test_tcp_protocol_in_userdata() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 80,
            ingress_protocol: "tcp".to_string(),
            port_mappings: vec![(80, "tcp".to_string())],
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: false,
            use_cloudfront: false,
            wg_proxy_ip: "172.17.0.1".to_string(),
            wg_origin_ip: "172.17.0.2".to_string(),
        };

        let userdata = template.generate_userdata();
        let userdata_str = serde_json::to_string(&userdata).unwrap();
        // Check for NixOS configuration syntax and port mappings
        assert!(userdata_str.contains("{ config, pkgs, lib, ... }:"));
        assert!(userdata_str.contains("debug = false"));
        assert!(userdata_str.contains("port = 80"));
        assert!(userdata_str.contains("protocol = \\\"tcp\\\""));
    }

    #[test]
    fn test_udp_protocol_in_userdata() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 53,
            ingress_protocol: "udp".to_string(),
            port_mappings: vec![(53, "udp".to_string())],
            origin_host: "localhost".to_string(),
            origin_port: 53,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: false,
            use_cloudfront: false,
            wg_proxy_ip: "172.17.0.1".to_string(),
            wg_origin_ip: "172.17.0.2".to_string(),
        };

        let userdata = template.generate_userdata();
        let userdata_str = serde_json::to_string(&userdata).unwrap();
        // Check for NixOS configuration syntax and port mappings
        assert!(userdata_str.contains("{ config, pkgs, lib, ... }:"));
        assert!(userdata_str.contains("debug = false"));
        assert!(userdata_str.contains("port = 53"));
        assert!(userdata_str.contains("protocol = \\\"udp\\\""));
    }

    #[test]
    fn test_debug_mode_enabled() {
        let template = CloudFormationTemplate {
            stack_name: "test".to_string(),
            region: "us-east-2".to_string(),
            ingress_host: "test.example.com".to_string(),
            ingress_port: 80,
            ingress_protocol: "tcp".to_string(),
            port_mappings: vec![(80, "tcp".to_string())],
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: true,
            use_cloudfront: false,
            wg_proxy_ip: "172.17.0.1".to_string(),
            wg_origin_ip: "172.17.0.2".to_string(),
        };

        let userdata = template.generate_userdata();
        let userdata_str = serde_json::to_string(&userdata).unwrap();
        // Check that debug mode is enabled
        assert!(userdata_str.contains("debug = true"));
        assert!(userdata_str.contains("services.openssh"));
        assert!(userdata_str.contains("lib.mkIf debug"));
    }
}

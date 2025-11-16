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
    pub debug: bool,
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
                "NixOSAMI": {
                    "Type": "AWS::EC2::Image::Id",
                    "Description": "NixOS AMI ID"
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

    fn generate_security_group_rules(&self) -> serde_json::Value {
        let rules = vec![
            json!({
                "IpProtocol": "udp",
                "FromPort": 51820,
                "ToPort": 51820,
                "CidrIp": format!("{}/32", self.origin_ip),
                "Description": "WireGuard from origin"
            }),
            json!({
                "IpProtocol": self.ingress_protocol.as_str(),
                "FromPort": self.ingress_port,
                "ToPort": self.ingress_port,
                "CidrIp": "0.0.0.0/0",
                "Description": "Ingress traffic"
            }),
        ];

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

        // Replace placeholders in the Nix template
        let nix_config = NIX_TEMPLATE
            .replace(
                "debug = false",
                &format!("debug = {}", if self.debug { "true" } else { "false" }),
            )
            .replace("{PROXY_WG_PRIVATE_KEY}", &self.proxy_wg_private_key)
            .replace("{PROTOCOL}", &self.ingress_protocol)
            .replace("{INGRESS_PORT}", &self.ingress_port.to_string())
            .replace("{ORIGIN_PORT}", &self.origin_port.to_string())
            .replace("{ORIGIN_WG_PUBLIC_KEY}", &self.origin_wg_public_key)
            .replace("{PRESHARED_KEY}", &self.preshared_key)
            .replace("{ORIGIN_IP}", &self.origin_ip)
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
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t3.micro".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: false,
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
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: false,
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
            origin_host: "localhost".to_string(),
            origin_port: 8080,
            origin_ip: "1.2.3.4".to_string(),
            instance_type: "t4g.nano".to_string(),
            proxy_wg_private_key: "test_key".to_string(),
            proxy_wg_public_key: "test_pub".to_string(),
            origin_wg_public_key: "origin_pub".to_string(),
            preshared_key: "preshared".to_string(),
            debug: false,
        };

        let userdata = template.generate_userdata();
        let userdata_str = serde_json::to_string(&userdata).unwrap();
        // Check for NixOS configuration syntax and TCP protocol
        assert!(userdata_str.contains("{ config, pkgs, lib, ... }:"));
        assert!(userdata_str.contains("debug = false"));
        assert!(userdata_str.contains("-p tcp --dport 80"));
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
            debug: false,
        };

        let userdata = template.generate_userdata();
        let userdata_str = serde_json::to_string(&userdata).unwrap();
        // Check for NixOS configuration syntax and UDP protocol
        assert!(userdata_str.contains("{ config, pkgs, lib, ... }:"));
        assert!(userdata_str.contains("debug = false"));
        assert!(userdata_str.contains("-p udp --dport 53"));
    }

    #[test]
    fn test_debug_mode_enabled() {
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
            debug: true,
        };

        let userdata = template.generate_userdata();
        let userdata_str = serde_json::to_string(&userdata).unwrap();
        // Check that debug mode is enabled
        assert!(userdata_str.contains("debug = true"));
        assert!(userdata_str.contains("services.openssh"));
        assert!(userdata_str.contains("lib.mkIf debug"));
    }
}

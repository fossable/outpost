use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

/// Represents a parsed endpoint with protocol, host, and port
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub protocol: String,
    pub host: String,
    pub port: u16,
}

impl Endpoint {
    /// Parse an endpoint from a string like "tcp://host:port" or
    /// "tls://host:port"
    pub fn parse(s: &str) -> Result<Self> {
        let url =
            url::Url::parse(s).with_context(|| format!("Failed to parse endpoint URL: {}", s))?;

        let protocol = url.scheme().to_string();
        let host = url
            .host_str()
            .with_context(|| format!("Missing host in endpoint URL: {}", s))?
            .to_string();
        let port = url
            .port()
            .with_context(|| format!("Missing port in endpoint URL: {}", s))?;

        Ok(Self {
            protocol,
            host,
            port,
        })
    }
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct CommandLine {
    #[clap(subcommand)]
    pub command: Option<ServiceConfig>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum ServiceConfig {
    #[cfg(feature = "cloudflare")]
    Cloudflare {
        /// Ingress endpoint (e.g., "tls://www.example.com:443")
        #[clap(long, env = "OUTPOST_CLOUDFLARE_INGRESS")]
        ingress: String,

        /// Origin endpoint (e.g., "tcp://www:80")
        #[clap(long, env = "OUTPOST_CLOUDFLARE_ORIGIN")]
        origin: String,

        /// Origin certificate
        #[clap(long, env = "OUTPOST_CLOUDFLARE_ORIGIN_CERT")]
        origin_cert: String,
    },

    #[cfg(feature = "aws")]
    Aws {
        /// Ingress endpoint (e.g., "tcp://www.example.com:80")
        #[clap(long, env = "OUTPOST_AWS_INGRESS")]
        ingress: String,

        /// Origin endpoint (e.g., "tcp://www:8080")
        #[clap(long, env = "OUTPOST_AWS_ORIGIN")]
        origin: String,

        /// AWS regions (comma-separated, defaults to us-east-2)
        #[clap(long, env = "OUTPOST_AWS_REGIONS", default_value = "us-east-2")]
        regions: String,

        /// EC2 instance type (defaults to t4g.nano for cost efficiency)
        #[clap(long, env = "OUTPOST_AWS_INSTANCE_TYPE", default_value = "t4g.nano")]
        instance_type: String,

        /// Route53 Hosted Zone ID for DNS
        #[clap(long, env = "OUTPOST_AWS_HOSTED_ZONE_ID")]
        hosted_zone_id: String,

        /// AWS access key ID
        #[clap(long, env = "AWS_ACCESS_KEY_ID")]
        access_key_id: String,

        /// AWS secret access key
        #[clap(long, env = "AWS_SECRET_ACCESS_KEY")]
        secret_access_key: String,

        /// Enable debug mode (allows SSH access to proxy instance with hardcoded password)
        #[clap(long, env = "OUTPOST_AWS_DEBUG")]
        debug: bool,

        /// Use CloudFront distribution (port 443 only)
        #[clap(long, env = "OUTPOST_AWS_USE_CLOUDFRONT")]
        use_cloudfront: bool,
    },
}

impl ServiceConfig {
    /// Parse the ingress endpoint
    pub fn ingress(&self) -> Result<Endpoint> {
        match self {
            #[cfg(feature = "cloudflare")]
            ServiceConfig::Cloudflare { ingress, .. } => Endpoint::parse(ingress),
            #[cfg(feature = "aws")]
            ServiceConfig::Aws { ingress, .. } => Endpoint::parse(ingress),
        }
    }

    /// Parse the origin endpoint
    pub fn origin(&self) -> Result<Endpoint> {
        match self {
            #[cfg(feature = "cloudflare")]
            ServiceConfig::Cloudflare { origin, .. } => Endpoint::parse(origin),
            #[cfg(feature = "aws")]
            ServiceConfig::Aws { origin, .. } => Endpoint::parse(origin),
        }
    }

    /// Get AWS regions as a vector
    #[cfg(feature = "aws")]
    pub fn aws_regions(&self) -> Option<Vec<String>> {
        match self {
            ServiceConfig::Aws { regions, .. } => {
                Some(regions.split(',').map(|s| s.trim().to_string()).collect())
            }
            #[cfg(feature = "cloudflare")]
            _ => None,
        }
    }

    /// Validate CloudFront configuration
    #[cfg(feature = "aws")]
    pub fn validate_cloudfront(&self) -> Result<()> {
        match self {
            ServiceConfig::Aws {
                use_cloudfront,
                ingress,
                ..
            } => {
                if *use_cloudfront {
                    let endpoint = Endpoint::parse(ingress)?;
                    if endpoint.port != 443 {
                        anyhow::bail!(
                            "CloudFront can only be used with port 443, but ingress port is {}",
                            endpoint.port
                        );
                    }
                }
                Ok(())
            }
            #[cfg(feature = "cloudflare")]
            _ => Ok(()),
        }
    }
}

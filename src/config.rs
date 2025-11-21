use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use validator::{Validate, ValidationError};

// Compile-time check: at least one feature must be enabled
#[cfg(not(any(feature = "cloudflare", feature = "aws")))]
compile_error!(
    "At least one feature must be enabled: 'cloudflare' or 'aws'. Use: cargo build --features aws"
);

/// Supported protocols for ingress and origin endpoints
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
    Tls,
}

impl Protocol {
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "tcp" => Ok(Protocol::Tcp),
            "udp" => Ok(Protocol::Udp),
            "tls" => Ok(Protocol::Tls),
            _ => bail!(
                "Unsupported protocol '{}'. Supported protocols: tcp, udp, tls",
                s
            ),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Protocol::Tcp => "tcp",
            Protocol::Udp => "udp",
            Protocol::Tls => "tls",
        }
    }
}

/// Represents a parsed endpoint with protocol, host, and port
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub protocol: Protocol,
    pub host: String,
    pub port: Option<u16>,
}

impl Endpoint {
    /// Parse an endpoint from a string like "tcp://host:port" or "tls://host:port"
    /// Port is optional for origin endpoints when using multiple ingress
    pub fn parse(s: &str, require_port: bool) -> Result<Self> {
        let url =
            url::Url::parse(s).with_context(|| format!("Failed to parse endpoint URL: {}", s))?;

        let protocol = Protocol::from_str(url.scheme())?;
        let host = url
            .host_str()
            .with_context(|| format!("Missing host in endpoint URL: {}", s))?
            .to_string();
        let port = url.port();

        if require_port && port.is_none() {
            bail!("Missing port in endpoint URL: {}", s);
        }

        Ok(Self {
            protocol,
            host,
            port,
        })
    }

    /// Get the port, returning an error if not set
    pub fn port(&self) -> Result<u16> {
        self.port
            .with_context(|| format!("Port not specified for endpoint {}", self.host))
    }
}

#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct CommandLine {
    #[clap(subcommand)]
    pub command: Option<ServiceConfig>,

    /// Upload bandwidth limit in Mbps (optional)
    #[clap(long, env = "OUTPOST_UPLOAD_LIMIT", global = true)]
    pub upload_limit: Option<u32>,

    /// Download bandwidth limit in Mbps (optional)
    #[clap(long, env = "OUTPOST_DOWNLOAD_LIMIT", global = true)]
    pub download_limit: Option<u32>,
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
        /// Ingress endpoint(s) (e.g., "tcp://www.example.com:80")
        /// Can be specified multiple times for multiple ports
        #[clap(long, env = "OUTPOST_AWS_INGRESS")]
        ingress: Vec<String>,

        /// Origin endpoint (e.g., "tcp://www:8080" or "tcp://www" for multiple ingress)
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

impl Validate for ServiceConfig {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        let mut errors = validator::ValidationErrors::new();

        match self {
            #[cfg(feature = "cloudflare")]
            ServiceConfig::Cloudflare { .. } => {
                // No specific validations for Cloudflare yet
            }
            #[cfg(feature = "aws")]
            ServiceConfig::Aws { ingress, .. } => {
                // Validate ingress list
                if let Err(e) = validate_ingress_list(ingress) {
                    errors.add("ingress", e);
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl Validate for CommandLine {
    fn validate(&self) -> Result<(), validator::ValidationErrors> {
        let mut errors = validator::ValidationErrors::new();

        // Validate upload_limit range
        if let Some(limit) = self.upload_limit {
            if limit < 1 || limit > 10000 {
                let mut error = validator::ValidationError::new("range");
                error.message = Some("Upload limit must be between 1 and 10000 Mbps".into());
                errors.add("upload_limit", error);
            }
        }

        // Validate download_limit range
        if let Some(limit) = self.download_limit {
            if limit < 1 || limit > 10000 {
                let mut error = validator::ValidationError::new("range");
                error.message = Some("Download limit must be between 1 and 10000 Mbps".into());
                errors.add("download_limit", error);
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

impl ServiceConfig {
    /// Parse all ingress endpoints
    pub fn ingresses(&self) -> Result<Vec<Endpoint>> {
        match self {
            #[cfg(feature = "cloudflare")]
            ServiceConfig::Cloudflare { ingress, .. } => Ok(vec![Endpoint::parse(ingress, true)?]),
            #[cfg(feature = "aws")]
            ServiceConfig::Aws { ingress, .. } => {
                if ingress.is_empty() {
                    bail!("At least one --ingress endpoint must be specified");
                }
                ingress
                    .iter()
                    .map(|s| Endpoint::parse(s, true))
                    .collect::<Result<Vec<_>>>()
            }
        }
    }

    /// Parse the ingress endpoint (backwards compatibility for single ingress)
    pub fn ingress(&self) -> Result<Endpoint> {
        let ingresses = self.ingresses()?;
        if ingresses.len() > 1 {
            bail!("Multiple ingress endpoints specified, use ingresses() instead");
        }
        Ok(ingresses.into_iter().next().unwrap())
    }

    /// Parse the origin endpoint
    pub fn origin(&self) -> Result<Endpoint> {
        match self {
            #[cfg(feature = "cloudflare")]
            ServiceConfig::Cloudflare { origin, .. } => Endpoint::parse(origin, true),
            #[cfg(feature = "aws")]
            ServiceConfig::Aws {
                origin, ingress, ..
            } => {
                // If multiple ingress, origin must NOT have a port
                let require_port = ingress.len() == 1;
                let endpoint = Endpoint::parse(origin, require_port)?;

                // Additional validation: if multiple ingress, ensure no port in origin
                if ingress.len() > 1 && endpoint.port.is_some() {
                    bail!(
                        "When multiple --ingress are specified, --origin must NOT include a port (only host). \
                        Found port {} in origin endpoint. Each ingress port will be mapped to the same port on the origin.",
                        endpoint.port.unwrap()
                    );
                }

                Ok(endpoint)
            }
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

    /// Validate all configuration using the validator crate
    pub fn validate_all(&self) -> Result<()> {
        // Run validator derive validations
        self.validate()
            .map_err(|e| anyhow::anyhow!("Validation failed: {}", e))?;

        // Run custom validations
        self.validate_protocols()?;

        #[cfg(feature = "aws")]
        {
            self.validate_tls_single_endpoint()?;
            self.validate_cloudfront()?;
        }

        Ok(())
    }

    /// Validate that all ingress endpoints use the same protocol
    fn validate_protocols(&self) -> Result<()> {
        let ingresses = self.ingresses()?;
        let origin = self.origin()?;

        if ingresses.is_empty() {
            return Ok(());
        }

        // Check all ingress endpoints use the same protocol
        let first_protocol = &ingresses[0].protocol;
        for (i, endpoint) in ingresses.iter().enumerate().skip(1) {
            if &endpoint.protocol != first_protocol {
                bail!(
                    "All --ingress endpoints must use the same protocol. \
                    Found {} in first endpoint but {} in endpoint {}",
                    first_protocol.as_str(),
                    endpoint.protocol.as_str(),
                    i + 1
                );
            }
        }

        // Check origin protocol matches ingress protocol
        if origin.protocol != *first_protocol {
            bail!(
                "--ingress protocol '{}' does not match --origin protocol '{}'. \
                Both must use the same protocol.",
                first_protocol.as_str(),
                origin.protocol.as_str()
            );
        }

        Ok(())
    }

    /// Validate TLS only supports single endpoint
    #[cfg(feature = "aws")]
    fn validate_tls_single_endpoint(&self) -> Result<()> {
        match self {
            ServiceConfig::Aws { ingress, .. } => {
                if ingress.len() > 1 {
                    // Parse first to check protocol
                    let first = Endpoint::parse(&ingress[0], true)?;
                    if first.protocol == Protocol::Tls {
                        bail!(
                            "TLS protocol does not support multiple --ingress endpoints. \
                            Found {} endpoints. Use TCP or UDP for multiple ports.",
                            ingress.len()
                        );
                    }
                }
                Ok(())
            }
            #[cfg(feature = "cloudflare")]
            _ => Ok(()),
        }
    }

    /// Validate CloudFront configuration
    #[cfg(feature = "aws")]
    fn validate_cloudfront(&self) -> Result<()> {
        match self {
            ServiceConfig::Aws {
                use_cloudfront,
                ingress,
                ..
            } => {
                if *use_cloudfront {
                    if ingress.len() > 1 {
                        bail!(
                            "CloudFront can only be used with a single --ingress endpoint. \
                            Found {} endpoints.",
                            ingress.len()
                        );
                    }
                    let endpoint = Endpoint::parse(&ingress[0], true)?;
                    if let Some(port) = endpoint.port {
                        if port != 443 {
                            bail!(
                                "CloudFront can only be used with port 443, but ingress port is {}",
                                port
                            );
                        }
                    }
                }
                Ok(())
            }
            #[cfg(feature = "cloudflare")]
            _ => Ok(()),
        }
    }
}

/// Custom validator for ingress list
fn validate_ingress_list(ingress: &Vec<String>) -> Result<(), ValidationError> {
    if ingress.is_empty() {
        return Err(ValidationError::new(
            "At least one --ingress endpoint must be specified",
        ));
    }
    Ok(())
}

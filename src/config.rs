use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "provider")]
pub enum ServiceConfig {
    #[cfg(feature = "cloudflare")]
    #[serde(rename = "cloudflare")]
    Cloudflare {
        /// Name of the container where the service is running
        service: String,

        /// Port mappings
        ports: Vec<String>,
    },

    #[cfg(feature = "aws")]
    #[serde(rename = "aws")]
    Aws {
        /// Name of the container where the service is running
        service: String,

        /// Port mappings
        ports: Vec<String>,
    },
}

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(tag = "provider")]
pub enum ServiceConfig {
    #[cfg(feature = "cloudflare")]
    #[serde(rename = "cloudflare")]
    Cloudflare {
        cert_path: String,

        /// Port mappings
        ports: Vec<String>,
    },
}

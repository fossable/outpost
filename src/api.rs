use askama::Template;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use rust_embed::RustEmbed;
use std::sync::Arc;
use tokio::sync::RwLock;

#[derive(RustEmbed)]
#[folder = "assets/"]
struct Assets;

#[derive(Clone)]
pub struct AppState {
    pub stats: Arc<RwLock<TunnelStats>>,
    pub proxy_info: Arc<RwLock<Option<ProxyInfo>>>,
    pub cloudfront_info: Arc<RwLock<Option<CloudFrontInfo>>>,
    pub tunnel: Arc<RwLock<Option<Arc<crate::wireguard::OriginTunnel>>>>,
    pub upload_limit: Option<u32>,
    pub download_limit: Option<u32>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            stats: Arc::new(RwLock::new(TunnelStats::default())),
            proxy_info: Arc::new(RwLock::new(None)),
            cloudfront_info: Arc::new(RwLock::new(None)),
            tunnel: Arc::new(RwLock::new(None)),
            upload_limit: None,
            download_limit: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct TunnelStats {
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub bytes_sent_formatted: String,
    pub bytes_received_formatted: String,
    pub packets_sent: u64,
    pub packets_received: u64,
    pub tunnel_up: bool,
    pub uptime_seconds: u64,
}

impl TunnelStats {
    pub fn format_sizes(&mut self) {
        self.bytes_sent_formatted = format_bytes(self.bytes_sent);
        self.bytes_received_formatted = format_bytes(self.bytes_received);
    }

    /// Create example stats for demo mode
    pub fn example() -> Self {
        let mut stats = Self {
            bytes_sent: 1_234_567_890,
            bytes_received: 9_876_543_210,
            bytes_sent_formatted: String::new(),
            bytes_received_formatted: String::new(),
            packets_sent: 45_678,
            packets_received: 123_456,
            tunnel_up: true,
            uptime_seconds: 3665, // 1h 1m 5s
        };
        stats.format_sizes();
        stats
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;

    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }

    if unit_idx == 0 {
        format!("{} {}", size as u64, UNITS[unit_idx])
    } else {
        format!("{:.2} {}", size, UNITS[unit_idx])
    }
}

/// Calculate uptime from a launch time string in RFC3339 format
pub fn calculate_uptime(launch_time: &str) -> String {
    use chrono::{DateTime, Utc};

    // Try to parse the launch time
    if let Ok(launch) = DateTime::parse_from_rfc3339(launch_time) {
        let now = Utc::now();
        let duration = now.signed_duration_since(launch.with_timezone(&Utc));

        let days = duration.num_days();
        let hours = duration.num_hours() % 24;
        let minutes = duration.num_minutes() % 60;

        if days > 0 {
            format!("{}d {}h {}m", days, hours, minutes)
        } else if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}m", minutes)
        }
    } else {
        "Unknown".to_string()
    }
}

#[derive(Clone)]
pub enum ProxyInfo {
    Aws {
        instance_id: String,
        instance_type: String,
        region: String,
        public_ip: String,
        private_ip: String,
        state: String,
        launch_time: String,
        uptime: String,
    },
    Cloudflare {
        hostname: String,
        connector_id: String,
        connections: u32,
    },
}

impl ProxyInfo {
    /// Create example AWS proxy info for demo mode
    pub fn example_aws() -> Self {
        ProxyInfo::Aws {
            instance_id: "i-0123456789abcdef0".to_string(),
            instance_type: "t4g.nano".to_string(),
            region: "us-east-2".to_string(),
            public_ip: "203.0.113.42".to_string(),
            private_ip: "172.17.0.1".to_string(),
            state: "running".to_string(),
            launch_time: "2025-11-11 19:30:00 UTC".to_string(),
            uptime: "2h 15m".to_string(),
        }
    }

    /// Create example Cloudflare proxy info for demo mode
    pub fn example_cloudflare() -> Self {
        ProxyInfo::Cloudflare {
            hostname: "www.example.com".to_string(),
            connector_id: "abc123-def456-ghi789".to_string(),
            connections: 4,
        }
    }
}

#[derive(Clone, Default)]
pub struct CloudFrontInfo {
    pub distribution_id: String,
    pub distribution_domain: String,
    pub status: String,
}

impl CloudFrontInfo {
    /// Create example CloudFront info for demo mode
    pub fn example() -> Self {
        CloudFrontInfo {
            distribution_id: "E1234ABCDEFGHI".to_string(),
            distribution_domain: "d1234abcdefghi.cloudfront.net".to_string(),
            status: "Deployed".to_string(),
        }
    }
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub tunnel_stats: TunnelStats,
    pub proxy_info: Option<ProxyInfo>,
    pub cloudfront_info: Option<CloudFrontInfo>,
    pub upload_limit: Option<u32>,
    pub download_limit: Option<u32>,
}

pub async fn assets(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    let path = file.trim_start_matches('/');

    match Assets::get(path) {
        Some(content) => {
            let mime = mime_guess::from_path(path).first_or_octet_stream();
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, mime.as_ref())],
                content.data,
            )
                .into_response()
        }
        None => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/stats", get(stats_api))
        .route("/assets/*file", get(assets))
        .with_state(state)
}

pub async fn stats_api(State(state): State<AppState>) -> impl IntoResponse {
    use axum::Json;
    use serde::Serialize;

    #[derive(Serialize)]
    struct StatsResponse {
        bytes_sent: u64,
        bytes_received: u64,
        bytes_sent_formatted: String,
        bytes_received_formatted: String,
        packets_sent: u64,
        packets_received: u64,
    }

    let mut stats = state.stats.read().await.clone();

    // Update stats from iptables if tunnel is available
    if let Some(tunnel) = state.tunnel.read().await.as_ref() {
        if let Ok((bytes_sent, bytes_received)) = tunnel.get_traffic_stats().await {
            stats.bytes_sent = bytes_sent;
            stats.bytes_received = bytes_received;
        }
    }

    stats.format_sizes();

    Json(StatsResponse {
        bytes_sent: stats.bytes_sent,
        bytes_received: stats.bytes_received,
        bytes_sent_formatted: stats.bytes_sent_formatted,
        bytes_received_formatted: stats.bytes_received_formatted,
        packets_sent: stats.packets_sent,
        packets_received: stats.packets_received,
    })
}

pub async fn index(State(state): State<AppState>) -> impl IntoResponse {
    let mut stats = state.stats.read().await.clone();

    // Update stats from iptables if tunnel is available
    if let Some(tunnel) = state.tunnel.read().await.as_ref() {
        if let Ok((bytes_sent, bytes_received)) = tunnel.get_traffic_stats().await {
            stats.bytes_sent = bytes_sent;
            stats.bytes_received = bytes_received;
        }
    }

    stats.format_sizes();
    let mut proxy_info = state.proxy_info.read().await.clone();
    let cloudfront_info = state.cloudfront_info.read().await.clone();

    // Calculate uptime dynamically for AWS proxy
    if let Some(ProxyInfo::Aws {
        launch_time,
        uptime,
        ..
    }) = &mut proxy_info
    {
        *uptime = calculate_uptime(launch_time);
    }

    let template = IndexTemplate {
        tunnel_stats: stats,
        proxy_info,
        cloudfront_info,
        upload_limit: state.upload_limit,
        download_limit: state.download_limit,
    };

    Html(template.render().unwrap())
}

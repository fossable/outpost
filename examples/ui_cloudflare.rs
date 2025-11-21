use askama::Template;
use axum::{response::Html, routing::get, Router};
use outpost::api::{IndexTemplate, ProxyInfo, TunnelStats};

async fn index() -> Html<String> {
    let mut stats = TunnelStats {
        bytes_sent: 5_500_000_000,
        bytes_received: 12_300_000_000,
        bytes_sent_formatted: String::new(),
        bytes_received_formatted: String::new(),
        packets_sent: 89_234,
        packets_received: 234_567,
        tunnel_up: true,
        uptime_seconds: 86_400, // 1 day
    };
    stats.format_sizes();

    let proxy_info = Some(ProxyInfo::Cloudflare {
        hostname: "www.example.com".to_string(),
        connector_id: "abc123-def456-ghi789".to_string(),
        connections: 4,
    });

    let template = IndexTemplate {
        tunnel_stats: stats,
        proxy_info,
        cloudfront_info: None,
        upload_limit: None,
        download_limit: None,
    };

    Html(template.render().unwrap())
}

#[tokio::main]
async fn main() {
    let app = Router::new()
        .route("/", get(index))
        .route("/assets/*file", get(outpost::api::assets));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080")
        .await
        .expect("Failed to bind to port 8080");

    println!("Serving UI with Cloudflare tunnel at http://localhost:8080");
    axum::serve(listener, app).await.unwrap();
}

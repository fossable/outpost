use askama::Template;
use axum::{response::Html, routing::get, Router};
use outpost::api::{IndexTemplate, ProxyInfo, TunnelStats};

async fn index() -> Html<String> {
    let mut stats = TunnelStats {
        bytes_sent: 500_000_000_000,       // 465 GB
        bytes_received: 1_200_000_000_000, // 1.09 TB
        bytes_sent_formatted: String::new(),
        bytes_received_formatted: String::new(),
        packets_sent: 5_678_901,
        packets_received: 12_345_678,
        tunnel_up: true,
        uptime_seconds: 2_592_000, // 30 days
    };
    stats.format_sizes();

    let proxy_info = Some(ProxyInfo::Aws {
        instance_id: "i-9876543210fedcba0".to_string(),
        instance_type: "t4g.small".to_string(),
        region: "us-west-2".to_string(),
        public_ip: "198.51.100.123".to_string(),
        private_ip: "172.17.0.1".to_string(),
        state: "running".to_string(),
        launch_time: "2025-10-12T08:15:00Z".to_string(),
        uptime: String::new(), // Will be calculated dynamically
    });

    let template = IndexTemplate {
        tunnel_stats: stats,
        proxy_info,
        cloudfront_info: None,
        upload_limit: Some(1000),
        download_limit: Some(1000),
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

    println!("Serving UI with high traffic scenario at http://localhost:8080");
    axum::serve(listener, app).await.unwrap();
}

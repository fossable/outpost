use askama::Template;
use axum::{response::Html, routing::get, Router};
use outpost::api::{IndexTemplate, ProxyInfo, TunnelStats};

async fn index() -> Html<String> {
    let mut stats = TunnelStats {
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

    let proxy_info = Some(ProxyInfo::Aws {
        instance_id: "i-0123456789abcdef0".to_string(),
        instance_type: "t4g.nano".to_string(),
        region: "us-east-2".to_string(),
        public_ip: "203.0.113.42".to_string(),
        private_ip: "172.17.0.1".to_string(),
        state: "running".to_string(),
        launch_time: "2025-11-11T19:30:00Z".to_string(),
        uptime: String::new(), // Will be calculated dynamically
    });

    let template = IndexTemplate {
        tunnel_stats: stats,
        proxy_info,
        cloudfront_info: None,
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

    println!("Serving UI with AWS proxy at http://localhost:8080");
    axum::serve(listener, app).await.unwrap();
}

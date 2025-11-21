use askama::Template;
use axum::{response::Html, routing::get, Router};
use outpost::api::{IndexTemplate, TunnelStats};

async fn index() -> Html<String> {
    let mut stats = TunnelStats {
        bytes_sent: 0,
        bytes_received: 0,
        bytes_sent_formatted: String::new(),
        bytes_received_formatted: String::new(),
        packets_sent: 0,
        packets_received: 0,
        tunnel_up: false,
        uptime_seconds: 0,
    };
    stats.format_sizes();

    let template = IndexTemplate {
        tunnel_stats: stats,
        proxy_info: None,
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

    println!("Serving UI with no proxy (tunnel down) at http://localhost:8080");
    axum::serve(listener, app).await.unwrap();
}

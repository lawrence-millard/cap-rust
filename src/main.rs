mod auth;
mod config;
mod error;
mod routes;
mod sign;
mod state;
mod storage;

use std::sync::Arc;

use axum::http::{HeaderValue, header};
use tower_http::cors::{Any, CorsLayer};
use tower_http::set_header::SetResponseHeaderLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer())
        .init();

    let config = config::Config::from_env();
    let state = Arc::new(
        state::AppState::new(config)
            .await
            .expect("failed to init app state"),
    );

    // run migrations
    sqlx::migrate!("./migrations")
        .run(&state.db)
        .await
        .expect("migrations failed");

    // ensure default user exists
    sqlx::query(
        "INSERT INTO users (id, name, email) VALUES ('u_single_user', 'Owner', NULL)
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&state.db)
    .await
    .ok();

    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods(Any)
        .allow_headers(Any);

    let app = routes::router(state.clone())
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("SAMEORIGIN"),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = format!("0.0.0.0:{}", state.config.port);
    tracing::info!("cap-server listening on {}", addr);
    tracing::info!("web_url: {}", state.config.web_url);
    tracing::info!("storage: {}", state.config.storage_dir.display());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind failed");
    axum::serve(listener, app).await.expect("serve failed");
}

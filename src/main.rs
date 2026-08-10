use axum::http::{HeaderName, HeaderValue, header};
use cap_server::{config, routes, state, storage};
use std::sync::Arc;
use tower_http::cors::{AllowOrigin, Any, CorsLayer};
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

    // ensure legacy default user exists (pre-multi-user recordings are owned by it)
    sqlx::query(
        "INSERT INTO users (id, name, email) VALUES ('u_single_user', 'Owner', NULL)
         ON CONFLICT (id) DO NOTHING",
    )
    .execute(&state.db)
    .await
    .expect("failed to ensure legacy default user");

    match routes::upload::recover_stale_mux_jobs(&state).await {
        Ok(count) if count > 0 => tracing::info!(count, "reclaimed stale mux jobs"),
        Ok(_) => {}
        Err(e) => tracing::error!("stale mux recovery failed: {e}"),
    }

    // Reclaim one bounded batch of stale mux work every hour.
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                match routes::upload::recover_stale_mux_jobs(&state).await {
                    Ok(count) if count > 0 => {
                        tracing::info!(count, "reclaimed stale mux jobs")
                    }
                    Ok(_) => {}
                    Err(e) => tracing::error!("stale mux recovery failed: {e}"),
                }
            }
        });
    }

    // background task: sweep abandoned multipart staging dirs every hour
    {
        let state = state.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                storage::cleanup_staging(&state, 24 * 3600).await;
            }
        });
    }

    let cors = build_cors(&state.config.web_url, &state.config.cors_origins);

    let app = routes::router(state.clone())
        .layer(SetResponseHeaderLayer::overriding(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::REFERRER_POLICY,
            HeaderValue::from_static("strict-origin-when-cross-origin"),
        ))
        .layer(TraceLayer::new_for_http())
        .layer(cors);

    let addr = format!("0.0.0.0:{}", state.config.port);
    tracing::info!("listening on {}", addr);
    tracing::info!("web_url: {}", state.config.web_url);
    tracing::info!("storage: {}", state.config.storage_dir.display());

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("bind failed");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .expect("serve failed");

    // Drain in-flight mux jobs (bound the wait, then mark leftovers failed).
    state
        .mux_jobs
        .shutdown(&state.db, std::time::Duration::from_secs(30))
        .await;
    tracing::info!("shutdown complete");
}

fn build_cors(web_url: &str, extra_origins: &[String]) -> CorsLayer {
    let mut origins = Vec::new();
    if let Ok(origin) = HeaderValue::from_str(web_url.trim_end_matches('/')) {
        origins.push(origin);
    }
    for origin in extra_origins {
        if let Ok(value) = HeaderValue::from_str(origin.trim()) {
            origins.push(value);
        }
    }
    if origins.is_empty() {
        return CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers([
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                header::ACCEPT,
                HeaderName::from_static("range"),
            ]);
    }
    CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods(Any)
        .allow_headers([
            header::AUTHORIZATION,
            header::CONTENT_TYPE,
            header::ACCEPT,
            HeaderName::from_static("range"),
        ])
}

/// Wait for SIGINT (Ctrl-C) or SIGTERM, then shut down cleanly so in-flight
/// uploads finish writing before the process exits.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl_c handler");
    };

    let terminate = async {
        signal(SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received, draining connections");
}

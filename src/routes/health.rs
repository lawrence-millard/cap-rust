use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::Arc;

use crate::state::AppState;

/// GET /health — liveness + DB reachability. Used by load balancers and
/// orchestration probes. Returns 200 when the pool can ping Postgres, else 503.
pub async fn health(State(state): State<Arc<AppState>>) -> Response {
    match state.db.acquire().await {
        Ok(mut conn) => match sqlx::query("SELECT 1").execute(&mut *conn).await {
            Ok(_) => axum::Json(json!({ "status": "ok" })).into_response(),
            Err(e) => {
                tracing::error!("health: db ping failed: {e}");
                (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    axum::Json(json!({ "status": "degraded", "db": "down" })),
                )
                    .into_response()
            }
        },
        Err(e) => {
            tracing::error!("health: pool acquire failed: {e}");
            (
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                axum::Json(json!({ "status": "degraded", "db": "down" })),
            )
                .into_response()
        }
    }
}

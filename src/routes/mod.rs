use axum::Router;
use axum::routing::{delete, get, post, put};
use std::sync::Arc;

use crate::state::AppState;

pub mod changelog;
pub mod desktop;
pub mod media;
pub mod playlist;
pub mod session;
pub mod share;
pub mod upload;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(share::blank))
        .route("/s/{video_id}", get(share::video))
        .route("/media/{*key}", get(media::get))
        .route("/up/{*key}", put(media::put).post(media::put))
        .nest("/api", api_router())
        .with_state(state)
}

fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/desktop/session/request", get(session::request))
        .route("/desktop/session/request", post(session::request_post))
        .route("/desktop/user/profile", get(desktop::user_profile))
        .route("/desktop/plan", get(desktop::plan))
        .route("/desktop/organizations", get(desktop::organizations))
        .route("/desktop/s3/config/get", get(desktop::s3_config_get))
        .route(
            "/desktop/storage/integrations",
            get(desktop::storage_integrations),
        )
        .route("/desktop/video/create", get(desktop::video_create))
        .route("/desktop/video/delete", delete(desktop::video_delete))
        .route("/desktop/video/progress", post(desktop::video_progress))
        .route("/desktop/feedback", post(desktop::feedback))
        .route("/desktop/logs", post(desktop::logs))
        .route("/upload/signed", post(upload::signed))
        .route("/upload/signed/batch", post(upload::signed_batch))
        .route(
            "/upload/multipart/initiate",
            post(upload::multipart_initiate),
        )
        .route(
            "/upload/multipart/presign-part",
            post(upload::multipart_presign_part),
        )
        .route(
            "/upload/multipart/complete",
            post(upload::multipart_complete),
        )
        .route("/upload/multipart/abort", post(upload::multipart_abort))
        .route(
            "/upload/recording-complete",
            post(upload::recording_complete),
        )
        .route("/changelog", get(changelog::posts))
        .route("/changelog/status", get(changelog::status))
        .route("/playlist", get(playlist::get))
}

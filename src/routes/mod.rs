use axum::Router;
use axum::routing::{delete, get, post, put};
use std::sync::Arc;

use crate::state::AppState;

pub mod access;
pub mod auth;
pub mod changelog;
pub mod collaboration;
pub mod desktop;
pub mod embed;
pub mod health;
pub mod media;
pub mod playlist;
pub mod session;
pub mod share;
pub mod upload;
pub mod videos;

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/health", get(health::health))
        .route("/", get(share::blank))
        .route("/s/{video_id}", get(share::video))
        .route("/embed/{video_id}", get(embed::embed))
        .route("/media/{*key}", get(media::get))
        .route("/up/{*key}", put(media::put).post(media::put))
        .nest("/api", api_router())
        .with_state(state)
}

fn api_router() -> Router<Arc<AppState>> {
    Router::new()
        .route("/desktop/session/request", get(session::request))
        .route("/desktop/session/request", post(session::request_post))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/oembed", get(embed::oembed))
        .route("/videos", get(videos::list))
        .route(
            "/videos/{video_id}",
            get(videos::get)
                .patch(videos::patch)
                .delete(videos::delete_video),
        )
        .route("/videos/{video_id}/status", get(videos::status))
        .route("/videos/{video_id}/download", get(videos::download))
        .route(
            "/videos/{video_id}/access",
            axum::routing::patch(access::set_access),
        )
        .route(
            "/public/videos/{video_id}/access/unlock",
            post(access::unlock),
        )
        .route(
            "/videos/{video_id}/captions",
            get(collaboration::owner_captions).post(collaboration::create_caption),
        )
        .route(
            "/videos/{video_id}/captions/{caption_id}",
            put(collaboration::patch_caption)
                .patch(collaboration::patch_caption)
                .delete(collaboration::delete_caption),
        )
        .route(
            "/videos/{video_id}/comments",
            get(collaboration::list_comments).post(collaboration::create_comment),
        )
        .route(
            "/videos/{video_id}/comments/{comment_id}",
            get(collaboration::get_comment)
                .patch(collaboration::patch_comment)
                .delete(collaboration::delete_comment),
        )
        .route(
            "/videos/{video_id}/reactions",
            get(collaboration::list_reactions).put(collaboration::toggle_reaction),
        )
        .route("/videos/{video_id}/views", get(collaboration::view_totals))
        .route(
            "/videos/{video_id}/collaboration",
            get(collaboration::owner_settings).patch(collaboration::patch_settings),
        )
        .route(
            "/public/videos/{video_id}/captions",
            get(collaboration::public_captions),
        )
        .route(
            "/public/videos/{video_id}/reactions",
            get(collaboration::public_reactions),
        )
        .route(
            "/public/videos/{video_id}/views",
            post(collaboration::record_view),
        )
        .route(
            "/public/videos/{video_id}/collaboration",
            get(collaboration::public_settings),
        )
        .route("/api-keys", get(videos::list_api_keys))
        .route("/api-keys/{key_id}", delete(videos::revoke_api_key))
        .route("/desktop/user/profile", get(desktop::user_profile))
        .route("/desktop/plan", get(desktop::plan))
        .route("/desktop/organizations", get(desktop::organizations))
        .route("/desktop/s3/config/get", get(desktop::s3_config_get))
        .route(
            "/desktop/storage/integrations",
            get(desktop::storage_integrations),
        )
        .route("/desktop/video/create", get(desktop::video_create))
        .route("/desktop/video/status", get(desktop::video_status))
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

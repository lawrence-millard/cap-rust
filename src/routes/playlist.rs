use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;

use crate::auth::OptionalUser;
use crate::error::ApiError;
use crate::routes::access;
use crate::state::AppState;
use crate::storage;

fn read_url(state: &AppState, backend: &str, key: &str) -> Result<String, ApiError> {
    match backend {
        "local" => Ok(state.signer.get_url(&state.config.web_url, key, 86400)),
        "s3" => state
            .config
            .s3
            .as_ref()
            .ok_or_else(|| ApiError::Internal("S3 backend is not configured".into()))?
            .presign_get_now(key, Duration::from_secs(86400))
            .map_err(|e| ApiError::Internal(e.to_string())),
        _ => Err(ApiError::Internal("invalid video storage backend".into())),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct PlaylistParams {
    pub video_id: String,
    pub video_type: String,
    pub user_id: Option<String>,
    pub require_complete: Option<String>,
    /// When true, treat this as an explicit download (not streaming playback).
    pub download: Option<String>,
}

fn wants_download(params: &PlaylistParams) -> bool {
    matches!(
        params
            .download
            .as_deref()
            .map(|v| v.to_ascii_lowercase())
            .as_deref(),
        Some("1" | "true" | "yes")
    )
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PlaylistParams>,
    headers: HeaderMap,
    OptionalUser(user): OptionalUser,
) -> Result<impl IntoResponse, ApiError> {
    let video_id = &params.video_id;

    // fetch video
    let row = sqlx::query_as::<_, (String, String, Option<serde_json::Value>, bool, Option<f64>, Option<i32>, Option<i32>, Option<f64>, String, String, bool, i32)>(
        "SELECT id, owner_id, source, is_screenshot, duration, width, height, fps, access_mode, storage_backend, downloads_enabled, access_cookie_epoch FROM videos WHERE id = $1",
    )
    .bind(video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    if !access::policy_allows(
        &row.8,
        &row.1,
        video_id,
        &headers,
        user.as_ref(),
        &state.signer,
        row.11,
    ) {
        return Err(ApiError::NotFound);
    }

    let is_owner = user.as_ref().is_some_and(|u| u.user_id() == row.1);
    if wants_download(&params) && !row.10 && !is_owner {
        return Err(ApiError::Forbidden);
    }

    let owner_id = &row.1;
    let backend = &row.9;

    match params.video_type.as_str() {
        "mp4" => {
            if backend == "s3" {
                let key = format!("{owner_id}/{video_id}/result.mp4");
                return Ok(Redirect::to(&read_url(&state, backend, &key)?).into_response());
            }
            // try result.mp4, then raw-upload.mp4
            let key = format!("{owner_id}/{video_id}/result.mp4");
            if storage::exists(&state, &key) {
                let url = state.signer.get_url(&state.config.web_url, &key, 86400);
                return Ok(Redirect::to(&url).into_response());
            }
            let key = format!("{owner_id}/{video_id}/raw-upload.mp4");
            if storage::exists(&state, &key) {
                let url = state.signer.get_url(&state.config.web_url, &key, 86400);
                return Ok(Redirect::to(&url).into_response());
            }
            Err(ApiError::NotFound)
        }
        "segments-master" => {
            let seg_prefix = format!("{owner_id}/{video_id}/segments");
            let manifest_key = format!("{seg_prefix}/manifest.json");
            if !storage::exists(&state, &manifest_key) {
                return Err(ApiError::NotFound);
            }
            let has_video = storage::exists(&state, &format!("{seg_prefix}/video/init.mp4"));
            if !has_video {
                return Err(ApiError::NotFound);
            }
            let has_audio = storage::exists(&state, &format!("{seg_prefix}/audio/init.mp4"));
            let audio_url = if has_audio {
                format!("/api/playlist?videoId={video_id}&videoType=segments-audio")
            } else {
                String::new()
            };
            let mut playlist =
                String::from("#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-INDEPENDENT-SEGMENTS\n");
            if has_audio {
                playlist.push_str(&format!(
                    "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"default\",DEFAULT=YES,AUTOSELECT=YES,URI=\"{audio_url}\"\n"
                ));
                playlist.push_str("#EXT-X-STREAM-INF:BANDWIDTH=2000000,AUDIO=\"audio\"\n");
            } else {
                playlist.push_str("#EXT-X-STREAM-INF:BANDWIDTH=2000000\n");
            }
            playlist.push_str(&format!(
                "/api/playlist?videoId={video_id}&videoType=segments-video\n"
            ));
            Ok((
                StatusCode::OK,
                [
                    ("Content-Type", "application/vnd.apple.mpegurl"),
                    ("Cache-Control", "no-cache"),
                ],
                playlist,
            )
                .into_response())
        }
        "segments-video" | "segments-audio" => {
            let kind = if params.video_type == "segments-video" {
                "video"
            } else {
                "audio"
            };
            let seg_prefix = format!("{owner_id}/{video_id}/segments/{kind}");

            // list segment files
            let seg_dir = storage::resolve(&state, &seg_prefix)?;
            let mut segs: Vec<String> = Vec::new();
            let mut init_exists = false;
            if let Ok(mut entries) = tokio::fs::read_dir(&seg_dir).await {
                let mut names: Vec<String> = Vec::new();
                while let Some(entry) = entries.next_entry().await.unwrap_or(None) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name == "init.mp4" {
                        init_exists = true;
                    } else if name.ends_with(".m4s") {
                        names.push(name);
                    }
                }
                names.sort();
                segs = names;
            }

            if !init_exists || segs.is_empty() {
                return Err(ApiError::NotFound);
            }

            // build signed init URL
            let init_key = format!("{seg_prefix}/init.mp4");
            let init_url = state
                .signer
                .get_url(&state.config.web_url, &init_key, 86400);

            // build segment URLs
            let mut playlist = String::from("#EXTM3U\n#EXT-X-VERSION:7\n");
            let target_duration = 6u64;
            playlist.push_str(&format!("#EXT-X-TARGETDURATION:{target_duration}\n"));
            playlist.push_str("#EXT-X-MEDIA-SEQUENCE:0\n");
            playlist.push_str(&format!("#EXT-X-MAP:URI=\"{init_url}\"\n"));

            for seg in &segs {
                let seg_key = format!("{seg_prefix}/{seg}");
                let seg_url = state.signer.get_url(&state.config.web_url, &seg_key, 86400);
                playlist.push_str("#EXTINF:2.000,\n");
                playlist.push_str(&seg_url);
                playlist.push('\n');
            }
            playlist.push_str("#EXT-X-ENDLIST\n");

            Ok((
                StatusCode::OK,
                [
                    ("Content-Type", "application/vnd.apple.mpegurl"),
                    ("Cache-Control", "no-cache"),
                ],
                playlist,
            )
                .into_response())
        }
        "raw-preview" => {
            let key = format!("{owner_id}/{video_id}/raw-upload.mp4");
            if backend == "s3" {
                return Ok(Redirect::to(&read_url(&state, backend, &key)?).into_response());
            }
            if storage::exists(&state, &key) {
                let url = state.signer.get_url(&state.config.web_url, &key, 86400);
                return Ok(Redirect::to(&url).into_response());
            }
            Err(ApiError::NotFound)
        }
        _ => Err(ApiError::NotFound),
    }
}

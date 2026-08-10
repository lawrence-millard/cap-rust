use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Redirect, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;

use crate::auth::CurrentUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage;

type VideoRow = (
    String,
    Option<String>,
    Option<Value>,
    bool,
    Option<Value>,
    Option<f64>,
    Option<i32>,
    Option<i32>,
    Option<f64>,
    bool,
    DateTime<Utc>,
    DateTime<Utc>,
    Option<String>,
    Option<String>,
);

const VIDEO_COLUMNS: &str = "id, name, metadata, public, source, duration, width, height, fps, is_screenshot, created_at, updated_at, mux_status, mux_error";

#[derive(Deserialize)]
pub struct ListQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchVideo {
    name: Option<String>,
    metadata: Option<Value>,
    visibility: Option<String>,
    public: Option<bool>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ApiKeySummary {
    id: i64,
    source: String,
    created_at: DateTime<Utc>,
}

fn video_json(row: VideoRow) -> Value {
    json!({
        "id": row.0,
        "name": row.1,
        "metadata": row.2,
        "visibility": if row.3 { "public" } else { "private" },
        "public": row.3,
        "source": row.4,
        "duration": row.5,
        "width": row.6,
        "height": row.7,
        "fps": row.8,
        "isScreenshot": row.9,
        "createdAt": row.10,
        "updatedAt": row.11,
        "muxStatus": row.12,
        "muxError": row.13,
    })
}

fn validate_patch(body: &PatchVideo) -> Result<Option<bool>, ApiError> {
    if let Some(name) = &body.name
        && (name.trim().is_empty() || name.len() > 200)
    {
        return Err(ApiError::BadRequest("name must be 1-200 characters".into()));
    }
    if let Some(metadata) = &body.metadata {
        if !metadata.is_object() {
            return Err(ApiError::BadRequest("metadata must be an object".into()));
        }
        if serde_json::to_vec(metadata).map_or(true, |v| v.len() > 64 * 1024) {
            return Err(ApiError::BadRequest(
                "metadata must be at most 64 KiB".into(),
            ));
        }
    }

    let visibility = match body.visibility.as_deref() {
        None => None,
        Some("public") => Some(true),
        Some("private") => Some(false),
        Some(_) => {
            return Err(ApiError::BadRequest(
                "visibility must be public or private".into(),
            ));
        }
    };
    if visibility.is_some() && body.public.is_some() && visibility != body.public {
        return Err(ApiError::BadRequest(
            "visibility and public disagree".into(),
        ));
    }
    Ok(visibility.or(body.public))
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or(0);
    if !(1..=100).contains(&limit) || offset < 0 {
        return Err(ApiError::BadRequest(
            "limit must be 1-100 and offset must be non-negative".into(),
        ));
    }
    let sql = format!(
        "SELECT {VIDEO_COLUMNS} FROM videos WHERE owner_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3"
    );
    let rows = sqlx::query_as::<_, VideoRow>(&sql)
        .bind(user.user_id())
        .bind(limit)
        .bind(offset)
        .fetch_all(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(Json(json!({
        "videos": rows.into_iter().map(video_json).collect::<Vec<_>>(),
        "limit": limit,
        "offset": offset,
    })))
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let sql = format!("SELECT {VIDEO_COLUMNS} FROM videos WHERE id = $1 AND owner_id = $2");
    let row = sqlx::query_as::<_, VideoRow>(&sql)
        .bind(video_id)
        .bind(user.user_id())
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(video_json(row)))
}

pub async fn patch(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    Json(body): Json<PatchVideo>,
) -> Result<Json<Value>, ApiError> {
    let public = validate_patch(&body)?;
    let name = body.name.map(|name| name.trim().to_string());
    let sql = format!(
        "UPDATE videos SET name = COALESCE($3, name), metadata = COALESCE($4, metadata), public = COALESCE($5, public), updated_at = now() WHERE id = $1 AND owner_id = $2 RETURNING {VIDEO_COLUMNS}"
    );
    let row = sqlx::query_as::<_, VideoRow>(&sql)
        .bind(video_id)
        .bind(user.user_id())
        .bind(name)
        .bind(body.metadata)
        .bind(public)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
    Ok(Json(video_json(row)))
}

pub async fn delete_video(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
) -> Result<StatusCode, ApiError> {
    if !delete_owned_video(&state, user.user_id(), &video_id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) async fn delete_owned_video(
    state: &AppState,
    owner_id: &str,
    video_id: &str,
) -> Result<bool, ApiError> {
    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT owner_id, storage_backend FROM videos WHERE id = $1",
    )
    .bind(video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let Some((stored_owner_id, backend)) = row else {
        return Ok(false);
    };
    if stored_owner_id != owner_id {
        return Err(ApiError::Forbidden);
    }
    if backend == "s3" {
        return Err(ApiError::BadRequest(
            "S3 video deletion is not supported; video was retained".into(),
        ));
    }

    let video_dir = storage::resolve(state, &format!("{owner_id}/{video_id}"))?;
    let trash_dir = video_dir.with_file_name(format!(".{video_id}.trash-{}", uuid::Uuid::new_v4()));
    let moved = match tokio::fs::rename(&video_dir, &trash_dir).await {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => return Err(ApiError::Internal(format!("trash video: {error}"))),
    };

    let deleted = sqlx::query("DELETE FROM videos WHERE id = $1 AND owner_id = $2")
        .bind(video_id)
        .bind(owner_id)
        .execute(&state.db)
        .await;
    if let Err(error) = deleted {
        if moved && let Err(restore_error) = tokio::fs::rename(&trash_dir, &video_dir).await {
            return Err(ApiError::Internal(format!(
                "delete video: {error}; restore video: {restore_error}"
            )));
        }
        return Err(ApiError::Internal(error.to_string()));
    }
    if moved {
        tokio::fs::remove_dir_all(&trash_dir)
            .await
            .map_err(|e| ApiError::Internal(format!("remove trashed video: {e}")))?;
    }
    Ok(true)
}

pub async fn status(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query_as::<
        _,
        (
            String,
            bool,
            Option<String>,
            Option<String>,
            Option<i64>,
            Option<i64>,
            Option<String>,
            Option<DateTime<Utc>>,
            String,
        ),
    >(
        "SELECT v.owner_id, v.is_screenshot, v.mux_status, v.mux_error, u.uploaded, u.total, u.phase, u.updated_at, v.storage_backend FROM videos v LEFT JOIN video_uploads u ON u.video_id = v.id WHERE v.id = $1 AND v.owner_id = $2",
    )
    .bind(&video_id)
    .bind(user.user_id())
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    let ready = if row.8 == "s3" {
        matches!((row.4, row.5), (Some(uploaded), Some(total)) if total > 0 && uploaded >= total)
    } else {
        storage::exists(&state, &format!("{}/{video_id}/result.mp4", row.0))
            || storage::exists(&state, &format!("{}/{video_id}/raw-upload.mp4", row.0))
            || (row.1 && screenshot_key(&state, &row.0, &video_id).await.is_some())
    };
    let status = row.2.as_deref().unwrap_or(if ready {
        "ready"
    } else if row.4.is_some() {
        "uploading"
    } else {
        "created"
    });
    Ok(Json(json!({
        "status": status,
        "error": row.3,
        "uploaded": row.4,
        "total": row.5,
        "phase": row.6,
        "updatedAt": row.7,
    })))
}

pub async fn download(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
) -> Result<Redirect, ApiError> {
    let row = sqlx::query_as::<_, (String, bool, String)>(
        "SELECT owner_id, is_screenshot, storage_backend FROM videos WHERE id = $1 AND owner_id = $2",
    )
    .bind(&video_id)
    .bind(user.user_id())
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    let key = if row.2 == "s3" {
        format!("{}/{video_id}/result.mp4", row.0)
    } else if row.1 {
        screenshot_key(&state, &row.0, &video_id)
            .await
            .ok_or(ApiError::NotFound)?
    } else {
        ["result.mp4", "raw-upload.mp4"]
            .into_iter()
            .map(|name| format!("{}/{video_id}/{name}", row.0))
            .find(|key| storage::exists(&state, key))
            .ok_or(ApiError::NotFound)?
    };
    let url = if row.2 == "s3" {
        state
            .config
            .s3
            .as_ref()
            .ok_or_else(|| ApiError::Internal("S3 backend is not configured".into()))?
            .presign_get_now(&key, Duration::from_secs(3600))
            .map_err(|e| ApiError::Internal(e.to_string()))?
    } else {
        state.signer.get_url(&state.config.web_url, &key, 3600)
    };
    Ok(Redirect::temporary(&url))
}

async fn screenshot_key(state: &AppState, owner_id: &str, video_id: &str) -> Option<String> {
    let dir = storage::resolve(state, &format!("{owner_id}/{video_id}/screenshot")).ok()?;
    let mut entries = tokio::fs::read_dir(dir).await.ok()?;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if [".png", ".jpg", ".jpeg", ".webp"]
            .iter()
            .any(|ext| name.to_ascii_lowercase().ends_with(ext))
        {
            return Some(format!("{owner_id}/{video_id}/screenshot/{name}"));
        }
    }
    None
}

pub async fn list_api_keys(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
) -> Result<Json<Value>, ApiError> {
    let rows = sqlx::query_as::<_, (i64, String, DateTime<Utc>)>(
        "SELECT key_id, source, created_at FROM auth_api_keys WHERE user_id = $1 ORDER BY created_at DESC",
    )
    .bind(user.user_id())
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let keys = rows
        .into_iter()
        .map(|(id, source, created_at)| ApiKeySummary {
            id,
            source,
            created_at,
        })
        .collect::<Vec<_>>();
    Ok(Json(json!({ "apiKeys": keys })))
}

pub async fn revoke_api_key(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(key_id): Path<i64>,
) -> Result<Response, ApiError> {
    let result = sqlx::query("DELETE FROM auth_api_keys WHERE key_id = $1 AND user_id = $2")
        .bind(key_id)
        .bind(user.user_id())
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_visibility_validation() {
        let valid = PatchVideo {
            name: Some(" Recording ".into()),
            metadata: Some(json!({"tag": "demo"})),
            visibility: Some("private".into()),
            public: None,
        };
        assert_eq!(validate_patch(&valid).unwrap(), Some(false));

        let conflict = PatchVideo {
            name: None,
            metadata: None,
            visibility: Some("private".into()),
            public: Some(true),
        };
        assert!(validate_patch(&conflict).is_err());
    }

    #[test]
    fn patch_rejects_bad_metadata_and_name() {
        let invalid = PatchVideo {
            name: Some(" ".into()),
            metadata: Some(json!([])),
            visibility: None,
            public: None,
        };
        assert!(validate_patch(&invalid).is_err());
    }
}

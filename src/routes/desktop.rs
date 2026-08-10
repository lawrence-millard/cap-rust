use axum::extract::{Query, State};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::auth::CurrentUser;
use crate::error::ApiError;
use crate::routes::upload::{self, get_now};
use crate::routes::videos::delete_owned_video;
use crate::state::AppState;

const VIDEO_CREATE_SQL: &str =
    "INSERT INTO videos (id, owner_id, name, source, public, is_screenshot, duration, width, height, fps, created_at, storage_backend)
     VALUES ($1, $2, $3, $4, $12, $5, $6, $7, $8, $9, to_timestamp($10), $11)
     ON CONFLICT (id) DO UPDATE
     SET name = $3, source = $4, is_screenshot = $5, duration = $6, width = $7, height = $8, fps = $9
     WHERE videos.owner_id = EXCLUDED.owner_id
     RETURNING id";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct VideoCreateQuery {
    pub recording_mode: Option<String>,
    pub video_id: Option<String>,
    pub is_screenshot: Option<String>,
    pub name: Option<String>,
    pub duration_in_secs: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<f64>,
    pub org_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct VideoProgressBody {
    pub video_id: String,
    pub uploaded: i64,
    pub total: i64,
    pub updated_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoDeleteQuery {
    pub video_id: String,
}

pub async fn user_profile(user: CurrentUser) -> Json<Value> {
    Json(json!({
        "name": user.0.name,
        "email": user.0.email,
        "username": user.0.username,
        "imageUrl": null,
    }))
}

pub async fn plan(_user: CurrentUser, State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "upgraded": state.config.plan_upgraded,
        "stripeSubscriptionStatus": if state.config.plan_upgraded { Some("active") } else { None },
    }))
}

pub async fn organizations(_user: CurrentUser) -> Json<Value> {
    Json(json!([]))
}

pub async fn s3_config_get(state: State<Arc<AppState>>, _user: CurrentUser) -> Json<Value> {
    Json(json!({
        "config": {
            "provider": "s3",
            "accessKeyId": "",
            "secretAccessKey": "",
            "endpoint": state.config.web_url,
            "bucketName": "cap",
            "region": "auto",
        },
        "source": "default",
        "managedByOrganization": null,
    }))
}

pub async fn storage_integrations(_user: CurrentUser) -> Json<Value> {
    Json(json!({
        "activeProvider": "s3",
        "managedByOrganization": null,
        "googleDrive": {
            "id": null,
            "connected": false,
            "active": false,
            "status": null,
            "displayName": null,
            "storageQuota": null,
        },
    }))
}

pub async fn video_create(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Query(query): Query<VideoCreateQuery>,
) -> Result<Json<Value>, ApiError> {
    let video_id = if let Some(ref vid) = query.video_id {
        upload::validate_component(vid, "videoId")?;
        vid.clone()
    } else {
        uuid::Uuid::new_v4().to_string()
    };

    let recording_mode = query.recording_mode.as_deref().unwrap_or("desktopMP4");
    let source = serde_json::json!({"type": recording_mode});
    let is_screenshot = query.is_screenshot.as_deref() == Some("true");
    if state.config.storage_backend_name() == "s3" {
        if is_screenshot {
            return Err(ApiError::BadRequest(
                "S3 screenshot creation is not supported".into(),
            ));
        }
        if recording_mode != "desktopMP4" {
            return Err(ApiError::BadRequest(
                "S3 supports only desktopMP4 recording creation".into(),
            ));
        }
    }
    let name = query.name.clone().unwrap_or_else(|| "Cap Recording".into());
    let now = get_now();

    let created_id = sqlx::query_scalar::<_, String>(VIDEO_CREATE_SQL)
        .bind(&video_id)
        .bind(user.user_id())
        .bind(&name)
        .bind(&source)
        .bind(is_screenshot)
        .bind(query.duration_in_secs)
        .bind(query.width)
        .bind(query.height)
        .bind(query.fps)
        .bind(now)
        .bind(state.config.storage_backend_name())
        .bind(state.config.video_default_public)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if created_id.is_none() {
        return Err(ApiError::Forbidden);
    }

    if !is_screenshot {
        sqlx::query(
            "INSERT INTO video_uploads (video_id, mode) VALUES ($1, 'singlepart') ON CONFLICT (video_id) DO NOTHING",
        )
        .bind(&video_id)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    Ok(Json(json!({
        "id": video_id,
        "user_id": user.user_id(),
        "aws_region": "n/a",
        "aws_bucket": "n/a",
    })))
}

pub async fn video_progress(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<VideoProgressBody>,
) -> Result<Json<Value>, ApiError> {
    let video_id = &body.video_id;
    if body.uploaded < 0 || body.total <= 0 {
        return Err(ApiError::BadRequest(
            "uploaded must be non-negative and total must be positive".into(),
        ));
    }
    let uploaded = body.uploaded.min(body.total);
    let total = body.total;

    let video: Option<(String, String)> =
        sqlx::query_as("SELECT owner_id, storage_backend FROM videos WHERE id = $1")
            .bind(video_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (owner, backend) = video.ok_or(ApiError::NotFound)?;
    if owner != user.user_id() {
        return Err(ApiError::Forbidden);
    }

    sqlx::query(
        "INSERT INTO video_uploads (video_id, uploaded, total, updated_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (video_id) DO UPDATE
         SET uploaded = GREATEST(video_uploads.uploaded, EXCLUDED.uploaded),
             total = EXCLUDED.total,
             updated_at = now()
         WHERE EXCLUDED.uploaded > video_uploads.uploaded
            OR EXCLUDED.total IS DISTINCT FROM video_uploads.total",
    )
    .bind(video_id)
    .bind(uploaded)
    .bind(total)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    if uploaded >= total && backend != "s3" {
        sqlx::query(
            "DELETE FROM video_uploads
             WHERE video_id = $1 AND mode = 'singlepart' AND uploaded >= total",
        )
        .bind(video_id)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    }

    Ok(Json(json!(true)))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoStatusQuery {
    pub video_id: String,
}

/// GET /api/desktop/video/status?videoId=... — poll the background ffmpeg
/// muxing state for Instant Mode (desktopSegments) recordings.
/// Returns muxStatus: null (not segments), "processing", "complete", or "error".
pub async fn video_status(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Query(query): Query<VideoStatusQuery>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query_as::<_, (Option<String>, Option<String>)>(
        "SELECT mux_status, mux_error FROM videos WHERE id = $1 AND owner_id = $2",
    )
    .bind(&query.video_id)
    .bind(user.user_id())
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    Ok(Json(json!({
        "muxStatus": row.0,
        "muxError": row.1,
    })))
}

pub async fn video_delete(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Query(query): Query<VideoDeleteQuery>,
) -> Result<Json<Value>, ApiError> {
    let video_id = &query.video_id;

    if !delete_owned_video(&state, user.user_id(), video_id).await? {
        return Err(ApiError::NotFound);
    }

    Ok(Json(json!(true)))
}

pub async fn feedback(
    _user: CurrentUser,
    axum::extract::Form(_form): axum::extract::Form<serde_json::Value>,
) -> Json<Value> {
    Json(json!({"success": true}))
}

pub async fn logs(
    _user: CurrentUser,
    axum::extract::Form(_form): axum::extract::Form<serde_json::Value>,
) -> Json<Value> {
    Json(json!({"success": true}))
}

#[cfg(test)]
mod tests {
    use super::VIDEO_CREATE_SQL;

    #[test]
    fn video_create_upsert_requires_same_owner_and_returns_row() {
        assert!(VIDEO_CREATE_SQL.contains("WHERE videos.owner_id = EXCLUDED.owner_id"));
        assert!(VIDEO_CREATE_SQL.contains("RETURNING id"));
    }
}

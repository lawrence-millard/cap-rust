use axum::extract::{Query, State};
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::auth::CurrentUser;
use crate::error::ApiError;
use crate::routes::upload::get_now;
use crate::state::AppState;
use crate::storage;

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
        if vid.contains('/') || vid.contains("..") {
            return Err(ApiError::BadRequest("invalid videoId".into()));
        }
        vid.clone()
    } else {
        uuid::Uuid::new_v4().to_string()
    };

    let recording_mode = query.recording_mode.as_deref().unwrap_or("desktopMP4");
    let source = serde_json::json!({"type": recording_mode});
    let is_screenshot = query.is_screenshot.as_deref() == Some("true");
    let name = query.name.clone().unwrap_or_else(|| "Cap Recording".into());
    let now = get_now();

    sqlx::query(
        "INSERT INTO videos (id, owner_id, name, source, public, is_screenshot, duration, width, height, fps, created_at)
         VALUES ($1, $2, $3, $4, true, $5, $6, $7, $8, $9, to_timestamp($10))
         ON CONFLICT (id) DO UPDATE
         SET name = $3, duration = $6, width = $7, height = $8, fps = $9",
    )
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
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    if !is_screenshot {
        sqlx::query(
            "INSERT INTO video_uploads (video_id, mode) VALUES ($1, 'singlepart') ON CONFLICT (video_id) DO NOTHING",
        )
        .bind(&video_id)
        .execute(&state.db)
        .await
        .ok();
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
    let uploaded = body.uploaded.min(body.total);
    let total = body.total;

    let owner: Option<String> = sqlx::query_scalar("SELECT owner_id FROM videos WHERE id = $1")
        .bind(video_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let owner = owner.ok_or(ApiError::NotFound)?;
    if owner != user.user_id() {
        return Err(ApiError::Forbidden);
    }

    sqlx::query(
        "INSERT INTO video_uploads (video_id, uploaded, total, updated_at)
         VALUES ($1, $2, $3, now())
         ON CONFLICT (video_id) DO UPDATE
         SET uploaded = $2, total = $3, updated_at = now()",
    )
    .bind(video_id)
    .bind(uploaded)
    .bind(total)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    if uploaded >= total {
        sqlx::query("DELETE FROM video_uploads WHERE video_id = $1 AND mode = 'singlepart'")
            .bind(video_id)
            .execute(&state.db)
            .await
            .ok();
    }

    Ok(Json(json!(true)))
}

pub async fn video_delete(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Query(query): Query<VideoDeleteQuery>,
) -> Result<Json<Value>, ApiError> {
    let video_id = &query.video_id;

    let owner: Option<String> = sqlx::query_scalar("SELECT owner_id FROM videos WHERE id = $1")
        .bind(video_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if let Some(owner_id) = owner {
        if owner_id != user.user_id() {
            return Err(ApiError::Forbidden);
        }
        let key = format!("{}/{}", user.user_id(), video_id);
        let _ = storage::remove_dir_all(&state, &key).await;
        sqlx::query("DELETE FROM video_uploads WHERE video_id = $1")
            .bind(video_id)
            .execute(&state.db)
            .await
            .ok();
        sqlx::query("DELETE FROM videos WHERE id = $1")
            .bind(video_id)
            .execute(&state.db)
            .await
            .ok();
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
    axum::extract::Form(_form): axum::extract::Form<serde_json::Value>,
) -> Json<Value> {
    Json(json!({"success": true}))
}

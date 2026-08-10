use axum::extract::{Path, Query, State};
use axum::http::header::{COOKIE, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

use crate::auth::CurrentUser;
use crate::error::ApiError;
use crate::routes::access;
use crate::state::AppState;

const CAPTION_UPLOAD_TTL: i64 = 3600;
const CAPTION_READ_TTL: i64 = 86400;
const MAX_PAGE: i64 = 100;
const REACTIONS: [&str; 6] = ["👍", "❤️", "😂", "😮", "😢", "🎉"];
const VIEW_COOKIE: &str = "cap_visitor";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionInput {
    language: String,
    label: String,
    #[serde(default = "default_true")]
    enabled: bool,
    #[serde(default)]
    is_default: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionPatch {
    language: Option<String>,
    label: Option<String>,
    enabled: Option<bool>,
    is_default: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentInput {
    content: String,
    timestamp_ms: i64,
    parent_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentPatch {
    content: Option<String>,
    timestamp_ms: Option<i64>,
}

#[derive(Deserialize)]
pub struct PageQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

#[derive(Deserialize)]
pub struct ReactionInput {
    emoji: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    downloads_enabled: bool,
}

type CaptionRow = (String, String, String, String, bool, bool, DateTime<Utc>);
type CommentRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    String,
    DateTime<Utc>,
    DateTime<Utc>,
);

fn default_true() -> bool {
    true
}

fn db_error(error: sqlx::Error) -> ApiError {
    ApiError::Internal(error.to_string())
}

fn page(query: PageQuery) -> Result<(i64, i64), ApiError> {
    let limit = query.limit.unwrap_or(50);
    let offset = query.offset.unwrap_or(0);
    if !(1..=MAX_PAGE).contains(&limit) || !(0..=10_000).contains(&offset) {
        return Err(ApiError::BadRequest(
            "limit must be 1-100 and offset must be 0-10000".into(),
        ));
    }
    Ok((limit, offset))
}

fn validate_caption(
    language: &str,
    label: &str,
    enabled: bool,
    is_default: bool,
) -> Result<(), ApiError> {
    if !(2..=35).contains(&language.len())
        || !language
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-')
    {
        return Err(ApiError::BadRequest("invalid caption language".into()));
    }
    if label.trim().is_empty() || label.len() > 100 {
        return Err(ApiError::BadRequest(
            "label must be 1-100 characters".into(),
        ));
    }
    if is_default && !enabled {
        return Err(ApiError::BadRequest(
            "default caption must be enabled".into(),
        ));
    }
    Ok(())
}

fn validate_comment(content: &str, timestamp_ms: i64) -> Result<(), ApiError> {
    if content.trim().is_empty() || content.len() > 2000 {
        return Err(ApiError::BadRequest(
            "content must be 1-2000 characters".into(),
        ));
    }
    if !(0..=86_400_000).contains(&timestamp_ms) {
        return Err(ApiError::BadRequest(
            "timestampMs must be 0-86400000".into(),
        ));
    }
    Ok(())
}

fn validate_reaction(emoji: &str) -> Result<(), ApiError> {
    if REACTIONS.contains(&emoji) {
        Ok(())
    } else {
        Err(ApiError::BadRequest("unsupported emoji".into()))
    }
}

async fn require_owner(
    state: &AppState,
    video_id: &str,
    user_id: &str,
) -> Result<(String, String), ApiError> {
    sqlx::query_as("SELECT owner_id, storage_backend FROM videos WHERE id = $1 AND owner_id = $2")
        .bind(video_id)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await
        .map_err(db_error)?
        .ok_or(ApiError::NotFound)
}

async fn require_visible(
    state: &AppState,
    video_id: &str,
    headers: &HeaderMap,
    user: Option<&CurrentUser>,
) -> Result<String, ApiError> {
    let (owner_id, access_mode, backend, epoch): (String, String, String, i32) = sqlx::query_as(
        "SELECT owner_id, access_mode, storage_backend, access_cookie_epoch FROM videos WHERE id = $1",
    )
    .bind(video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(db_error)?
    .ok_or(ApiError::NotFound)?;
    if access::policy_allows(
        &access_mode,
        &owner_id,
        video_id,
        headers,
        user,
        &state.signer,
        epoch,
    ) {
        Ok(backend)
    } else {
        Err(ApiError::NotFound)
    }
}

fn caption_url(state: &AppState, backend: &str, key: &str, put: bool) -> Result<String, ApiError> {
    let ttl = if put {
        CAPTION_UPLOAD_TTL
    } else {
        CAPTION_READ_TTL
    };
    match backend {
        "local" if put => Ok(state.signer.put_url(&state.config.web_url, key, ttl)),
        "local" => Ok(state.signer.get_url(&state.config.web_url, key, ttl)),
        "s3" => {
            let s3 = state
                .config
                .s3
                .as_ref()
                .ok_or_else(|| ApiError::Internal("S3 backend is not configured".into()))?;
            let expires = Duration::from_secs(ttl as u64);
            if put {
                s3.presign_put_now(key, expires)
            } else {
                s3.presign_get_now(key, expires)
            }
            .map_err(|e| ApiError::Internal(e.to_string()))
        }
        _ => Err(ApiError::Internal("invalid video storage backend".into())),
    }
}

fn caption_json(
    state: &AppState,
    backend: &str,
    row: CaptionRow,
    upload_url: Option<String>,
) -> Result<Value, ApiError> {
    let mut value = json!({
        "id": row.0,
        "language": row.1,
        "label": row.2,
        "url": caption_url(state, backend, &row.3, false)?,
        "enabled": row.4,
        "isDefault": row.5,
        "createdAt": row.6,
    });
    if let Some(url) = upload_url {
        value["upload"] =
            json!({"url": url, "method": "PUT", "headers": {"content-type": "text/vtt"}});
    }
    Ok(value)
}

fn comment_json(row: CommentRow) -> Value {
    json!({
        "id": row.0,
        "parentId": row.2,
        "author": {"id": row.1, "name": row.3, "username": row.4, "imageUrl": row.5},
        "timestampMs": row.6,
        "content": row.7,
        "createdAt": row.8,
        "updatedAt": row.9,
    })
}

pub async fn create_caption(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    Json(body): Json<CaptionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    validate_caption(&body.language, &body.label, body.enabled, body.is_default)?;
    let (owner, backend) = require_owner(&state, &video_id, user.user_id()).await?;
    let id = uuid::Uuid::new_v4().to_string();
    let key = format!("{owner}/{video_id}/captions/{id}.vtt");
    let mut tx = state.db.begin().await.map_err(db_error)?;
    if body.is_default {
        sqlx::query(
            "UPDATE video_captions SET is_default = false, updated_at = now() WHERE video_id = $1",
        )
        .bind(&video_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    }
    let row = sqlx::query_as::<_, CaptionRow>(
        "INSERT INTO video_captions (id, video_id, language, label, storage_key, enabled, is_default) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING id, language, label, storage_key, enabled, is_default, created_at",
    )
    .bind(&id).bind(&video_id).bind(&body.language).bind(body.label.trim()).bind(&key)
    .bind(body.enabled).bind(body.is_default).fetch_one(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    let upload = caption_url(&state, &backend, &key, true)?;
    Ok((
        StatusCode::CREATED,
        Json(caption_json(&state, &backend, row, Some(upload))?),
    ))
}

pub async fn owner_captions(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (_, backend) = require_owner(&state, &video_id, user.user_id()).await?;
    let rows = sqlx::query_as::<_, CaptionRow>(
        "SELECT id, language, label, storage_key, enabled, is_default, created_at FROM video_captions WHERE video_id = $1 ORDER BY created_at, id",
    ).bind(video_id).fetch_all(&state.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"captions": rows.into_iter().map(|row| caption_json(&state, &backend, row, None)).collect::<Result<Vec<_>, _>>()?}),
    ))
}

pub async fn public_captions(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let backend = require_visible(&state, &video_id, &headers, None).await?;
    let rows = sqlx::query_as::<_, CaptionRow>(
        "SELECT id, language, label, storage_key, enabled, is_default, created_at FROM video_captions WHERE video_id = $1 AND enabled ORDER BY is_default DESC, created_at, id",
    ).bind(video_id).fetch_all(&state.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"captions": rows.into_iter().map(|row| caption_json(&state, &backend, row, None)).collect::<Result<Vec<_>, _>>()?}),
    ))
}

pub async fn patch_caption(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path((video_id, caption_id)): Path<(String, String)>,
    Json(body): Json<CaptionPatch>,
) -> Result<Json<Value>, ApiError> {
    let (_, backend) = require_owner(&state, &video_id, user.user_id()).await?;
    let current = sqlx::query_as::<_, (String, String, bool, bool)>(
        "SELECT language, label, enabled, is_default FROM video_captions WHERE id = $1 AND video_id = $2",
    ).bind(&caption_id).bind(&video_id).fetch_optional(&state.db).await.map_err(db_error)?.ok_or(ApiError::NotFound)?;
    let language = body.language.as_deref().unwrap_or(&current.0);
    let label = body.label.as_deref().unwrap_or(&current.1);
    let enabled = body.enabled.unwrap_or(current.2);
    let is_default = body.is_default.unwrap_or(current.3);
    validate_caption(language, label, enabled, is_default)?;
    let mut tx = state.db.begin().await.map_err(db_error)?;
    if is_default {
        sqlx::query("UPDATE video_captions SET is_default = false, updated_at = now() WHERE video_id = $1 AND id <> $2")
            .bind(&video_id).bind(&caption_id).execute(&mut *tx).await.map_err(db_error)?;
    }
    let row = sqlx::query_as::<_, CaptionRow>(
        "UPDATE video_captions SET language=$3, label=$4, enabled=$5, is_default=$6, updated_at=now() WHERE id=$1 AND video_id=$2 RETURNING id, language, label, storage_key, enabled, is_default, created_at",
    ).bind(caption_id).bind(video_id).bind(language).bind(label.trim()).bind(enabled).bind(is_default)
    .fetch_one(&mut *tx).await.map_err(db_error)?;
    tx.commit().await.map_err(db_error)?;
    Ok(Json(caption_json(&state, &backend, row, None)?))
}

pub async fn delete_caption(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path((video_id, caption_id)): Path<(String, String)>,
) -> Result<StatusCode, ApiError> {
    let (_, backend) = require_owner(&state, &video_id, user.user_id()).await?;
    let mut tx = state.db.begin().await.map_err(db_error)?;
    let storage_key: String = sqlx::query_scalar(
        "SELECT storage_key FROM video_captions WHERE id = $1 AND video_id = $2 FOR UPDATE",
    )
    .bind(&caption_id)
    .bind(&video_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_error)?
    .ok_or(ApiError::NotFound)?;
    if backend == "s3" {
        return Err(ApiError::BadRequest(
            "S3 caption deletion is not supported".into(),
        ));
    }
    let result = sqlx::query("DELETE FROM video_captions WHERE id = $1 AND video_id = $2")
        .bind(caption_id)
        .bind(video_id)
        .execute(&mut *tx)
        .await
        .map_err(db_error)?;
    debug_assert_eq!(result.rows_affected(), 1);
    let path = crate::storage::resolve(&state, &storage_key)?;
    let trash = path.with_file_name(format!(
        ".{}.trash-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("caption"),
        uuid::Uuid::new_v4()
    ));
    let moved = match tokio::fs::rename(&path, &trash).await {
        Ok(()) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(ApiError::Internal(format!("trash caption object: {error}")));
        }
    };
    if let Err(error) = tx.commit().await {
        if moved && let Err(restore_error) = tokio::fs::rename(&trash, &path).await {
            return Err(ApiError::Internal(format!(
                "delete caption: {error}; restore caption object: {restore_error}"
            )));
        }
        return Err(db_error(error));
    }
    if moved {
        tokio::fs::remove_file(&trash)
            .await
            .map_err(|error| ApiError::Internal(format!("remove trashed caption: {error}")))?;
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_comments(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    Query(query): Query<PageQuery>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_visible(&state, &video_id, &headers, Some(&user)).await?;
    let (limit, offset) = page(query)?;
    let rows = sqlx::query_as::<_, CommentRow>(
        "SELECT c.id, c.author_id, c.parent_id, u.name, u.username, u.image_url, c.timestamp_ms, c.content, c.created_at, c.updated_at FROM video_comments c JOIN users u ON u.id=c.author_id WHERE c.video_id=$1 ORDER BY c.created_at DESC, c.id DESC LIMIT $2 OFFSET $3",
    ).bind(video_id).bind(limit).bind(offset).fetch_all(&state.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"comments": rows.into_iter().map(comment_json).collect::<Vec<_>>(), "limit": limit, "offset": offset}),
    ))
}

pub async fn get_comment(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path((video_id, comment_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_visible(&state, &video_id, &headers, Some(&user)).await?;
    let row = sqlx::query_as::<_, CommentRow>(
        "SELECT c.id, c.author_id, c.parent_id, u.name, u.username, u.image_url, c.timestamp_ms, c.content, c.created_at, c.updated_at FROM video_comments c JOIN users u ON u.id=c.author_id WHERE c.video_id=$1 AND c.id=$2",
    ).bind(video_id).bind(comment_id).fetch_optional(&state.db).await.map_err(db_error)?.ok_or(ApiError::NotFound)?;
    Ok(Json(comment_json(row)))
}

pub async fn create_comment(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CommentInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    require_visible(&state, &video_id, &headers, Some(&user)).await?;
    validate_comment(&body.content, body.timestamp_ms)?;
    if let Some(parent_id) = &body.parent_id {
        let parent_exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM video_comments WHERE id=$1 AND video_id=$2 AND parent_id IS NULL)")
            .bind(parent_id).bind(&video_id).fetch_one(&state.db).await.map_err(db_error)?;
        if !parent_exists {
            return Err(ApiError::BadRequest("parent comment not found".into()));
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    let row = sqlx::query_as::<_, CommentRow>(
        "WITH inserted AS (INSERT INTO video_comments (id, video_id, author_id, parent_id, timestamp_ms, content) VALUES ($1,$2,$3,$4,$5,$6) RETURNING *) SELECT c.id,c.author_id,c.parent_id,u.name,u.username,u.image_url,c.timestamp_ms,c.content,c.created_at,c.updated_at FROM inserted c JOIN users u ON u.id=c.author_id",
    ).bind(id).bind(video_id).bind(user.user_id()).bind(body.parent_id).bind(body.timestamp_ms).bind(body.content.trim())
    .fetch_one(&state.db).await.map_err(db_error)?;
    Ok((StatusCode::CREATED, Json(comment_json(row))))
}

pub async fn patch_comment(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path((video_id, comment_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(body): Json<CommentPatch>,
) -> Result<Json<Value>, ApiError> {
    require_visible(&state, &video_id, &headers, Some(&user)).await?;
    if body.content.is_none() && body.timestamp_ms.is_none() {
        return Err(ApiError::BadRequest(
            "content or timestampMs required".into(),
        ));
    }
    let current = sqlx::query_as::<_, (String, i64)>("SELECT content,timestamp_ms FROM video_comments WHERE id=$1 AND video_id=$2 AND author_id=$3")
        .bind(&comment_id).bind(&video_id).bind(user.user_id()).fetch_optional(&state.db).await.map_err(db_error)?.ok_or(ApiError::NotFound)?;
    let content = body.content.as_deref().unwrap_or(&current.0);
    let timestamp = body.timestamp_ms.unwrap_or(current.1);
    validate_comment(content, timestamp)?;
    let row = sqlx::query_as::<_, CommentRow>(
        "WITH changed AS (UPDATE video_comments SET content=$4,timestamp_ms=$5,updated_at=now() WHERE id=$1 AND video_id=$2 AND author_id=$3 RETURNING *) SELECT c.id,c.author_id,c.parent_id,u.name,u.username,u.image_url,c.timestamp_ms,c.content,c.created_at,c.updated_at FROM changed c JOIN users u ON u.id=c.author_id",
    ).bind(comment_id).bind(video_id).bind(user.user_id()).bind(content.trim()).bind(timestamp)
    .fetch_one(&state.db).await.map_err(db_error)?;
    Ok(Json(comment_json(row)))
}

pub async fn delete_comment(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path((video_id, comment_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    require_visible(&state, &video_id, &headers, Some(&user)).await?;
    let result = sqlx::query(
        "DELETE FROM video_comments c USING videos v WHERE c.id=$1 AND c.video_id=$2 AND v.id=c.video_id AND (c.author_id=$3 OR v.owner_id=$3)",
    ).bind(comment_id).bind(video_id).bind(user.user_id()).execute(&state.db).await.map_err(db_error)?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

pub async fn toggle_reaction(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ReactionInput>,
) -> Result<Json<Value>, ApiError> {
    require_visible(&state, &video_id, &headers, Some(&user)).await?;
    validate_reaction(&body.emoji)?;
    // Atomic toggle: delete if present, otherwise insert.
    let toggled = sqlx::query_as::<_, (bool,)>(
        "WITH deleted AS ( \
            DELETE FROM video_reactions \
            WHERE video_id=$1 AND user_id=$2 AND emoji=$3 \
            RETURNING 1 \
         ), inserted AS ( \
            INSERT INTO video_reactions (video_id,user_id,emoji) \
            SELECT $1,$2,$3 WHERE NOT EXISTS (SELECT 1 FROM deleted) \
            ON CONFLICT DO NOTHING \
            RETURNING 1 \
         ) \
         SELECT EXISTS(SELECT 1 FROM inserted) AS active",
    )
    .bind(&video_id)
    .bind(user.user_id())
    .bind(&body.emoji)
    .fetch_one(&state.db)
    .await
    .map_err(db_error)?;
    Ok(Json(json!({"emoji": body.emoji, "active": toggled.0})))
}

pub async fn list_reactions(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_visible(&state, &video_id, &headers, Some(&user)).await?;
    let rows = sqlx::query_as::<_, (String, i64, bool)>(
        "SELECT emoji,count(*),bool_or(user_id=$2) FROM video_reactions WHERE video_id=$1 GROUP BY emoji ORDER BY emoji",
    )
    .bind(video_id)
    .bind(user.user_id())
    .fetch_all(&state.db)
    .await
    .map_err(db_error)?;
    Ok(Json(
        json!({"reactions": rows.into_iter().map(|(emoji,count,active)| json!({"emoji":emoji,"count":count,"active":active})).collect::<Vec<_>>()}),
    ))
}

pub async fn public_reactions(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_visible(&state, &video_id, &headers, None).await?;
    let rows = sqlx::query_as::<_, (String, i64)>(
        "SELECT emoji, count(*) FROM video_reactions WHERE video_id=$1 GROUP BY emoji ORDER BY emoji",
    ).bind(video_id).fetch_all(&state.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"reactions": rows.into_iter().map(|(emoji,count)| json!({"emoji":emoji,"count":count})).collect::<Vec<_>>()}),
    ))
}

fn visitor_from_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|part| {
            let (name, value) = part.trim().split_once('=')?;
            (name == VIEW_COOKIE && uuid::Uuid::parse_str(value).is_ok()).then(|| value.to_string())
        })
}

pub async fn record_view(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<(StatusCode, HeaderMap, Json<Value>), ApiError> {
    require_visible(&state, &video_id, &headers, None).await?;
    let existing = visitor_from_cookie(&headers);
    let visitor = existing
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let result = sqlx::query(
        "INSERT INTO video_views (video_id,visitor_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
    )
    .bind(video_id)
    .bind(&visitor)
    .execute(&state.db)
    .await
    .map_err(db_error)?;
    let mut response_headers = HeaderMap::new();
    if existing.is_none() {
        let secure = if state.config.web_url.starts_with("https://") {
            "; Secure"
        } else {
            ""
        };
        let cookie = format!(
            "{VIEW_COOKIE}={visitor}; Path=/; Max-Age=31536000; HttpOnly; SameSite=Lax{secure}"
        );
        response_headers.insert(
            SET_COOKIE,
            HeaderValue::from_str(&cookie).map_err(|e| ApiError::Internal(e.to_string()))?,
        );
    }
    Ok((
        StatusCode::OK,
        response_headers,
        Json(json!({"counted": result.rows_affected() == 1})),
    ))
}

pub async fn view_totals(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    require_owner(&state, &video_id, user.user_id()).await?;
    let total: i64 = sqlx::query_scalar("SELECT count(*) FROM video_views WHERE video_id=$1")
        .bind(&video_id)
        .fetch_one(&state.db)
        .await
        .map_err(db_error)?;
    let daily = sqlx::query_as::<_, (NaiveDate, i64)>(
        "SELECT viewed_on,count(*) FROM video_views WHERE video_id=$1 GROUP BY viewed_on ORDER BY viewed_on DESC LIMIT 366",
    ).bind(video_id).fetch_all(&state.db).await.map_err(db_error)?;
    Ok(Json(
        json!({"total":total,"daily":daily.into_iter().map(|(date,count)|json!({"date":date,"count":count})).collect::<Vec<_>>()}),
    ))
}

pub async fn owner_settings(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let enabled: bool =
        sqlx::query_scalar("SELECT downloads_enabled FROM videos WHERE id=$1 AND owner_id=$2")
            .bind(video_id)
            .bind(user.user_id())
            .fetch_optional(&state.db)
            .await
            .map_err(db_error)?
            .ok_or(ApiError::NotFound)?;
    Ok(Json(json!({"downloadsEnabled":enabled})))
}

pub async fn patch_settings(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    Json(body): Json<SettingsPatch>,
) -> Result<Json<Value>, ApiError> {
    let enabled: bool = sqlx::query_scalar("UPDATE videos SET downloads_enabled=$3,updated_at=now() WHERE id=$1 AND owner_id=$2 RETURNING downloads_enabled")
        .bind(video_id).bind(user.user_id()).bind(body.downloads_enabled).fetch_optional(&state.db).await.map_err(db_error)?.ok_or(ApiError::NotFound)?;
    Ok(Json(json!({"downloadsEnabled":enabled})))
}

pub async fn public_settings(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    require_visible(&state, &video_id, &headers, None).await?;
    let enabled: bool = sqlx::query_scalar("SELECT downloads_enabled FROM videos WHERE id=$1")
        .bind(video_id)
        .fetch_one(&state.db)
        .await
        .map_err(db_error)?;
    Ok(Json(json!({"downloadsEnabled":enabled})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{self, CurrentUser};
    use crate::sign::Signer;

    #[test]
    fn strict_input_validation() {
        assert!(validate_caption("en-US", "English", true, true).is_ok());
        assert!(validate_caption("../en", "English", true, false).is_err());
        assert!(validate_caption("en", "English", false, true).is_err());
        assert!(validate_comment("hello", 42).is_ok());
        assert!(validate_comment(" ", 42).is_err());
        assert!(validate_comment("hello", 86_400_001).is_err());
        assert!(validate_reaction("🎉").is_ok());
        assert!(validate_reaction("🔥").is_err());
    }

    #[test]
    fn pagination_is_bounded() {
        assert_eq!(
            page(PageQuery {
                limit: Some(100),
                offset: Some(10_000)
            })
            .unwrap(),
            (100, 10_000)
        );
        assert!(
            page(PageQuery {
                limit: Some(101),
                offset: Some(0)
            })
            .is_err()
        );
        assert!(
            page(PageQuery {
                limit: Some(1),
                offset: Some(10_001)
            })
            .is_err()
        );
    }

    #[test]
    fn visitor_cookie_requires_uuid() {
        let mut headers = HeaderMap::new();
        headers.insert(COOKIE, HeaderValue::from_static("other=x; cap_visitor=bad"));
        assert!(visitor_from_cookie(&headers).is_none());
        headers.insert(
            COOKIE,
            HeaderValue::from_static("cap_visitor=67e55044-10b1-426f-9247-bb680e5fe0c8"),
        );
        assert_eq!(
            visitor_from_cookie(&headers).as_deref(),
            Some("67e55044-10b1-426f-9247-bb680e5fe0c8")
        );
    }

    #[test]
    fn collaboration_policy_allows_unlocked_authenticated_non_owner() {
        let signer = Signer::new(b"secret");
        let signed = signer.get_url("", "recording-access/video/0", 900);
        let query = signed.split_once('?').unwrap().1;
        let (exp, sig) = query.split_once('&').unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            COOKIE,
            HeaderValue::from_str(&format!(
                "cap_recording_access=video.0.{}.{}",
                exp.strip_prefix("exp=").unwrap(),
                sig.strip_prefix("sig=").unwrap()
            ))
            .unwrap(),
        );
        let user = CurrentUser(auth::User {
            id: "viewer".into(),
            name: None,
            email: None,
            username: None,
        });

        assert!(access::policy_allows(
            "password",
            "owner",
            "video",
            &headers,
            Some(&user),
            &signer,
            0
        ));
        assert!(access::policy_allows(
            "password", "owner", "video", &headers, None, &signer, 0
        ));
        assert!(!access::policy_allows(
            "private",
            "owner",
            "video",
            &headers,
            Some(&user),
            &signer,
            0
        ));
    }
}

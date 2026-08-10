use axum::extract::State;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};

use crate::auth::CurrentUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage;

const PUT_TTL: i64 = 3600;
const GET_TTL: i64 = 86400;
const MAX_BATCH: usize = 10_000;
const MAX_PARTS: usize = 10_000;
const MAX_PART_SIZE: u64 = 5 * 1024 * 1024 * 1024;
const MAX_UPLOAD_SIZE: u64 = 20 * 1024 * 1024 * 1024;
const MAX_MANIFEST_SIZE: u64 = 8 * 1024 * 1024;
const MAX_SEGMENTS: usize = 100_000;
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MUX_RECOVERY_BATCH: i64 = 10;

struct TempFileGuard {
    path: PathBuf,
    armed: bool,
}

impl TempFileGuard {
    fn new(path: PathBuf) -> Self {
        Self { path, armed: true }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn cleanup(&mut self) {
        match fs::remove_file(&self.path).await {
            Ok(()) => self.disarm(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => self.disarm(),
            Err(e) => tracing::warn!("failed to remove temp file {}: {e}", self.path.display()),
        }
    }
}

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub fn get_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct SignedRequest {
    pub video_id: String,
    pub subpath: String,
    pub method: Option<String>,
    pub duration_in_secs: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignedBatchRequest {
    pub video_id: String,
    pub subpaths: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct MultipartInitiateRequest {
    pub video_id: Option<String>,
    pub subpath: Option<String>,
    pub content_type: Option<String>,
    pub file_key: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartPresignPartRequest {
    pub video_id: Option<String>,
    pub subpath: Option<String>,
    pub file_key: Option<String>,
    pub upload_id: String,
    pub part_number: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Part {
    pub part_number: i32,
    pub etag: String,
    pub size: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartCompleteRequest {
    pub video_id: Option<String>,
    pub subpath: Option<String>,
    pub file_key: Option<String>,
    pub upload_id: String,
    pub parts: Vec<Part>,
    pub duration_in_secs: Option<f64>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub fps: Option<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartAbortRequest {
    pub video_id: Option<String>,
    pub subpath: Option<String>,
    pub file_key: Option<String>,
    pub upload_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingCompleteRequest {
    pub video_id: String,
}

/// Resolve the user+video file key for a request, handling both videoId/subpath and deprecated fileKey.
fn file_key(
    user_id: &str,
    video_id: Option<&str>,
    subpath: Option<&str>,
    file_key: Option<&str>,
) -> Result<String, ApiError> {
    if let Some(fk) = file_key {
        let mut components = fk.split('/');
        if components.next() != Some(user_id) {
            return Err(ApiError::BadRequest("invalid fileKey owner".into()));
        }
        let video = components
            .next()
            .ok_or_else(|| ApiError::BadRequest("invalid fileKey".into()))?;
        validate_component(video, "fileKey")?;
        let subpath: Vec<_> = components.collect();
        if subpath.is_empty() || subpath.iter().any(|part| !valid_component(part)) {
            return Err(ApiError::BadRequest("invalid fileKey".into()));
        }
        return Ok(fk.to_string());
    }
    let vid = video_id.ok_or(ApiError::BadRequest("videoId required".into()))?;
    validate_component(vid, "videoId")?;
    let sub = subpath
        .map(|s| s.to_string())
        .unwrap_or_else(|| "result.mp4".into());
    validate_subpath(&sub)?;
    Ok(format!("{user_id}/{vid}/{sub}"))
}

fn validate_subpath(sub: &str) -> Result<(), ApiError> {
    if sub.is_empty() || sub.split('/').any(|part| !valid_component(part)) {
        return Err(ApiError::BadRequest("invalid subpath".into()));
    }
    Ok(())
}

fn valid_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value.contains('\\')
        && !value.chars().any(char::is_control)
}

fn validate_component(value: &str, field: &str) -> Result<(), ApiError> {
    if !valid_component(value) || value.contains('/') {
        return Err(ApiError::BadRequest(format!("invalid {field}")));
    }
    Ok(())
}

fn video_id_from_key(key: &str) -> &str {
    key.split('/').nth(1).expect("validated file key")
}

fn validate_parts(parts: &[Part]) -> Result<u64, ApiError> {
    if parts.is_empty() || parts.len() > MAX_PARTS {
        return Err(ApiError::BadRequest("invalid part count".into()));
    }
    let mut seen = vec![false; parts.len()];
    let mut total = 0_u64;
    for part in parts {
        let number = usize::try_from(part.part_number)
            .ok()
            .filter(|number| (1..=parts.len()).contains(number))
            .ok_or_else(|| ApiError::BadRequest("parts must be contiguous from 1".into()))?;
        if std::mem::replace(&mut seen[number - 1], true) {
            return Err(ApiError::BadRequest("duplicate part number".into()));
        }
        let size = u64::try_from(part.size)
            .ok()
            .filter(|size| *size > 0 && *size <= MAX_PART_SIZE)
            .ok_or_else(|| ApiError::BadRequest("invalid part size".into()))?;
        if part.etag.trim().is_empty() {
            return Err(ApiError::BadRequest("invalid part etag".into()));
        }
        total = total
            .checked_add(size)
            .filter(|total| *total <= MAX_UPLOAD_SIZE)
            .ok_or_else(|| ApiError::BadRequest("upload exceeds 20 GiB".into()))?;
    }
    Ok(total)
}

async fn assemble_multipart(
    state: &AppState,
    upload_id: &str,
    key: &str,
    parts: &[Part],
) -> Result<(), ApiError> {
    let dest = storage::resolve(state, key)?;
    storage::ensure_parent(state, key).await?;
    let temp = dest.with_file_name(format!(
        ".multipart-{upload_id}-{}.tmp",
        uuid::Uuid::new_v4()
    ));
    let mut guard = TempFileGuard::new(temp);
    let result = async {
        let mut out = BufWriter::new(
            fs::File::create(guard.path())
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?,
        );
        for part in parts {
            let part_key = format!("staging/{upload_id}/{}", part.part_number);
            let part_path = storage::resolve(state, &part_key)?;
            let mut file = fs::File::open(&part_path)
                .await
                .map_err(|e| ApiError::Internal(format!("open part: {e}")))?;
            let metadata = file
                .metadata()
                .await
                .map_err(|e| ApiError::Internal(format!("stat part: {e}")))?;
            if !metadata.is_file() || metadata.len() != part.size as u64 {
                return Err(ApiError::BadRequest(format!(
                    "part {} size mismatch",
                    part.part_number
                )));
            }
            tokio::io::copy(&mut file, &mut out)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        out.flush()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        out.get_ref()
            .sync_all()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        drop(out);
        fs::rename(guard.path(), &dest)
            .await
            .map_err(|e| ApiError::Internal(format!("publish upload: {e}")))
    }
    .await;
    if result.is_ok() {
        guard.disarm();
    } else {
        guard.cleanup().await;
    }
    result
}

async fn verify_upload_association(
    state: &AppState,
    upload_id: &str,
    user_id: &str,
    video_id: &str,
    destination: &str,
) -> Result<String, ApiError> {
    let row = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT owner_id, video_id, destination, status FROM multipart_uploads WHERE upload_id = $1",
    )
    .bind(upload_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;
    if row.0 != user_id || row.1 != video_id || row.2 != destination {
        return Err(ApiError::Forbidden);
    }
    Ok(row.3)
}

async fn verify_video_owned(
    state: &AppState,
    user_id: &str,
    video_id: &str,
) -> Result<String, ApiError> {
    let video: Option<(String, String)> =
        sqlx::query_as("SELECT owner_id, storage_backend FROM videos WHERE id = $1")
            .bind(video_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    match video {
        Some((owner_id, backend)) if owner_id == user_id => Ok(backend),
        Some(_) => Err(ApiError::Forbidden),
        None => Err(ApiError::NotFound),
    }
}

fn put_url(state: &AppState, backend: &str, key: &str, ttl: i64) -> Result<String, ApiError> {
    match backend {
        "local" => Ok(state.signer.put_url(&state.config.web_url, key, ttl)),
        "s3" => state
            .config
            .s3
            .as_ref()
            .ok_or_else(|| ApiError::Internal("S3 backend is not configured".into()))?
            .presign_put_now(key, Duration::from_secs(ttl as u64))
            .map_err(|e| ApiError::Internal(e.to_string())),
        _ => Err(ApiError::Internal("invalid video storage backend".into())),
    }
}

fn get_url(state: &AppState, backend: &str, key: &str, ttl: i64) -> Result<String, ApiError> {
    match backend {
        "local" => Ok(state.signer.get_url(&state.config.web_url, key, ttl)),
        "s3" => state
            .config
            .s3
            .as_ref()
            .ok_or_else(|| ApiError::Internal("S3 backend is not configured".into()))?
            .presign_get_now(key, Duration::from_secs(ttl as u64))
            .map_err(|e| ApiError::Internal(e.to_string())),
        _ => Err(ApiError::Internal("invalid video storage backend".into())),
    }
}

/// POST /api/upload/signed
pub async fn signed(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<SignedRequest>,
) -> Result<Json<Value>, ApiError> {
    let key = file_key(
        user.user_id(),
        Some(&body.video_id),
        Some(&body.subpath),
        None,
    )?;
    let backend = verify_video_owned(&state, user.user_id(), &body.video_id).await?;

    // update video meta if provided
    if body.duration_in_secs.is_some()
        || body.width.is_some()
        || body.height.is_some()
        || body.fps.is_some()
    {
        sqlx::query(
            "UPDATE videos SET duration = $1, width = $2, height = $3, fps = $4 WHERE id = $5",
        )
        .bind(body.duration_in_secs)
        .bind(body.width)
        .bind(body.height)
        .bind(body.fps)
        .bind(&body.video_id)
        .execute(&state.db)
        .await
        .ok();
    }

    let url = put_url(&state, &backend, &key, PUT_TTL)?;
    Ok(Json(json!({
        "presignedPutData": {
            "url": url,
            "fields": {},
            "headers": {},
            "type": "put",
        }
    })))
}

/// POST /api/upload/signed/batch
pub async fn signed_batch(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<SignedBatchRequest>,
) -> Result<Json<Value>, ApiError> {
    if body.subpaths.len() > MAX_BATCH {
        return Err(ApiError::BadRequest("batch exceeds 10000 paths".into()));
    }
    for sub in &body.subpaths {
        validate_subpath(sub)?;
    }
    let backend = verify_video_owned(&state, user.user_id(), &body.video_id).await?;
    let mut urls = serde_json::Map::new();
    let mut uploads = serde_json::Map::new();

    for sub in &body.subpaths {
        let key = format!("{}/{}/{}", user.user_id(), body.video_id, sub);
        let url = put_url(&state, &backend, &key, PUT_TTL)?;
        urls.insert(sub.clone(), Value::String(url.clone()));
        uploads.insert(
            sub.clone(),
            json!({"url": url, "headers": {}, "type": "put"}),
        );
    }

    Ok(Json(json!({ "uploads": uploads, "urls": urls })))
}

/// POST /api/upload/multipart/initiate
pub async fn multipart_initiate(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<MultipartInitiateRequest>,
) -> Result<Json<Value>, ApiError> {
    let key = file_key(
        user.user_id(),
        body.video_id.as_deref(),
        body.subpath.as_deref(),
        body.file_key.as_deref(),
    )?;
    let video_id = video_id_from_key(&key).to_string();
    let backend = verify_video_owned(&state, user.user_id(), &video_id).await?;
    if backend == "s3" {
        return Err(ApiError::BadRequest(
            "S3 multipart uploads are not supported".into(),
        ));
    }

    let upload_id = uuid::Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO multipart_uploads (upload_id, owner_id, video_id, destination) VALUES ($1, $2, $3, $4)",
    )
    .bind(&upload_id)
    .bind(user.user_id())
    .bind(&video_id)
    .bind(&key)
    .execute(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    storage::touch_staging_activity(&state, &upload_id).await?;

    // mark video_uploads as multipart
    sqlx::query(
        "INSERT INTO video_uploads (video_id, mode, phase) VALUES ($1, 'multipart', 'uploading')
         ON CONFLICT (video_id) DO UPDATE SET mode = 'multipart', phase = 'uploading'",
    )
    .bind(&video_id)
    .execute(&state.db)
    .await
    .ok();

    Ok(Json(json!({ "uploadId": upload_id, "provider": "s3" })))
}

/// POST /api/upload/multipart/presign-part
pub async fn multipart_presign_part(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<MultipartPresignPartRequest>,
) -> Result<Json<Value>, ApiError> {
    let key = file_key(
        user.user_id(),
        body.video_id.as_deref(),
        body.subpath.as_deref(),
        body.file_key.as_deref(),
    )?;

    // derive video_id for ownership check
    let video_id = video_id_from_key(&key);
    let backend = verify_video_owned(&state, user.user_id(), video_id).await?;
    if backend == "s3" {
        return Err(ApiError::BadRequest(
            "S3 multipart uploads are not supported".into(),
        ));
    }
    if !(1..=MAX_PARTS as u32).contains(&body.part_number) {
        return Err(ApiError::BadRequest("invalid part number".into()));
    }
    let status =
        verify_upload_association(&state, &body.upload_id, user.user_id(), video_id, &key).await?;
    if status != "uploading" {
        return Err(ApiError::BadRequest("upload is not active".into()));
    }

    storage::touch_staging_activity(&state, &body.upload_id).await?;

    let part_key = format!("staging/{}/{}", body.upload_id, body.part_number);
    let url = state
        .signer
        .put_url(&state.config.web_url, &part_key, PUT_TTL);
    Ok(Json(json!({ "presignedUrl": url, "provider": "s3" })))
}

/// POST /api/upload/multipart/complete
pub async fn multipart_complete(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<MultipartCompleteRequest>,
) -> Result<Json<Value>, ApiError> {
    let key = file_key(
        user.user_id(),
        body.video_id.as_deref(),
        body.subpath.as_deref(),
        body.file_key.as_deref(),
    )?;

    let video_id = video_id_from_key(&key).to_string();
    let backend = verify_video_owned(&state, user.user_id(), &video_id).await?;
    if backend == "s3" {
        return Err(ApiError::BadRequest(
            "S3 multipart uploads are not supported".into(),
        ));
    }
    validate_parts(&body.parts)?;
    let mut parts = body.parts;
    parts.sort_by_key(|p| p.part_number);

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let claimed = sqlx::query_scalar::<_, String>(
        "UPDATE multipart_uploads SET status = 'finalizing', updated_at = now() \
         WHERE upload_id = $1 AND owner_id = $2 AND video_id = $3 AND destination = $4 \
           AND (status = 'uploading' OR (status = 'finalizing' AND updated_at < now() - interval '6 hours')) \
         RETURNING upload_id",
    )
    .bind(&body.upload_id)
    .bind(user.user_id())
    .bind(&video_id)
    .bind(&key)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    let status = if claimed.is_none() {
        let row = sqlx::query_as::<_, (String, String, String, String)>(
            "SELECT owner_id, video_id, destination, status FROM multipart_uploads WHERE upload_id = $1",
        )
        .bind(&body.upload_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::NotFound)?;
        if row.0 != user.user_id() || row.1 != video_id || row.2 != key {
            return Err(ApiError::Forbidden);
        }
        Some(row.3)
    } else {
        None
    };
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if let Some(status) = status {
        if status == "completed" {
            let location = get_url(&state, &backend, &key, GET_TTL)?;
            return Ok(Json(
                json!({ "location": location, "success": true, "fileKey": key }),
            ));
        }
        if status == "finalizing" {
            return Err(ApiError::BadRequest("upload is finalizing".into()));
        }
        return Err(ApiError::BadRequest("upload is not active".into()));
    }

    if let Err(error) = assemble_multipart(&state, &body.upload_id, &key, &parts).await {
        sqlx::query(
            "UPDATE multipart_uploads SET status = 'uploading', updated_at = now() \
             WHERE upload_id = $1 AND status = 'finalizing'",
        )
        .bind(&body.upload_id)
        .execute(&state.db)
        .await
        .ok();
        return Err(error);
    }

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("UPDATE multipart_uploads SET status = 'completed', updated_at = now() WHERE upload_id = $1")
        .bind(&body.upload_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("UPDATE videos SET duration = $1, width = $2, height = $3, fps = $4 WHERE id = $5")
        .bind(body.duration_in_secs)
        .bind(body.width)
        .bind(body.height)
        .bind(body.fps)
        .bind(&video_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("DELETE FROM video_uploads WHERE video_id = $1")
        .bind(&video_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // clean up staging dir for this upload
    let _ = storage::remove_dir_all(&state, &format!("staging/{}", body.upload_id)).await;

    let location = get_url(&state, &backend, &key, GET_TTL)?;
    Ok(Json(json!({
        "location": location,
        "success": true,
        "fileKey": key,
    })))
}

/// POST /api/upload/multipart/abort
pub async fn multipart_abort(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<MultipartAbortRequest>,
) -> Result<Json<Value>, ApiError> {
    let key = file_key(
        user.user_id(),
        body.video_id.as_deref(),
        body.subpath.as_deref(),
        body.file_key.as_deref(),
    )?;
    let video_id = video_id_from_key(&key).to_string();
    verify_video_owned(&state, user.user_id(), &video_id).await?;

    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let row = sqlx::query_as::<_, (String, String, String, String)>(
        "SELECT owner_id, video_id, destination, status FROM multipart_uploads WHERE upload_id = $1 FOR UPDATE",
    )
    .bind(&body.upload_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;
    if row.0 != user.user_id() || row.1 != video_id || row.2 != key {
        return Err(ApiError::Forbidden);
    }
    if row.3 == "completed" {
        return Err(ApiError::BadRequest("upload already completed".into()));
    }
    if row.3 == "finalizing" {
        return Err(ApiError::BadRequest("upload is finalizing".into()));
    }
    sqlx::query(
        "UPDATE multipart_uploads SET status = 'aborted', updated_at = now() WHERE upload_id = $1",
    )
    .bind(&body.upload_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    sqlx::query("DELETE FROM video_uploads WHERE video_id = $1")
        .bind(&video_id)
        .execute(&mut *tx)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    tx.commit()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let parts_dir = format!("staging/{}", body.upload_id);
    let _ = storage::remove_dir_all(&state, &parts_dir).await;

    Ok(Json(
        json!({ "success": true, "fileKey": key, "uploadId": body.upload_id }),
    ))
}

/// POST /api/upload/recording-complete
pub async fn recording_complete(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<RecordingCompleteRequest>,
) -> Result<Json<Value>, ApiError> {
    let backend = verify_video_owned(&state, user.user_id(), &body.video_id).await?;

    let source_row =
        sqlx::query_scalar::<_, serde_json::Value>("SELECT source FROM videos WHERE id = $1")
            .bind(&body.video_id)
            .fetch_one(&state.db)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

    let source_type = source_row
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("desktopMP4");

    if source_type != "desktopSegments" {
        return Ok(Json(
            json!({ "success": true, "status": "already-complete" }),
        ));
    }

    if backend == "s3" {
        return Err(ApiError::BadRequest(
            "S3 desktopSegments completion is not supported".into(),
        ));
    }

    if state.mux_jobs.is_shutting_down() {
        return Err(ApiError::Internal("server shutting down".into()));
    }

    // Atomically claim the video for muxing so concurrent complete calls
    // cannot start duplicate work.
    let claimed = sqlx::query_scalar::<_, String>(
        "UPDATE videos SET mux_status = 'processing', mux_error = NULL, updated_at = now() \
         WHERE id = $1 \
           AND source->>'type' = 'desktopSegments' \
           AND (mux_status IS NULL OR mux_status IN ('queued', 'error')) \
         RETURNING id",
    )
    .bind(&body.video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    if claimed.is_none() {
        let status =
            sqlx::query_scalar::<_, Option<String>>("SELECT mux_status FROM videos WHERE id = $1")
                .bind(&body.video_id)
                .fetch_optional(&state.db)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .flatten()
                .unwrap_or_else(|| "already-complete".into());
        return Ok(Json(json!({ "success": true, "status": status })));
    }

    let state_bg = state.clone();
    let user_id = user.user_id().to_string();
    let video_id = body.video_id.clone();
    if !state.mux_jobs.try_spawn(video_id.clone(), async move {
        mux_segments(&state_bg, &user_id, &video_id).await;
    }) {
        set_mux_error(&state, &body.video_id, "server shutting down").await;
        return Err(ApiError::Internal("server shutting down".into()));
    }

    Ok(Json(json!({ "success": true, "status": "queued" })))
}

/// Reclaims one bounded batch of mux jobs left stale by a stopped process.
pub async fn recover_stale_mux_jobs(state: &Arc<AppState>) -> Result<u64, sqlx::Error> {
    // Ten serial jobs can spend five hours in MuxJobs at the ffmpeg timeout.
    // Six hours keeps this process's queued and running jobs outside recovery.
    let claimed = sqlx::query_as::<_, (String, String)>(
        "WITH candidates AS ( \
             SELECT id FROM videos \
             WHERE source->>'type' = 'desktopSegments' \
               AND storage_backend = 'local' \
               AND mux_status IN ('processing', 'error', 'queued') \
               AND updated_at < now() - interval '6 hours' \
             ORDER BY id \
             LIMIT $1 \
             FOR UPDATE SKIP LOCKED \
         ) \
         UPDATE videos AS v \
         SET mux_status = 'processing', mux_error = NULL, updated_at = now() \
         FROM candidates AS c \
         WHERE v.id = c.id \
         RETURNING v.id, v.owner_id",
    )
    .bind(MUX_RECOVERY_BATCH)
    .fetch_all(&state.db)
    .await?;

    let count = claimed.len() as u64;
    for (video_id, user_id) in claimed {
        let state_bg = state.clone();
        let job_id = video_id.clone();
        let failed_id = video_id.clone();
        if !state.mux_jobs.try_spawn(job_id, async move {
            mux_segments(&state_bg, &user_id, &video_id).await;
        }) {
            set_mux_error(state, &failed_id, "server shutting down").await;
        }
    }
    Ok(count)
}

async fn mux_segments(state: &AppState, user_id: &str, video_id: &str) {
    let seg_dir_key = format!("{user_id}/{video_id}/segments");
    let seg_dir = match storage::resolve(state, &seg_dir_key) {
        Ok(d) => d,
        Err(_) => {
            set_mux_error(state, video_id, "missing segments dir").await;
            return;
        }
    };
    let manifest_path = seg_dir.join("manifest.json");
    let manifest = match read_limited(&manifest_path, MAX_MANIFEST_SIZE).await {
        Ok(b) => match serde_json::from_slice::<Value>(&b) {
            Ok(v) => v,
            Err(_) => {
                set_mux_error(state, video_id, "invalid manifest.json").await;
                return;
            }
        },
        Err(_) => {
            set_mux_error(state, video_id, "missing manifest.json").await;
            return;
        }
    };

    let concat_ok = concat_mp4(state, user_id, video_id, &seg_dir, &manifest).await;
    if !concat_ok {
        set_mux_error(state, video_id, "ffmpeg mux failed").await;
        return;
    }

    // update source to desktopMP4 and mark complete
    let src = json!({"type": "desktopMP4"});
    if let Err(e) = sqlx::query(
        "UPDATE videos SET source = $1, mux_status = 'complete', mux_error = NULL, updated_at = now() WHERE id = $2",
    )
    .bind(&src)
    .bind(video_id)
    .execute(&state.db)
    .await
    {
        tracing::error!("mux: failed to mark video {video_id} complete: {e}");
        // Leave the row re-claimable instead of stranding it in 'processing'.
        set_mux_error(state, video_id, "mux completed but status update failed").await;
    }
}

async fn read_limited(path: &std::path::Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)
        .await?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .await?;
    if bytes.len() as u64 > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds size limit",
        ));
    }
    Ok(bytes)
}

async fn set_mux_error(state: &AppState, video_id: &str, msg: &str) {
    tracing::warn!("mux failed for video {video_id}: {msg}");
    sqlx::query(
        "UPDATE videos SET mux_status = 'error', mux_error = $1, updated_at = now() WHERE id = $2",
    )
    .bind(msg)
    .bind(video_id)
    .execute(&state.db)
    .await
    .ok();
}

async fn list_segments(
    dir: &std::path::Path,
) -> std::io::Result<(std::path::PathBuf, Vec<String>)> {
    let init = dir.join("init.mp4");
    let mut segs = Vec::new();
    let mut entries = fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".m4s") {
            if segs.len() == MAX_SEGMENTS {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "segment count exceeds limit",
                ));
            }
            segs.push(name);
        }
    }
    segs.sort();
    Ok((init, segs))
}

async fn concat_mp4(
    state: &AppState,
    user_id: &str,
    video_id: &str,
    seg_dir: &std::path::Path,
    manifest: &Value,
) -> bool {
    let (video_init, video_segs) = match list_segments(&seg_dir.join("video")).await {
        Ok(segments) => segments,
        Err(e) => {
            tracing::error!("mux: video segment scan failed: {e}");
            return false;
        }
    };
    let (audio_init, audio_segs) = match list_segments(&seg_dir.join("audio")).await {
        Ok(segments) => segments,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            (seg_dir.join("audio/init.mp4"), Vec::new())
        }
        Err(e) => {
            tracing::error!("mux: audio segment scan failed: {e}");
            return false;
        }
    };

    // prefer manifest-declared segment ordering when available
    let video_segs =
        if let Some(entries) = manifest.get("video_segments").and_then(|v| v.as_array()) {
            if !entries.is_empty() && entries[0].is_object() {
                if entries.len() > MAX_SEGMENTS {
                    return false;
                }
                ordered_segments(&seg_dir.join("video"), entries)
            } else {
                video_segs
            }
        } else {
            video_segs
        };

    let has_video = !video_segs.is_empty();
    let has_audio = !audio_segs.is_empty();
    if !has_video || video_segs.len().saturating_add(audio_segs.len()) > MAX_SEGMENTS {
        return false;
    }

    // build m3u8 playlists ffmpeg can consume (EXT-X-MAP handles fMP4 init)
    // write each into its own subdir so relative URIs resolve to the segments
    if has_audio
        && write_m3u8(&seg_dir.join("audio"), "audio", &audio_init, &audio_segs)
            .await
            .is_err()
    {
        return false;
    }
    if write_m3u8(&seg_dir.join("video"), "video", &video_init, &video_segs)
        .await
        .is_err()
    {
        return false;
    }

    let dest_key = format!("{user_id}/{video_id}/result.mp4");
    let dest = match storage::resolve(state, &dest_key) {
        Ok(d) => d,
        Err(_) => return false,
    };
    if let Err(e) = storage::ensure_parent(state, &dest_key).await {
        tracing::error!("mux: mkdir failed {e}");
        return false;
    }
    let temp = dest.with_file_name(format!(".result-{}.tmp.mp4", uuid::Uuid::new_v4()));
    let mut temp_guard = TempFileGuard::new(temp);

    let mut cmd = tokio::process::Command::new(&state.config.ffmpeg_path);
    cmd.kill_on_drop(true)
        .arg("-y")
        .arg("-loglevel")
        .arg("error")
        .arg("-allowed_extensions")
        .arg("ALL")
        .arg("-i")
        .arg(seg_dir.join("video/video.m3u8"));
    if has_audio {
        cmd.arg("-i").arg(seg_dir.join("audio/audio.m3u8"));
    }
    cmd.arg("-map").arg("0:v:0");
    if has_audio {
        cmd.arg("-map").arg("1:a:0");
    }
    cmd.arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(temp_guard.path());
    cmd.stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            tracing::error!("ffmpeg not found or failed: {e}");
            temp_guard.cleanup().await;
            return false;
        }
    };
    let mut stderr = child.stderr.take().expect("piped ffmpeg stderr");
    let stderr_task = tokio::spawn(async move {
        const RETAIN: usize = 1024 * 1024;
        let mut retained = Vec::new();
        let mut buffer = [0_u8; 8192];
        loop {
            match stderr.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(read) if retained.len() < RETAIN => {
                    retained.extend_from_slice(&buffer[..read.min(RETAIN - retained.len())]);
                }
                Ok(_) => {}
            }
        }
        retained
    });
    let status = tokio::time::timeout(FFMPEG_TIMEOUT, child.wait()).await;
    match status {
        Ok(Ok(status)) => {
            let stderr = stderr_task.await.unwrap_or_default();
            if status.success() {
                if let Err(e) = fs::rename(temp_guard.path(), &dest).await {
                    tracing::error!("mux: publish failed: {e}");
                    temp_guard.cleanup().await;
                    return false;
                }
                temp_guard.disarm();
                let _ = fs::remove_file(seg_dir.join("video/video.m3u8")).await;
                let _ = fs::remove_file(seg_dir.join("audio/audio.m3u8")).await;
                true
            } else {
                tracing::error!("ffmpeg mux failed: {}", String::from_utf8_lossy(&stderr));
                temp_guard.cleanup().await;
                false
            }
        }
        Ok(Err(e)) => {
            stderr_task.abort();
            tracing::error!("ffmpeg not found or failed: {e}");
            temp_guard.cleanup().await;
            false
        }
        Err(_) => {
            let _ = child.kill().await;
            stderr_task.abort();
            tracing::error!("ffmpeg mux timed out after 30 minutes");
            temp_guard.cleanup().await;
            false
        }
    }
}

fn ordered_segments(dir: &std::path::Path, entries: &[Value]) -> Vec<String> {
    let mut items: Vec<(i64, String)> = entries
        .iter()
        .filter_map(|e| {
            let index = e.get("index")?.as_i64()?;
            Some((index, format!("segment_{:03}.m4s", index)))
        })
        .collect();
    items.sort_by_key(|(i, _)| *i);
    items
        .iter()
        .map(|(_, name)| {
            dir.join(name)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect()
}

async fn write_m3u8(
    seg_dir: &std::path::Path,
    kind: &str,
    init: &std::path::Path,
    segs: &[String],
) -> std::io::Result<()> {
    let init_name = init
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "init.mp4".into());
    let mut content = String::from(
        "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:0\n",
    );
    content.push_str(&format!("#EXT-X-MAP:URI=\"{init_name}\"\n"));
    for s in segs {
        content.push_str("#EXTINF:2.000,\n");
        content.push_str(s);
        content.push('\n');
    }
    content.push_str("#EXT-X-ENDLIST\n");
    fs::write(seg_dir.join(format!("{kind}.m3u8")), content).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(number: i32, size: i64) -> Part {
        Part {
            part_number: number,
            etag: "etag".into(),
            size,
        }
    }

    #[test]
    fn file_key_binds_owner_and_validates_structure() {
        assert_eq!(
            file_key("user", None, None, Some("user/video/path/file.mp4")).unwrap(),
            "user/video/path/file.mp4"
        );
        assert!(file_key("user", None, None, Some("other/video/file.mp4")).is_err());
        assert!(file_key("user", None, None, Some("user/video")).is_err());
        assert!(file_key("user", None, None, Some("user/video/../secret")).is_err());
        assert!(file_key("user", None, None, Some("user/video/bad\nname")).is_err());
        assert!(file_key("user", Some("../video"), Some("file.mp4"), None).is_err());
        assert!(file_key("user", Some("video"), Some("dir//file.mp4"), None).is_err());
    }

    #[test]
    fn parts_must_be_positive_contiguous_unique_and_bounded() {
        assert_eq!(validate_parts(&[part(2, 2), part(1, 3)]).unwrap(), 5);
        assert!(validate_parts(&[]).is_err());
        assert!(validate_parts(&[part(0, 1)]).is_err());
        assert!(validate_parts(&[part(1, 1), part(1, 1)]).is_err());
        assert!(validate_parts(&[part(1, 0)]).is_err());
        assert!(validate_parts(&[part(1, MAX_PART_SIZE as i64 + 1)]).is_err());
        assert!(validate_parts(&[part(1, MAX_UPLOAD_SIZE as i64), part(2, 1)]).is_err());
    }
}

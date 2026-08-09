use axum::extract::State;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncWriteExt, BufWriter};

use crate::auth::CurrentUser;
use crate::error::ApiError;
use crate::state::AppState;
use crate::storage;

const PUT_TTL: i64 = 3600;
const GET_TTL: i64 = 86400;

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
#[allow(dead_code)]
pub struct Part {
    pub part_number: i32,
    pub etag: String,
    pub size: i64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
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
#[allow(dead_code)]
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
        if fk.is_empty() {
            return Err(ApiError::BadRequest("invalid fileKey".into()));
        }
        // fileKey format: {user_id}/{video_id}/{subpath}
        return Ok(fk.to_string());
    }
    let vid = video_id.ok_or(ApiError::BadRequest("videoId required".into()))?;
    if vid.contains("..") || vid.contains('/') {
        return Err(ApiError::BadRequest("invalid videoId".into()));
    }
    let sub = subpath
        .map(|s| s.to_string())
        .unwrap_or_else(|| "result.mp4".into());
    validate_subpath(&sub)?;
    Ok(format!("{user_id}/{vid}/{sub}"))
}

fn validate_subpath(sub: &str) -> Result<(), ApiError> {
    if sub.is_empty() || sub.contains("..") || sub.starts_with('/') {
        return Err(ApiError::BadRequest("invalid subpath".into()));
    }
    Ok(())
}

async fn verify_video_owned(
    state: &AppState,
    user_id: &str,
    video_id: &str,
) -> Result<(), ApiError> {
    let owner: Option<String> = sqlx::query_scalar("SELECT owner_id FROM videos WHERE id = $1")
        .bind(video_id)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    match owner {
        Some(oid) if oid == user_id => Ok(()),
        Some(_) => Err(ApiError::Forbidden),
        None => Err(ApiError::NotFound),
    }
}

/// POST /api/upload/signed
pub async fn signed(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    axum::extract::Json(body): axum::extract::Json<SignedRequest>,
) -> Result<Json<Value>, ApiError> {
    verify_video_owned(&state, user.user_id(), &body.video_id).await?;
    let key = file_key(
        user.user_id(),
        Some(&body.video_id),
        Some(&body.subpath),
        None,
    )?;

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

    let url = state.signer.put_url(&state.config.web_url, &key, PUT_TTL);
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
    verify_video_owned(&state, user.user_id(), &body.video_id).await?;
    let mut urls = serde_json::Map::new();
    let mut uploads = serde_json::Map::new();

    for sub in &body.subpaths {
        validate_subpath(sub)?;
        let key = format!("{}/{}/{}", user.user_id(), body.video_id, sub);
        let url = state.signer.put_url(&state.config.web_url, &key, PUT_TTL);
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
    let video_id = match &body.file_key {
        Some(fk) => fk
            .split('/')
            .nth(1)
            .ok_or(ApiError::BadRequest("invalid fileKey".into()))?
            .to_string(),
        None => body
            .video_id
            .clone()
            .ok_or_else(|| ApiError::BadRequest("videoId required".into()))?,
    };
    verify_video_owned(&state, user.user_id(), &video_id).await?;

    let upload_id = uuid::Uuid::new_v4().to_string();

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
    let video_id = key.split('/').nth(1).unwrap_or("").to_string();
    verify_video_owned(&state, user.user_id(), &video_id).await?;

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

    let video_id = key.split('/').nth(1).unwrap_or("").to_string();
    verify_video_owned(&state, user.user_id(), &video_id).await?;

    // sort parts by number
    let mut parts = body.parts;
    parts.sort_by_key(|p| p.part_number);

    let dest = storage::resolve(&state, &key)?;
    storage::ensure_parent(&state, &key).await?;

    let mut out = BufWriter::new(
        fs::File::create(&dest)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?,
    );

    for part in &parts {
        let part_key = format!("staging/{}/{}", body.upload_id, part.part_number);
        let part_path = storage::resolve(&state, &part_key)?;
        let mut f = fs::File::open(&part_path)
            .await
            .map_err(|e| ApiError::Internal(format!("open part: {e}")))?;
        tokio::io::copy(&mut f, &mut out)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    out.flush()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // clean up staging dir for this upload
    let _ = storage::remove_dir_all(&state, &format!("staging/{}", body.upload_id)).await;

    // update video meta + delete upload row
    sqlx::query("UPDATE videos SET duration = $1, width = $2, height = $3, fps = $4 WHERE id = $5")
        .bind(body.duration_in_secs)
        .bind(body.width)
        .bind(body.height)
        .bind(body.fps)
        .bind(&video_id)
        .execute(&state.db)
        .await
        .ok();

    sqlx::query("DELETE FROM video_uploads WHERE video_id = $1")
        .bind(&video_id)
        .execute(&state.db)
        .await
        .ok();

    let location = state.signer.get_url(&state.config.web_url, &key, GET_TTL);
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
    let video_id = key.split('/').nth(1).unwrap_or("").to_string();
    verify_video_owned(&state, user.user_id(), &video_id).await?;

    let parts_dir = format!("staging/{}", body.upload_id);
    let _ = storage::remove_dir_all(&state, &parts_dir).await;
    sqlx::query("DELETE FROM video_uploads WHERE video_id = $1")
        .bind(&video_id)
        .execute(&state.db)
        .await
        .ok();

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
    verify_video_owned(&state, user.user_id(), &body.video_id).await?;

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

    if source_type == "desktopSegments" {
        // spawn muxing in the background
        let state = state.clone();
        let user_id = user.user_id().to_string();
        let video_id = body.video_id.clone();
        tokio::spawn(async move {
            mux_segments(&state, &user_id, &video_id).await;
        });
        return Ok(Json(json!({ "success": true, "status": "queued" })));
    }

    Ok(Json(
        json!({ "success": true, "status": "already-complete" }),
    ))
}

async fn mux_segments(state: &AppState, user_id: &str, video_id: &str) {
    let seg_dir_key = format!("{user_id}/{video_id}/segments");
    let seg_dir = match storage::resolve(state, &seg_dir_key) {
        Ok(d) => d,
        Err(_) => return,
    };
    let manifest_path = seg_dir.join("manifest.json");
    let manifest = match fs::read(&manifest_path).await {
        Ok(b) => match serde_json::from_slice::<Value>(&b) {
            Ok(v) => v,
            Err(_) => return,
        },
        Err(_) => return,
    };

    let concat_ok = concat_mp4(state, user_id, video_id, &seg_dir, &manifest).await;
    if !concat_ok {
        tracing::warn!("ffmpeg mux failed for video {video_id}");
        return;
    }

    // update source to desktopMP4
    let src = json!({"type": "desktopMP4"});
    sqlx::query("UPDATE videos SET source = $1 WHERE id = $2")
        .bind(&src)
        .bind(video_id)
        .execute(&state.db)
        .await
        .ok();
}

fn list_segments(dir: &std::path::Path) -> (std::path::PathBuf, Vec<String>) {
    let init = dir.join("init.mp4");
    let mut segs = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut names: Vec<String> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".m4s"))
            .collect();
        names.sort();
        segs = names;
    }
    (init, segs)
}

async fn concat_mp4(
    state: &AppState,
    user_id: &str,
    video_id: &str,
    seg_dir: &std::path::Path,
    manifest: &Value,
) -> bool {
    let (video_init, video_segs) = list_segments(&seg_dir.join("video"));
    let (audio_init, audio_segs) = list_segments(&seg_dir.join("audio"));

    // prefer manifest-declared segment ordering when available
    let video_segs =
        if let Some(entries) = manifest.get("video_segments").and_then(|v| v.as_array()) {
            if !entries.is_empty() && entries[0].is_object() {
                ordered_segments(&seg_dir.join("video"), entries)
            } else {
                video_segs
            }
        } else {
            video_segs
        };

    let has_video = !video_segs.is_empty();
    let has_audio = !audio_segs.is_empty();
    if !has_video {
        return false;
    }

    // build m3u8 playlists ffmpeg can consume (EXT-X-MAP handles fMP4 init)
    // write each into its own subdir so relative URIs resolve to the segments
    if has_audio {
        write_m3u8(&seg_dir.join("audio"), "audio", &audio_init, &audio_segs);
    }
    write_m3u8(&seg_dir.join("video"), "video", &video_init, &video_segs);

    let dest_key = format!("{user_id}/{video_id}/result.mp4");
    let dest = match storage::resolve(state, &dest_key) {
        Ok(d) => d,
        Err(_) => return false,
    };
    if let Err(e) = storage::ensure_parent(state, &dest_key).await {
        tracing::error!("mux: mkdir failed {e}");
        return false;
    }

    let mut cmd = tokio::process::Command::new(&state.config.ffmpeg_path);
    cmd.arg("-y").arg("-allowed_extensions").arg("ALL");
    if has_audio {
        cmd.arg("-i").arg(seg_dir.join("audio/audio.m3u8"));
    }
    cmd.arg("-i").arg(seg_dir.join("video/video.m3u8"));
    cmd.arg("-map").arg("0:v:0");
    if has_audio {
        cmd.arg("-map").arg("1:a:0");
    }
    cmd.arg("-c")
        .arg("copy")
        .arg("-movflags")
        .arg("+faststart")
        .arg(&dest);
    cmd.stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::null());

    let out = cmd.output().await;
    match out {
        Ok(o) => {
            if o.status.success() {
                let _ = fs::remove_file(seg_dir.join("video/video.m3u8")).await;
                let _ = fs::remove_file(seg_dir.join("audio/audio.m3u8")).await;
                true
            } else {
                tracing::error!("ffmpeg mux failed: {}", String::from_utf8_lossy(&o.stderr));
                false
            }
        }
        Err(e) => {
            tracing::error!("ffmpeg not found or failed: {e}");
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

fn write_m3u8(seg_dir: &std::path::Path, kind: &str, init: &std::path::Path, segs: &[String]) {
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
    let _ = std::fs::write(seg_dir.join(format!("{kind}.m3u8")), content);
}

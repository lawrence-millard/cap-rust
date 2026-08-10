use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::io::ReaderStream;

use crate::error::ApiError;
use crate::state::AppState;
use crate::storage;

const MAX_UPLOAD_BYTES: u64 = 20 * 1024 * 1024 * 1024;

#[derive(Deserialize)]
pub struct SignedParams {
    pub exp: i64,
    pub sig: String,
}

/// GET /media/{*key}?exp=&sig=  — signed playback of stored objects with Range support.
pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(params): Query<SignedParams>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !state.signer.verify("GET", &key, params.exp, &params.sig) {
        return Err(ApiError::Unauthorized);
    }
    let path = storage::resolve(&state, &key)?;
    let cache_control = cache_control_for_key(&state, &key).await;
    serve_file_with_range(&path, &headers, cache_control).await
}

/// POST /up/{*key}?exp=&sig=  — signed upload target. Streams body to disk.
pub async fn put(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(params): Query<SignedParams>,
    headers: HeaderMap,
    body: Body,
) -> Result<StatusCode, ApiError> {
    if !state.signer.verify("PUT", &key, params.exp, &params.sig) {
        return Err(ApiError::Unauthorized);
    }
    let path = storage::resolve(&state, &key)?;
    let content_length = headers
        .get(header::CONTENT_LENGTH)
        .ok_or_else(|| ApiError::BadRequest("Content-Length required".into()))?
        .to_str()
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or_else(|| ApiError::BadRequest("invalid Content-Length".into()))?;
    if content_length > MAX_UPLOAD_BYTES {
        return Err(ApiError::BadRequest("upload exceeds 20 GiB limit".into()));
    }

    storage::ensure_parent(&state, &key).await?;
    storage::touch_staging_activity_for_key(&state, &key).await;
    write_upload_atomic(&path, body, Some(content_length)).await?;

    // Refresh again after the write so long part uploads keep the heartbeat fresh.
    storage::touch_staging_activity_for_key(&state, &key).await;

    Ok(StatusCode::OK)
}

async fn cache_control_for_key(state: &AppState, key: &str) -> &'static str {
    // Keys are `{owner}/{videoId}/...`. Non-public videos must not be cached by shared proxies.
    let mut parts = key.split('/');
    let _owner = parts.next();
    let Some(video_id) = parts.next() else {
        return "private, no-store";
    };
    let mode: Option<String> = sqlx::query_scalar("SELECT access_mode FROM videos WHERE id = $1")
        .bind(video_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    match mode.as_deref() {
        Some("public") => "public, max-age=31536000, immutable",
        _ => "private, no-store",
    }
}

async fn write_upload_atomic(
    path: &std::path::Path,
    body: Body,
    content_length: Option<u64>,
) -> Result<(), ApiError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ApiError::BadRequest("invalid upload path".into()))?;
    let temp_path = path.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let result = async {
        let mut bytes_written = 0_u64;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| ApiError::Internal(e.to_string()))?;
            bytes_written = bytes_written
                .checked_add(chunk.len() as u64)
                .ok_or_else(|| ApiError::BadRequest("upload exceeds 20 GiB limit".into()))?;
            if bytes_written > MAX_UPLOAD_BYTES {
                return Err(ApiError::BadRequest("upload exceeds 20 GiB limit".into()));
            }
            file.write_all(&chunk)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
        }
        if content_length.is_some_and(|length| length != bytes_written) {
            return Err(ApiError::BadRequest(
                "Content-Length does not match body".into(),
            ));
        }
        file.flush()
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
        drop(file);
        fs::rename(&temp_path, path)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))
    }
    .await;

    if result.is_err() {
        let _ = fs::remove_file(&temp_path).await;
    }

    result
}

fn content_type_for(path: &std::path::Path) -> &'static str {
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".mp4") || lower.ends_with(".m4s") {
        "video/mp4"
    } else if lower.ends_with(".m3u8") {
        "application/x-mpegURL"
    } else if lower.ends_with(".json") {
        "application/json"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".aac") {
        "audio/aac"
    } else if lower.ends_with(".webm") {
        "audio/webm"
    } else if lower.ends_with(".vtt") {
        "text/vtt"
    } else {
        "application/octet-stream"
    }
}

async fn serve_file_with_range(
    path: &std::path::Path,
    headers: &HeaderMap,
    cache_control: &'static str,
) -> Result<Response, ApiError> {
    let meta = fs::metadata(path).await.map_err(|_| ApiError::NotFound)?;
    if !meta.is_file() {
        return Err(ApiError::NotFound);
    }
    let file_size = meta.len();

    let ctype = content_type_for(path);

    // Parse Range header (single range only)
    let range_header = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    // Empty files cannot satisfy any byte range; return 416 before parse_range
    // so suffix forms cannot underflow on `file_size - 1`.
    if file_size == 0 && range_header.is_some_and(|r| r.starts_with("bytes=")) {
        return Ok(axum::response::Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, "bytes */0")
            .body(Body::empty())?);
    }

    // `requested_range` is None when no valid "bytes=" Range was sent;
    // Some(parse result) otherwise. We keep the two apart so a malformed or
    // unsatisfiable range can be answered with 416 instead of silently 200.
    let requested_range = range_header
        .filter(|r| r.starts_with("bytes="))
        .map(|r| parse_range(&r[6..], file_size));

    let mut resp_builder = axum::response::Response::builder();

    let range_requested = requested_range.is_some();

    // 416 for malformed or unsatisfiable ranges (RFC 9110 §14.2)
    if matches!(requested_range, Some(None)) {
        return Ok(resp_builder
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
            .body(Body::empty())?);
    }

    let (start, end) = match requested_range {
        None => (None, None),
        Some(Some((s, e))) => match (s, e) {
            // open-ended "bytes=start-" => to end of file
            (Some(s), None) if s < file_size => (Some(s), Some(file_size - 1)),
            other => other,
        },
        Some(None) => unreachable!(),
    };

    match (start, end) {
        (Some(s), Some(e)) if s <= e && e < file_size => {
            let len = e - s + 1;
            let file = fs::File::open(path)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            let mut file = tokio::io::BufReader::new(file);
            tokio::io::AsyncSeekExt::seek(&mut file, std::io::SeekFrom::Start(s))
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            let reader = ReaderStream::with_capacity(file.take(len), 64 * 1024);
            let stream = Body::from_stream(reader);
            resp_builder = resp_builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, ctype)
                .header(header::CONTENT_LENGTH, len.to_string())
                .header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{file_size}"))
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CACHE_CONTROL, cache_control)
                .header(header::CONTENT_DISPOSITION, "inline");
            Ok(resp_builder.body(stream)?)
        }
        _ if range_requested => Ok(resp_builder
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{file_size}"))
            .body(Body::empty())?),
        _ => {
            let file = fs::File::open(path)
                .await
                .map_err(|e| ApiError::Internal(e.to_string()))?;
            let reader = ReaderStream::with_capacity(file, 64 * 1024);
            let stream = Body::from_stream(reader);
            resp_builder = resp_builder
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, ctype)
                .header(header::CONTENT_LENGTH, file_size.to_string())
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CACHE_CONTROL, cache_control)
                .header(header::CONTENT_DISPOSITION, "inline");
            Ok(resp_builder.body(stream)?)
        }
    }
}

/// Parse "start-end", "start-", "-suffix" forms. Returns (start, end).
fn parse_range(range: &str, file_size: u64) -> Option<(Option<u64>, Option<u64>)> {
    if file_size == 0 {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    let start = start.trim();
    let end = end.trim();
    if start.is_empty() {
        // suffix range: last N bytes
        let suffix: u64 = end.parse().ok()?;
        if suffix == 0 {
            return None;
        }
        let s = file_size.saturating_sub(suffix);
        return Some((Some(s), Some(file_size - 1)));
    }
    let s: u64 = start.parse().ok()?;
    if end.is_empty() {
        return Some((Some(s), None));
    }
    let e: u64 = end.parse().ok()?;
    Some((Some(s), Some(e)))
}

impl From<axum::http::Error> for ApiError {
    fn from(e: axum::http::Error) -> Self {
        ApiError::Internal(e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;

    fn temp_dir() -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("cap-rust-media-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn assert_no_upload_temp_files(dir: &std::path::Path) {
        let entries = std::fs::read_dir(dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            entries
                .iter()
                .all(|entry| { !entry.file_name().to_string_lossy().ends_with(".tmp") })
        );
    }

    #[test]
    fn parse_range_full() {
        assert_eq!(parse_range("0-99", 200), Some((Some(0), Some(99))));
    }

    #[test]
    fn parse_range_open_ended() {
        assert_eq!(parse_range("50-", 200), Some((Some(50), None)));
    }

    #[test]
    fn parse_range_suffix() {
        assert_eq!(parse_range("-50", 200), Some((Some(150), Some(199))));
    }

    #[test]
    fn parse_range_suffix_exact_file() {
        assert_eq!(parse_range("-200", 200), Some((Some(0), Some(199))));
    }

    #[test]
    fn parse_range_suffix_overflow() {
        // suffix larger than file clamps to start of file
        assert_eq!(parse_range("-500", 200), Some((Some(0), Some(199))));
    }

    #[test]
    fn parse_range_suffix_zero_is_none() {
        assert_eq!(parse_range("-0", 200), None);
    }

    #[test]
    fn parse_range_suffix_on_empty_file_is_none() {
        assert_eq!(parse_range("-5", 0), None);
    }

    #[test]
    fn parse_range_start_beyond_file() {
        assert_eq!(parse_range("500-600", 200), Some((Some(500), Some(600))));
    }

    #[test]
    fn parse_range_reversed() {
        assert_eq!(parse_range("100-50", 200), Some((Some(100), Some(50))));
    }

    #[test]
    fn parse_range_empty() {
        assert_eq!(parse_range("", 200), None);
    }

    #[test]
    fn parse_range_only_dash() {
        assert_eq!(parse_range("-", 200), None);
    }

    #[test]
    fn parse_range_non_numeric_start() {
        assert_eq!(parse_range("abc-50", 200), None);
    }

    #[test]
    fn parse_range_non_numeric_end() {
        assert_eq!(parse_range("50-abc", 200), None);
    }

    #[test]
    fn parse_range_just_start() {
        assert_eq!(parse_range("0-", 200), Some((Some(0), None)));
    }

    #[test]
    fn parse_range_trim_whitespace() {
        assert_eq!(parse_range(" 50 - 100 ", 200), Some((Some(50), Some(100))));
    }

    #[tokio::test]
    async fn partial_range_body_stops_at_requested_end() {
        let dir = temp_dir();
        let path = dir.join("video.mp4");
        fs::write(&path, b"0123456789").await.unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::RANGE, "bytes=2-4".parse().unwrap());

        let response = serve_file_with_range(&path, &headers, "private, no-store")
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();

        assert_eq!(&body[..], b"234");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn upload_replaces_target_only_after_complete_body() {
        let dir = temp_dir();
        let path = dir.join("object");
        fs::write(&path, b"old").await.unwrap();

        write_upload_atomic(&path, Body::from("new body"), Some(8))
            .await
            .unwrap();

        assert_eq!(fs::read(&path).await.unwrap(), b"new body");
        assert_no_upload_temp_files(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn content_length_mismatch_preserves_target_and_removes_temp() {
        let dir = temp_dir();
        let path = dir.join("object");
        fs::write(&path, b"old").await.unwrap();

        let result = write_upload_atomic(&path, Body::from("short"), Some(10)).await;

        assert!(matches!(result, Err(ApiError::BadRequest(_))));
        assert_eq!(fs::read(&path).await.unwrap(), b"old");
        assert_no_upload_temp_files(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn body_error_preserves_target_and_removes_temp() {
        let dir = temp_dir();
        let path = dir.join("object");
        fs::write(&path, b"old").await.unwrap();
        let stream = futures_util::stream::iter([
            Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"partial")),
            Err(std::io::Error::other("body failed")),
        ]);

        let result = write_upload_atomic(&path, Body::from_stream(stream), None).await;

        assert!(matches!(result, Err(ApiError::Internal(_))));
        assert_eq!(fs::read(&path).await.unwrap(), b"old");
        assert_no_upload_temp_files(&dir);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

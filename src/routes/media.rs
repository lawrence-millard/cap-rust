use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use futures_util::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::error::ApiError;
use crate::state::AppState;
use crate::storage;

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
    serve_file_with_range(&path, &headers).await
}

/// POST /up/{*key}?exp=&sig=  — signed upload target. Streams body to disk.
pub async fn put(
    State(state): State<Arc<AppState>>,
    Path(key): Path<String>,
    Query(params): Query<SignedParams>,
    body: Body,
) -> Result<StatusCode, ApiError> {
    if !state.signer.verify("PUT", &key, params.exp, &params.sig) {
        return Err(ApiError::Unauthorized);
    }
    storage::ensure_parent(&state, &key).await?;
    storage::touch_staging_activity_for_key(&state, &key).await;
    let path = storage::resolve(&state, &key)?;

    let mut file = fs::File::create(&path)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // stream body to disk
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::Internal(e.to_string()))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;
    }
    file.flush()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    // Refresh again after the write so long part uploads keep the heartbeat fresh.
    storage::touch_staging_activity_for_key(&state, &key).await;

    Ok(StatusCode::OK)
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
            let reader = ReaderStream::with_capacity(file, 64 * 1024);
            let stream = Body::from_stream(reader);
            resp_builder = resp_builder
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_TYPE, ctype)
                .header(header::CONTENT_LENGTH, len.to_string())
                .header(header::CONTENT_RANGE, format!("bytes {s}-{e}/{file_size}"))
                .header(header::ACCEPT_RANGES, "bytes")
                .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
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
                .header(header::CACHE_CONTROL, "public, max-age=31536000, immutable");
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
}

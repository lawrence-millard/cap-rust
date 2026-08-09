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

    let (start, end) = if let Some(range) = range_header {
        if let Some(range) = range.strip_prefix("bytes=") {
            parse_range(range, file_size).unwrap_or((None, None))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    let mut resp_builder = axum::response::Response::builder();

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

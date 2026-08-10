use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{Html, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::error::ApiError;
use crate::routes::access;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct OEmbedQuery {
    url: String,
    maxwidth: Option<u32>,
    maxheight: Option<u32>,
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn video_id_from_url(web_url: &str, url: &str) -> Option<String> {
    let base = web_url.trim_end_matches('/');
    let path = url.strip_prefix(base)?;
    let path = path.split(['?', '#']).next()?;
    let id = path
        .strip_prefix("/s/")
        .or_else(|| path.strip_prefix("/embed/"))?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
    {
        return None;
    }
    Some(id.to_string())
}

async fn public_video(
    state: &AppState,
    video_id: &str,
) -> Result<(Option<String>, Option<i32>, Option<i32>), ApiError> {
    sqlx::query_as(
        "SELECT name, width, height FROM videos WHERE id = $1 AND access_mode = 'public'",
    )
    .bind(video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)
}

pub async fn embed(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<Html<String>, ApiError> {
    let (name, owner_id, mode) = sqlx::query_as::<_, (Option<String>, String, String)>(
        "SELECT name, owner_id, access_mode FROM videos WHERE id = $1",
    )
    .bind(&video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;
    if !access::policy_allows(&mode, &owner_id, &video_id, &headers, None, &state.signer) {
        return Err(ApiError::NotFound);
    }
    let title = html_escape(name.as_deref().unwrap_or("Cap Recording"));
    let src = html_escape(&format!("/s/{video_id}"));
    Ok(Html(format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>{title}</title><style>html,body,iframe{{width:100%;height:100%;margin:0;border:0;background:#f5f5f5}}iframe{{display:block}}</style></head><body><iframe src="{src}" title="{title}" allow="autoplay; fullscreen" allowfullscreen></iframe></body></html>"#
    )))
}

pub async fn oembed(
    State(state): State<Arc<AppState>>,
    Query(query): Query<OEmbedQuery>,
) -> Result<Json<Value>, ApiError> {
    let video_id = video_id_from_url(&state.config.web_url, &query.url)
        .ok_or_else(|| ApiError::BadRequest("url must be a local share or embed URL".into()))?;
    let (name, stored_width, stored_height) = public_video(&state, &video_id).await?;
    let width = query
        .maxwidth
        .unwrap_or_else(|| {
            stored_width
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(960)
        })
        .clamp(1, 4096);
    let height = query
        .maxheight
        .unwrap_or_else(|| {
            stored_height
                .and_then(|v| u32::try_from(v).ok())
                .unwrap_or(540)
        })
        .clamp(1, 4096);
    let embed_url = format!(
        "{}/embed/{video_id}",
        state.config.web_url.trim_end_matches('/')
    );
    let html = format!(
        "<iframe src=\"{}\" width=\"{width}\" height=\"{height}\" title=\"{}\" frameborder=\"0\" allow=\"autoplay; fullscreen\" allowfullscreen></iframe>",
        html_escape(&embed_url),
        html_escape(name.as_deref().unwrap_or("Cap Recording")),
    );
    Ok(Json(json!({
        "version": "1.0",
        "type": "video",
        "provider_name": "Cap",
        "provider_url": state.config.web_url,
        "title": name.unwrap_or_else(|| "Cap Recording".into()),
        "width": width,
        "height": height,
        "html": html,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_local_video_urls() {
        assert_eq!(
            video_id_from_url("https://cap.example", "https://cap.example/s/video-1?x=1"),
            Some("video-1".into())
        );
        assert_eq!(
            video_id_from_url("https://cap.example", "https://evil.example/s/video-1"),
            None
        );
        assert_eq!(
            video_id_from_url("https://cap.example", "https://cap.example.evil/s/video-1"),
            None
        );
    }

    #[test]
    fn escapes_embed_attributes() {
        assert_eq!(html_escape("a\"<&'"), "a&quot;&lt;&amp;&#39;");
    }
}

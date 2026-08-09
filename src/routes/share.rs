use axum::extract::{Path, State};
use axum::response::{Html, IntoResponse};
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;
use crate::storage;

pub async fn blank() -> Html<&'static str> {
    Html("")
}

/// Escape a string for safe interpolation into HTML text/attribute contexts.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub async fn video(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<serde_json::Value>, bool, Option<f64>, Option<i32>, Option<i32>, Option<f64>, chrono::DateTime<chrono::Utc>)>(
        "SELECT id, owner_id, name, source, is_screenshot, duration, width, height, fps, created_at FROM videos WHERE id = $1 AND public = true",
    )
    .bind(&video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    let name = html_escape(&row.2.unwrap_or_else(|| "Cap Recording".into()));
    let owner_id = &row.1;
    let source = &row.3;
    let source_type = source
        .as_ref()
        .and_then(|s| s.get("type"))
        .and_then(|v| v.as_str())
        .unwrap_or("desktopMP4");
    let is_screenshot = row.4;

    // determine media kind for player
    let is_segments = source_type == "desktopSegments";
    let has_result = storage::exists(&state, &format!("{owner_id}/{video_id}/result.mp4"));

    // screenshot handling
    if is_screenshot {
        let img_key = find_screenshot(&state, owner_id, &video_id).await;
        if let Some(key) = img_key {
            let url = state.signer.get_url(&state.config.web_url, &key, 86400);
            return Ok(render_image_page(&name, &url).into_response());
        }
        return Err(ApiError::NotFound);
    }

    // HLS for segments
    if is_segments && !has_result {
        let hls_src = format!("/api/playlist?videoId={video_id}&videoType=segments-master");
        return Ok(render_hls_page(&name, &hls_src).into_response());
    }

    // mp4 native
    let mp4_src = format!("/api/playlist?videoId={video_id}&videoType=mp4");
    Ok(render_mp4_page(&name, &mp4_src).into_response())
}

async fn find_screenshot(state: &Arc<AppState>, owner_id: &str, video_id: &str) -> Option<String> {
    let dir = storage::resolve(state, &format!("{owner_id}/{video_id}/screenshot")).ok()?;
    let mut entries = tokio::fs::read_dir(&dir).await.ok()?;
    let mut found = None;
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".png")
            || name.ends_with(".jpg")
            || name.ends_with(".jpeg")
            || name.ends_with(".webp")
        {
            found = Some(format!("{owner_id}/{video_id}/screenshot/{name}"));
            break;
        }
    }
    found
}

/// Escape a string for safe use inside a single-quoted JS string literal.
fn js_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('<', "\\x3c")
        .replace('>', "\\x3e")
        .replace('&', "\\x26")
}

fn render_image_page(name: &str, src: &str) -> Html<String> {
    let src = html_escape(src);
    Html(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>{name}</title>
<style>
  :root {{ color-scheme: dark; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif; }}
  body {{ margin: 0; min-height: 100vh; background: #0b0f19; display: grid; place-items: center; padding: 24px; }}
  img {{ max-width: 100%; max-height: 92vh; border-radius: 16px; box-shadow: 0 30px 80px rgba(0,0,0,0.6); }}
</style>
</head>
<body><img src="{src}" alt="{name}" /></body>
</html>"##,
        name = name,
        src = src,
    ))
}

fn render_mp4_page(name: &str, src: &str) -> Html<String> {
    let src = html_escape(src);
    Html(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>{name} &mdash; Cap</title>
<style>
  :root {{ color-scheme: dark; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif; }}
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; min-height: 100vh; background: radial-gradient(1000px 500px at 50% -10%, #1e293b 0%, #0b0f19 60%); color: #e2e8f0; display: grid; place-items: center; padding: 32px; }}
  .player {{ width: min(900px, 100%); background: rgba(255,255,255,0.04); border: 1px solid rgba(255,255,255,0.08); border-radius: 20px; overflow: hidden; box-shadow: 0 40px 100px rgba(0,0,0,0.5); }}
  video {{ width: 100%; display: block; background: #000; }}
  .info {{ padding: 20px 24px; }}
  h1 {{ margin: 0 0 6px; font-size: 20px; }}
  .meta {{ color: #64748b; font-size: 14px; margin: 0; }}
</style>
</head>
<body>
  <div class="player">
    <video controls autoplay playsinline src="{src}"></video>
    <div class="info">
      <h1>{name}</h1>
      <p class="meta">Cap Recording</p>
    </div>
  </div>
</body>
</html>"##,
        name = name,
        src = src,
    ))
}

fn render_hls_page(name: &str, src: &str) -> Html<String> {
    let src = js_escape(src);
    Html(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>{name} &mdash; Cap</title>
<style>
  :root {{ color-scheme: dark; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif; }}
  * {{ box-sizing: border-box; }}
  body {{ margin: 0; min-height: 100vh; background: radial-gradient(1000px 500px at 50% -10%, #1e293b 0%, #0b0f19 60%); color: #e2e8f0; display: grid; place-items: center; padding: 32px; }}
  .player {{ width: min(900px, 100%); background: rgba(255,255,255,0.04); border: 1px solid rgba(255,255,255,0.08); border-radius: 20px; overflow: hidden; box-shadow: 0 40px 100px rgba(0,0,0,0.5); }}
  video {{ width: 100%; display: block; background: #000; }}
  .info {{ padding: 20px 24px; }}
  h1 {{ margin: 0 0 6px; font-size: 20px; }}
  .meta {{ color: #64748b; font-size: 14px; margin: 0; }}
</style>
</head>
<body>
  <div class="player">
    <video id="video" controls autoplay playsinline></video>
    <div class="info">
      <h1>{name}</h1>
      <p class="meta">Cap Recording</p>
    </div>
  </div>
  <script src="https://cdn.jsdelivr.net/npm/hls.js@1"></script>
  <script>
    const video = document.getElementById('video');
    const src = '{src}';
    if (Hls.isSupported()) {{
      const hls = new Hls();
      hls.loadSource(src);
      hls.attachMedia(video);
    }} else if (video.canPlayType('application/vnd.apple.mpegurl')) {{
      video.src = src;
    }}
  </script>
</body>
</html>"##,
        name = name,
        src = src,
    ))
}

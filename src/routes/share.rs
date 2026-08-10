use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};
use std::sync::Arc;

use crate::error::ApiError;
use crate::routes::access;
use crate::routes::ui::{self, LOGO_SVG};
use crate::state::AppState;
use crate::storage;

pub async fn blank() -> Html<&'static str> {
    Html("")
}

pub async fn video(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    let row = sqlx::query_as::<_, (String, String, Option<String>, Option<serde_json::Value>, bool, Option<f64>, Option<i32>, Option<i32>, Option<f64>, chrono::DateTime<chrono::Utc>, String)>(
        "SELECT id, owner_id, name, source, is_screenshot, duration, width, height, fps, created_at, access_mode FROM videos WHERE id = $1",
    )
    .bind(&video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    if !access::policy_allows(&row.10, &row.1, &video_id, &headers, None, &state.signer) {
        if row.10 == access::AccessMode::Password.as_str() {
            return Ok(render_unlock_page(&video_id).into_response());
        }
        return Err(ApiError::NotFound);
    }

    let name = row.2.unwrap_or_else(|| "Cap Recording".into());
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

fn share_css() -> &'static str {
    r##"
  body {
    min-height: 100vh;
    display: flex;
    flex-direction: column;
  }
  .topbar {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px 20px;
    background: var(--bg-elevated);
    border-bottom: 1px solid var(--line);
  }
  .topbar .logo-mark { width: 28px; height: 28px; flex-shrink: 0; }
  .topbar .wordmark {
    font-size: 15px;
    font-weight: 700;
    letter-spacing: -0.02em;
    color: var(--ink);
  }
  .topbar .sep {
    width: 1px;
    height: 18px;
    background: var(--line-strong);
    margin: 0 4px;
  }
  .topbar .title {
    font-size: 14px;
    font-weight: 500;
    color: var(--ink-soft);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .stage {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    padding: 32px 20px 48px;
    gap: 20px;
  }
  .player {
    width: min(960px, 100%);
    background: #0a0a0a;
    border-radius: 14px;
    overflow: hidden;
    box-shadow: var(--shadow-lg);
    border: 1px solid rgba(0,0,0,0.08);
  }
  .player video, .player img {
    width: 100%;
    display: block;
    background: #000;
    max-height: min(72vh, 720px);
    object-fit: contain;
  }
  .player img {
    background: var(--bg-elevated);
  }
  .meta {
    width: min(960px, 100%);
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
  }
  .meta h1 {
    margin: 0;
    font-size: 18px;
    font-weight: 600;
    letter-spacing: -0.02em;
    color: var(--ink);
  }
  .meta p {
    margin: 4px 0 0;
    font-size: 13px;
    color: var(--ink-faint);
  }
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 6px 10px;
    border-radius: 999px;
    background: var(--bg-elevated);
    border: 1px solid var(--line);
    color: var(--ink-soft);
    font-size: 12px;
    font-weight: 500;
    white-space: nowrap;
  }
  .badge .logo-mark { width: 14px; height: 14px; }
  @media (max-width: 640px) {
    .stage { padding: 20px 14px 32px; }
    .meta { flex-direction: column; align-items: flex-start; }
    .player { border-radius: 10px; }
  }
"##
}

fn unlock_css() -> &'static str {
    r##"
  body {
    min-height: 100vh;
    display: grid;
    place-items: center;
    padding: 24px;
  }
  .card {
    width: min(380px, 100%);
    padding: 36px 28px 28px;
    border-radius: 20px;
    background: var(--bg-elevated);
    border: 1px solid var(--line);
    box-shadow: var(--shadow);
    text-align: center;
  }
  .brand {
    display: grid;
    place-items: center;
    gap: 12px;
    margin-bottom: 24px;
  }
  h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 700;
    letter-spacing: -0.02em;
  }
  .subtitle {
    margin: 6px 0 0;
    font-size: 14px;
    color: var(--ink-soft);
    line-height: 1.45;
  }
  form { text-align: left; }
  .field {
    display: grid;
    gap: 6px;
    margin-bottom: 12px;
  }
  .field span {
    font-size: 13px;
    font-weight: 500;
    color: var(--ink-soft);
  }
  input {
    width: 100%;
    padding: 12px 14px;
    border-radius: var(--radius-sm);
    border: 1px solid var(--line-strong);
    background: var(--bg);
    color: var(--ink);
    font: inherit;
    font-size: 15px;
    outline: none;
    transition: border-color .15s ease, box-shadow .15s ease;
  }
  input:focus {
    border-color: var(--brand);
    box-shadow: 0 0 0 3px var(--brand-soft);
  }
  button {
    width: 100%;
    padding: 12px 16px;
    border: 0;
    border-radius: var(--radius-sm);
    background: var(--ink);
    color: #fff;
    font: inherit;
    font-size: 15px;
    font-weight: 600;
    cursor: pointer;
  }
  button:hover { background: #000; }
  .alert {
    min-height: 1.25em;
    margin: 12px 0 0;
    font-size: 13px;
    color: var(--danger);
    text-align: center;
  }
"##
}

fn render_unlock_page(video_id: &str) -> Html<String> {
    let endpoint = ui::html_escape(&format!("/api/public/videos/{video_id}/access/unlock"));
    Html(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
{head}
</head>
<body>
<div class="card">
  <div class="brand">
    {logo}
    <div>
      <h1>Password protected</h1>
      <p class="subtitle">Enter the password to view this recording.</p>
    </div>
  </div>
  <form data-endpoint="{endpoint}">
    <label class="field"><span>Password</span>
      <input id="password" name="password" type="password" autocomplete="current-password" required autofocus />
    </label>
    <button type="submit">Unlock recording</button>
    <p class="alert" role="alert"></p>
  </form>
</div>
<script>
document.querySelector('form').addEventListener('submit', async event => {{
  event.preventDefault();
  const form = event.currentTarget;
  const response = await fetch(form.dataset.endpoint, {{
    method: 'POST',
    headers: {{ 'Content-Type': 'application/json' }},
    body: JSON.stringify({{ password: form.password.value }})
  }});
  if (response.ok) location.reload();
  else form.querySelector('.alert').textContent = 'Incorrect password';
}});
</script>
</body>
</html>"##,
        head = ui::head("Unlock recording", unlock_css()),
        logo = LOGO_SVG,
        endpoint = endpoint,
    ))
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

fn render_shell(name: &str, media: &str) -> Html<String> {
    let safe_name = ui::html_escape(name);
    Html(format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
{head}
</head>
<body>
  <header class="topbar">
    {logo}
    <span class="wordmark">Cap</span>
    <span class="sep" aria-hidden="true"></span>
    <span class="title">{safe_name}</span>
  </header>
  <main class="stage">
    <div class="player">
      {media}
    </div>
    <div class="meta">
      <div>
        <h1>{safe_name}</h1>
        <p>Shared recording</p>
      </div>
      <span class="badge">{logo_sm} Cap</span>
    </div>
  </main>
</body>
</html>"##,
        head = ui::head(&format!("{name} — Cap"), share_css()),
        logo = LOGO_SVG,
        logo_sm = LOGO_SVG,
        safe_name = safe_name,
        media = media,
    ))
}

fn render_image_page(name: &str, src: &str) -> Html<String> {
    let src = ui::html_escape(src);
    let alt = ui::html_escape(name);
    render_shell(
        name,
        &format!(r#"<img src="{src}" alt="{alt}" />"#),
    )
}

fn render_mp4_page(name: &str, src: &str) -> Html<String> {
    let src = ui::html_escape(src);
    render_shell(
        name,
        &format!(r#"<video controls autoplay playsinline src="{src}"></video>"#),
    )
}

fn render_hls_page(name: &str, src: &str) -> Html<String> {
    let src = js_escape(src);
    let media = format!(
        r##"<video id="video" controls autoplay playsinline></video>
<script src="https://cdn.jsdelivr.net/npm/hls.js@1"></script>
<script>
  const video = document.getElementById('video');
  const src = '{src}';
  if (typeof Hls !== 'undefined' && Hls.isSupported()) {{
    const hls = new Hls();
    hls.loadSource(src);
    hls.attachMedia(video);
  }} else if (video.canPlayType('application/vnd.apple.mpegurl')) {{
    video.src = src;
  }}
</script>"##,
        src = src,
    );
    render_shell(name, &media)
}

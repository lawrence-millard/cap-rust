use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;

use crate::auth::ensure_user;
use crate::error::ApiError;
use crate::state::AppState;
use std::sync::Arc;

#[derive(Deserialize)]
#[allow(dead_code)]
pub struct SessionRequestParams {
    #[serde(rename = "type")]
    pub request_type: Option<String>,
    pub port: Option<u16>,
    pub platform: Option<String>,
}

#[derive(Deserialize)]
pub struct PasscodeForm {
    pub passcode: String,
    pub port: Option<u16>,
}

/// GET /api/desktop/session/request?type=api_key&port=...&platform=...
/// Shows a passcode page (or immediately redirects if no passcode is configured).
pub async fn request(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SessionRequestParams>,
) -> Result<axum::response::Response, ApiError> {
    let passcode = state.config.cap_passcode.clone();

    // no passcode configured: auto-authorize and mint a key immediately
    if passcode.as_deref().unwrap_or_default().is_empty() {
        return auto_authorize(&state, params.port, params.request_type.as_deref()).await;
    }

    let html = render_passcode_page(params.port, false);
    Ok((StatusCode::OK, Html(html)).into_response())
}

/// POST /api/desktop/session/request  (form body)
pub async fn request_post(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<PasscodeForm>,
) -> Result<axum::response::Response, ApiError> {
    let configured = state.config.cap_passcode.clone().unwrap_or_default();
    if !configured.is_empty() && !constant_time_eq(configured.as_bytes(), form.passcode.as_bytes())
    {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Html(render_passcode_page(form.port, false)),
        )
            .into_response());
    }
    auto_authorize(&state, form.port, None).await
}

async fn auto_authorize(
    state: &AppState,
    port: Option<u16>,
    _request_type: Option<&str>,
) -> Result<axum::response::Response, ApiError> {
    let key = uuid::Uuid::new_v4().to_string();
    let user_id = "u_single_user".to_string();
    ensure_user(&state.db, &user_id, "Owner", None).await?;

    sqlx::query("INSERT INTO auth_api_keys (id, user_id, source) VALUES ($1, $2, 'desktop')")
        .bind(&key)
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let params = format!("type=api_key&api_key={key}&user_id={user_id}");

    if let Some(port) = port {
        if port == 0 {
            return Err(ApiError::BadRequest("invalid port".into()));
        }
        let url = format!("http://127.0.0.1:{port}/?{params}");
        return Ok(Redirect::to(&url).into_response());
    }

    let url = format!("cap-desktop://signin?{params}");
    Ok(Redirect::to(&url).into_response())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn render_passcode_page(port: Option<u16>, _auto_issue: bool) -> String {
    let notice = String::new();
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
<title>Connect Cap</title>
<style>
  :root {{ color-scheme: light; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Inter, sans-serif; }}
  * {{ box-sizing: border-box; }}
  body {{
    margin: 0; min-height: 100vh; display: grid; place-items: center;
    background: linear-gradient(160deg, #0f172a 0%, #1e293b 60%, #2563eb 160%);
    color: #e2e8f0; padding: 24px;
  }}
  .card {{
    width: min(420px, 100%); padding: 36px 32px; border-radius: 24px;
    background: rgba(255,255,255,0.06); backdrop-filter: blur(20px);
    border: 1px solid rgba(255,255,255,0.12);
    box-shadow: 0 30px 80px rgba(0,0,0,0.5);
    text-align: center;
  }}
  .logo {{
    width: 64px; height: 64px; margin: 0 auto 18px; border-radius: 18px;
    background: linear-gradient(135deg, #3b82f6, #8b5cf6);
    display: grid; place-items: center; font-weight: 800; font-size: 26px; color: white;
    box-shadow: 0 12px 30px rgba(59,130,246,0.4);
  }}
  h1 {{ margin: 0 0 8px; font-size: 24px; }}
  p {{ margin: 0 0 24px; color: #94a3b8; font-size: 15px; line-height: 1.5; }}
  input[type=password] {{
    width: 100%; padding: 14px 16px; border-radius: 14px; border: 1px solid rgba(255,255,255,0.16);
    background: rgba(255,255,255,0.08); color: #f1f5f9; font-size: 16px; text-align: center; outline: none;
  }}
  input[type=password]:focus {{ border-color: #3b82f6; box-shadow: 0 0 0 3px rgba(59,130,246,0.3); }}
  button {{
    margin-top: 14px; width: 100%; padding: 14px 16px; border: 0; border-radius: 14px;
    background: linear-gradient(135deg, #3b82f6, #2563eb); color: white; font-size: 16px; font-weight: 700;
    cursor: pointer; transition: transform .06s ease, box-shadow .15s ease;
  }}
  button:hover {{ box-shadow: 0 10px 30px rgba(59,130,246,0.5); transform: translateY(-1px); }}
  .notice {{ background: rgba(34,197,94,0.15); border: 1px solid rgba(34,197,94,0.3); color: #86efac;
    padding: 10px 14px; border-radius: 12px; font-size: 14px; margin-bottom: 18px; }}
  .error {{ background: rgba(239,68,68,0.15); border: 1px solid rgba(239,68,68,0.3); color: #fca5a5;
    padding: 10px 14px; border-radius: 12px; font-size: 14px; margin-bottom: 18px; }}
</style>
</head>
<body>
<div class="card">
  <div class="logo">&#9679;</div>
  <h1>Connect Cap Desktop</h1>
  <p>Enter the passcode to authorize this device and upload recordings.</p>
  {notice}
  <form method="post" action="/api/desktop/session/request">
    <input type="hidden" name="port" value="{port_val}" />
    <input type="password" name="passcode" placeholder="Passcode" autofocus autocomplete="off" />
    <button type="submit">Authorize</button>
  </form>
</div>
</body>
</html>"##,
        notice = notice,
        port_val = port.unwrap_or_default(),
    )
}

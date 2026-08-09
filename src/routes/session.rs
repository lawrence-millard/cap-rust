use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;

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
pub struct SessionForm {
    pub action: String,
    pub username: String,
    pub password: String,
    pub port: Option<u16>,
}

/// GET /api/desktop/session/request?type=api_key&port=...&platform=...
/// Shows a login/register page. On success the desktop receives an API key.
pub async fn request(
    State(state): State<Arc<AppState>>,
    Query(params): Query<SessionRequestParams>,
) -> Result<axum::response::Response, ApiError> {
    let html = render_session_page(
        params.port,
        params.request_type.as_deref(),
        state.config.cap_signups,
        None,
    );
    Ok((StatusCode::OK, Html(html)).into_response())
}

/// POST /api/desktop/session/request  (form body)
pub async fn request_post(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<SessionForm>,
) -> Result<axum::response::Response, ApiError> {
    let username = form.username.trim().to_string();
    if username.len() < 3 {
        return Err(ApiError::BadRequest("username too short".into()));
    }
    if form.password.len() < 8 {
        return Err(ApiError::BadRequest("password too short".into()));
    }

    let user_id = match form.action.as_str() {
        "register" => {
            if !state.config.cap_signups {
                return Ok((
                    StatusCode::FORBIDDEN,
                    Html(render_session_page(
                        form.port,
                        None,
                        state.config.cap_signups,
                        Some("Registration is disabled on this server"),
                    )),
                )
                    .into_response());
            }
            match create_user(&state, &username, &form.password).await {
                Ok(uid) => uid,
                Err(e) => {
                    return Ok((
                        StatusCode::BAD_REQUEST,
                        Html(render_session_page(
                            form.port,
                            None,
                            state.config.cap_signups,
                            Some(&e.to_string()),
                        )),
                    )
                        .into_response());
                }
            }
        }
        "login" => match authenticate(&state, &username, &form.password).await {
            Ok(uid) => uid,
            Err(_) => {
                return Ok((
                    StatusCode::UNAUTHORIZED,
                    Html(render_session_page(
                        form.port,
                        None,
                        state.config.cap_signups,
                        Some("Invalid username or password"),
                    )),
                )
                    .into_response());
            }
        },
        _ => return Err(ApiError::BadRequest("invalid action".into())),
    };

    // mint an API key for the desktop device
    let key = uuid::Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO auth_api_keys (id, user_id, source) VALUES ($1, $2, 'desktop')")
        .bind(&key)
        .bind(&user_id)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    let params = format!("type=api_key&api_key={key}&user_id={user_id}");

    if let Some(port) = form.port {
        if port == 0 {
            return Err(ApiError::BadRequest("invalid port".into()));
        }
        let url = format!("http://127.0.0.1:{port}/?{params}");
        return Ok(Redirect::to(&url).into_response());
    }

    let url = format!("cap-desktop://signin?{params}");
    Ok(Redirect::to(&url).into_response())
}

async fn create_user(state: &AppState, username: &str, password: &str) -> Result<String, ApiError> {
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)")
        .bind(username)
        .fetch_one(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if exists {
        return Err(ApiError::BadRequest(
            "username already taken; try logging in instead".into(),
        ));
    }

    let user_id = uuid::Uuid::new_v4().to_string();
    let password_hash = crate::auth::hash_password(password)?;

    sqlx::query("INSERT INTO users (id, name, username, password_hash) VALUES ($1, $2, $3, $4)")
        .bind(&user_id)
        .bind(username)
        .bind(username)
        .bind(&password_hash)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(user_id)
}

async fn authenticate(
    state: &AppState,
    username: &str,
    password: &str,
) -> Result<String, ApiError> {
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, password_hash FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::Unauthorized)?;

    let hash = row.1.ok_or(ApiError::Unauthorized)?;
    if !crate::auth::verify_password(password, &hash) {
        return Err(ApiError::Unauthorized);
    }
    Ok(row.0)
}

fn render_session_page(
    port: Option<u16>,
    _request_type: Option<&str>,
    signups: bool,
    error: Option<&str>,
) -> String {
    let error_html = match error {
        Some(msg) => format!(r#"<div class="error">{}</div>"#, html_escape(msg)),
        None => String::new(),
    };
    let register_row = if signups {
        r#"<p class="switch">No account? <a href='#' onclick="show('register');return false;">Create one</a></p>"#.to_string()
    } else {
        String::new()
    };
    let register_panel = if signups {
        r#"<div id="register" class="form hidden">
  <form method="post" action="/api/desktop/session/request">
    <input type="hidden" name="action" value="register" />
    <input type="hidden" name="port" value="{port_val}" />
    <input type="text" name="username" placeholder="Username" autocomplete="username" />
    <input type="password" name="password" placeholder="Password (8+ chars)" autocomplete="new-password" />
    <button type="submit">Create account &amp; connect</button>
  </form>
  <p class="switch">Already have an account? <a href='#' onclick="show('login');return false;">Log in</a></p>
</div>"#.to_string()
    } else {
        String::new()
    };
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
  input {{
    width: 100%; padding: 14px 16px; margin-bottom: 12px; border-radius: 14px; border: 1px solid rgba(255,255,255,0.16);
    background: rgba(255,255,255,0.08); color: #f1f5f9; font-size: 16px; text-align: center; outline: none;
  }}
  input:focus {{ border-color: #3b82f6; box-shadow: 0 0 0 3px rgba(59,130,246,0.3); }}
  button {{
    width: 100%; padding: 14px 16px; border: 0; border-radius: 14px;
    background: linear-gradient(135deg, #3b82f6, #2563eb); color: white; font-size: 16px; font-weight: 700;
    cursor: pointer; transition: transform .06s ease, box-shadow .15s ease;
  }}
  button:hover {{ box-shadow: 0 10px 30px rgba(59,130,246,0.5); transform: translateY(-1px); }}
  .switch {{ font-size: 14px; margin: 16px 0 0; }}
  .switch a {{ color: #93c5fd; }}
  .hidden {{ display: none; }}
  .error {{ background: rgba(239,68,68,0.15); border: 1px solid rgba(239,68,68,0.3); color: #fca5a5;
    padding: 10px 14px; border-radius: 12px; font-size: 14px; margin-bottom: 18px; }}
</style>
</head>
<body>
<div class="card">
  <div class="logo">&#9679;</div>
  <h1>Connect Cap Desktop</h1>
  <p>Log in or create an account to authorize this device.</p>
  {error_html}
  <div id="login" class="form">
  <form method="post" action="/api/desktop/session/request">
    <input type="hidden" name="action" value="login" />
    <input type="hidden" name="port" value="{port_val}" />
    <input type="text" name="username" placeholder="Username" autofocus autocomplete="username" />
    <input type="password" name="password" placeholder="Password" autocomplete="current-password" />
    <button type="submit">Log in &amp; connect</button>
  </form>
  {register_row}
  </div>
  {register_panel}
</div>
<script>
  function show(name) {{
    document.getElementById('login').classList.toggle('hidden', name !== 'login');
    var r = document.getElementById('register');
    if (r) r.classList.toggle('hidden', name !== 'register');
  }}
</script>
</body>
</html>"##,
        port_val = port.unwrap_or_default(),
        error_html = error_html,
        register_row = register_row,
        register_panel = register_panel,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect};
use serde::Deserialize;

use crate::error::ApiError;
use crate::routes::ui::{self, LOGO_SVG};
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
            match crate::auth::register_user(&state.db, &username, &form.password).await {
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
        "login" => match crate::auth::login_user(&state.db, &username, &form.password).await {
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

fn render_session_page(
    port: Option<u16>,
    _request_type: Option<&str>,
    signups: bool,
    error: Option<&str>,
) -> String {
    let error_html = match error {
        Some(msg) => format!(
            r#"<div class="error" role="alert">{}</div>"#,
            ui::html_escape(msg)
        ),
        None => String::new(),
    };
    let port_val = port.unwrap_or_default();
    let register_row = if signups {
        r##"<p class="switch">Don&#39;t have an account? <a href="#" onclick="show('register');return false;">Sign up here</a></p>"##
            .to_string()
    } else {
        String::new()
    };
    let register_panel = if signups {
        format!(
            r##"<div id="register" class="form hidden">
  <form method="post" action="/api/desktop/session/request">
    <input type="hidden" name="action" value="register" />
    <input type="hidden" name="port" value="{port_val}" />
    <label class="field"><span>Username</span>
      <input type="text" name="username" placeholder="Choose a username" autocomplete="username" required />
    </label>
    <label class="field"><span>Password</span>
      <input type="password" name="password" placeholder="At least 8 characters" autocomplete="new-password" required minlength="8" />
    </label>
    <button type="submit">Create account &amp; connect</button>
  </form>
  <p class="switch">Already have an account? <a href="#" onclick="show('login');return false;">Log in</a></p>
</div>"##,
            port_val = port_val,
        )
    } else {
        String::new()
    };

    let css = r##"
  body {
    display: grid;
    place-items: center;
    padding: 24px;
    min-height: 100vh;
  }
  .card {
    width: min(400px, 100%);
    padding: 40px 32px 32px;
    border-radius: 20px;
    background: var(--bg-muted);
    border: 1px solid var(--line);
    box-shadow: var(--shadow);
    text-align: center;
  }
  .brand {
    display: grid;
    place-items: center;
    gap: 14px;
    margin-bottom: 28px;
  }
  .brand .logo-mark { width: 40px; height: 40px; }
  h1 {
    margin: 0;
    font-size: 22px;
    font-weight: 700;
    letter-spacing: -0.02em;
    color: var(--ink);
  }
  .subtitle {
    margin: 6px 0 0;
    color: var(--ink-soft);
    font-size: 14px;
    line-height: 1.5;
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
    background: var(--bg-elevated);
    color: var(--ink);
    font: inherit;
    font-size: 15px;
    outline: none;
    transition: border-color .15s ease, box-shadow .15s ease;
  }
  input::placeholder { color: var(--ink-faint); }
  input:focus {
    border-color: var(--brand);
    box-shadow: 0 0 0 3px var(--brand-soft);
  }
  button {
    width: 100%;
    margin-top: 4px;
    padding: 12px 16px;
    border: 0;
    border-radius: var(--radius-sm);
    background: var(--ink);
    color: #fff;
    font: inherit;
    font-size: 15px;
    font-weight: 600;
    cursor: pointer;
    transition: background .15s ease, transform .06s ease;
  }
  button:hover { background: #000; }
  button:active { transform: translateY(1px); }
  .switch {
    margin: 18px 0 0;
    font-size: 13px;
    color: var(--ink-soft);
    text-align: center;
  }
  .hidden { display: none; }
  .error {
    background: var(--danger-bg);
    border: 1px solid var(--danger-line);
    color: var(--danger);
    padding: 10px 12px;
    border-radius: var(--radius-sm);
    font-size: 13px;
    margin-bottom: 16px;
    text-align: left;
  }
"##;

    format!(
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
      <h1>Sign in to Cap</h1>
      <p class="subtitle">Authorize Cap Desktop on this server.</p>
    </div>
  </div>
  {error_html}
  <div id="login" class="form">
  <form method="post" action="/api/desktop/session/request">
    <input type="hidden" name="action" value="login" />
    <input type="hidden" name="port" value="{port_val}" />
    <label class="field"><span>Username</span>
      <input type="text" name="username" placeholder="Your username" autofocus autocomplete="username" required />
    </label>
    <label class="field"><span>Password</span>
      <input type="password" name="password" placeholder="Your password" autocomplete="current-password" required />
    </label>
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
        head = ui::head("Sign in to Cap", css),
        logo = LOGO_SVG,
        port_val = port_val,
        error_html = error_html,
        register_row = register_row,
        register_panel = register_panel,
    )
}

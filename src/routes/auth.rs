use axum::extract::State;
use axum::response::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::auth;
use crate::error::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct RegisterReq {
    pub username: String,
    pub password: String,
}

#[derive(Deserialize)]
pub struct LoginReq {
    pub username: String,
    pub password: String,
}

/// POST /api/auth/register
pub async fn register(
    State(state): State<Arc<AppState>>,
    axum::extract::Json(body): axum::extract::Json<RegisterReq>,
) -> Result<Json<Value>, ApiError> {
    if !state.config.cap_signups {
        return Err(ApiError::Forbidden);
    }

    let user_id = auth::register_user(&state.db, &body.username, &body.password).await?;
    let username = body.username.trim();

    let token = auth::issue_jwt(&state, &user_id)?;

    Ok(Json(json!({
        "token": token,
        "user": {
            "id": user_id,
            "name": username,
            "username": username,
            "email": null,
        },
    })))
}

/// POST /api/auth/login
pub async fn login(
    State(state): State<Arc<AppState>>,
    axum::extract::Json(body): axum::extract::Json<LoginReq>,
) -> Result<Json<Value>, ApiError> {
    let user_id = auth::login_user(&state.db, &body.username, &body.password).await?;
    let token = auth::issue_jwt(&state, &user_id)?;

    let row = sqlx::query_as::<_, (Option<String>, Option<String>, Option<String>)>(
        "SELECT name, email, username FROM users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::Unauthorized)?;

    Ok(Json(json!({
        "token": token,
        "user": {
            "id": user_id,
            "name": row.0,
            "email": row.1,
            "username": row.2,
        },
    })))
}

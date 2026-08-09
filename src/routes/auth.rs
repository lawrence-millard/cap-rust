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

    let username = body.username.trim();
    if username.len() < 3 {
        return Err(ApiError::BadRequest(
            "username must be at least 3 characters".into(),
        ));
    }
    if body.password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }

    // Check username uniqueness
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)")
        .bind(username)
        .fetch_one(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    if exists {
        return Err(ApiError::BadRequest("username already taken".into()));
    }

    let user_id = uuid::Uuid::new_v4().to_string();
    let password_hash = auth::hash_password(&body.password)?;

    sqlx::query("INSERT INTO users (id, name, username, password_hash) VALUES ($1, $2, $3, $4)")
        .bind(&user_id)
        .bind(username)
        .bind(username)
        .bind(&password_hash)
        .execute(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

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
    let username = body.username.trim();

    let row =
        sqlx::query_as::<
            _,
            (
                String,
                Option<String>,
                Option<String>,
                Option<String>,
                Option<String>,
            ),
        >("SELECT id, name, email, username, password_hash FROM users WHERE username = $1")
        .bind(username)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .ok_or(ApiError::Unauthorized)?;

    let hash = row.4.ok_or(ApiError::Unauthorized)?;
    if !auth::verify_password(&body.password, &hash) {
        return Err(ApiError::Unauthorized);
    }

    let user_id = row.0;
    let token = auth::issue_jwt(&state, &user_id)?;

    Ok(Json(json!({
        "token": token,
        "user": {
            "id": user_id,
            "name": row.1,
            "email": row.2,
            "username": row.3,
        },
    })))
}

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use serde::Deserialize;
use sqlx::PgPool;
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CurrentUser(pub User);

impl CurrentUser {
    pub fn user_id(&self) -> &str {
        &self.0.id
    }
}

impl<S> FromRequestParts<S> for CurrentUser
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let headers = &parts.headers;
        let bearer = headers
            .get("Authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;

        let user = lookup_user(&app_state.db, bearer)
            .await
            .map_err(|_| ApiError::Unauthorized)?;

        Ok(CurrentUser(user))
    }
}

pub async fn lookup_user(db: &PgPool, token: &str) -> Result<User, ApiError> {
    let user_id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM auth_api_keys WHERE id = $1")
            .bind(token)
            .fetch_optional(db)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

    let user_id = user_id.ok_or(ApiError::Unauthorized)?;

    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT id, name, email FROM users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::Unauthorized)?;

    Ok(User {
        id: row.0,
        name: row.1,
        email: row.2,
    })
}

pub async fn ensure_user(
    db: &PgPool,
    user_id: &str,
    name: &str,
    email: Option<&str>,
) -> Result<User, ApiError> {
    sqlx::query(
        "INSERT INTO users (id, name, email) VALUES ($1, $2, $3) ON CONFLICT (id) DO UPDATE SET name = $2",
    )
    .bind(user_id)
    .bind(name)
    .bind(email)
    .execute(db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(User {
        id: user_id.to_string(),
        name: Some(name.to_string()),
        email: email.map(|s| s.to_string()),
    })
}

impl FromRef<Arc<AppState>> for AppState {
    fn from_ref(input: &Arc<AppState>) -> Self {
        (**input).clone()
    }
}

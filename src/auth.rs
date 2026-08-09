use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::Arc;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub id: String,
    pub name: Option<String>,
    pub email: Option<String>,
    pub username: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CurrentUser(pub User);

impl CurrentUser {
    pub fn user_id(&self) -> &str {
        &self.0.id
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
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

        // First try JWT (user-facing auth), then fall back to a desktop API key.
        if let Some(user) = lookup_user_by_jwt(&app_state, bearer).await {
            return Ok(CurrentUser(user));
        }
        if let Ok(user) = lookup_user(&app_state.db, bearer).await {
            return Ok(CurrentUser(user));
        }
        Err(ApiError::Unauthorized)
    }
}

/// Hash a password with Argon2. Returns the encoded PHC string.
pub fn hash_password(password: &str) -> Result<String, ApiError> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))
}

/// Verify a password against an Argon2 PHC string.
pub fn verify_password(password: &str, hash: &str) -> bool {
    match PasswordHash::new(hash) {
        Ok(parsed) => Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok(),
        Err(_) => false,
    }
}

/// Mint a JWT for a user id. Uses SIGN_SECRET as the HMAC key.
pub fn issue_jwt(app_state: &AppState, user_id: &str) -> Result<String, ApiError> {
    let exp = crate::sign::now() + app_state.config.jwt_ttl_secs;
    let claims = Claims {
        sub: user_id.to_string(),
        exp: exp as usize,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(app_state.config.sign_secret.as_bytes()),
    )
    .map_err(|e| ApiError::Internal(e.to_string()))
}

async fn lookup_user_by_jwt(app_state: &AppState, token: &str) -> Option<User> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(app_state.config.sign_secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .ok()?;
    let user_id = data.claims.sub;
    let pool = app_state.db.clone();
    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT id, name, email, username FROM users WHERE id = $1",
    )
    .bind(&user_id)
    .fetch_optional(&pool)
    .await
    .ok()?;
    row.map(|(id, name, email, username)| User {
        id,
        name,
        email,
        username,
    })
}

pub async fn lookup_user(db: &PgPool, token: &str) -> Result<User, ApiError> {
    let user_id: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM auth_api_keys WHERE id = $1")
            .bind(token)
            .fetch_optional(db)
            .await
            .map_err(|e| ApiError::Internal(e.to_string()))?;

    let user_id = user_id.ok_or(ApiError::Unauthorized)?;

    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT id, name, email, username FROM users WHERE id = $1",
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
        username: row.3,
    })
}

impl FromRef<Arc<AppState>> for AppState {
    fn from_ref(input: &Arc<AppState>) -> Self {
        (**input).clone()
    }
}

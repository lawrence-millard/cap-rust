use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;

use crate::error::ApiError;
use crate::state::AppState;

static ARGON2_JOBS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(4));

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
    Arc<AppState>: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = Arc::<AppState>::from_ref(state);
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
pub async fn hash_password(password: &str) -> Result<String, ApiError> {
    let permit = ARGON2_JOBS
        .acquire()
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let password = password.to_owned();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let salt = SaltString::generate(&mut OsRng);
        Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| ApiError::Internal(e.to_string()))
    })
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
}

/// Verify a password against an Argon2 PHC string.
pub async fn verify_password(password: &str, hash: &str) -> bool {
    let Ok(permit) = ARGON2_JOBS.acquire().await else {
        return false;
    };
    let password = password.to_owned();
    let hash = hash.to_owned();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        match PasswordHash::new(&hash) {
            Ok(parsed) => Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok(),
            Err(_) => false,
        }
    })
    .await
    .unwrap_or(false)
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
    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT users.id, users.name, users.email, users.username \
         FROM auth_api_keys JOIN users ON users.id = auth_api_keys.user_id \
         WHERE auth_api_keys.id = $1",
    )
    .bind(token)
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

/// Validate + create a user account. Usernames are trimmed; must be 3-32
/// chars; passwords must be at least 8 chars.
/// ponytail: usernames are case-sensitive (stored as given). To allow "Bob"/"bob"
/// collisions we'd need a lowercase normalization migration; add when accounts
/// matter enough to care.
pub async fn register_user(
    db: &PgPool,
    username: &str,
    password: &str,
) -> Result<String, ApiError> {
    let username = username.trim();
    if !(3..=32).contains(&username.len()) {
        return Err(ApiError::BadRequest(
            "username must be 3-32 characters".into(),
        ));
    }
    if password.len() < 8 {
        return Err(ApiError::BadRequest(
            "password must be at least 8 characters".into(),
        ));
    }

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)")
        .bind(username)
        .fetch_one(db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    if exists {
        return Err(ApiError::BadRequest(
            "username already taken; try logging in instead".into(),
        ));
    }

    let user_id = uuid::Uuid::new_v4().to_string();
    let password_hash = hash_password(password).await?;

    sqlx::query("INSERT INTO users (id, name, username, password_hash) VALUES ($1, $2, $3, $4)")
        .bind(&user_id)
        .bind(username)
        .bind(username)
        .bind(&password_hash)
        .execute(db)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;

    Ok(user_id)
}

/// Authenticate username/password against the stored Argon2 hash.
pub async fn login_user(db: &PgPool, username: &str, password: &str) -> Result<String, ApiError> {
    let username = username.trim();

    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, password_hash FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::Unauthorized)?;

    let hash = row.1.ok_or(ApiError::Unauthorized)?;
    if !verify_password(password, &hash).await {
        return Err(ApiError::Unauthorized);
    }
    Ok(row.0)
}

#[cfg(test)]
mod tests {
    use super::{hash_password, verify_password};

    #[tokio::test]
    async fn password_hash_roundtrip() {
        let hash = hash_password("correct horse battery staple")
            .await
            .expect("hash password");

        assert!(verify_password("correct horse battery staple", &hash).await);
        assert!(!verify_password("wrong password", &hash).await);
        assert!(!verify_password("correct horse battery staple", "invalid hash").await);
    }
}

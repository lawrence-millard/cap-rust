use argon2::Argon2;
use argon2::password_hash::{
    PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng,
};
use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::sync::{Arc, LazyLock};
use tokio::sync::Semaphore;

use crate::error::ApiError;
use crate::state::AppState;

static ARGON2_JOBS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(4));

/// Dummy PHC used to equalize login timing when the username is missing.
static DUMMY_PASSWORD_HASH: LazyLock<String> = LazyLock::new(|| {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(b"timing-equalization-dummy", &salt)
        .expect("dummy password hash")
        .to_string()
});

pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 1024;

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

/// Optional bearer auth: missing/invalid credentials become `None` instead of 401.
#[derive(Debug, Clone, Default)]
pub struct OptionalUser(pub Option<CurrentUser>);

impl<S> FromRequestParts<S> for OptionalUser
where
    S: Send + Sync,
    Arc<AppState>: FromRef<S>,
{
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Ok(OptionalUser(
            CurrentUser::from_request_parts(parts, state).await.ok(),
        ))
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

pub fn validate_password_length(password: &str) -> Result<(), ApiError> {
    if !(MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.len()) {
        return Err(ApiError::BadRequest(
            "password must be 8-1024 characters".into(),
        ));
    }
    Ok(())
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

/// SHA-256 hex digest of an API key secret (used as a lookup key, not the stored secret).
pub fn api_key_token_hash(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn lookup_user(db: &PgPool, token: &str) -> Result<User, ApiError> {
    let token_hash = api_key_token_hash(token);
    // Prefer hashed keys; fall back to legacy plaintext `id` rows until re-login.
    let row = sqlx::query_as::<_, (String, Option<String>, Option<String>, Option<String>)>(
        "SELECT users.id, users.name, users.email, users.username \
         FROM auth_api_keys JOIN users ON users.id = auth_api_keys.user_id \
         WHERE auth_api_keys.token_hash = $1 \
            OR (auth_api_keys.token_hash IS NULL AND auth_api_keys.id = $2)",
    )
    .bind(&token_hash)
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

/// Insert a desktop API key. Returns the secret that Cap Desktop must store.
pub async fn mint_api_key(db: &PgPool, user_id: &str, source: &str) -> Result<String, ApiError> {
    let secret = uuid::Uuid::new_v4().to_string();
    let row_id = uuid::Uuid::new_v4().to_string();
    let token_hash = api_key_token_hash(&secret);
    sqlx::query(
        "INSERT INTO auth_api_keys (id, user_id, source, token_hash) VALUES ($1, $2, $3, $4)",
    )
    .bind(&row_id)
    .bind(user_id)
    .bind(source)
    .bind(&token_hash)
    .execute(db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;
    Ok(secret)
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(db) => db.code().as_deref() == Some("23505"),
        _ => false,
    }
}

/// Validate + create a user account. Usernames are trimmed; must be 3-32
/// chars; passwords must be 8-1024 chars.
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
    validate_password_length(password)?;

    let user_id = uuid::Uuid::new_v4().to_string();
    let password_hash = hash_password(password).await?;

    let result = sqlx::query(
        "INSERT INTO users (id, name, username, password_hash) VALUES ($1, $2, $3, $4)",
    )
    .bind(&user_id)
    .bind(username)
    .bind(username)
    .bind(&password_hash)
    .execute(db)
    .await;

    match result {
        Ok(_) => Ok(user_id),
        Err(error) if is_unique_violation(&error) => Err(ApiError::BadRequest(
            "username already taken; try logging in instead".into(),
        )),
        Err(error) => Err(ApiError::Internal(error.to_string())),
    }
}

/// Authenticate username/password against the stored Argon2 hash.
pub async fn login_user(db: &PgPool, username: &str, password: &str) -> Result<String, ApiError> {
    let username = username.trim();
    if password.len() > MAX_PASSWORD_LEN {
        return Err(ApiError::Unauthorized);
    }

    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT id, password_hash FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?;

    let (user_id, hash) = match row {
        Some((id, Some(hash))) => (Some(id), hash),
        _ => (None, DUMMY_PASSWORD_HASH.clone()),
    };

    if !verify_password(password, &hash).await {
        return Err(ApiError::Unauthorized);
    }
    user_id.ok_or(ApiError::Unauthorized)
}

#[cfg(test)]
mod tests {
    use super::{api_key_token_hash, hash_password, validate_password_length, verify_password};

    #[tokio::test]
    async fn password_hash_roundtrip() {
        let hash = hash_password("correct horse battery staple")
            .await
            .expect("hash password");

        assert!(verify_password("correct horse battery staple", &hash).await);
        assert!(!verify_password("wrong password", &hash).await);
        assert!(!verify_password("correct horse battery staple", "invalid hash").await);
    }

    #[test]
    fn password_length_bounds() {
        assert!(validate_password_length("short").is_err());
        assert!(validate_password_length("longenough").is_ok());
        assert!(validate_password_length(&"x".repeat(1025)).is_err());
    }

    #[test]
    fn api_key_hash_is_stable() {
        assert_eq!(
            api_key_token_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

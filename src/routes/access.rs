use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use crate::auth::{self, CurrentUser};
use crate::error::ApiError;
use crate::sign::Signer;
use crate::state::AppState;

const ACCESS_COOKIE: &str = "cap_recording_access";
const ACCESS_TTL_SECS: i64 = 15 * 60;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessMode {
    Public,
    Private,
    Password,
}

impl AccessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Private => "private",
            Self::Password => "password",
        }
    }
}

#[derive(Deserialize)]
pub struct SetAccessRequest {
    pub mode: AccessMode,
    pub password: Option<String>,
}

#[derive(Deserialize)]
pub struct UnlockRequest {
    pub password: String,
}

pub async fn set_access(
    State(state): State<Arc<AppState>>,
    user: CurrentUser,
    Path(video_id): Path<String>,
    Json(body): Json<SetAccessRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let password_hash = match body.mode {
        AccessMode::Password => {
            let password = body.password.as_deref().unwrap_or_default();
            if !(8..=1024).contains(&password.len()) {
                return Err(ApiError::BadRequest("password must be 8-1024 bytes".into()));
            }
            Some(auth::hash_password(password).await?)
        }
        AccessMode::Public | AccessMode::Private => None,
    };

    let updated = sqlx::query_scalar::<_, String>(
        "UPDATE videos SET access_mode = $3, access_password_hash = $4 \
         WHERE id = $1 AND owner_id = $2 RETURNING access_mode",
    )
    .bind(&video_id)
    .bind(user.user_id())
    .bind(body.mode.as_str())
    .bind(password_hash)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    Ok(Json(json!({ "accessMode": updated })))
}

pub async fn unlock(
    State(state): State<Arc<AppState>>,
    Path(video_id): Path<String>,
    Json(body): Json<UnlockRequest>,
) -> Result<Response, ApiError> {
    let row = sqlx::query_as::<_, (String, Option<String>)>(
        "SELECT access_mode, access_password_hash FROM videos WHERE id = $1",
    )
    .bind(&video_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| ApiError::Internal(e.to_string()))?
    .ok_or(ApiError::NotFound)?;

    if row.0 != AccessMode::Password.as_str() {
        return Err(ApiError::NotFound);
    }
    let hash = row.1.ok_or(ApiError::Unauthorized)?;
    if !auth::verify_password(&body.password, &hash).await {
        return Err(ApiError::Unauthorized);
    }

    let mut response = Json(json!({ "unlocked": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&issue_access_cookie(
            &state.signer,
            &video_id,
            state.config.web_url.starts_with("https://"),
        ))
        .map_err(|e| ApiError::Internal(e.to_string()))?,
    );
    Ok(response)
}

/// Check access using data already fetched by share/playlist handlers.
pub fn policy_allows(
    mode: &str,
    owner_id: &str,
    video_id: &str,
    headers: &HeaderMap,
    current_user: Option<&CurrentUser>,
    signer: &Signer,
) -> bool {
    current_user.is_some_and(|user| user.user_id() == owner_id)
        || mode == AccessMode::Public.as_str()
        || (mode == AccessMode::Password.as_str() && has_access_cookie(headers, signer, video_id))
}

/// Validate recording access cookie from request headers.
pub fn has_access_cookie(headers: &HeaderMap, signer: &Signer, video_id: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .any(|(name, value)| name == ACCESS_COOKIE && verify_cookie_value(signer, video_id, value))
}

fn signing_path(video_id: &str) -> String {
    format!("recording-access/{video_id}")
}

fn issue_access_cookie(signer: &Signer, video_id: &str, secure: bool) -> String {
    let signed = signer.get_url("", &signing_path(video_id), ACCESS_TTL_SECS);
    let query = signed.split_once('?').expect("signer URL has query").1;
    let (exp, sig) = query.split_once('&').expect("signer URL has signature");
    let exp = exp.strip_prefix("exp=").expect("signer URL has expiry");
    let sig = sig.strip_prefix("sig=").expect("signer URL has sig");
    let secure = if secure { "; Secure" } else { "" };
    format!(
        "{ACCESS_COOKIE}={video_id}.{exp}.{sig}; Path=/; Max-Age={ACCESS_TTL_SECS}; HttpOnly; SameSite=Lax{secure}"
    )
}

fn verify_cookie_value(signer: &Signer, video_id: &str, value: &str) -> bool {
    let Some((payload, sig)) = value.rsplit_once('.') else {
        return false;
    };
    let Some((cookie_video_id, exp)) = payload.rsplit_once('.') else {
        return false;
    };
    let Ok(exp) = exp.parse::<i64>() else {
        return false;
    };
    cookie_video_id == video_id && signer.verify("GET", &signing_path(video_id), exp, sig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_round_trip_and_tampering() {
        let signer = Signer::new(b"secret");
        let set_cookie = issue_access_cookie(&signer, "video.1", false);
        let cookie = set_cookie.split_once(';').unwrap().0;
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_str(cookie).unwrap());

        assert!(has_access_cookie(&headers, &signer, "video.1"));
        assert!(!has_access_cookie(&headers, &signer, "video.2"));
        assert!(!has_access_cookie(
            &headers,
            &Signer::new(b"other secret"),
            "video.1"
        ));
    }

    #[test]
    fn policy_requires_matching_unlock_for_password_mode() {
        let signer = Signer::new(b"secret");
        let headers = HeaderMap::new();
        let owner = CurrentUser(auth::User {
            id: "owner".into(),
            name: None,
            email: None,
            username: None,
        });

        assert!(policy_allows(
            "public", "owner", "video", &headers, None, &signer
        ));
        assert!(!policy_allows(
            "private", "owner", "video", &headers, None, &signer
        ));
        assert!(!policy_allows(
            "password", "owner", "video", &headers, None, &signer
        ));
        assert!(policy_allows(
            "private",
            "owner",
            "video",
            &headers,
            Some(&owner),
            &signer
        ));

        let cookie = issue_access_cookie(&signer, "video", false);
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(cookie.split_once(';').unwrap().0).unwrap(),
        );
        assert!(policy_allows(
            "password", "owner", "video", &headers, None, &signer
        ));
    }

    #[test]
    fn secure_cookie_for_https() {
        assert!(issue_access_cookie(&Signer::new(b"secret"), "video", true).ends_with("; Secure"));
    }
}

use crate::error::ApiError;
use crate::state::AppState;
use std::path::{Path, PathBuf};

/// Marker file written/refreshed while a multipart upload is active.
pub const STAGING_ACTIVE_MARKER: &str = ".active";

/// Returns the on-disk path for an object key like `owner/video/result.mp4`.
/// Guards against path traversal.
pub fn resolve(state: &AppState, key: &str) -> Result<PathBuf, ApiError> {
    let parts: Vec<&str> = key.split('/').collect();
    if parts.is_empty()
        || parts.iter().any(|p| {
            p.is_empty()
                || *p == "."
                || *p == ".."
                || p.contains('\\')
                || p.chars().any(char::is_control)
        })
    {
        return Err(ApiError::BadRequest("invalid key".into()));
    }
    let root = Path::new(&state.config.storage_dir);
    let mut p = root.to_path_buf();
    for part in &parts {
        p.push(part);
    }
    Ok(p)
}

pub fn parent_dir(state: &AppState, key: &str) -> Result<PathBuf, ApiError> {
    let path = resolve(state, key)?;
    Ok(path.parent().unwrap_or(&path).to_path_buf())
}

pub async fn ensure_parent(state: &AppState, key: &str) -> Result<(), ApiError> {
    let dir = parent_dir(state, key)?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| ApiError::Internal(format!("mkdir: {e}")))?;
    Ok(())
}

pub async fn remove_dir_all(state: &AppState, key: &str) -> Result<(), ApiError> {
    let path = resolve(state, key)?;
    if let Err(error) = tokio::fs::remove_dir_all(&path).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        return Err(ApiError::Internal(format!("rmdir: {error}")));
    }
    Ok(())
}

pub fn exists(state: &AppState, key: &str) -> bool {
    resolve(state, key).map(|p| p.exists()).unwrap_or(false)
}

fn valid_upload_id(upload_id: &str) -> bool {
    !upload_id.is_empty()
        && !upload_id.contains('/')
        && !upload_id.contains('\\')
        && upload_id != ".."
        && upload_id != "."
}

/// Refresh the per-upload activity heartbeat used by staging cleanup.
pub async fn touch_staging_activity(state: &AppState, upload_id: &str) -> Result<(), ApiError> {
    if !valid_upload_id(upload_id) {
        return Err(ApiError::BadRequest("invalid upload id".into()));
    }
    let dir_key = format!("staging/{upload_id}");
    ensure_parent(state, &format!("{dir_key}/{STAGING_ACTIVE_MARKER}")).await?;
    let marker = resolve(state, &format!("{dir_key}/{STAGING_ACTIVE_MARKER}"))?;
    tokio::fs::write(&marker, b"1")
        .await
        .map_err(|e| ApiError::Internal(format!("touch staging activity: {e}")))?;
    Ok(())
}

/// If `key` is under `staging/{upload_id}/...`, refresh that upload's heartbeat.
pub async fn touch_staging_activity_for_key(state: &AppState, key: &str) {
    let mut parts = key.split('/');
    if parts.next() != Some("staging") {
        return;
    }
    if let Some(upload_id) = parts.next()
        && let Err(error) = touch_staging_activity(state, upload_id).await
    {
        tracing::warn!("failed to refresh staging activity: {error}");
    }
}

/// Remove staging directories (multipart upload parts) whose activity marker
/// (or directory mtime, for legacy dirs) is older than `max_age_secs`.
/// Leftover dirs appear when the client abandons an upload without calling
/// multipart/abort. Never touches user data. Active uploads with a fresh
/// `.active` heartbeat are preserved even if the directory itself is old.
/// Uploads currently marked `finalizing` in the DB are also preserved.
pub async fn cleanup_staging(state: &AppState, max_age_secs: u64) {
    let staging = match resolve(state, "staging") {
        Ok(p) => p,
        Err(error) => {
            tracing::error!("failed to resolve staging directory: {error}");
            return;
        }
    };
    let finalizing: std::collections::HashSet<String> = match sqlx::query_scalar::<_, String>(
        "SELECT upload_id FROM multipart_uploads WHERE status = 'finalizing'",
    )
    .fetch_all(&state.db)
    .await
    {
        Ok(ids) => ids.into_iter().collect(),
        Err(error) => {
            tracing::warn!(
                "failed to load finalizing uploads; continuing filesystem cleanup: {error}"
            );
            std::collections::HashSet::new()
        }
    };
    let now = std::time::SystemTime::now();
    let mut entries = match tokio::fs::read_dir(&staging).await {
        Ok(e) => e,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::error!("failed to read staging directory: {error}");
            return;
        }
    };
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                tracing::error!("failed to read staging entry: {error}");
                break;
            }
        };
        if !entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if finalizing.contains(&name) {
            continue;
        }
        let activity_mtime = activity_mtime(entry.path()).await.unwrap_or(now);
        if now
            .duration_since(activity_mtime)
            .map(|d| d.as_secs() > max_age_secs)
            .unwrap_or(false)
        {
            tracing::info!("cleaning stale staging dir: {name}");
            if let Err(error) = tokio::fs::remove_dir_all(entry.path()).await {
                tracing::error!("failed to clean stale staging directory: {error}");
            }
        }
    }
}

async fn activity_mtime(dir: PathBuf) -> Option<std::time::SystemTime> {
    let marker = dir.join(STAGING_ACTIVE_MARKER);
    if let Ok(meta) = tokio::fs::metadata(&marker).await {
        return meta.modified().ok();
    }
    tokio::fs::metadata(&dir)
        .await
        .ok()
        .and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::state::MuxJobs;
    use std::path::PathBuf;
    use std::time::Duration;

    fn test_state(storage_dir: PathBuf) -> AppState {
        let config = Config {
            database_url: "postgres://unused".into(),
            web_url: "http://localhost".into(),
            cap_signups: true,
            jwt_ttl_secs: 86400,
            storage_dir,
            port: 8080,
            sign_secret: "test-secret-test-secret".into(),
            ffmpeg_path: "ffmpeg".into(),
            plan_upgraded: true,
            db_max_connections: 5,
            storage_backend: crate::config::StorageBackend::Local,
            s3: None,
            cors_origins: Vec::new(),
            video_default_public: true,
        };
        let signer = crate::sign::Signer::new(config.sign_secret.as_bytes());
        AppState {
            db: sqlx::PgPool::connect_lazy("postgres://unused").expect("lazy pool"),
            config,
            signer,
            mux_jobs: MuxJobs::default(),
        }
    }

    fn temp_storage() -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cap-rust-storage-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn resolve_normal_key() {
        let state = test_state(PathBuf::from("/data"));
        let p = resolve(&state, "user/vid/result.mp4").unwrap();
        assert_eq!(p, PathBuf::from("/data/user/vid/result.mp4"));
    }

    #[tokio::test]
    async fn resolve_rejects_dotdot() {
        let state = test_state(PathBuf::from("/data"));
        for key in ["user/../etc/passwd", "../secret", "a/b/../../x"] {
            assert!(
                resolve(&state, key).is_err(),
                "key should be rejected: {key}"
            );
        }
    }

    #[tokio::test]
    async fn resolve_rejects_backslash() {
        let state = test_state(PathBuf::from("/data"));
        assert!(resolve(&state, "user\\..\\etc").is_err());
    }

    #[tokio::test]
    async fn resolve_rejects_empty_parts() {
        let state = test_state(PathBuf::from("/data"));
        assert!(resolve(&state, "a//b").is_err());
        assert!(resolve(&state, "/leading").is_err());
    }

    #[tokio::test]
    async fn resolve_accepts_nested_subpaths() {
        let state = test_state(PathBuf::from("/data"));
        let p = resolve(&state, "u/v/segments/video/init.mp4").unwrap();
        assert_eq!(p, PathBuf::from("/data/u/v/segments/video/init.mp4"));
    }

    #[tokio::test]
    async fn cleanup_preserves_active_upload_past_dir_age() {
        let root = temp_storage();
        let state = test_state(root.clone());
        let upload_id = "active-upload";
        let staging_dir = root.join("staging").join(upload_id);
        std::fs::create_dir_all(&staging_dir).unwrap();

        // Simulate an old staging directory (created well before the threshold).
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Fresh heartbeat means the upload is still active.
        touch_staging_activity(&state, upload_id).await.unwrap();
        cleanup_staging(&state, 1).await;

        assert!(
            staging_dir.exists(),
            "active upload must not be cleaned even when older than max_age"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn cleanup_removes_stale_activity_marker() {
        let root = temp_storage();
        let state = test_state(root.clone());
        let upload_id = "stale-upload";
        touch_staging_activity(&state, upload_id).await.unwrap();
        let staging_dir = root.join("staging").join(upload_id);
        assert!(staging_dir.exists());

        tokio::time::sleep(Duration::from_secs(2)).await;
        cleanup_staging(&state, 1).await;

        assert!(!staging_dir.exists(), "stale staging dir should be removed");
        let _ = std::fs::remove_dir_all(&root);
    }
}

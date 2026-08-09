use crate::error::ApiError;
use crate::state::AppState;
use std::path::{Path, PathBuf};

/// Returns the on-disk path for an object key like `owner/video/result.mp4`.
/// Guards against path traversal.
pub fn resolve(state: &AppState, key: &str) -> Result<PathBuf, ApiError> {
    let parts: Vec<&str> = key.split('/').collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || *p == ".." || p.contains('\\'))
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
    if path.exists() {
        tokio::fs::remove_dir_all(&path)
            .await
            .map_err(|e| ApiError::Internal(format!("rmdir: {e}")))?;
    }
    Ok(())
}

pub fn exists(state: &AppState, key: &str) -> bool {
    resolve(state, key).map(|p| p.exists()).unwrap_or(false)
}

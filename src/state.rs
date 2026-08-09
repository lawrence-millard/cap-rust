use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::sign::Signer;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub config: Config,
    pub signer: Signer,
    pub mux_jobs: MuxJobs,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self, Box<dyn std::error::Error>> {
        let db = PgPoolOptions::new()
            .max_connections(config.db_max_connections.max(1))
            .connect(&config.database_url)
            .await?;

        let signer = Signer::new(config.sign_secret.as_bytes());

        Ok(AppState {
            db,
            config,
            signer,
            mux_jobs: MuxJobs::default(),
        })
    }
}

impl std::convert::From<Arc<AppState>> for AppState {
    fn from(state: Arc<AppState>) -> Self {
        (*state).clone()
    }
}

/// Tracks background mux tasks so graceful shutdown can drain (or fail) them.
#[derive(Clone, Default)]
pub struct MuxJobs {
    inner: Arc<MuxJobsInner>,
}

struct MuxJobsInner {
    shutting_down: AtomicBool,
    active: Mutex<Vec<(String, JoinHandle<()>)>>,
}

impl Default for MuxJobsInner {
    fn default() -> Self {
        Self {
            shutting_down: AtomicBool::new(false),
            active: Mutex::new(Vec::new()),
        }
    }
}

impl MuxJobs {
    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutting_down.load(Ordering::Acquire)
    }

    /// Spawn a mux job unless shutdown has begun. Returns false if rejected.
    pub fn try_spawn<F>(&self, video_id: String, fut: F) -> bool
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let mut active = self.inner.active.lock().expect("mux jobs mutex");
        // Re-check under the lock so `shutdown` cannot drain the list
        // between the check and the push.
        if self.is_shutting_down() {
            return false;
        }
        let handle = tokio::spawn(fut);
        active.retain(|(_, h)| !h.is_finished());
        active.push((video_id, handle));
        true
    }

    /// Stop accepting new jobs, wait for in-flight work up to `timeout`, then
    /// abort leftovers and mark them failed in the database.
    pub async fn shutdown(&self, db: &sqlx::PgPool, timeout: Duration) {
        self.inner.shutting_down.store(true, Ordering::Release);

        let mut jobs = {
            let mut active = self.inner.active.lock().expect("mux jobs mutex");
            std::mem::take(&mut *active)
        };
        if jobs.is_empty() {
            return;
        }

        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            jobs.retain(|(_, h)| !h.is_finished());
            if jobs.is_empty() {
                tracing::info!("all mux jobs finished before shutdown");
                return;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        tracing::warn!(
            count = jobs.len(),
            "mux jobs still running after shutdown timeout; aborting"
        );
        for (video_id, handle) in jobs {
            handle.abort();
            let _ = sqlx::query(
                "UPDATE videos SET mux_status = 'error', mux_error = $1 \
                 WHERE id = $2 AND mux_status = 'processing'",
            )
            .bind("server shutting down")
            .bind(&video_id)
            .execute(db)
            .await;
        }
    }
}

use std::sync::Arc;

use sqlx::postgres::PgPoolOptions;

use crate::config::Config;
use crate::sign::Signer;

#[derive(Clone)]
pub struct AppState {
    pub db: sqlx::PgPool,
    pub config: Config,
    pub signer: Signer,
}

impl AppState {
    pub async fn new(config: Config) -> Result<Self, Box<dyn std::error::Error>> {
        let db = PgPoolOptions::new()
            .max_connections(5)
            .connect(&config.database_url)
            .await?;

        let signer = Signer::new(config.sign_secret.as_bytes());

        Ok(AppState { db, config, signer })
    }
}

impl std::convert::From<Arc<AppState>> for AppState {
    fn from(state: Arc<AppState>) -> Self {
        (*state).clone()
    }
}

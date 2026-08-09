use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub web_url: String,
    pub cap_signups: bool,
    pub jwt_ttl_secs: i64,
    pub storage_dir: PathBuf,
    pub port: u16,
    pub sign_secret: String,
    pub ffmpeg_path: String,
    pub plan_upgraded: bool,
}

impl Config {
    pub fn from_env() -> Self {
        let database_url = env::var("DATABASE_URL").expect("DATABASE_URL is required");
        // strip Neon channel_binding param sqlx doesn't support
        let database_url = strip_db_params(&database_url);

        let web_url = env::var("WEB_URL").unwrap_or_else(|_| "http://localhost:8080".into());
        let port = env::var("PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(8080);
        let storage_dir = env::var("STORAGE_DIR").unwrap_or_else(|_| "./data".into());
        let ffmpeg_path = env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".into());

        let sign_secret = env::var("SIGN_SECRET").expect(
            "SIGN_SECRET is required: set it to a long random string, e.g. `openssl rand -hex 32`",
        );
        assert!(
            sign_secret.len() >= 16,
            "SIGN_SECRET must be at least 16 characters; use `openssl rand -hex 32`"
        );

        let cap_signups = env::var("CAP_SIGNUPS")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);

        let jwt_ttl_secs = env::var("JWT_TTL")
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
            .unwrap_or(30 * 86400);

        let plan_upgraded = env::var("CAP_PLAN_UPGRADED")
            .map(|v| v != "0" && v.to_lowercase() != "false")
            .unwrap_or(true);

        Config {
            database_url,
            web_url,
            cap_signups,
            jwt_ttl_secs,
            storage_dir: PathBuf::from(storage_dir),
            port,
            sign_secret,
            ffmpeg_path,
            plan_upgraded,
        }
    }
}

fn strip_db_params(url: &str) -> String {
    let s = url.to_string();
    if let Some(qpos) = s.find('?') {
        let base = &s[..qpos];
        let params = &s[qpos + 1..];
        let cleaned: Vec<&str> = params
            .split('&')
            .filter(|p| !p.starts_with("channel_binding"))
            .collect();
        if cleaned.is_empty() {
            base.to_string()
        } else {
            format!("{}?{}", base, cleaned.join("&"))
        }
    } else {
        s
    }
}

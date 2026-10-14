use anyhow::{bail, Context, Result};
use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub listen: String,
    pub watch_root: PathBuf,
    pub data_root: PathBuf,
    pub app_user: String,
    pub app_password: String,
    pub poll_interval: u64,
    pub stable_seconds: u64,
    pub max_file_mb: u64,
    pub ocr_max_pages: usize,
    pub require_mount: bool,
}
fn var(name: &str, default: &str) -> String { env::var(name).unwrap_or_else(|_| default.into()) }
fn boolean(name: &str, default: bool) -> bool {
    matches!(var(name, if default {"true"} else {"false"}).to_lowercase().as_str(), "true" | "1" | "yes" | "on")
}
impl Config {
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();
        let password = var("APP_PASSWORD", "");
        if password.len() < 16 || password.starts_with("REPLACE") || password.starts_with("CHANGE") {
            bail!("APP_PASSWORD must be a real secret of at least 16 characters");
        }
        let database_url = var("DATABASE_URL", "");
        if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
            bail!("DATABASE_URL must point to PostgreSQL");
        }
        let data_root = PathBuf::from(var("DATA_ROOT", "./storage"));
        std::fs::create_dir_all(&data_root).with_context(|| format!("DATA_ROOT: {}", data_root.display()))?;
        Ok(Self {
            database_url,
            listen: var("LISTEN", "127.0.0.1:8090"),
            watch_root: PathBuf::from(var("WATCH_ROOT", "/mnt/synology/id")),
            data_root,
            app_user: var("APP_USER", "admin"),
            app_password: password,
            poll_interval: var("POLL_INTERVAL", "45").parse::<u64>()?.max(10),
            stable_seconds: var("STABLE_SECONDS", "15").parse::<u64>()?,
            max_file_mb: var("MAX_FILE_MB", "100").parse::<u64>()?.max(1),
            ocr_max_pages: var("OCR_MAX_PAGES", "20").parse::<usize>()?.max(1),
            require_mount: boolean("REQUIRE_MOUNT", true),
        })
    }
}

pub fn mount_available(cfg: &Config) -> bool {
    if !cfg.watch_root.is_dir() { return false; }
    if !cfg.require_mount { return true; }
    // Checks the actual root mount point, not merely a potentially stale directory.
    std::process::Command::new("mountpoint").arg("-q").arg(&cfg.watch_root)
        .status().map(|status| status.success()).unwrap_or(false)
}

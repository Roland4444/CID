use anyhow::{bail, Context, Result};
use std::{env, path::PathBuf};

#[derive(Clone, Debug)]
pub struct Config {
    pub database_url: String,
    pub listen: String,
    pub synology_url: String,
    pub synology_root: String,
    pub synology_username: String,
    pub synology_password: String,
    pub synology_verify_tls: bool,
    pub synology_ca_cert: Option<PathBuf>,
    pub data_root: PathBuf,
    pub app_user: String,
    pub app_password: String,
    pub poll_interval: u64,
    pub stable_seconds: u64,
    pub max_file_mb: u64,
    pub ocr_max_pages: usize,
}
fn var(name: &str, default: &str) -> String { env::var(name).unwrap_or_else(|_| default.into()) }
fn boolean(name: &str, default: bool) -> bool {
    matches!(var(name, if default {"true"} else {"false"}).to_lowercase().as_str(), "true" | "1" | "yes" | "on")
}
impl Config {
    pub fn load() -> Result<Self> {
        dotenvy::dotenv().ok();
        let password=var("APP_PASSWORD", "");
        if password.len()<16 || password.starts_with("REPLACE") || password.starts_with("CHANGE") {
            bail!("APP_PASSWORD must have at least 16 characters");
        }
        let database_url=var("DATABASE_URL", "");
        if !database_url.starts_with("postgres://") && !database_url.starts_with("postgresql://") {
            bail!("DATABASE_URL must point to PostgreSQL");
        }
        let synology_url=var("SYNOLOGY_URL", "");
        let root=var("SYNOLOGY_ROOT", "");
        if !root.starts_with('/') || root.len()<2 || root.ends_with('/') ||
           root.split('/').any(|c|c=="."||c=="..") || root.contains('\\') ||
           root.chars().any(|c|c.is_control()) {
            bail!("SYNOLOGY_ROOT must be an absolute File Station path, e.g. /resdata/ИД");
        }
        let synology_username=var("SYNOLOGY_USERNAME", "");
        let synology_password=var("SYNOLOGY_PASSWORD", "");
        if synology_username.is_empty() || synology_password.is_empty() || synology_password.starts_with("REPLACE") {
            bail!("SYNOLOGY_USERNAME / SYNOLOGY_PASSWORD are required");
        }
        let data_root=PathBuf::from(var("DATA_ROOT", "./storage"));
        std::fs::create_dir_all(&data_root).with_context(||format!("DATA_ROOT: {}",data_root.display()))?;
        Ok(Self {
            database_url,
            listen:var("LISTEN","127.0.0.1:8090"),
            synology_url,
            synology_root:root,
            synology_username,
            synology_password,
            synology_verify_tls:boolean("SYNOLOGY_VERIFY_TLS",true),
            synology_ca_cert:env::var("SYNOLOGY_CA_CERT").ok().filter(|s|!s.is_empty()).map(PathBuf::from),
            data_root,
            app_user:var("APP_USER","admin"),
            app_password:password,
            poll_interval:var("POLL_INTERVAL","45").parse::<u64>()?.max(10),
            stable_seconds:var("STABLE_SECONDS","15").parse::<u64>()?,
            max_file_mb:var("MAX_FILE_MB","100").parse::<u64>()?.max(1),
            ocr_max_pages:var("OCR_MAX_PAGES","20").parse::<usize>()?.max(1),
        })
    }
}

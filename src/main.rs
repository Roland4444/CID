mod catalog;
mod config;
mod docs;
mod engine;
mod models;
mod norms;
mod repo;
mod reports;
mod scanner;
mod synology;
mod web;

use anyhow::Result;
use rand::RngCore;
use sqlx::postgres::PgPoolOptions;
use std::{sync::{Arc,atomic::{AtomicBool,Ordering}}, time::Duration};
use tokio::{net::TcpListener,signal};
use tracing::{error,info};

#[tokio::main]
async fn main()->Result<()> {
    tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_|"id_control_axum=info,tower_http=info,info".into())
    ).init();
    let cfg=config::Config::load()?;
    // Test DSM connectivity without touching PostgreSQL or scanning/downloading files.
    if std::env::args().any(|arg|arg=="--check-synology") {
        let nas=synology::SynologyClient::new(&cfg)?;
        let session=nas.connect().await?;
        let files=session.list_files().await?;
        session.logout().await;
        println!("Synology HTTPS connection OK. Files in configured folder: {}",files.len());
        return Ok(());
    }
    let catalog=catalog::Catalog::load()?;
    info!("Loaded {} work types and {} common errors",catalog.work_types.len(),catalog.errors.len());
    let db=PgPoolOptions::new().max_connections(8).connect(&cfg.database_url).await?;
    sqlx::raw_sql(include_str!("../migrations/001_init.sql")).execute(&db).await?;
    sqlx::raw_sql(include_str!("../migrations/002_norms.sql")).execute(&db).await?;
    sqlx::raw_sql(include_str!("../migrations/003_norm_patterns.sql")).execute(&db).await?;
    sqlx::raw_sql(include_str!("../migrations/004_synology_index.sql")).execute(&db).await?;
    let mut bytes=[0u8;32];rand::thread_rng().fill_bytes(&mut bytes);
    let csrf=hex::encode(bytes);
    let nas=synology::SynologyClient::new(&cfg)?;
    let st=Arc::new(web::AppState{cfg:cfg.clone(),db,catalog,nas,
        synology_connected:AtomicBool::new(false),scan_lock:tokio::sync::Mutex::new(()),csrf});
    let background=st.clone();
    tokio::spawn(async move{
        let mut interval=tokio::time::interval(Duration::from_secs(background.cfg.poll_interval));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop{
            interval.tick().await;
            let _guard=background.scan_lock.lock().await;
            match scanner::scan_once(&background.db,&background.cfg,&background.catalog,&background.nas).await {
                Ok(_)=>background.synology_connected.store(true,Ordering::Relaxed),
                Err(e)=>{background.synology_connected.store(false,Ordering::Relaxed);error!("Synology scan failed: {e:#}");}
            }
        }
    });
    let addr=cfg.listen.parse::<std::net::SocketAddr>()?;
    let listener=TcpListener::bind(addr).await?;
    info!("Listening at http://{addr}; put HTTPS reverse proxy in front of Basic authentication");
    axum::serve(listener,web::router(st)).with_graceful_shutdown(async{let _=signal::ctrl_c().await;}).await?;
    Ok(())
}

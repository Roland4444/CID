use crate::{catalog::Catalog, config::Config, docs, engine, repo, synology::SynologyClient};
use anyhow::{bail, Context, Result};
use sha2::{Digest,Sha256};
use sqlx::PgPool;
use std::{collections::HashSet,fs::{self,File},io::Read,path::Path,time::{SystemTime,UNIX_EPOCH}};
use tracing::{info,warn};

fn source_details(root:&Path,path:&Path)->Option<(String,String,String,String)>{
    let comps:Vec<String>=path.strip_prefix(root).ok()?.components()
        .map(|c|c.as_os_str().to_string_lossy().into_owned()).collect();
    if comps.len()<4{return None;}
    let kind=match comps[2].to_lowercase().as_str(){
        "id"|"ид"|"исполнительная документация"=>"ID",
        "reference"|"эталон"|"пд-рд"|"пд_рд"|"pd_rd"=>"REFERENCE",
        _=>return None
    };
    if comps.iter().any(|c|repo::validate_segment(c).is_err()) {return None;}
    Some((comps[0].clone(),comps[1].clone(),kind.into(),comps[2..].join("/")))
}
fn sha256(path:&Path)->Result<String>{
    let mut hasher=Sha256::new();let mut file=File::open(path)?;let mut buf=[0;1024*1024];
    loop{let n=file.read(&mut buf)?;if n==0{break;}hasher.update(&buf[..n]);}
    Ok(hex::encode(hasher.finalize()))
}
fn relative_remote_path<'a>(root:&str, full:&'a str)->Option<&'a str>{
    full.strip_prefix(root)?.strip_prefix('/')
}
pub async fn ingest(db:&PgPool,cfg:&Config,catalog:&Catalog,root:&Path,path:&Path,origin:&str)->Result<Option<i64>>{
    let Some((project,work,kind,relative))=source_details(root,path) else {return Ok(None)};
    let pr=repo::get_or_create_project(db,&project).await?;
    let mut pkg=repo::get_or_create_package(db,pr.id,&work).await?;
    if pkg.catalog_name.is_none() {
        if let Some(w)=catalog.closest(&work){
            sqlx::query("UPDATE packages SET catalog_name=$1 WHERE id=$2 AND catalog_name IS NULL")
                .bind(&w.name).bind(pkg.id).execute(db).await?;
            pkg.catalog_name=Some(w.name.clone());
        }
    }
    let src=path.to_owned();
    let digest=tokio::task::spawn_blocking(move||sha256(&src)).await??;
    let latest=repo::latest_document(db,pkg.id,&relative).await?;
    if let Some(ref prev)=latest {
        if prev.sha256==digest {
            if prev.is_present {return Ok(None);}
            sqlx::query("UPDATE documents SET is_present=true WHERE id=$1").bind(prev.id).execute(db).await?;
            return Ok(Some(pkg.id));
        }
    }
    let version=latest.as_ref().map_or(1,|d|d.version+1);
    let suffix=path.extension().and_then(|e|e.to_str()).unwrap_or("bin").to_lowercase();
    let archive=cfg.data_root.join("originals").join(pr.id.to_string()).join(pkg.id.to_string())
        .join(&digest[..2]).join(format!("{digest}.{suffix}"));
    let src=path.to_owned();let dst=archive.clone();let digest_again=digest.clone();
    tokio::task::spawn_blocking(move||->Result<()>{
        fs::create_dir_all(dst.parent().unwrap())?;
        if dst.exists(){
            if sha256(&dst)?!=digest_again{bail!("archive checksum collision/corruption");}
        }else{
            let tmp=tempfile::NamedTempFile::new_in(dst.parent().unwrap())?;
            fs::copy(&src,tmp.path())?;
            if sha256(tmp.path())?!=digest_again {bail!("file changed while reading: {}",src.display());}
            match tmp.persist_noclobber(&dst){Ok(_)=>{},Err(e) if dst.exists()=>{let _=e;},Err(e)=>return Err(e.error.into())}
        }
        Ok(())
    }).await??;
    let ocr=cfg.ocr_max_pages;
    let stored=archive.clone();
    let parsed=tokio::task::spawn_blocking(move||docs::extract(&stored,ocr)).await?;
    let classified=if kind=="ID"{
        catalog.classify(path.file_name().unwrap_or_default().to_str().unwrap_or(""),&parsed.text,
            pkg.catalog_name.as_deref().and_then(|n|catalog.work_type(n)))
    }else{None};
    let (code,dtype)=classified.map(|(a,b)|(Some(a),Some(b)))
        .unwrap_or((None,if kind=="REFERENCE"{Some("Эталон ПД/РД".to_string())}else{None}));
    let mut tx=db.begin().await?;
    if let Some(old)=&latest {
        sqlx::query("UPDATE documents SET is_present=false WHERE id=$1")
            .bind(old.id).execute(&mut *tx).await?;
    }
    let rec:(i64,)=sqlx::query_as("INSERT INTO documents(package_id,relative_path,source_kind,origin,sha256,version,archive_path,document_type,catalog_code,recognized_text,recognition_status,recognition_confidence) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id")
        .bind(pkg.id).bind(&relative).bind(&kind).bind(origin).bind(&digest).bind(version)
        .bind(archive.to_string_lossy().to_string()).bind(dtype).bind(code)
        .bind(parsed.text).bind(parsed.status).bind(parsed.confidence).fetch_one(&mut *tx).await?;
    tx.commit().await?;
    repo::audit(db,origin,"file_ingested",&format!("{} sha256={} version={}",path.display(),digest,version)).await?;
    Ok(Some(pkg.id))
}

#[derive(serde::Serialize)]
pub struct ScanResult {pub synology_connected:bool,pub new_documents:usize,pub affected_packages:usize}

/// A complete successful remote traversal is required before disappearance reconciliation.
/// Errors in listing, paging, login or decoding never mark documents deleted.
pub async fn scan_once(db:&PgPool,cfg:&Config,catalog:&Catalog,client:&SynologyClient)->Result<ScanResult>{
    let session=client.connect().await?;
    let result=scan_session(db,cfg,catalog,&session).await;
    session.logout().await;
    result
}
async fn scan_session(
    db:&PgPool,cfg:&Config,catalog:&Catalog,session:&crate::synology::SynologySession<'_>
)->Result<ScanResult>{
    let entries=session.list_files().await?;
    let allow_removals=!entries.is_empty();
    let mut seen=HashSet::new();let mut affected=HashSet::new();let mut ingested=0;
    for e in entries {
        let Some(rel)=relative_remote_path(&cfg.synology_root,&e.path) else {continue};
        if !crate::docs::supported(Path::new(rel)) ||
           rel.split('/').last().is_some_and(|n|n.starts_with("~$") || n.starts_with('.')) {continue;}
        // A temporary local mirror is used ONLY to reuse the tested classification pipeline.
        // Validate all path segments before creating any directories.
        let parts:Vec<&str>=rel.split('/').collect();
        if parts.len()<4 || parts.iter().any(|p|repo::validate_segment(p).is_err()) {continue;}
        let kind=parts[2].to_lowercase();
        if !["ид","id","исполнительная документация","reference","эталон","пд-рд","пд_рд","pd_rd"].contains(&kind.as_str()) {continue;}
        seen.insert((parts[0].to_string(),parts[1].to_string(),parts[2..].join("/")));
        if e.size>cfg.max_file_mb*1024*1024 {warn!("Ignoring oversized NAS file: {}",e.path);continue;}
        let now=SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        if e.mtime>now || now-e.mtime<cfg.stable_seconds as i64 {continue;}
        // Avoid repeatedly downloading unchanged files. If an archived document is missing
        // from the active set, do not trust an old metadata signature.
        let meta=sqlx::query_as::<_,(i64,i64)>("SELECT size_bytes,modified_epoch FROM synology_file_index WHERE remote_path=$1")
            .bind(&e.path).fetch_optional(db).await?;
        let pr=repo::get_or_create_project(db,parts[0]).await?;
        let pkg=repo::get_or_create_package(db,pr.id,parts[1]).await?;
        let latest=repo::latest_document(db,pkg.id,&parts[2..].join("/")).await?;
        if meta.is_some_and(|(s,m)|s==e.size as i64 && m==e.mtime) &&
           latest.is_some_and(|d|d.is_present && d.origin=="synology") {continue;}
        // Isolated tempdir prevents read-only NAS files from being modified and leaves no
        // half-completed temporary scans after the current operation finishes.
        let tmp=tempfile::tempdir_in(&cfg.data_root)?;
        let dest=tmp.path().join(rel);
        fs::create_dir_all(dest.parent().context("Bad relative path")?)?;
        if let Err(err)=session.download_to(&e.path,&dest,cfg.max_file_mb*1024*1024).await {
            warn!("Cannot download {}: {err:#}",e.path);
            continue;
        }
        let got=fs::metadata(&dest)?.len();
        if got!=e.size {warn!("Size changed during Synology download: {} expected={} actual={}",e.path,e.size,got);continue;}
        match ingest(db,cfg,catalog,tmp.path(),&dest,"synology").await {
            Ok(updated)=>{
                if let Some(id)=updated {affected.insert(id);ingested+=1;}
                sqlx::query("INSERT INTO synology_file_index(remote_path,size_bytes,modified_epoch) VALUES ($1,$2,$3) ON CONFLICT(remote_path) DO UPDATE SET size_bytes=EXCLUDED.size_bytes,modified_epoch=EXCLUDED.modified_epoch,scanned_at=now()")
                    .bind(&e.path).bind(e.size as i64).bind(e.mtime).execute(db).await?;
            },
            Err(err)=>warn!("Failed to analyze Synology file {}: {err:#}",e.path)
        }
    }
    // Never interpret an empty NAS folder as deletion of every active document.
    if !allow_removals {warn!("No files reported by DSM: bulk disappearance reconciliation skipped");}
    if allow_removals {
        let all=sqlx::query_as::<_,(i64,String,String,String,bool)>("SELECT d.id,p.name,k.name,d.relative_path,d.is_present FROM documents d JOIN packages k ON k.id=d.package_id JOIN projects p ON p.id=k.project_id WHERE d.origin='synology' AND d.id=(SELECT max(d2.id) FROM documents d2 WHERE d2.package_id=d.package_id AND d2.relative_path=d.relative_path) ORDER BY d.id DESC")
            .fetch_all(db).await?;
        for (id,pr,work,rel,present) in all {
            if present && !seen.contains(&(pr.clone(),work.clone(),rel.clone())) {
                let pkgid:(i64,)=sqlx::query_as("UPDATE documents SET is_present=false WHERE id=$1 RETURNING package_id")
                    .bind(id).fetch_one(db).await?;
                affected.insert(pkgid.0);
                repo::audit(db,"scanner","nas_file_missing",&format!("{pr}/{work}/{rel}")).await?;
            }
        }
    }
    for id in &affected {engine::analyze(db,catalog,*id,"synology").await?;}
    info!("Synology scan: {ingested} new/updated docs, {} affected packages",affected.len());
    Ok(ScanResult{synology_connected:true,new_documents:ingested,affected_packages:affected.len()})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn paths() {
        let root=Path::new("/nas");
        let d=source_details(root,Path::new("/nas/ЖК/Земляные работы/ИД/акт.docx")).unwrap();
        assert_eq!(d.2,"ID"); assert_eq!(d.3,"ИД/акт.docx");
        assert!(source_details(root,Path::new("/nas/ЖК/непонятно/акт.docx")).is_none());
        assert!(source_details(root,Path::new("/nas/ЖК/Вид/ИД/../escape.pdf")).is_none());
    }
    #[test] fn remote_prefix(){
        assert_eq!(relative_remote_path("/resdata/ИД","/resdata/ИД/ЖК/Работы/ИД/а.pdf"),Some("ЖК/Работы/ИД/а.pdf"));
        assert_eq!(relative_remote_path("/resdata/ИД","/resdata/ИД2/x"),None);
    }
}

use crate::{catalog::Catalog, config::{Config,mount_available}, docs, engine, repo};
use anyhow::{bail, Context, Result};
use sha2::{Digest,Sha256};
use sqlx::PgPool;
use std::{collections::HashSet,fs::{self,File},io::Read,path::{Path,PathBuf},time::Duration};
use tracing::{info,warn};

#[derive(Clone,Debug)]
struct Entry {path:PathBuf,project:String,work:String,kind:String,relative:String,ready:bool}

fn source_details(root:&Path,path:&Path)->Option<(String,String,String,String)>{
    let comps:Vec<String>=path.strip_prefix(root).ok()?.components().map(|c|c.as_os_str().to_string_lossy().into_owned()).collect();
    if comps.len()<4{return None;}
    let kind=match comps[2].to_lowercase().as_str(){
        "id"|"ид"|"исполнительная документация"=>"ID",
        "reference"|"эталон"|"пд-рд"|"пд_рд"|"pd_rd"=>"REFERENCE",
        _=>return None
    };
    if repo::validate_segment(&comps[0]).is_err()||repo::validate_segment(&comps[1]).is_err(){return None;}
    Some((comps[0].clone(),comps[1].clone(),kind.into(),comps[2..].join("/")))
}
fn sha256(path:&Path)->Result<String>{
    let mut hasher=Sha256::new();let mut file=File::open(path)?;let mut buf=[0;1024*1024];
    loop{let n=file.read(&mut buf)?;if n==0{break;}hasher.update(&buf[..n]);}
    Ok(hex::encode(hasher.finalize()))
}
fn collect(cfg:&Config)->Result<Vec<Entry>>{
    let mut out=Vec::new();
    for e in walkdir::WalkDir::new(&cfg.watch_root).follow_links(false) {
        let entry=match e {Ok(e)=>e,Err(e)=>{return Err(anyhow::anyhow!("NAS directory scan was incomplete; skipping deletion detection: {e}"));}};
        if !entry.file_type().is_file() || entry.file_type().is_symlink(){continue;}
        let path=entry.path();
        if !docs::supported(path) || path.file_name().is_some_and(|n|n.to_string_lossy().starts_with('.')){continue;}
        let name=path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with("~$"){continue;}
        let Some((project,work,kind,relative))=source_details(&cfg.watch_root,path) else{continue;};
        let (size,ready)=match entry.metadata(){
            Ok(m)=>{
                let elapsed=m.modified().ok().and_then(|t|t.elapsed().ok()).unwrap_or(Duration::ZERO);
                (m.len(),m.len()<=cfg.max_file_mb*1024*1024 && elapsed>=Duration::from_secs(cfg.stable_seconds))
            },Err(_)=>(0,false)
        };
        let _=size;
        out.push(Entry{path:path.to_path_buf(),project,work,kind,relative,ready});
    }
    Ok(out)
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
pub struct ScanResult {pub mount_available:bool,pub new_documents:usize,pub affected_packages:usize}
pub async fn scan_once(db:&PgPool,cfg:&Config,catalog:&Catalog)->Result<ScanResult>{
    if !mount_available(cfg){
        warn!("Synology mount missing: {}",cfg.watch_root.display());
        return Ok(ScanResult{mount_available:false,new_documents:0,affected_packages:0});
    }
    let c=cfg.clone();
    let entries=tokio::task::spawn_blocking(move||collect(&c)).await??;
    // Never interpret an unexpectedly empty share as "all originals deleted".
    // Real complete removal must be confirmed manually before reconciling the active set.
    let allow_removals=!entries.is_empty();
    let mut seen=HashSet::new();let mut affected=HashSet::new();let mut ingested=0;
    for e in entries{
        seen.insert((e.project.clone(),e.work.clone(),e.relative.clone()));
        if !e.ready{continue;}
        match ingest(db,cfg,catalog,&cfg.watch_root,&e.path,"synology").await{
            Ok(Some(id))=>{affected.insert(id);ingested+=1;},
            Ok(None)=>{},
            Err(err)=>warn!("ingest failed {}: {err:#}",e.path.display()),
        }
    }
    // Only mark deletions when the share itself is confirmed mounted.
    if !allow_removals {
        warn!("Share produced no supported files; avoiding bulk disappearance updates");
    }
    let all=if allow_removals {sqlx::query_as::<_,(i64,String,String,String,bool)>("SELECT d.id,p.name,k.name,d.relative_path,d.is_present FROM documents d JOIN packages k ON k.id=d.package_id JOIN projects p ON p.id=k.project_id WHERE d.origin='synology' AND d.id=(SELECT max(d2.id) FROM documents d2 WHERE d2.package_id=d.package_id AND d2.relative_path=d.relative_path) ORDER BY d.id DESC")
        .fetch_all(db).await?}else{Vec::new()};
    for (id,pr,work,rel,present) in all{
        if present && !seen.contains(&(pr.clone(),work.clone(),rel.clone())){
            let pkgid:(i64,)=sqlx::query_as("UPDATE documents SET is_present=false WHERE id=$1 RETURNING package_id")
                .bind(id).fetch_one(db).await?;
            affected.insert(pkgid.0);
            repo::audit(db,"scanner","file_removed",&format!("{pr}/{work}/{rel}")).await?;
        }
    }
    for id in &affected {engine::analyze(db,catalog,*id,"synology").await?;}
    info!("scan complete: {ingested} new, {} packages",affected.len());
    Ok(ScanResult{mount_available:true,new_documents:ingested,affected_packages:affected.len()})
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn paths(){
        let root=Path::new("/nas");
        let d=source_details(root,Path::new("/nas/ЖК/Земляные работы/ИД/акт.docx")).unwrap();
        assert_eq!(d.2,"ID"); assert_eq!(d.3,"ИД/акт.docx");
        assert!(source_details(root,Path::new("/nas/ЖК/непонятно/акт.docx")).is_none());
    }
}

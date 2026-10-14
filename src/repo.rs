use crate::models::*;
use anyhow::{bail, Result};
use sqlx::PgPool;

pub async fn get_or_create_project(db:&PgPool, name:&str) -> Result<Project> {
    Ok(sqlx::query_as::<_,Project>("INSERT INTO projects(name) VALUES ($1) ON CONFLICT(name) DO UPDATE SET name=EXCLUDED.name RETURNING id,name")
        .bind(name).fetch_one(db).await?)
}
pub async fn get_or_create_package(db:&PgPool, project_id:i64,name:&str) -> Result<Package> {
    Ok(sqlx::query_as::<_,Package>("INSERT INTO packages(project_id,name) VALUES ($1,$2) ON CONFLICT(project_id,name) DO UPDATE SET name=EXCLUDED.name RETURNING id,project_id,name,catalog_name,required_codes_json")
        .bind(project_id).bind(name).fetch_one(db).await?)
}
pub async fn package(db:&PgPool, id:i64)->Result<Package>{
    let p=sqlx::query_as::<_,Package>("SELECT id,project_id,name,catalog_name,required_codes_json FROM packages WHERE id=$1")
        .bind(id).fetch_optional(db).await?;
    p.ok_or_else(||anyhow::anyhow!("package not found"))
}
pub async fn documents(db:&PgPool,package_id:i64)->Result<Vec<Document>>{
    Ok(sqlx::query_as::<_,Document>("SELECT id,package_id,relative_path,source_kind,origin,is_present,sha256,version,archive_path,draft_path,document_type,catalog_code,recognized_text,recognition_status,recognition_confidence,spelling_notes_json FROM documents WHERE package_id=$1 ORDER BY id DESC")
        .bind(package_id).fetch_all(db).await?)
}
pub async fn document(db:&PgPool, id:i64)->Result<Document>{
    Ok(sqlx::query_as::<_,Document>("SELECT id,package_id,relative_path,source_kind,origin,is_present,sha256,version,archive_path,draft_path,document_type,catalog_code,recognized_text,recognition_status,recognition_confidence,spelling_notes_json FROM documents WHERE id=$1")
        .bind(id).fetch_optional(db).await?.ok_or_else(||anyhow::anyhow!("document not found"))?)
}
pub async fn latest_document(db:&PgPool,package_id:i64,rel:&str)->Result<Option<Document>> {
    Ok(sqlx::query_as::<_,Document>("SELECT id,package_id,relative_path,source_kind,origin,is_present,sha256,version,archive_path,draft_path,document_type,catalog_code,recognized_text,recognition_status,recognition_confidence,spelling_notes_json FROM documents WHERE package_id=$1 AND relative_path=$2 ORDER BY version DESC LIMIT 1")
        .bind(package_id).bind(rel).fetch_optional(db).await?)
}
pub async fn runs(db:&PgPool,package_id:i64)->Result<Vec<CheckRun>>{
    Ok(sqlx::query_as::<_,CheckRun>("SELECT id,package_id,started_at,source FROM check_runs WHERE package_id=$1 ORDER BY id DESC")
        .bind(package_id).fetch_all(db).await?)
}
pub async fn findings(db:&PgPool,run_id:i64)->Result<Vec<Finding>>{
    Ok(sqlx::query_as::<_,Finding>("SELECT id,run_id,package_id,document_id,block,rule_code,description,source_location,reference,ntd,severity,result,review_status,reviewed_by,reviewed_at,comment FROM findings WHERE run_id=$1 ORDER BY id")
        .bind(run_id).fetch_all(db).await?)
}
pub async fn audit(db:&PgPool,actor:&str,action:&str,detail:&str)->Result<()> {
    sqlx::query("INSERT INTO audit(actor,action,details) VALUES ($1,$2,$3)")
        .bind(actor).bind(action).bind(detail).execute(db).await?;
    Ok(())
}
pub fn validate_segment(input:&str)->Result<&str> {
    let name=input.trim();
    if name.is_empty() || name.len()>300 || name=="." || name==".." || name.starts_with('.') ||
       (name.contains('/') || name.contains('\\')) || name.chars().any(|c|c.is_control()) {
        bail!("invalid name");
    }
    Ok(name)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn prevents_path_traversal(){ assert!(validate_segment("../test").is_err()); assert!(validate_segment("Test").is_ok()); }
}

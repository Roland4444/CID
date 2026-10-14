use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::FromRow;

#[derive(Debug, Clone, FromRow)]
pub struct Project { pub id: i64, pub name: String }
#[derive(Debug, Clone, FromRow)]
pub struct Package {
    pub id: i64, pub project_id: i64, pub name: String,
    pub catalog_name: Option<String>, pub required_codes_json: String,
}
#[derive(Debug, Clone, FromRow)]
pub struct Document {
    pub id: i64, pub package_id: i64, pub relative_path: String,
    pub source_kind: String, pub origin: String, pub is_present: bool,
    pub sha256: String, pub version: i32, pub archive_path: String,
    pub draft_path: Option<String>, pub document_type: Option<String>,
    pub catalog_code: Option<String>, pub recognized_text: String,
    pub recognition_status: String, pub recognition_confidence: Option<f32>,
    pub spelling_notes_json: String,
}
#[derive(Debug, Clone, FromRow)]
pub struct CheckRun {
    pub id: i64, pub package_id: i64, pub started_at: DateTime<Utc>,
    pub source: String,
}
#[derive(Debug, Clone, FromRow, Serialize)]
pub struct Finding {
    pub id: i64, pub run_id: i64, pub package_id: i64,
    pub document_id: Option<i64>, pub block: i32, pub rule_code: String,
    pub description: String, pub source_location: String,
    pub reference: String, pub ntd: String, pub severity: String,
    pub result: String, pub review_status: String,
    pub reviewed_by: Option<String>, pub reviewed_at: Option<DateTime<Utc>>,
    pub comment: String,
}

#[derive(Clone)]
pub struct FindingInput {
    pub block: i32, pub rule_code: String, pub description: String,
    pub document_id: Option<i64>, pub source_location: String,
    pub reference: String, pub ntd: String, pub severity: String,
    pub result: String,
}
impl FindingInput {
    pub fn new(block: i32, code: impl Into<String>, desc: impl Into<String>) -> Self {
        Self { block, rule_code:code.into(), description:desc.into(), document_id:None,
            source_location:String::new(), reference:String::new(), ntd:String::new(),
            severity:"review".into(), result:"manual".into() }
    }
    pub fn doc(mut self, d: &Document) -> Self {
        self.document_id=Some(d.id); self.source_location=d.relative_path.clone(); self
    }
    pub fn mismatch(mut self) -> Self { self.result="mismatch".into();self.severity="high".into();self }
}

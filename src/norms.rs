//! Narrow, auditable compliance checks. Normative rules require human approval.
//! Evidence in a document is not a general declaration of GOST/SP compliance.
use anyhow::Result;
use serde::Serialize;
use regex::Regex;
use sqlx::{FromRow, PgPool};

#[derive(Clone, Debug, FromRow)]
pub struct NormRule {
    pub id: i64,
    pub work_type: String,
    pub document_code: String,
    pub norm_name: String,
    pub norm_edition: String,
    pub norm_clause: String,
    pub source_reference: String,
    pub description: String,
    pub check_kind: String,
    pub expected_text: String,
    pub approval_status: String,
    pub version: i32,
    pub approved_by: Option<String>,
}
#[derive(Debug,Clone,FromRow,Serialize)]
pub struct NormCheck {
    pub document_id:i64,
    pub rule_id:i64,
    pub result:String,
    pub details:String,
    pub normative_basis:String,
    pub source_reference:String,
}

pub async fn list_rules(db:&PgPool)->Result<Vec<NormRule>>{
    Ok(sqlx::query_as::<_, NormRule>("SELECT id,work_type,document_code,norm_name,norm_edition,norm_clause,source_reference,description,check_kind,expected_text,approval_status,version,approved_by FROM normative_rules ORDER BY id DESC")
       .fetch_all(db).await?)
}
pub async fn approved_rules(db:&PgPool,work_type:&str)->Result<Vec<NormRule>>{
    Ok(sqlx::query_as::<_, NormRule>("SELECT id,work_type,document_code,norm_name,norm_edition,norm_clause,source_reference,description,check_kind,expected_text,approval_status,version,approved_by FROM normative_rules WHERE approval_status='approved' AND (work_type='' OR work_type=$1) ORDER BY id")
       .bind(work_type).fetch_all(db).await?)
}
pub async fn checks_for_run(db:&PgPool,run_id:i64)->Result<Vec<NormCheck>>{
    Ok(sqlx::query_as::<_, NormCheck>("SELECT document_id,rule_id,result,details,normative_basis,source_reference FROM normative_check_results WHERE run_id=$1 ORDER BY id")
        .bind(run_id).fetch_all(db).await?)
}

pub fn basis(rule:&NormRule)->String{
    format!("{} (редакция {}), п. {}",rule.norm_name,rule.norm_edition,rule.norm_clause)
}
fn clean(s:&str)->String{
    s.to_lowercase().replace('ё',"е").split_whitespace().collect::<Vec<&str>>().join(" ")
}
/// ('evidence_found' is only a structural/textual observation, not proof of full compliance).
/// OCR and partial extraction cannot support a definitive negative conclusion.
pub fn evaluate(rule:&NormRule,text:&str,extraction:&str,confidence:Option<f32>)->(String,String){
    if rule.approval_status!="approved"{
        return ("manual".into(),"Правило не утверждено ответственным специалистом".into());
    }
    if rule.check_kind=="manual"{
        return ("manual".into(),"Инженер должен сверить документ с утверждённым нормативным источником".into());
    }
    if extraction!="extracted" || text.trim().is_empty() || confidence.is_some_and(|c| c < 0.9){
        return ("manual".into(),"Нет полного надёжного текстового слоя: требуется ручная проверка PDF/скана/таблиц".into());
    }
    let expected=clean(&rule.expected_text);
    if expected.is_empty(){
        return ("manual".into(),"Правило не содержит поискового условия".into());
    }
    let present=if rule.check_kind=="required_pattern" {
        match Regex::new(&rule.expected_text){
            Ok(pattern)=>pattern.is_match(text),
            Err(_)=>return ("manual".into(),"Шаблон регулярного выражения недействителен; проверьте правило".into()),
        }
    } else {clean(text).contains(&expected)};
    match (rule.check_kind.as_str(),present){
        ("required_phrase",true)=>("evidence_found".into(),"Обязательный текст обнаружен; это не подтверждает полного соответствия НТД".into()),
        ("required_phrase",false)=>("mismatch".into(),format!("Не найдены обязательные реквизиты/текст по утверждённому правилу: «{}». Проверьте структуру и источник.",rule.expected_text)),
        ("required_pattern",true)=>("evidence_found".into(),"Формальное выражение найдено; это не подтверждает соответствие всему нормативу".into()),
        ("required_pattern",false)=>("mismatch".into(),format!("Не найден реквизит по утверждённому шаблону: «{}». Требуется подтверждение инженера.",rule.expected_text)),
        ("forbidden_phrase",true)=>("mismatch".into(),format!("Обнаружен текст, запрещённый утверждённым правилом: «{}». Проверьте применимость требования.",rule.expected_text)),
        ("forbidden_phrase",false)=>("evidence_found".into(),"Запрещённый текст не найден в извлечённом слое; это не подтверждает полного соответствия НТД".into()),
        _=>("manual".into(),"Неизвестный тип правила".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn rule(kind:&str)->NormRule{
        NormRule{id:1,work_type:"".into(),document_code:"".into(),norm_name:"Нормативный документ".into(),norm_edition:"Тест".into(),norm_clause:"X".into(),source_reference:"Тестовый файл".into(),description:"test".into(),check_kind:kind.into(),expected_text:"Акт освидетельствования".into(),approval_status:"approved".into(),version:1,approved_by:Some("tester".into())}
    }
    #[test]fn normalized_yo_and_spaces(){assert_eq!(evaluate(&rule("required_phrase"),"акт  освидетельствования", "extracted",None).0,"evidence_found");}
    #[test]fn missing_required_text_flags_review(){assert_eq!(evaluate(&rule("required_phrase"),"другая строка", "extracted",None).0,"mismatch");}
    #[test]fn scan_never_claims_absence(){assert_eq!(evaluate(&rule("required_phrase"),"другая строка", "ocr",Some(0.99)).0,"manual");}
    #[test]fn draft_never_applied(){let mut r=rule("forbidden_phrase");r.approval_status="draft".into();assert_eq!(evaluate(&r,"Акт освидетельствования", "extracted",None).0,"manual");}
    #[test]fn manual_checks_require_engineer(){assert_eq!(evaluate(&rule("manual"),"Акт освидетельствования", "extracted",None).0,"manual");}
    #[test]fn required_pattern_matches_number(){let mut r=rule("required_pattern");r.expected_text=String::from(r"№\s*\d+");assert_eq!(evaluate(&r,"Акт № 15", "extracted",None).0,"evidence_found");}
    #[test]fn bad_pattern_never_gives_false_finding(){let mut r=rule("required_pattern");r.expected_text="[".into();assert_eq!(evaluate(&r,"Акт №15", "extracted",None).0,"manual");}
}

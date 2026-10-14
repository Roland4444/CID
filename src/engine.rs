//! MVP rules: absence of evidence NEVER implies compliance.
use crate::{catalog::Catalog, models::{Document, FindingInput}, norms, repo};
use anyhow::Result;
use chrono::NaiveDate;
use once_cell::sync::Lazy;
use regex::Regex;
use sqlx::PgPool;
use std::collections::{HashMap,HashSet};

static ANY_DATE:Lazy<Regex>=Lazy::new(||Regex::new(r"\b([0-3]?\d)[.\-/]([01]?\d)[.\-/](20\d{2})\b").unwrap());
static ACT:Lazy<Regex>=Lazy::new(||Regex::new(r"(?i)(?:дата\s+(?:составления\s+)?акта|акт\s*№.{0,35}?от)\s*[:\-]?\s*([0-3]?\d[.]\d{1,2}[.]20\d{2})").unwrap());
static PROTOCOL:Lazy<Regex>=Lazy::new(||Regex::new(r"(?i)протокол[^\n]{0,110}?(?:от|дата)\s*[:\-]?\s*([0-3]?\d[.]\d{1,2}[.]20\d{2})").unwrap());

pub fn timeline_discrepancies(text:&str)->Vec<String>{
    let mut result=HashSet::new();
    let acts:Vec<_>=ACT.captures_iter(text).filter_map(|cap|NaiveDate::parse_from_str(&cap[1],"%d.%m.%Y").ok().map(|d|(cap[1].to_string(),d))).collect();
    let prot:Vec<_>=PROTOCOL.captures_iter(text).filter_map(|cap|NaiveDate::parse_from_str(&cap[1],"%d.%m.%Y").ok().map(|d|(cap[1].to_string(),d))).collect();
    for (at,a) in &acts{for (pt,p) in &prot{if p>a {
        result.insert(format!("Дата протокола {pt} позже даты акта {at}. Проверить ссылку, применимость и исключения."));
    }}}
    let mut all:Vec<_>=result.into_iter().collect(); all.sort();all
}
pub async fn analyze(db:&PgPool, catalog:&Catalog,package_id:i64,source:&str)->Result<i64>{
    let package=repo::package(db,package_id).await?;
    let docs=repo::documents(db,package_id).await?;
    let mut latest:HashMap<String,Document>=HashMap::new();
    for doc in docs{latest.entry(doc.relative_path.clone()).or_insert(doc);}
    let inputs:Vec<_>=latest.values().filter(|d|d.is_present && d.source_kind=="ID").collect();
    let reference_exists=latest.values().any(|d|d.is_present && d.source_kind=="REFERENCE");
    let mut findings=Vec::new();
    let norm_rules=norms::approved_rules(db,package.catalog_name.as_deref().unwrap_or("")).await?;
    // Results are snapshotted at run time, so revoking a rule does not erase history.
    let mut norm_results:Vec<(i64,i64,i32,String,String,String,String)>=Vec::new();
    let mut matched_rules=0usize;
    if inputs.is_empty(){findings.push(FindingInput::new(1,"E01","В комплекте отсутствуют документы ИД.").mismatch());}
    let required:Vec<String>=serde_json::from_str(&package.required_codes_json).unwrap_or_default();
    let classified:HashSet<_>=inputs.iter().filter_map(|d|d.catalog_code.as_ref()).collect();
    for code in &required {
        match catalog.item(package.catalog_name.as_deref().unwrap_or(""),code){
            None=>findings.push(FindingInput::new(1,"C-GONE",format!("Пункт {code} отсутствует в каталоге. Проверьте настройки."))),
            Some(item) if !classified.contains(&item.code)=>{
                let mut f=FindingInput::new(1,format!("C-{}",code),format!("Не найден обязательный документ: {} (пункт {}).",item.title,code)).mismatch();
                f.reference=format!("Исходный справочник: {}",item.norm_reference);
                findings.push(f);
            },
            _=>{}
        }
    }
    if package.catalog_name.is_none(){findings.push(FindingInput::new(1,"C-UNKNOWN","Вид работ не сопоставлен с каталогом. Выберите применимый перечень."));}
    else if required.is_empty(){findings.push(FindingInput::new(1,"C-UNAPPROVED","Инженер ещё не утвердил обязательные пункты для комплекта."));}
    for doc in &inputs {
        if doc.catalog_code.is_none(){findings.push(FindingInput::new(1,"CLASSIFY","Тип документа не определён достоверно. Требуется ручная классификация.").doc(doc));}
        if doc.recognition_status!="extracted" && doc.recognition_status!="ocr" {
            findings.push(FindingInput::new(3,"OCR",format!("Текст распознан не полностью: {}. Соответствие не подтверждено.",doc.recognition_status)).doc(doc));
        }
        if doc.recognition_confidence.is_some_and(|c|c<0.7){
            findings.push(FindingInput::new(3,"OCR-CONF","Низкая уверенность распознавания (<70%). Нужна ручная проверка.").doc(doc));
        }
        if doc.catalog_code.is_some() && doc.document_type.as_ref().is_some_and(|s|s.to_lowercase().contains("акт")) && !ANY_DATE.is_match(&doc.recognized_text){
            findings.push(FindingInput::new(3,"E08","Не удалось определить дату акта; проверьте обязательные поля.").doc(doc));
        }
        for message in timeline_discrepancies(&doc.recognized_text){
            let mut f=FindingInput::new(3,"E12",message).doc(doc);f.severity="high".into();findings.push(f);
        }
        for rule in &norm_rules {
            if !rule.document_code.is_empty() && doc.catalog_code.as_deref()!=Some(rule.document_code.as_str()) {continue;}
            matched_rules+=1;
            let (mut status,mut details)=norms::evaluate(rule,&doc.recognized_text,&doc.recognition_status,doc.recognition_confidence);
            // A text-layer PDF can hide image-only pages: do not conclude absence from it.
            let ext=doc.relative_path.rsplit('.').next().unwrap_or("").to_lowercase();
            if status=="mismatch" && ext!="docx" {
                status="manual".into();
                details=format!("Предварительная проверка (неполный контроль структуры {ext}): {details}");
            }
            let basis=norms::basis(rule);
            norm_results.push((doc.id,rule.id,rule.version,status.clone(),details.clone(),basis.clone(),rule.source_reference.clone()));
            if status!="evidence_found" {
                let mut f=FindingInput::new(if rule.check_kind=="manual" {2}else{4},format!("N-{}-v{}",rule.id,rule.version),format!("{}: {}",rule.description,details)).doc(doc);
                f.result=if status=="mismatch" {"mismatch"}else{"manual"}.into();
                f.severity=if status=="mismatch" {"high"}else{"review"}.into();
                f.ntd=basis;
                f.reference=rule.source_reference.clone();
                findings.push(f);
            }
        }

    }
    if !inputs.is_empty(){
        if norm_rules.is_empty(){
            findings.push(FindingInput::new(4,"N-RULES-NONE","Нет утверждённых нормативных правил для вида работ. Комплект нельзя считать проверенным по ГОСТ/СП/НТД."));
        }else if matched_rules==0 {
            findings.push(FindingInput::new(4,"N-NOT-APPLICABLE","Утверждённые нормативные правила не сопоставились с типами загруженных документов; требуется настройка и классификация."));
        }
        if !reference_exists {
            findings.push(FindingInput::new(2,"NO-REFERENCE","Нет эталонных ПД/РД. Сопоставление размеров, отметок, материалов и шифров невозможно."));
        }else{
            findings.push(FindingInput::new(2,"DRAWING-REVIEW","Сопоставление чертежей, допусков и материалов требует проверки инженера (этап 3 ТЗ)."));
        }
        findings.push(FindingInput::new(4,"FORMS-REVIEW","Полное соответствие форм ГОСТ/СП/приказам нельзя подтвердить поиском реквизитов: инженер должен сверить применимый утверждённый шаблон и визуальное оформление."));
    }
    let mut tx=db.begin().await?;
    let run_id:(i64,)=sqlx::query_as("INSERT INTO check_runs(package_id,source,rule_version) VALUES($1,$2,2) RETURNING id")
        .bind(package_id).bind(source).fetch_one(&mut *tx).await?;
    for (document_id,rule_id,rule_version,result,details,normative_basis,source_reference) in norm_results {
        sqlx::query("INSERT INTO normative_check_results(run_id,package_id,document_id,rule_id,rule_version,result,details,normative_basis,source_reference) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)")
            .bind(run_id.0).bind(package_id).bind(document_id).bind(rule_id).bind(rule_version)
            .bind(result).bind(details).bind(normative_basis).bind(source_reference)
            .execute(&mut *tx).await?;
    }
    for f in findings{
        sqlx::query("INSERT INTO findings(run_id,package_id,document_id,block,rule_code,description,source_location,reference,ntd,severity,result) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(run_id.0).bind(package_id).bind(f.document_id).bind(f.block).bind(f.rule_code)
            .bind(f.description).bind(f.source_location).bind(f.reference).bind(f.ntd)
            .bind(f.severity).bind(f.result).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(run_id.0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]fn detects_later_protocol(){
        let t="Акт №15 от 06.09.2026\nПротокол №32 от 09.09.2026";
        assert_eq!(timeline_discrepancies(t).len(),1);
    }
    #[test]fn does_not_invent_discrepancy(){assert!(timeline_discrepancies("Акт №15 от 09.09.2026").is_empty());}
}

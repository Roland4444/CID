//! Document extraction and local OCR. Originals are immutable.
use anyhow::{Context, Result};
use calamine::{open_workbook_auto, Reader};
use regex::Regex;
use std::{fs::File, io::Read, path::{Path, PathBuf}, process::Command};
use zip::ZipArchive;

pub const SUPPORTED: &[&str] = &["docx","xlsx","pdf","png","jpg","jpeg"];
pub fn supported(p:&Path) -> bool { p.extension().and_then(|x|x.to_str()).map(|s|SUPPORTED.contains(&s.to_lowercase().as_str())).unwrap_or(false) }

#[derive(Clone, Default)]
pub struct Extracted { pub text: String, pub status:String, pub confidence:Option<f32> }

fn unzip_member(path:&Path,member:&str)->Result<String>{
    let mut archive=ZipArchive::new(File::open(path)?)?;
    let mut body=String::new();
    archive.by_name(member)?.read_to_string(&mut body)?;
    Ok(body)
}
/// Word splits words across multiple w:r/w:t runs (e.g. bold text).
/// Do not inject spaces between adjacent runs: otherwise an actual requisite
/// can be incorrectly reported as missing.
fn docx_xml_to_text(xml:&str)->Result<String>{
    let tokens=Regex::new(r"(?s)<w:t(?:\s[^>]*)?>(.*?)</w:t>|</w:p\s*>|<w:tab(?:\s[^>]*)?\s*/>|<w:br(?:\s[^>]*)?\s*/>")?;
    let mut text=String::new();
    for capture in tokens.captures_iter(xml) {
        if let Some(fragment)=capture.get(1) {
            text.push_str(&html_escape::decode_html_entities(fragment.as_str()));
        } else if let Some(tag)=capture.get(0){
            if tag.as_str().starts_with("<w:tab") { text.push('\t'); }
            else {text.push('\n');}
        }
    }
    Ok(text)
}
fn docx_text(path:&Path)->Result<String>{
    let xml=unzip_member(path,"word/document.xml")?;
    docx_xml_to_text(&xml)
}
fn xlsx_text(path:&Path)->Result<String>{
    let mut wb=open_workbook_auto(path)?;
    let mut out=String::new();
    for name in wb.sheet_names().to_vec(){
        out.push_str(&format!("Лист: {name}\n"));
        let range=wb.worksheet_range(&name)?;
        for (n,row) in range.rows().enumerate(){
            if n>=10000 {break;}
            let vals:Vec<String>=row.iter().map(ToString::to_string).filter(|s| !s.trim().is_empty()).collect();
            if !vals.is_empty(){out.push_str(&vals.join(" | "));out.push('\n');}
        }
    }
    Ok(out)
}
fn tesseract(path:&Path)->Result<Extracted>{
    let output=Command::new("tesseract").arg(path).args(["stdout","-l","rus+eng","tsv"]).output()
        .context("tesseract unavailable")?;
    if !output.status.success() {anyhow::bail!("tesseract failed");}
    let stdout=String::from_utf8_lossy(&output.stdout);
    let mut words=Vec::new();let mut sum=0f32;let mut n=0;
    for line in stdout.lines().skip(1) {
        let parts:Vec<&str>=line.splitn(12,'\t').collect();
        if parts.len()!=12 {continue;}
        let word=parts[11].trim();
        if word.is_empty(){continue;}
        words.push(word.to_owned());
        if let Ok(conf)=parts[10].parse::<f32>(){if conf>=0.0 {sum+=conf/100.0;n+=1;}}
    }
    let text=words.join(" ");
    Ok(Extracted {status:if text.is_empty(){"manual"}else{"ocr"}.into(),text,confidence:if n>0{Some(sum/n as f32)}else{None}})
}
fn pdf_text(path:&Path, max_pages:usize)->Result<Extracted>{
    let pdf=Command::new("pdftotext").args(["-layout","-enc","UTF-8"])
        .arg(path).arg("-").output().context("pdftotext unavailable")?;
    if pdf.status.success(){
        let text=String::from_utf8_lossy(&pdf.stdout).trim().to_owned();
        let pages=Command::new("pdfinfo").arg(path).output().ok()
            .and_then(|o|String::from_utf8(o.stdout).ok())
            .and_then(|s|s.lines().find(|s|s.starts_with("Pages:")).and_then(|s|s.split_whitespace().nth(1)?.parse::<usize>().ok()));
        let chars=text.chars().filter(|c|!c.is_whitespace()).count();
        if chars>80 {
            // `pdftotext` separates pages with form feeds. Mixed PDFs can have
            // good text on page 1 but scanned pages with no text afterwards.
            // Do not treat such a document as fully extracted.
            let text_pages: Vec<&str> = text.split('\u{000C}').collect();
            let count=pages.unwrap_or(text_pages.len());
            let sparse=count>1 && (chars/count<30 || text_pages.iter().take(count).any(|page| page.chars().filter(|c|!c.is_whitespace()).count()<20));
            return Ok(Extracted{ text, status:if sparse {"partial"}else{"extracted"}.into(),confidence:None });
        }
    }
    let info=Command::new("pdfinfo").arg(path).output();
    let pages=info.ok().and_then(|o|String::from_utf8(o.stdout).ok())
        .and_then(|s|s.lines().find(|s|s.starts_with("Pages:")).and_then(|s|s.split_whitespace().nth(1)?.parse::<usize>().ok()));
    let tmp=tempfile::tempdir()?;
    let prefix=tmp.path().join("page");
    let render=Command::new("pdftoppm").args(["-f","1","-l"])
        .arg(max_pages.to_string()).args(["-r","150","-png"])
        .arg(path).arg(&prefix).output().context("pdftoppm unavailable")?;
    if !render.status.success(){anyhow::bail!("pdftoppm failed");}
    let mut files:Vec<PathBuf>=std::fs::read_dir(tmp.path())?.flatten().map(|e|e.path()).filter(|p|p.extension().is_some_and(|e|e=="png")).collect();
    files.sort();
    let count=files.len();
    let mut parts=Vec::new();let mut confs=Vec::new();
    for (n,path) in files.iter().enumerate() {
        let recognized=tesseract(path)?;
        parts.push(format!("=== OCR Страница {} ===\n{}",n+1,recognized.text));
        if let Some(c)=recognized.confidence{confs.push(c);}
    }
    let text=parts.join("\n");
    let partial=pages.map(|p|p>count).unwrap_or(true);
    let status=if text.trim().is_empty(){"manual"}else if partial{"partial"}else{"ocr"};
    Ok(Extracted{ text, status:status.into(),confidence:if confs.is_empty(){None}else{Some(confs.iter().sum::<f32>()/confs.len() as f32)} })
}
pub fn extract(path:&Path,max_pages:usize)->Extracted{
    let res:Result<Extracted>=(||{
        let suffix=path.extension().and_then(|s|s.to_str()).unwrap_or("").to_lowercase();
        let mut parsed=match suffix.as_str(){
            "docx"=>Extracted{text:docx_text(path)?,status:"extracted".into(),confidence:None},
            "xlsx"=>Extracted{text:xlsx_text(path)?,status:"extracted".into(),confidence:None},
            "pdf"=>pdf_text(path,max_pages)?,
            "jpg"|"jpeg"|"png"=>tesseract(path)?,
            _=>Extracted{text:String::new(),status:"unsupported".into(),confidence:None},
        };
        if parsed.text.trim().is_empty() && parsed.status=="extracted" {parsed.status="manual".into();}
        Ok(parsed)
    })();
    res.unwrap_or_else(|e|Extracted{text:String::new(),status:format!("error: {}",e.to_string().chars().take(120).collect::<String>()),confidence:None})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn joins_docx_runs_without_fake_spaces(){
        let xml=r#"<w:p><w:r><w:t>Акт </w:t></w:r><w:r><w:t>освидетельствования</w:t></w:r></w:p>"#;
        let actual=docx_xml_to_text(xml).unwrap();
        assert_eq!(actual, "Акт освидетельствования\n");
    }
    #[test] fn preserves_requisite_across_bold_run(){
        let xml=r#"<w:p><w:r><w:t>ГО</w:t></w:r><w:r><w:t>СТ</w:t></w:r></w:p>"#;
        assert_eq!(docx_xml_to_text(xml).unwrap(),"ГОСТ\n");
    }
}

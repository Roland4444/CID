//! Official four-column register. Only confirmed/resolved remarks in official export.
use crate::models::Finding;
use anyhow::{Context,Result};
use rust_xlsxwriter::{Workbook,Format,Color,FormatAlign};
use std::{io::{Cursor,Write},process::Command};
use zip::{ZipWriter,write::FileOptions};

pub const HEADERS:[&str;4]=["Документ / пункт","Описание несоответствия","НТД и пункт НТД","Отметка об устранении"];
fn columns(f:&Finding)->[&str;4]{
    [if f.source_location.is_empty(){&f.rule_code}else{&f.source_location},&f.description,&f.ntd,""]
}
pub fn xlsx(findings:&[Finding])->Result<Vec<u8>>{
    let mut wb=Workbook::new();
    let ws=wb.add_worksheet();ws.set_name("Замечания")?;
    let heading=Format::new().set_background_color(Color::RGB(0x233850)).set_font_color(Color::White)
        .set_bold().set_text_wrap().set_align(FormatAlign::VerticalCenter);
    let cell=Format::new().set_text_wrap().set_align(FormatAlign::Top);
    for(i,h)in HEADERS.iter().enumerate(){ws.write_string_with_format(0,i as u16,*h,&heading)?;}
    for(i,w)in [42,75,36,30].iter().enumerate(){ws.set_column_width(i as u16,*w as f64)?;}
    ws.set_row_height(0,33.)?;
    for (i,f) in findings.iter().enumerate(){
        let row=(i+1) as u32;ws.set_row_height(row,56.)?;
        for (col,value) in columns(f).iter().enumerate(){ws.write_string_with_format(row,col as u16,*value,&cell)?;}
    }
    ws.set_freeze_panes(1,0)?;
    Ok(wb.save_to_buffer()?)
}
fn escape(s:&str)->String{html_escape::encode_text(s).into_owned()}
fn para(s:&str)->String{format!("<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",escape(s))}
fn table_cell(s:&str)->String{format!("<w:tc><w:tcPr><w:tcW w:w=\"4000\" w:type=\"dxa\"/></w:tcPr>{}</w:tc>",para(s))}
fn table_row(cells:&[&str;4])->String{let mut out=String::from("<w:tr>");for c in cells{out.push_str(&table_cell(c));}out.push_str("</w:tr>");out}
pub fn docx(findings:&[Finding])->Result<Vec<u8>>{
    let mut body=String::new();
    body.push_str(&para("Реестр замечаний к исполнительной документации"));
    body.push_str("<w:tbl><w:tblPr><w:tblBorders><w:top w:val=\"single\"/><w:left w:val=\"single\"/><w:bottom w:val=\"single\"/><w:right w:val=\"single\"/><w:insideH w:val=\"single\"/><w:insideV w:val=\"single\"/></w:tblBorders></w:tblPr>");
    body.push_str(&table_row(&HEADERS));
    for f in findings {body.push_str(&table_row(&columns(f)));}
    body.push_str("</w:tbl><w:sectPr><w:pgSz w:w=\"16838\" w:h=\"11906\" w:orient=\"landscape\"/><w:pgMar w:top=\"850\" w:bottom=\"850\" w:left=\"850\" w:right=\"850\"/></w:sectPr>");
    let document=format!(r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#);
    let types=r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels=r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    let buffer=Cursor::new(Vec::new());let mut zip=ZipWriter::new(buffer);
    let options=FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for(name,content)in [("[Content_Types].xml",types),("_rels/.rels",rels),("word/document.xml",document.as_str())]{
        zip.start_file(name,options)?;zip.write_all(content.as_bytes())?;
    }
    Ok(zip.finish()?.into_inner())
}
pub fn pdf(findings:&[Finding])->Result<Vec<u8>>{
    // Deliberate external dependency: only for DOCX -> PDF conversion, never document analysis.
    let temp=tempfile::tempdir()?;let source=temp.path().join("remarks.docx");
    std::fs::write(&source,docx(findings)?)?;
    let profile=temp.path().join("lo-profile");
    let profile_url=url::Url::from_directory_path(&profile).map_err(|_|anyhow::anyhow!("bad profile dir"))?;
    let output=Command::new("libreoffice").arg("--headless")
        .arg(format!("-env:UserInstallation={profile_url}"))
        .args(["--convert-to","pdf","--outdir"]).arg(temp.path()).arg(&source)
        .output().context("Install libreoffice for PDF export")?;
    if !output.status.success(){anyhow::bail!("PDF conversion failed: {}",String::from_utf8_lossy(&output.stderr));}
    Ok(std::fs::read(temp.path().join("remarks.pdf"))?)
}
#[cfg(test)]
mod tests{
    use super::*;
    #[test]fn four_columns(){assert_eq!(HEADERS.len(),4);assert_eq!(HEADERS[3],"Отметка об устранении");}
    #[test]fn creates_openxml(){let bytes=docx(&[]).unwrap();let mut zip=zip::ZipArchive::new(Cursor::new(bytes)).unwrap();assert!(zip.by_name("word/document.xml").is_ok());}
}

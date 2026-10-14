use crate::{catalog::Catalog,config::Config,engine,models::{Finding,Package},norms,repo,reports,scanner};
use anyhow::Result;
use axum::{
    body::{Body,Bytes},
    extract::{DefaultBodyLimit,Multipart,Path,Query,State},
    http::{header,Request,StatusCode},
    middleware::{self,Next},
    response::{Html,IntoResponse,Redirect,Response},
    routing::{get,post}, Form,Json,Router,
};
use base64::Engine;
use serde::Deserialize;
use sqlx::PgPool;
use std::{collections::{HashMap,HashSet},io::Write,path::PathBuf,sync::{Arc,atomic::{AtomicBool,Ordering}}};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use tracing::error;

pub struct AppState {
    pub cfg:Config, pub db:PgPool, pub catalog:Catalog,
    pub nas:crate::synology::SynologyClient, pub synology_connected:AtomicBool,
    pub scan_lock:Mutex<()>, pub csrf:String,
}
pub type Shared=Arc<AppState>;

pub struct WebError {status:StatusCode, detail:String}
impl WebError {
    fn bad(s:impl Into<String>)->Self{Self{status:StatusCode::BAD_REQUEST,detail:s.into()}}
    fn not_found()->Self{Self{status:StatusCode::NOT_FOUND,detail:"Not found".into()}}
    fn forbidden()->Self{Self{status:StatusCode::FORBIDDEN,detail:"CSRF validation failed".into()}}
}
impl From<anyhow::Error> for WebError {
    fn from(err:anyhow::Error)->Self{error!("Request failed: {err:#}");Self{status:StatusCode::INTERNAL_SERVER_ERROR,detail:"Internal server error (check server logs)".into()}}
}
impl From<sqlx::Error> for WebError { fn from(err:sqlx::Error)->Self {Self::from(anyhow::Error::from(err))} }
impl From<std::io::Error> for WebError { fn from(err:std::io::Error)->Self {Self::from(anyhow::Error::from(err))} }
impl From<http::Error> for WebError { fn from(err:http::Error)->Self {Self::from(anyhow::Error::from(err))} }
impl From<serde_json::Error> for WebError { fn from(err:serde_json::Error)->Self {Self::from(anyhow::Error::from(err))} }
impl From<tokio::task::JoinError> for WebError { fn from(err:tokio::task::JoinError)->Self {Self::from(anyhow::Error::from(err))} }
impl IntoResponse for WebError {fn into_response(self)->Response{(self.status,self.detail).into_response()}}
type WebResult<T>=std::result::Result<T,WebError>;
fn h(s:&str)->String {html_escape::encode_safe(s).to_string()}
fn hidden(csrf:&str)->String {format!("<input type=\"hidden\" name=\"csrf\" value=\"{}\">",h(csrf))}
fn verify(csrf:&str,state:&Shared)->WebResult<()> {
    if bool::from(csrf.as_bytes().ct_eq(state.csrf.as_bytes())) {Ok(())}else{Err(WebError::forbidden())}
}
fn layout(title:&str,body:String)->Html<String>{
    Html(format!(r#"<!DOCTYPE html><html lang="ru"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>{title} — Контроль ИД</title>
<style>body{{background:#f4f7fa;color:#1d334a;font:15px system-ui,Arial;margin:0}}header{{padding:17px 5%;background:#223851;color:white}}header a{{color:white;margin-right:24px;text-decoration:none}}main{{max-width:1180px;margin:28px auto;padding:0 16px}}article{{background:white;border:1px solid #d8e0e8;border-radius:10px;padding:18px;margin:18px 0;overflow-x:auto}}table{{width:100%;border-collapse:collapse}}td,th{{padding:9px;border-bottom:1px solid #dae1e8;text-align:left;vertical-align:top}}th{{background:#eef3f7}}input,select,textarea,button{{padding:8px;border-radius:5px;border:1px solid #bbc7d2;font:inherit;margin:3px}}button{{background:#224a69;color:white;border:0;cursor:pointer}}a{{color:#116caa}}small,.muted{{color:#536778}}.warn{{color:#8b4d17}}.ok{{color:#386e4c}}.row{{display:flex;gap:12px;flex-wrap:wrap;align-items:center}}.chips{{display:flex;gap:8px;flex-wrap:wrap}}.chip{{background:#e8eff5;border-radius:15px;padding:5px 9px}}.status{{font-weight:600}}</style></head>
<body><header><a href="/">Контроль ИД / Synology</a><a href="/catalog/errors">34 типовые ошибки</a><a href="/norms">Нормативные правила</a></header><main>{body}</main></body></html>"#,
        title=h(title),body=body))
}

async fn basic_auth(State(st):State<Shared>,request:Request<Body>,next:Next)->Response{
    let header=request.headers().get(header::AUTHORIZATION).and_then(|v|v.to_str().ok()).unwrap_or("");
    let auth=header.strip_prefix("Basic ").and_then(|token|base64::engine::general_purpose::STANDARD.decode(token).ok())
        .and_then(|v|String::from_utf8(v).ok());
    let authenticated=auth.and_then(|s|{
        let (user,pass)=s.split_once(':')?;
        let eq_user=bool::from(user.as_bytes().ct_eq(st.cfg.app_user.as_bytes()));
        let eq_pass=bool::from(pass.as_bytes().ct_eq(st.cfg.app_password.as_bytes()));
        Some(eq_user & eq_pass)
    }).unwrap_or(false);
    if !authenticated{
        return (StatusCode::UNAUTHORIZED,[(header::WWW_AUTHENTICATE,"Basic realm=\"ID-Control\"")],"Authentication required").into_response();
    }
    let mut response=next.run(request).await;
    response.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS,"nosniff".parse().unwrap());
    response.headers_mut().insert(header::CACHE_CONTROL,"no-store".parse().unwrap());
    response
}
pub fn router(state:Shared)->Router{
    Router::new()
        .route("/",get(home))
        .route("/health",get(health))
        .route("/catalog/errors",get(errors))
        .route("/norms",get(norms_view).post(create_norm_rule))
        .route("/norms/{id}/status",post(norm_rule_status))
        .route("/projects",post(create_project))
        .route("/packages/{id}",get(package_view))
        .route("/packages/{id}/catalog",post(set_catalog))
        .route("/packages/{id}/scan",post(manual_scan))
        .route("/packages/{id}/upload",post(upload))
        .route("/documents/{id}/classify",post(classify))
        .route("/documents/{id}/download",get(download))
        .route("/findings/{id}/review",post(review))
        .route("/packages/{id}/report/{fmt}",get(report))
        .layer(DefaultBodyLimit::max((state.cfg.max_file_mb*1024*1024+1024*1024) as usize))
        .layer(middleware::from_fn_with_state(state.clone(),basic_auth))
        .with_state(state)
}

async fn home(State(st):State<Shared>)->WebResult<Html<String>> {
    let projects=sqlx::query_as::<_,crate::models::Project>("SELECT id,name FROM projects ORDER BY name").fetch_all(&st.db).await?;
    let mut body=String::from("<h1>Проекты и комплекты ИД</h1>");
    if st.synology_connected.load(Ordering::Relaxed) {body.push_str("<p class=ok>Synology HTTPS API: последняя проверка успешна</p>");}
    else {body.push_str("<p class=warn>Synology HTTPS API: нет успешной проверки соединения. Проверьте журнал systemd.</p>");}
    body.push_str(&format!("<article><h2>Создать проект и комплект</h2><form method=post action=/projects>{}",hidden(&st.csrf)));
    body.push_str("<input name=name placeholder=\"Объект строительства\" required maxlength=300><select name=work_type>");
    for wt in &st.catalog.work_types{body.push_str(&format!("<option value=\"{}\">{}</option>",h(&wt.name),h(&wt.name)));}
    body.push_str("</select><button>Создать</button></form></article>");
    for p in projects {
        body.push_str(&format!("<article><h2>{}</h2><div class=chips>",h(&p.name)));
        let packages=sqlx::query_as::<_,Package>("SELECT id,project_id,name,catalog_name,required_codes_json FROM packages WHERE project_id=$1 ORDER BY name")
            .bind(p.id).fetch_all(&st.db).await?;
        for pkg in packages{body.push_str(&format!("<a class=chip href=\"/packages/{}\">{}</a>",pkg.id,h(&pkg.name)));}
        body.push_str("</div></article>");
    }
    Ok(layout("Проекты",body))
}
#[derive(Deserialize)]struct CreateProject {name:String,work_type:String,csrf:String}
async fn create_project(State(st):State<Shared>,Form(form):Form<CreateProject>)->WebResult<Redirect>{
    verify(&form.csrf,&st)?;
    let name=repo::validate_segment(&form.name).map_err(|_|WebError::bad("Invalid project name"))?;
    let work=repo::validate_segment(&form.work_type).map_err(|_|WebError::bad("Invalid work name"))?;
    let project=repo::get_or_create_project(&st.db,name).await?;
    let pkg=repo::get_or_create_package(&st.db,project.id,work).await?;
    if let Some(wt)=st.catalog.closest(work){
        sqlx::query("UPDATE packages SET catalog_name=$1 WHERE id=$2 AND catalog_name IS NULL")
            .bind(&wt.name).bind(pkg.id).execute(&st.db).await?;
    }
    repo::audit(&st.db,&st.cfg.app_user,"create_package",&format!("{name}/{work}")).await?;
    Ok(Redirect::to(&format!("/packages/{}",pkg.id)))
}
#[derive(Deserialize,Default)]struct RunQuery{run_id:Option<i64>}
async fn package_view(State(st):State<Shared>,Path(id):Path<i64>,Query(params):Query<RunQuery>)->WebResult<Html<String>>{
    let pkg=repo::package(&st.db,id).await.map_err(|_|WebError::not_found())?;
    let projects=sqlx::query_as::<_,crate::models::Project>("SELECT id,name FROM projects WHERE id=$1")
        .bind(pkg.project_id).fetch_one(&st.db).await?;
    let docs=repo::documents(&st.db,id).await?;
    let mut current=HashMap::new();
    for doc in &docs{current.entry(doc.relative_path.clone()).or_insert(doc);}
    let runs=repo::runs(&st.db,id).await?;
    let selected=match params.run_id {Some(run_id)=>runs.iter().find(|r|r.id==run_id).ok_or_else(WebError::not_found).map(Some)?,None=>runs.first()};
    let findings=if let Some(run)=selected{repo::findings(&st.db,run.id).await?}else{vec![]};
    let norm_checks=if let Some(run)=selected{norms::checks_for_run(&st.db,run.id).await?}else{vec![]};
    let mut body=format!("<p><a href=\"/\">← Проекты</a></p><h1>{} / {}</h1><p class=muted>Папка NAS → проект → вид работ → ID/REFERENCE</p>",h(&projects.name),h(&pkg.name));
    body.push_str(&format!("<article><h2>Загрузка документов</h2><form action=\"/packages/{id}/upload\" method=post enctype=multipart/form-data>{}<label>Тип <select name=kind><option value=ID>Исполнительная документация</option><option value=REFERENCE>Эталон ПД/РД</option></select></label><input type=file name=file required><button>Загрузить</button></form>",hidden(&st.csrf)));
    body.push_str(&format!("<form action=\"/packages/{id}/scan\" method=post>{}<button>Просканировать Synology и повторить проверку</button></form></article>",hidden(&st.csrf)));
    body.push_str(&format!("<article><h2>Чек-лист и вид работ</h2><form method=post action=\"/packages/{id}/catalog\">{}<select name=work_type>",hidden(&st.csrf)));
    for wt in &st.catalog.work_types{let sel=if pkg.catalog_name.as_deref()==Some(wt.name.as_str()){ "selected" }else{""};body.push_str(&format!("<option {sel} value=\"{}\">{}</option>",h(&wt.name),h(&wt.name)));}
    body.push_str("</select><p class=muted>Отметьте документы, обязательность которых утвердил инженер. До утверждения ни один пункт не считается автоматически обязательным.</p>");
    let selected_codes:HashSet<String>=serde_json::from_str::<Vec<String>>(&pkg.required_codes_json).unwrap_or_default().into_iter().collect();
    if let Some(wt)=pkg.catalog_name.as_deref().and_then(|n|st.catalog.work_type(n)){
        for item in &wt.items{
            let checked=if selected_codes.contains(&item.code){"checked"}else{""};
            body.push_str(&format!("<p><label><input type=checkbox name=required_codes value=\"{}\" {checked}> <b>{}</b> {} <small>({})</small></label></p>",h(&item.code),h(&item.code),h(&item.title),h(&item.norm_reference)));
        }
    }
    body.push_str("<button>Сохранить чек-лист и запустить проверку</button></form></article>");
    body.push_str("<article><h2>Текущие документы</h2><table><tr><th>Файл</th><th>Тип и распознавание</th><th>Версия</th><th>Действия</th></tr>");
    let mut shown:Vec<_>=current.values().collect();shown.sort_by(|a,b|a.relative_path.cmp(&b.relative_path));
    for doc in shown{
        let delete_msg=if !doc.is_present{" <span class=warn>(отсутствует на NAS)</span>"}else{""};
        body.push_str(&format!("<tr><td>{}{}<br><small>{}</small></td><td>{}<br><small>{}</small></td><td>v{}</td><td><a href=\"/documents/{}/download\">Оригинал</a>",h(&doc.relative_path),delete_msg,h(&doc.sha256[..12]),h(doc.document_type.as_deref().unwrap_or("не классифицирован")),h(&doc.recognition_status),doc.version,doc.id));
        if doc.source_kind=="ID"{
            body.push_str(&format!("<form method=post action=\"/documents/{}/classify\">{}<select name=catalog_code><option value=\"\">Не определён</option>",doc.id,hidden(&st.csrf)));
            if let Some(wt)=pkg.catalog_name.as_deref().and_then(|n|st.catalog.work_type(n)){
                for item in &wt.items{let sel=if doc.catalog_code.as_deref()==Some(item.code.as_str()){"selected"}else{""};body.push_str(&format!("<option {sel} value=\"{}\">{} — {}</option>",h(&item.code),h(&item.code),h(&item.title)));}
            }
            body.push_str("</select><button>Классифицировать</button></form>");
        }
        body.push_str("</td></tr>");
    }
    body.push_str("</table></article>");
    body.push_str("<article><h2>Проверки и замечания</h2><div class=row>");
    for run in &runs{body.push_str(&format!("<a class=chip href=\"/packages/{id}?run_id={}\">Проверка #{} — {}</a>",run.id,run.id,run.started_at.format("%d.%m.%Y %H:%M")));}
    body.push_str("</div>");
    if let Some(run)=selected{
        body.push_str(&format!("<p><a href=\"/packages/{id}/report/xlsx?run_id={}\">XLSX</a> · <a href=\"/packages/{id}/report/docx?run_id={}\">DOCX</a> · <a href=\"/packages/{id}/report/pdf?run_id={}\">PDF</a> — официальная выгрузка только подтверждённых/устранённых замечаний. <a href=\"/packages/{id}/report/xlsx?run_id={}&draft=true\">Черновик всех</a></p>",run.id,run.id,run.id,run.id));
    }
    body.push_str("<table><tr><th>Блок и код</th><th>Источник и описание</th><th>Результат</th><th>Решение инженера</th></tr>");
    for f in findings{
        body.push_str(&format!("<tr><td>{} / {}</td><td><small>{}</small><p>{}</p>{}</td><td class=status>{}</td><td><form method=post action=\"/findings/{}/review\">{}<select name=status>",f.block,h(&f.rule_code),h(&f.source_location),h(&f.description),if f.reference.is_empty(){"".to_string()}else{format!("<small>{}</small>",h(&f.reference))},if f.result=="mismatch"{"Выявлено несоответствие"}else{"Требуется ручная проверка"},f.id,hidden(&st.csrf)));
        for (value,label) in [("pending","На рассмотрении"),("confirmed","Подтверждено"),("rejected","Отклонено"),("resolved","Устранено")]{
            let sel=if f.review_status==value{"selected"}else{""};body.push_str(&format!("<option value={value} {sel}>{label}</option>"));
        }
        body.push_str(&format!("</select><input name=comment placeholder=Комментарий value=\"{}\"><button>Сохранить</button></form></td></tr>",h(&f.comment)));
    }
    body.push_str("</table><p class=muted>Отсутствие замечаний не подтверждает соответствие всем строительным нормам. Применяются только утверждённые правила; НТД не придумываются.</p></article>");
    body.push_str("<article><h2>Проверки утверждённых нормативных правил</h2><p class=muted>«Найдено подтверждение» означает обнаружение заданного текста, а не полное соответствие ГОСТ/СП. Настроить правила: <a href=/norms>НТД и пункты</a>.</p><table><tr><th>Документ / правило</th><th>Норматив и источник</th><th>Результат</th></tr>");
    for check in &norm_checks{
        let label=match check.result.as_str(){"evidence_found"=>"Найдено подтверждение в тексте","mismatch"=>"Вероятное несоответствие","manual"=>"Требуется проверка инженера",_=>"Неизвестно"};
        body.push_str(&format!("<tr><td>Документ #{} / правило #{}</td><td>{}<br><small>{}</small></td><td><b>{}</b><p>{}</p></td></tr>",check.document_id,check.rule_id,h(&check.normative_basis),h(&check.source_reference),label,h(&check.details)));
    }
    body.push_str("</table></article>");
    if docs.len()>current.len(){body.push_str(&format!("<p>Предыдущие версии: {} (сохраняются в архиве и доступны по ID).</p>",docs.len()-current.len()));}
    Ok(layout("Комплект ИД",body))
}

/// Normative rules must be entered from a source the organisation possesses and explicitly approved.
async fn norms_view(State(st):State<Shared>)->WebResult<Html<String>> {
    let rules=norms::list_rules(&st.db).await?;
    let mut body=String::from("<h1>Нормативные правила (ГОСТ, СП, приказы)</h1><p>Здесь регистрируются <b>проверенные специалистом</b> требования конкретной редакции НТД. Ссылки из исходного ТЗ не считаются автоматически утверждёнными. Пустая база правил означает, что нормативная проверка не пройдена.</p>");
    body.push_str(&format!("<article><h2>Добавить правило на утверждение</h2><form method=post action=/norms>{}",hidden(&st.csrf)));
    body.push_str(r#"<p>Вид работ <select name=work_type><option value="">Все виды работ</option>"#);
    for wt in &st.catalog.work_types{body.push_str(&format!("<option value=\"{}\">{}</option>",h(&wt.name),h(&wt.name)));}
    body.push_str("</select> Код документа по справочнику (необязательно; например 1.4): <input name=document_code placeholder=1.4 maxlength=30></p>");
    body.push_str("<p><input name=norm_name required maxlength=250 placeholder=\"Название ГОСТ / СП / приказа\"> <input name=norm_edition required maxlength=80 placeholder=\"Редакция / год\"> <input name=norm_clause required maxlength=100 placeholder=\"Пункт / приложение НТД\"></p>");
    body.push_str("<p><input name=source_reference required maxlength=500 style=width:80% placeholder=\"Источник: утверждённый PDF / ссылка на файл с нормой\"></p>");
    body.push_str("<p><input name=description required maxlength=500 style=width:80% placeholder=\"Что именно проверяем и почему правило применимо\"></p>");
    body.push_str("<p>Тип проверки: <select name=check_kind><option value=required_phrase>Наличие обязательного текста/реквизита</option><option value=forbidden_phrase>Недопустимый текст/устаревшая ссылка</option><option value=required_pattern>Обязательный реквизит по регулярному выражению</option><option value=manual>Требует инженерной сверки</option></select></p>");
    body.push_str("<p><input name=expected_text maxlength=300 style=width:80% placeholder=\"Точный текст для поиска (для ручной проверки не нужен)\"></p>");
    body.push_str("<button>Создать черновик правила</button></form><p class=muted>Поиск фразы проверяет только присутствие/отсутствие текста. Автоматической правовой экспертизы и анализа геометрии здесь нет.</p></article>");
    body.push_str("<article><h2>Каталог правил</h2><table><tr><th>Правило</th><th>НТД и источник</th><th>Условие</th><th>Состояние</th></tr>");
    for rule in &rules {
        body.push_str(&format!("<tr><td><b>#{} v{}</b> {}<p><small>Вид работ: {}, код: {}</small></p></td><td>{}<p><small>Источник: {}</small></p></td><td>{}<p><small>Текст: {}</small></p></td><td>{}",rule.id,rule.version,h(&rule.description),h(if rule.work_type.is_empty(){"все"}else{&rule.work_type}),h(if rule.document_code.is_empty(){"все"}else{&rule.document_code}),h(&norms::basis(rule)),h(&rule.source_reference),h(&rule.check_kind),h(&rule.expected_text),h(&rule.approval_status)));
        if rule.approval_status!="approved" && rule.approval_status!="revoked" {
            body.push_str(&format!("<form method=post action=\"/norms/{}/status\">{}<input type=hidden name=status value=approved><button>Утвердить</button></form>",rule.id,hidden(&st.csrf)));
        }else if rule.approval_status=="approved" {
            body.push_str(&format!("<p><small>Утвердил: {}</small></p><form method=post action=\"/norms/{}/status\">{}<input type=hidden name=status value=revoked><button>Отозвать</button></form>",h(rule.approved_by.as_deref().unwrap_or("неизвестно")),rule.id,hidden(&st.csrf)));
        }
        body.push_str("</td></tr>");
    }
    body.push_str("</table><p>Редактирование утверждённого правила запрещено: создайте новую версию новым правилом и отзовите старую. Проверки прошлых периодов сохраняются.</p></article>");
    Ok(layout("Нормативные правила",body))
}
#[derive(Deserialize)]
struct NormForm {
    csrf:String, work_type:String, document_code:String, norm_name:String, norm_edition:String,
    norm_clause:String, source_reference:String, description:String, check_kind:String, expected_text:String,
}
async fn create_norm_rule(State(st):State<Shared>,Form(form):Form<NormForm>)->WebResult<Redirect>{
    verify(&form.csrf,&st)?;
    let filled=[(&form.norm_name,250usize),(&form.norm_edition,80),(&form.norm_clause,100),(&form.source_reference,500),(&form.description,500)];
    if filled.iter().any(|(s,max)|s.trim().is_empty() || s.len()>*max || s.chars().any(char::is_control)) {
        return Err(WebError::bad("Укажите нормативный документ, редакцию, пункт, источник и описание (без управляющих символов)."));
    }
    if !["required_phrase","forbidden_phrase","required_pattern","manual"].contains(&form.check_kind.as_str()) {
        return Err(WebError::bad("Неизвестный тип правила"));
    }
    if form.expected_text.len()>300 || (form.check_kind!="manual" && form.expected_text.trim().chars().count()<3) {
        return Err(WebError::bad("Нужен точный проверяемый текст (минимум 3 символа)."));
    }
    if form.check_kind=="required_pattern" && regex::Regex::new(&form.expected_text).is_err(){
        return Err(WebError::bad("Неверное регулярное выражение (используйте синтаксис Rust regex)."));
    }
    if !form.work_type.is_empty() && st.catalog.work_type(&form.work_type).is_none() {
        return Err(WebError::bad("Неизвестный вид работ"));
    }
    if !form.document_code.is_empty() && (form.work_type.is_empty() || st.catalog.item(&form.work_type,&form.document_code).is_none()){
        return Err(WebError::bad("Код документа должен существовать в выбранном виде работ"));
    }
    let new:(i64,)=sqlx::query_as("INSERT INTO normative_rules(work_type,document_code,norm_name,norm_edition,norm_clause,source_reference,description,check_kind,expected_text,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,(SELECT COALESCE(MAX(version),0)+1 FROM normative_rules WHERE work_type=$1 AND document_code=$2 AND norm_name=$3 AND norm_clause=$5)) RETURNING id")
        .bind(&form.work_type).bind(&form.document_code).bind(form.norm_name.trim()).bind(form.norm_edition.trim())
        .bind(form.norm_clause.trim()).bind(form.source_reference.trim()).bind(form.description.trim())
        .bind(&form.check_kind).bind(form.expected_text.trim()).fetch_one(&st.db).await?;
    repo::audit(&st.db,&st.cfg.app_user,"create_norm_rule",&format!("id={}",new.0)).await?;
    Ok(Redirect::to("/norms"))
}
#[derive(Deserialize)]struct NormStatusForm{csrf:String,status:String}
async fn norm_rule_status(State(st):State<Shared>,Path(id):Path<i64>,Form(form):Form<NormStatusForm>)->WebResult<Redirect>{
    verify(&form.csrf,&st)?;
    if !["approved","revoked"].contains(&form.status.as_str()) {return Err(WebError::bad("Неизвестный статус"));}
    let count=if form.status=="approved"{
        sqlx::query("UPDATE normative_rules SET approval_status='approved',approved_by=$1,approved_at=now() WHERE id=$2 AND approval_status='draft'")
            .bind(&st.cfg.app_user).bind(id).execute(&st.db).await?.rows_affected()
    }else{
        sqlx::query("UPDATE normative_rules SET approval_status='revoked' WHERE id=$1 AND approval_status='approved'")
            .bind(id).execute(&st.db).await?.rows_affected()
    };
    if count!=1 {return Err(WebError::bad("Правило не найдено или уже изменило статус"));}
    repo::audit(&st.db,&st.cfg.app_user,"norm_rule_status",&format!("id={id} status={}",form.status)).await?;
    Ok(Redirect::to("/norms"))
}

async fn errors(State(st):State<Shared>)->WebResult<Html<String>>{
    let mut body=String::from("<h1>Справочник 34 типовых ошибок</h1><article><table><tr><th>Код</th><th>Ошибка</th><th>Алгоритм</th><th>Обработка</th></tr>");
    for err in &st.catalog.errors{body.push_str(&format!("<tr><td>{}</td><td>{}<p><small>{}</small></p></td><td>{}</td><td>{}</td></tr>",h(&err.code),h(&err.title),h(&err.description),h(&err.algorithm),h(&err.check_type)));}
    body.push_str("</table></article>");Ok(layout("Частые ошибки",body))
}

async fn set_catalog(State(st):State<Shared>,Path(id):Path<i64>,body:Bytes)->WebResult<Redirect>{
    let mut work=None;let mut csrf=None;let mut selected=HashSet::new();
    for (key,value) in url::form_urlencoded::parse(&body){
        match key.as_ref(){"csrf"=>csrf=Some(value.into_owned()),"work_type"=>work=Some(value.into_owned()),"required_codes"=>{selected.insert(value.into_owned());},_=>{}}
    }
    verify(csrf.as_deref().unwrap_or(""),&st)?;
    let wt=st.catalog.work_type(work.as_deref().unwrap_or("")).ok_or_else(||WebError::bad("Unknown work type"))?;
    let valid:HashSet<_>=wt.items.iter().map(|i|i.code.as_str()).collect();
    if selected.iter().any(|code|!valid.contains(code.as_str())){return Err(WebError::bad("Invalid checklist entry"));}
    let _=repo::package(&st.db,id).await.map_err(|_|WebError::not_found())?;
    let mut codes:Vec<_>=selected.into_iter().collect();codes.sort();
    sqlx::query("UPDATE packages SET catalog_name=$1,required_codes_json=$2 WHERE id=$3")
        .bind(&wt.name).bind(serde_json::to_string(&codes)?).bind(id).execute(&st.db).await?;
    engine::analyze(&st.db,&st.catalog,id,"manual").await?;
    repo::audit(&st.db,&st.cfg.app_user,"configure_catalog",&format!("package={id} codes={codes:?}")).await?;
    Ok(Redirect::to(&format!("/packages/{id}")))
}
#[derive(Deserialize)]struct CsrfForm{csrf:String}
async fn manual_scan(State(st):State<Shared>,Path(id):Path<i64>,Form(form):Form<CsrfForm>)->WebResult<Redirect>{
    verify(&form.csrf,&st)?;
    let _=repo::package(&st.db,id).await.map_err(|_|WebError::not_found())?;
    let _guard=st.scan_lock.lock().await;
    match scanner::scan_once(&st.db,&st.cfg,&st.catalog,&st.nas).await {
        Ok(_)=>st.synology_connected.store(true,Ordering::Relaxed),
        Err(e)=>{st.synology_connected.store(false,Ordering::Relaxed);return Err(e.into());}
    }
    engine::analyze(&st.db,&st.catalog,id,"manual").await?;
    repo::audit(&st.db,&st.cfg.app_user,"manual_scan",&format!("package={id}")).await?;
    Ok(Redirect::to(&format!("/packages/{id}")))
}
async fn upload(State(st):State<Shared>,Path(id):Path<i64>,mut multipart:Multipart)->WebResult<Redirect>{
    let pkg=repo::package(&st.db,id).await.map_err(|_|WebError::not_found())?;
    let project=sqlx::query_as::<_,crate::models::Project>("SELECT id,name FROM projects WHERE id=$1")
        .bind(pkg.project_id).fetch_one(&st.db).await?;
    let mut kind=String::from("ID");let mut csrf=String::new();
    let mut staged:Option<(tempfile::NamedTempFile,String)>=None;
    while let Some(mut field)=multipart.next_field().await.map_err(|e|WebError::bad(e.to_string()))?{
        let field_name=field.name().unwrap_or("").to_owned();
        match field_name.as_str(){
            "csrf"=>csrf=field.text().await.map_err(|e|WebError::bad(e.to_string()))?,
            "kind"=>kind=field.text().await.map_err(|e|WebError::bad(e.to_string()))?,
            "file"=>{
                if staged.is_some() {return Err(WebError::bad("Only one document per upload"));}
                let original_name=field.file_name().unwrap_or("file").to_owned();
                let filename=repo::validate_segment(&original_name).map_err(|_|WebError::bad("Invalid filename"))?;
                if !crate::docs::supported(&PathBuf::from(filename)) || filename.starts_with("~$") {
                    return Err(WebError::bad("Unsupported filename/extension"));
                }
                let staging=st.cfg.data_root.join("inbox");
                std::fs::create_dir_all(&staging)?;
                let mut tmp=tempfile::NamedTempFile::new_in(&staging)?;
                let mut count=0u64;
                while let Some(chunk)=field.chunk().await.map_err(|e|WebError::bad(e.to_string()))?{
                    count+=chunk.len() as u64;
                    if count>st.cfg.max_file_mb*1024*1024 {return Err(WebError::bad("File too large"));}
                    tmp.write_all(&chunk)?;
                }
                staged=Some((tmp,filename.to_owned()));
            },
            _=>{}
        }
    }
    verify(&csrf,&st)?;
    if kind!="ID" && kind!="REFERENCE"{return Err(WebError::bad("Invalid kind"));}
    let (tmp,filename)=staged.ok_or_else(||WebError::bad("Missing file"))?;
    let _guard=st.scan_lock.lock().await;
    let root=st.cfg.data_root.join("inbox");
    let file=root.join(&project.name).join(&pkg.name).join(&kind).join(&filename);
    std::fs::create_dir_all(file.parent().unwrap())?;
    tmp.persist(&file).map_err(|e|WebError::from(anyhow::Error::from(e.error)))?;
    if scanner::ingest(&st.db,&st.cfg,&st.catalog,&root,&file,"upload").await?.is_some(){
        engine::analyze(&st.db,&st.catalog,id,"upload").await?;
    }
    repo::audit(&st.db,&st.cfg.app_user,"upload",&format!("package={id} file={}",file.display())).await?;
    Ok(Redirect::to(&format!("/packages/{id}")))
}

#[derive(Deserialize)]struct ClassifyForm{csrf:String,catalog_code:String}
async fn classify(State(st):State<Shared>,Path(id):Path<i64>,Form(form):Form<ClassifyForm>)->WebResult<Redirect>{
    verify(&form.csrf,&st)?;
    let doc=repo::document(&st.db,id).await.map_err(|_|WebError::not_found())?;
    let pkg=repo::package(&st.db,doc.package_id).await?;
    let item=if form.catalog_code.is_empty(){None}else{
        Some(st.catalog.item(pkg.catalog_name.as_deref().unwrap_or(""),&form.catalog_code).ok_or_else(||WebError::bad("Unknown code"))?)
    };
    sqlx::query("UPDATE documents SET catalog_code=$1,document_type=$2 WHERE id=$3")
        .bind(item.map(|i|i.code.clone())).bind(item.map(|i|i.title.clone())).bind(id).execute(&st.db).await?;
    engine::analyze(&st.db,&st.catalog,pkg.id,"manual").await?;
    repo::audit(&st.db,&st.cfg.app_user,"classify_document",&format!("doc={id} code={}",form.catalog_code)).await?;
    Ok(Redirect::to(&format!("/packages/{}",pkg.id)))
}
#[derive(Deserialize)]struct ReviewForm{csrf:String,status:String,#[serde(default)]comment:String}
async fn review(State(st):State<Shared>,Path(id):Path<i64>,Form(form):Form<ReviewForm>)->WebResult<Redirect>{
    verify(&form.csrf,&st)?;
    if !["pending","confirmed","rejected","resolved"].contains(&form.status.as_str()){
        return Err(WebError::bad("Invalid review status"));
    }
    let row:Option<(i64,)>=sqlx::query_as("UPDATE findings SET review_status=$1,comment=$2,reviewed_by=$3,reviewed_at=now() WHERE id=$4 RETURNING package_id")
        .bind(&form.status).bind(form.comment.chars().take(5000).collect::<String>()).bind(&st.cfg.app_user).bind(id)
        .fetch_optional(&st.db).await?;
    let package_id=row.ok_or_else(WebError::not_found)?.0;
    repo::audit(&st.db,&st.cfg.app_user,"review",&format!("finding={id} status={}",form.status)).await?;
    Ok(Redirect::to(&format!("/packages/{package_id}")))
}
async fn download(State(st):State<Shared>,Path(id):Path<i64>)->WebResult<Response>{
    let doc=repo::document(&st.db,id).await.map_err(|_|WebError::not_found())?;
    let path=&doc.archive_path;
    let safe=std::fs::canonicalize(path).map_err(|_|WebError::not_found())?;
    let root=std::fs::canonicalize(&st.cfg.data_root)?;
    if !safe.starts_with(&root){return Err(WebError{status:StatusCode::FORBIDDEN,detail:"Invalid storage path".into()});}
    let file=tokio::fs::File::open(safe).await?;
    let stream=tokio_util::io::ReaderStream::new(file);
    let fname=format!("document-{}-v{}.{}",id,doc.version,doc.relative_path.rsplit('.').next().unwrap_or("bin"));
    let response=Response::builder().header(header::CONTENT_TYPE,"application/octet-stream")
        .header(header::CONTENT_DISPOSITION,format!("attachment; filename=\"{}\"",fname))
        .body(Body::from_stream(stream))?;
    Ok(response)
}
#[derive(Deserialize,Default)]struct ReportQuery{#[serde(default)]draft:bool,run_id:Option<i64>}
async fn report(State(st):State<Shared>,Path((id,fmt)):Path<(i64,String)>,Query(query):Query<ReportQuery>)->WebResult<Response>{
    if !["xlsx","docx","pdf"].contains(&fmt.as_str()){return Err(WebError::bad("Unknown report format"));}
    let _=repo::package(&st.db,id).await.map_err(|_|WebError::not_found())?;
    let runs=repo::runs(&st.db,id).await?;
    let run=if let Some(want)=query.run_id {runs.iter().find(|r|r.id==want)}else{runs.first()};
    let run=run.ok_or_else(WebError::not_found)?;
    let all=repo::findings(&st.db,run.id).await?;
    let filtered:Vec<Finding>=all.into_iter().filter(|f|query.draft || ["confirmed","resolved"].contains(&f.review_status.as_str())).collect();
    let fmt_for_task=fmt.clone();
    let output=tokio::task::spawn_blocking(move||->Result<Vec<u8>>{
        match fmt_for_task.as_str(){"xlsx"=>reports::xlsx(&filtered),"docx"=>reports::docx(&filtered),"pdf"=>reports::pdf(&filtered),_=>unreachable!()}
    }).await??;
    let mime=match fmt.as_str(){"xlsx"=>"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet","docx"=>"application/vnd.openxmlformats-officedocument.wordprocessingml.document","pdf"=>"application/pdf",_=>"application/octet-stream"};
    Ok(Response::builder().header(header::CONTENT_TYPE,mime)
        .header(header::CONTENT_DISPOSITION,format!("attachment; filename=\"findings-{}-{}.{}\"",id,run.id,fmt))
        .body(Body::from(output))?)
}
async fn health(State(st):State<Shared>)->WebResult<Json<serde_json::Value>>{
    let approved:(i64,) = sqlx::query_as("SELECT count(*) FROM normative_rules WHERE approval_status='approved'")
        .fetch_one(&st.db).await?;
    Ok(Json(serde_json::json!({"synology_connected":st.synology_connected.load(Ordering::Relaxed),
        "approved_normative_rules":approved.0,"normative_rules_configured":approved.0>0,
        "app_version":"0.5.0"})))
}

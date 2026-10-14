//! Read-only Synology File Station API client over HTTPS (DSM port 5001).
//! The NAS is never mounted, and no mutating File Station APIs are called.
use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::{redirect::Policy, Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::{collections::HashMap, path::Path, time::Duration};
use tokio::{fs::File, io::AsyncWriteExt};

#[derive(Clone)]
pub struct SynologyClient {
    client: Client,
    base: Url,
    root: String,
    username: String,
    password: String,
}
#[derive(Debug, Clone)]
pub struct RemoteFile {
    pub path: String,
    pub size: u64,
    pub mtime: i64,
}
#[derive(Debug, Deserialize)]
struct Envelope<T> {
    success: bool,
    data: Option<T>,
    error: Option<ApiError>,
}
#[derive(Debug, Deserialize)]
struct ApiError { code: i32 }
#[derive(Debug, Deserialize)]
struct ApiSpec {
    path: String,
    #[serde(rename="minVersion")]
    min_version: u32,
    #[serde(rename="maxVersion")]
    max_version: u32,
}
#[derive(Debug, Deserialize)]
struct Login { sid: String }
#[derive(Debug, Deserialize)]
struct FileListing { files: Vec<FileEntry>, total: usize }
#[derive(Debug, Deserialize)]
struct FileEntry {
    path: String,
    name: String,
    isdir: bool,
    #[serde(default)]
    additional: Option<Extra>,
    #[serde(default)]
    size: Option<u64>,
}
#[derive(Debug, Deserialize)]
struct Extra { size: Option<u64>, time: Option<FileTimes> }
#[derive(Debug, Deserialize)]
struct FileTimes { mtime: Option<i64> }

pub struct SynologySession<'a> {
    client: &'a SynologyClient,
    auth: ApiSpec,
    list: ApiSpec,
    download: ApiSpec,
    sid: String,
    auth_version: u32,
}

impl SynologyClient {
    pub fn new(cfg: &Config) -> Result<Self> {
        let base=Url::parse(&cfg.synology_url).context("SYNOLOGY_URL must be an HTTPS URL")?;
        if base.scheme()!="https" || base.host_str().is_none() || base.username()!="" ||
           base.password().is_some() || base.query().is_some() || base.fragment().is_some() ||
           base.path()!="/" {
            bail!("SYNOLOGY_URL must be https://hostname-or-ip:5001 without path, query, or credentials");
        }
        let mut builder=Client::builder()
            .timeout(Duration::from_secs(240))
            .connect_timeout(Duration::from_secs(15))
            .redirect(Policy::none()); // Never send SID to a redirect host.
        if !cfg.synology_verify_tls {
            tracing::warn!("TLS certificate verification DISABLED for Synology; only use temporarily on trusted networks");
            builder=builder.danger_accept_invalid_certs(true);
        }
        if let Some(ref ca)=cfg.synology_ca_cert {
            let pem=std::fs::read(ca).with_context(||format!("Cannot read Synology CA certificate: {}",ca.display()))?;
            builder=builder.add_root_certificate(reqwest::Certificate::from_pem(&pem)?);
        }
        Ok(Self {client:builder.build()?,base,root:cfg.synology_root.clone(),
            username:cfg.synology_username.clone(),password:cfg.synology_password.clone()})
    }
    fn api_url(&self, path: &str) -> Result<Url> {
        // API.Info data is untrusted. Restrict routes to the NAS's /webapi/ tree.
        if path.is_empty() || path.starts_with('/') || path.contains("..") || path.contains('?') ||
           path.contains('#') || path.contains('\\') || path.contains("://") ||
           !path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c)) {
            bail!("Unsafe API path advertised by DSM");
        }
        Ok(self.base.join(&format!("webapi/{path}"))?)
    }
    async fn discover(&self) -> Result<HashMap<String, ApiSpec>> {
        let keys="SYNO.API.Auth,SYNO.FileStation.List,SYNO.FileStation.Download";
        // entry.cgi is standard on current DSM. query.cgi is supported on older DSM.
        let mut last_error=None;
        for script in ["entry.cgi","query.cgi"] {
            let url=self.api_url(script)?;
            let resp=self.client.get(url).query(&[("api","SYNO.API.Info"),("version","1"),
                ("method","query"),("query",keys)]).send().await?;
            if !resp.status().is_success() {last_error=Some(anyhow!("DSM discovery HTTP {}",resp.status()));continue;}
            let reply:Envelope<HashMap<String,ApiSpec>>=resp.json().await.context("Invalid API.Info response")?;
            if reply.success {return reply.data.context("API.Info returned no data");}
            last_error=Some(anyhow!("API.Info error code {:?}",reply.error.map(|e|e.code)));
        }
        Err(last_error.unwrap_or_else(||anyhow!("API.Info discovery failed")))
    }
    pub async fn connect(&self) -> Result<SynologySession<'_>> {
        let mut info=self.discover().await?;
        let auth=info.remove("SYNO.API.Auth").context("DSM lacks SYNO.API.Auth")?;
        let list=info.remove("SYNO.FileStation.List").context("DSM lacks SYNO.FileStation.List")?;
        let download=info.remove("SYNO.FileStation.Download").context("DSM lacks SYNO.FileStation.Download")?;
        let version=if auth.min_version<=6 && auth.max_version>=6 {6} else {auth.max_version};
        if version<3 || version<auth.min_version || list.max_version<2 || download.max_version<2 {
            bail!("Unsupported DSM File Station API versions");
        }
        let form:Vec<(&str,String)>=vec![
            ("api","SYNO.API.Auth".into()),("version",version.to_string()),
            ("method","login".into()),("account",self.username.clone()),
            ("passwd",self.password.clone()),("session","FileStation".into()),
            ("format","sid".into())];
        let reply:Envelope<Login>=self.client.post(self.api_url(&auth.path)?)
            .form(&form)
            .send().await.context("DSM login request failed")?
            .error_for_status()?.json().await.context("Invalid DSM login response")?;
        if !reply.success {bail!("DSM login failed (code {:?})",reply.error.map(|e|e.code));}
        let sid=reply.data.context("DSM login returned no session")?.sid;
        if sid.is_empty(){bail!("DSM login returned empty session");}
        Ok(SynologySession{client:self,auth,list,download,sid,auth_version:version})
    }
}
impl SynologySession<'_> {
    async fn list_dir(&self, folder:&str) -> Result<Vec<FileEntry>> {
        let mut result=Vec::new();
        let mut offset=0usize;
        loop {
            let params=[("api", "SYNO.FileStation.List".to_owned()),
              ("version", "2".to_owned()), ("method", "list".to_owned()),
              ("folder_path",json!(folder).to_string()),
              ("additional",json!(["size","time"]).to_string()),
              ("offset",offset.to_string()),("limit","200".to_string()),
              ("_sid",self.sid.clone())];
            // POST avoids exposing session tokens in reverse-proxy access logs and history.
            let response:Envelope<FileListing>=self.client.client.post(self.client.api_url(&self.list.path)?)
                .form(&params).send().await.context("DSM list request failed")?
                .error_for_status()?.json().await.context("DSM list returned invalid JSON")?;
            if !response.success {bail!("DSM list failed for folder {folder} (code {:?})",response.error.map(|e|e.code));}
            let page=response.data.context("DSM list omitted data")?;
            if page.files.is_empty() && offset < page.total {bail!("Incomplete DSM folder listing: {folder}");}
            offset+=page.files.len(); result.extend(page.files);
            if offset>=page.total {break;}
            if result.len()>100_000 {bail!("Excessive number of entries in one NAS directory");}
        }
        Ok(result)
    }
    pub async fn list_files(&self) -> Result<Vec<RemoteFile>> {
        let mut pending=vec![(self.client.root.clone(),0usize)];
        let mut output=Vec::new();
        while let Some((folder,depth))=pending.pop() {
            if depth>24 {bail!("NAS directory nesting exceeds 24: {folder}");}
            for item in self.list_dir(&folder).await? {
                let expected_prefix=format!("{folder}/");
                if !item.path.starts_with(&expected_prefix) ||
                   item.path[expected_prefix.len()..].contains('/') ||
                   item.name!=item.path[expected_prefix.len()..] ||
                   item.name.is_empty() || item.name=="." || item.name==".." ||
                   item.name.contains('\\') || item.name.chars().any(|c|c.is_control()) {
                    bail!("Unexpected File Station path outside requested folder");
                }
                if item.name.starts_with('.') || item.name=="@eaDir" || item.name=="#recycle" {continue;}
                if item.isdir {pending.push((item.path,depth+1));}
                else {
                    let size=item.additional.as_ref().and_then(|a|a.size).or(item.size)
                        .context("DSM did not return file size")?;
                    let mtime=item.additional.and_then(|a|a.time).and_then(|t|t.mtime)
                        .context("DSM did not return file modification time")?;
                    output.push(RemoteFile{path:item.path,size,mtime});
                    if output.len()>250_000 {bail!("Too many files in NAS scan");}
                }
            }
        }
        Ok(output)
    }
    pub async fn download_to(&self, remote:&str, target:&Path, max_bytes:u64) -> Result<()> {
        let root_prefix=format!("{}/",self.client.root);
        if !remote.starts_with(&root_prefix) || remote.contains("/../") || remote.contains("/./") {
            bail!("Refusing to download file outside configured Synology folder");
        }
        // Synology's documented download endpoint is a GET returning binary file content.
        // SID in the query is unavoidable here; never log this URL and disable proxy access logs for /webapi.
        let download_params:Vec<(&str,String)>=vec![
            ("api","SYNO.FileStation.Download".into()),("version","2".into()),
            ("method","download".into()),("path",json!([remote]).to_string()),
            ("mode",json!("open").to_string()),("_sid",self.sid.clone())];
        let response=self.client.client.get(self.client.api_url(&self.download.path)?)
            .query(&download_params)
            .send().await.map_err(|e|anyhow!("DSM download transport failure: {:?}",e.status()))?;
        if !response.status().is_success() {
            bail!("DSM download returned HTTP {}",response.status());
        }
        if response.content_length().is_some_and(|n| n>max_bytes) {bail!("NAS file exceeds MAX_FILE_MB");}
        if response.headers().get(reqwest::header::CONTENT_TYPE)
            .and_then(|h|h.to_str().ok()).is_some_and(|s|s.contains("application/json") || s.contains("text/html")) {
            bail!("DSM returned an API response instead of file content");
        }
        let mut file=File::create(target).await?;
        let mut resp=response;
        let mut received=0u64;
        while let Some(chunk)=resp.chunk().await.map_err(|e|anyhow!("DSM download stream failed: {:?}",e.status()))? {
            received+=chunk.len() as u64;
            if received>max_bytes {bail!("NAS download exceeded configured maximum size");}
            file.write_all(&chunk).await?;
        }
        file.sync_all().await?;
        Ok(())
    }
    pub async fn logout(&self) {
        if let Ok(url)=self.client.api_url(&self.auth.path) {
            let fields:Vec<(&str,String)>=vec![
                ("api","SYNO.API.Auth".into()),("version",self.auth_version.to_string()),
                ("method","logout".into()),("session","FileStation".into()),
                ("_sid",self.sid.clone())];
            let _=self.client.client.post(url).form(&fields).send().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn path_is_restricted() {
        let base=Url::parse("https://nas.local:5001/").unwrap();
        let c=SynologyClient { client:Client::new(),base,root:"/share/doc".into(),username:String::new(),password:String::new()};
        assert!(c.api_url("entry.cgi").is_ok());
        assert!(c.api_url("https://evil.test/").is_err());
        assert!(c.api_url("../auth.cgi").is_err());
    }
}

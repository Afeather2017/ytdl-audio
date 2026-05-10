use futures_util::StreamExt;
use regex::Regex;
use reqwest_cookie_store::{CookieStore, CookieStoreMutex};
use reqwest::StatusCode;
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const INNERTUBE_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const VR_USER_AGENT: &str =
    "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
const SAFARI_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)";
const WEB_CLIENT_VERSION: &str = "2.20260114.08.00";
const TV_CLIENT_VERSION: &str = "5.20260114";
const CHUNK_SIZE: u64 = 10 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("node.js error: {0}")]
    NodeJs(String),
    #[error("signature cipher error: {0}")]
    SigCipher(String),
    #[error("{0}")]
    Other(String),
}

impl From<&str> for Error {
    fn from(s: &str) -> Self {
        Error::Other(s.to_string())
    }
}

impl From<Box<dyn std::error::Error>> for Error {
    fn from(e: Box<dyn std::error::Error>) -> Self {
        Error::Other(e.to_string())
    }
}

// ── Public types ──

pub struct Video {
    pub id: String,
    pub title: String,
    pub channel: String,
    pub duration: Option<String>,
    pub views: Option<String>,
    pub publish_time: Option<String>,
}

pub struct DownloadOpts {
    pub itag: String,
    pub output_dir: String,
    pub lang: Option<String>,
    pub cookies: Option<String>,
    pub cookies_from_browser: Option<String>,
}

impl Default for DownloadOpts {
    fn default() -> Self {
        Self {
            itag: "251".into(),
            output_dir: ".".into(),
            lang: None,
            cookies: None,
            cookies_from_browser: None,
        }
    }
}

pub struct DownloadResult {
    pub audio_path: PathBuf,
    pub subtitle_paths: Vec<PathBuf>,
    pub thumbnail_path: Option<PathBuf>,
}

pub struct YoutubeClient {
    client: reqwest::Client,
    #[allow(dead_code)]
    proxy: Option<String>,
}

impl YoutubeClient {
    pub fn new(proxy: Option<&str>) -> Result<Self, Error> {
        let proxy = proxy.map(String::from);
        let mut builder = reqwest::Client::builder()
            .user_agent(BROWSER_USER_AGENT)
            .timeout(std::time::Duration::from_secs(60))
            .cookie_store(true)
            .connect_timeout(std::time::Duration::from_secs(30))
            .http1_only();
        if let Some(ref p) = proxy {
            builder = builder.proxy(reqwest::Proxy::all(p)?);
        }
        Ok(Self {
            client: builder.build()?,
            proxy,
        })
    }


    pub fn search(
        &self,
        query: &str,
        max_results: usize,
    ) -> impl std::future::Future<Output = Result<Vec<Video>, Error>> + Send {
        search_youtube(&self.client, query, max_results)
    }

    pub fn download(
        &self,
        url: &str,
        opts: DownloadOpts,
    ) -> impl std::future::Future<Output = Result<DownloadResult, Error>> + Send {
        run_download(self, url, opts)
    }

    /// Download video thumbnail. Tries maxresdefault first, falls back to hqdefault.
    pub async fn download_thumbnail(
        &self,
        video_id: &str,
        path: &Path,
    ) -> Result<(), Error> {
        let urls = [
            format!(
                "https://i.ytimg.com/vi/{}/maxresdefault.jpg",
                video_id
            ),
            format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", video_id),
        ];

        for url in &urls {
            let resp = self.client.get(url).send().await?;
            if resp.status() != StatusCode::OK {
                continue;
            }
            let bytes = resp.bytes().await?;
            if bytes.len() < 1000 {
                continue;
            }
            std::fs::write(path, &bytes)?;
            return Ok(());
        }
        Err(Error::Other("Failed to download thumbnail".into()))
    }
}

/// Convert audio format and optionally embed cover art using ffmpeg.
///
/// - `input`: path to the source audio file
/// - `output`: desired output path (can be same as input — uses a temp file)
/// - `cover`: optional path to a cover art image (jpg/png)
///
/// If the output container doesn't support the input codec (e.g. Opus in M4A),
/// ffmpeg will re-encode automatically.
///
/// Requires `ffmpeg` to be on PATH.
pub fn convert_audio(input: &Path, output: &Path, cover: Option<&Path>) -> Result<(), Error> {
    // If input == output, ffmpeg can't edit in-place, use a temp file
    let need_temp = input == output;
    let actual_output = if need_temp {
        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        let ext = output.extension().and_then(|e| e.to_str()).unwrap_or("m4a");
        input.with_file_name(format!("{}.tmp.{}", stem, ext))
    } else {
        output.to_path_buf()
    };

    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.arg("-y").arg("-i").arg(input);

    if let Some(cover_path) = cover {
        cmd.arg("-i").arg(cover_path);
        cmd.args([
            "-map", "0:a", "-map", "1:v",
            "-c:a", "copy", "-c:v", "copy",
            "-disposition:v:0", "attached_pic",
        ]);
    } else {
        cmd.args(["-c:a", "copy"]);
    }

    cmd.arg(&actual_output);

    let status = cmd.status()?;
    if !status.success() {
        if need_temp {
            let _ = std::fs::remove_file(&actual_output);
        }
        return Err(Error::Other(format!("ffmpeg exited with {}", status)));
    }

    if need_temp {
        std::fs::rename(&actual_output, output)?;
    }

    Ok(())
}

// ── Internal types ──

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlayerResponse {
    playability_status: Playability,
    streaming_data: Option<StreamingData>,
    video_details: Option<VideoDetails>,
    captions: Option<Captions>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Playability {
    status: String,
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoDetails {
    title: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StreamingData {
    #[serde(default)]
    formats: Vec<AdaptiveFormat>,
    adaptive_formats: Vec<AdaptiveFormat>,
    hls_manifest_url: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveFormat {
    itag: u32,
    mime_type: String,
    bitrate: u64,
    url: Option<String>,
    signature_cipher: Option<String>,
    #[serde(default, deserialize_with = "deserialize_string_or_number")]
    content_length: Option<u64>,
}

fn deserialize_string_or_number<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr + serde::Deserialize<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrNum<T> {
        Str(String),
        Num(T),
    }

    match StringOrNum::<T>::deserialize(deserializer)? {
        StringOrNum::Str(s) => Ok(s.parse().ok()),
        StringOrNum::Num(n) => Ok(Some(n)),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Captions {
    player_captions_tracklist_renderer: Option<CaptionTracklist>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptionTracklist {
    caption_tracks: Vec<CaptionTrack>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptionTrack {
    base_url: String,
    language_code: String,
}

struct AuthContext {
    sapisid: String,
    sapisid_1p: Option<String>,
    sapisid_3p: Option<String>,
    visitor_data: Option<String>,
    ytcfg_visitor_data: Option<String>,
    ytcfg_client_version: Option<String>,
    data_sync_id: Option<String>,
    sts: Option<u32>,
    session_index: Option<u32>,
    delegated_session_id: Option<String>,
    user_session_id: Option<String>,
    logged_in: bool,
}

// ── Helpers ──

fn load_netscape_cookies(path: &str) -> Result<AuthContext, Error> {
    let contents = std::fs::read_to_string(path)?;
    parse_cookie_source(&contents)
}

fn load_cookie_store_from_netscape(path: &str) -> Result<CookieStore, Error> {
    let contents = std::fs::read_to_string(path)?;
    let mut store = CookieStore::default();
    let mut saw_netscape = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }
        saw_netscape = true;

        let domain = fields[0].trim();
        let path = fields[2].trim();
        let secure = fields[3].trim().eq_ignore_ascii_case("TRUE");
        let name = fields[5].trim();
        let value = fields[6].trim();
        if domain.is_empty() || name.is_empty() {
            continue;
        }

        let scheme = if secure { "https" } else { "http" };
        let host = domain.trim_start_matches('.');
        let url = format!("{}://{}{}", scheme, host, path);
        let url = reqwest::Url::parse(&url)
            .map_err(|e| Error::Other(format!("invalid cookie URL {}: {}", url, e)))?;

        let cookie = if secure {
            format!("{}={}; Domain={}; Path={}; Secure", name, value, domain, path)
        } else {
            format!("{}={}; Domain={}; Path={}", name, value, domain, path)
        };

        store
            .parse(&cookie, &url)
            .map_err(|e| Error::Other(format!("failed to parse cookie {}: {:?}", name, e)))?;
    }

    if saw_netscape {
        return Ok(store);
    }

    load_cookie_store_from_header(&contents)
}

fn debug_cookie_header(store: &CookieStore, url: &str, label: &str) {
    if let Ok(url) = reqwest::Url::parse(url) {
        let cookie_header = store
            .get_request_values(&url)
            .map(|(name, value)| format!("{}={}", name, value))
            .collect::<Vec<_>>()
            .join("; ");
        eprintln!("cookie_header[{label}]: {}", cookie_header);
    }
}

fn load_cookie_store_from_header(contents: &str) -> Result<CookieStore, Error> {
    let mut store = CookieStore::default();
    let mut added = 0usize;

    for pair in contents.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            continue;
        }
        let (domain, url) = header_cookie_domain(name);
        let cookie = format!("{}={}; Domain={}; Path=/; Secure", name, value, domain);
        store
            .parse(&cookie, &url)
            .map_err(|e| Error::Other(format!("failed to parse cookie {}: {:?}", name, e)))?;
        added += 1;
    }

    if added == 0 {
        return Err(Error::Other("No cookies found in cookie file".into()));
    }
    Ok(store)
}

fn header_cookie_domain(name: &str) -> (&'static str, reqwest::Url) {
    let google_names = [
        "LOGIN_INFO",
        "HSID",
        "SSID",
        "APISID",
        "SAPISID",
        "SID",
        "SIDCC",
        "__Secure-1PSID",
        "__Secure-3PSID",
        "__Secure-1PAPISID",
        "__Secure-3PAPISID",
        "__Secure-1PSIDTS",
        "__Secure-3PSIDTS",
        "__Secure-1PSIDCC",
        "__Secure-3PSIDCC",
    ];
    if google_names.contains(&name) {
        return (
            ".google.com",
            reqwest::Url::parse("https://accounts.google.com/").expect("static URL"),
        );
    }
    (
        ".youtube.com",
        reqwest::Url::parse("https://www.youtube.com/").expect("static URL"),
    )
}

fn export_browser_cookies(port: u16) -> Result<Vec<CdpCookie>, Error> {
    eprintln!("Extracting cookies from Chrome via CDP on port {}...", port);
    let script = crate_dir().join("js").join("export_cookies_cdp.mjs");
    let output = std::process::Command::new("node")
        .arg(&script)
        .arg("--port")
        .arg(port.to_string())
        .arg("--json-stdout")
        .arg("--no-wait")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Other(format!(
            "failed to export cookies via CDP: {}",
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let cookies: Vec<CdpCookie> =
        serde_json::from_str(&stdout).map_err(|e| Error::Other(format!("invalid CDP cookie JSON: {}", e)))?;
    eprintln!("Extracted {} cookies via CDP", cookies.len());
    Ok(cookies)
}

#[derive(Deserialize)]
struct CdpCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    secure: bool,
    #[serde(rename = "httpOnly")]
    #[allow(dead_code)]
    http_only: bool,
    #[allow(dead_code)]
    expires: f64,
    #[serde(rename = "sameSite")]
    #[allow(dead_code)]
    same_site: Option<String>,
}

fn build_cookie_store_and_auth(cookies: &[CdpCookie]) -> Result<(CookieStore, AuthContext), Error> {
    let mut store = CookieStore::default();
    let mut sapisid = String::new();
    let mut sapisid_1p = None;
    let mut sapisid_3p = None;
    let mut session_index = None;
    let mut delegated_session_id = None;
    let mut user_session_id = None;
    let mut visitor_data = None;
    let mut logged_in = false;

    for c in cookies {
        if c.name.is_empty() || c.domain.is_empty() {
            continue;
        }

        let scheme = if c.secure { "https" } else { "http" };
        let host = c.domain.trim_start_matches('.');
        let url = reqwest::Url::parse(&format!("{}://{}{}", scheme, host, c.path))
            .map_err(|e| Error::Other(format!("invalid cookie URL: {}", e)))?;

        let cookie = if c.secure {
            format!("{}={}; Domain={}; Path={}; Secure", c.name, c.value, c.domain, c.path)
        } else {
            format!("{}={}; Domain={}; Path={}", c.name, c.value, c.domain, c.path)
        };
        store
            .parse(&cookie, &url)
            .map_err(|e| Error::Other(format!("failed to parse cookie {}: {:?}", c.name, e)))?;

        if c.name == "SAPISID" || c.name == "__Secure-3PAPISID" {
            sapisid = c.value.clone();
        }
        if c.name == "__Secure-1PAPISID" && sapisid_1p.is_none() {
            sapisid_1p = Some(c.value.clone());
        }
        if c.name == "__Secure-3PAPISID" && sapisid_3p.is_none() {
            sapisid_3p = Some(c.value.clone());
        }
        if c.name == "LOGIN_INFO" {
            logged_in = true;
        }
        if c.name == "VISITOR_INFO1_LIVE" && visitor_data.is_none() {
            visitor_data = Some(c.value.clone());
        }
        if c.name == "SESSION_INDEX" && session_index.is_none() {
            session_index = c.value.parse::<u32>().ok();
        }
        if c.name == "DELEGATED_SESSION_ID" && delegated_session_id.is_none() {
            delegated_session_id = Some(c.value.clone());
        }
        if c.name == "USER_SESSION_ID" && user_session_id.is_none() {
            user_session_id = Some(c.value.clone());
        }
    }

    let auth = AuthContext {
        sapisid,
        sapisid_1p,
        sapisid_3p,
        visitor_data,
        ytcfg_visitor_data: None,
        ytcfg_client_version: None,
        data_sync_id: None,
        sts: None,
        session_index,
        delegated_session_id,
        user_session_id,
        logged_in,
    };
    Ok((store, auth))
}

fn crate_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("YTDL_AUDIO_CRATE_DIR") {
        let path = PathBuf::from(dir);
        if path.join("js/export_cookies_cdp.mjs").exists() {
            return path;
        }
    }

    if let Ok(dir) = std::env::var("CARGO_MANIFEST_DIR") {
        let path = PathBuf::from(dir);
        if path.join("js/export_cookies_cdp.mjs").exists() {
            return path;
        }
    }

    let mut dir = std::env::current_exe().expect("current exe").canonicalize().ok();
    while let Some(d) = dir.as_ref() {
        if d.join("js/export_cookies_cdp.mjs").exists() {
            return d.to_path_buf();
        }
        dir = d.parent().map(|p| p.to_path_buf());
    }
    PathBuf::from(".")
}

fn parse_cookie_source(contents: &str) -> Result<AuthContext, Error> {
    let mut sapisid = String::new();
    let mut sapisid_1p = None;
    let mut sapisid_3p = None;
    let mut cookies: Vec<(String, String)> = Vec::new();
    let mut session_index = None;
    let mut delegated_session_id = None;
    let mut user_session_id = None;
    let mut visitor_data = None;
    let mut logged_in = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() >= 7 {
            let name = fields[5].trim();
            let value = fields[6].trim();
            if !name.is_empty() {
                if name == "SAPISID" || name == "__Secure-3PAPISID" {
                    sapisid = value.to_string();
                }
                if name == "__Secure-1PAPISID" && sapisid_1p.is_none() {
                    sapisid_1p = Some(value.to_string());
                }
                if name == "__Secure-3PAPISID" && sapisid_3p.is_none() {
                    sapisid_3p = Some(value.to_string());
                }
                if name == "LOGIN_INFO" {
                    logged_in = true;
                }
                if name == "VISITOR_INFO1_LIVE" && visitor_data.is_none() {
                    visitor_data = Some(value.to_string());
                }
                if name == "SESSION_INDEX" && session_index.is_none() {
                    session_index = value.parse::<u32>().ok();
                }
                if name == "DELEGATED_SESSION_ID" && delegated_session_id.is_none() {
                    delegated_session_id = Some(value.to_string());
                }
                if name == "USER_SESSION_ID" && user_session_id.is_none() {
                    user_session_id = Some(value.to_string());
                }
                cookies.push((name.to_string(), value.to_string()));
            }
            continue;
        }

        for pair in line.split(';') {
            let pair = pair.trim();
            if pair.is_empty() {
                continue;
            }
            let Some((name, value)) = pair.split_once('=') else {
                continue;
            };
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() {
                continue;
            }
            if name == "SAPISID" || name == "__Secure-3PAPISID" {
                sapisid = value.to_string();
            }
            if name == "__Secure-1PAPISID" && sapisid_1p.is_none() {
                sapisid_1p = Some(value.to_string());
            }
            if name == "__Secure-3PAPISID" && sapisid_3p.is_none() {
                sapisid_3p = Some(value.to_string());
            }
            if name == "LOGIN_INFO" {
                logged_in = true;
            }
            if name == "VISITOR_INFO1_LIVE" && visitor_data.is_none() {
                visitor_data = Some(value.to_string());
            }
            if name == "SESSION_INDEX" && session_index.is_none() {
                session_index = value.parse::<u32>().ok();
            }
            if name == "DELEGATED_SESSION_ID" && delegated_session_id.is_none() {
                delegated_session_id = Some(value.to_string());
            }
            if name == "USER_SESSION_ID" && user_session_id.is_none() {
                user_session_id = Some(value.to_string());
            }
            cookies.push((name.to_string(), value.to_string()));
        }
    }

    if cookies.is_empty() {
        return Err(Error::Other("No cookies found in cookie file".into()));
    }

    Ok(AuthContext {
        sapisid,
        sapisid_1p,
        sapisid_3p,
        visitor_data,
        ytcfg_visitor_data: None,
        ytcfg_client_version: None,
        data_sync_id: None,
        sts: None,
        session_index,
        delegated_session_id,
        user_session_id,
        logged_in,
    })
}

#[derive(Default)]
struct WatchPageContext {
    visitor_data: Option<String>,
    session_index: Option<u32>,
    delegated_session_id: Option<String>,
    user_session_id: Option<String>,
    logged_in: Option<bool>,
    ytcfg_client_version: Option<String>,
    data_sync_id: Option<String>,
    sts: Option<u32>,
}

#[derive(Default)]
struct ClientConfig {
    client_name: Option<String>,
    client_version: Option<String>,
    user_agent: Option<String>,
    visitor_data: Option<String>,
}

fn extract_ytcfg(page_html: &str) -> Option<String> {
    let re = Regex::new(r#"ytcfg\.set\(\s*(\{.+?\})\s*\)\s*;"#).ok()?;
    re.captures(page_html).map(|c| c[1].to_string())
}

fn parse_ytcfg_context(page_html: &str) -> WatchPageContext {
    let mut ctx = WatchPageContext::default();
    let Some(ytcfg_json) = extract_ytcfg(page_html) else {
        return ctx;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&ytcfg_json) else {
        return ctx;
    };

    ctx.visitor_data = v
        .pointer("/INNERTUBE_CONTEXT/client/visitorData")
        .and_then(|x| x.as_str())
        .map(str::to_string)
        .or_else(|| v.get("VISITOR_DATA").and_then(|x| x.as_str()).map(str::to_string));
    ctx.session_index = v.get("SESSION_INDEX").and_then(|x| x.as_str()).and_then(|s| s.parse().ok());
    ctx.delegated_session_id = v
        .get("DELEGATED_SESSION_ID")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    ctx.user_session_id = v
        .get("USER_SESSION_ID")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    ctx.logged_in = v.get("LOGGED_IN").and_then(|x| x.as_bool());
    ctx.ytcfg_client_version = v
        .get("INNERTUBE_CONTEXT_CLIENT_VERSION")
        .and_then(|x| x.as_str())
        .map(str::to_string);
    ctx.data_sync_id = v.get("DATASYNC_ID").and_then(|x| x.as_str()).map(str::to_string);
    ctx.sts = v
        .get("STS")
        .and_then(|x| x.as_u64())
        .and_then(|n| u32::try_from(n).ok());
    ctx
}

fn parse_client_config(page_html: &str) -> ClientConfig {
    let Some(ytcfg_json) = extract_ytcfg(page_html) else {
        return ClientConfig::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&ytcfg_json) else {
        return ClientConfig::default();
    };
    ClientConfig {
        client_name: v
            .pointer("/INNERTUBE_CONTEXT/client/clientName")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        client_version: v
            .pointer("/INNERTUBE_CONTEXT/client/clientVersion")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .or_else(|| {
                v.get("INNERTUBE_CONTEXT_CLIENT_VERSION")
                    .and_then(|x| x.as_str())
                    .map(str::to_string)
            }),
        user_agent: v
            .pointer("/INNERTUBE_CONTEXT/client/userAgent")
            .and_then(|x| x.as_str())
            .map(str::to_string),
        visitor_data: v
            .pointer("/INNERTUBE_CONTEXT/client/visitorData")
            .and_then(|x| x.as_str())
            .map(str::to_string)
            .or_else(|| v.get("VISITOR_DATA").and_then(|x| x.as_str()).map(str::to_string)),
    }
}

fn parse_data_sync_id(value: &str) -> (Option<String>, Option<String>) {
    let Some((left, right)) = value.split_once("||") else {
        return (None, None);
    };
    let delegated = if right.is_empty() { None } else { Some(left.to_string()) };
    let user = if right.is_empty() {
        if left.is_empty() { None } else { Some(left.to_string()) }
    } else if right.is_empty() {
        None
    } else {
        Some(right.to_string())
    };
    (delegated, user)
}

fn log_player_response_summary(label: &str, text: &str) {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => {
            let status = v
                .pointer("/playabilityStatus/status")
                .and_then(|x| x.as_str())
                .unwrap_or("unknown");
            let reason = v
                .pointer("/playabilityStatus/reason")
                .and_then(|x| x.as_str())
                .unwrap_or("");
            let formats = v
                .pointer("/streamingData/formats")
                .and_then(|x| x.as_array())
                .map(|x| x.len())
                .unwrap_or(0);
            let adaptive_formats = v
                .pointer("/streamingData/adaptiveFormats")
                .and_then(|x| x.as_array())
                .map(|x| x.len())
                .unwrap_or(0);
            let has_hls = v.pointer("/streamingData/hlsManifestUrl").is_some();
            if reason.is_empty() {
                eprintln!(
                    "{} response summary: playability={} formats={} adaptive_formats={} hls={}",
                    label, status, formats, adaptive_formats, has_hls
                );
            } else {
                eprintln!(
                    "{} response summary: playability={} reason={} formats={} adaptive_formats={} hls={}",
                    label, status, reason, formats, adaptive_formats, has_hls
                );
            }
        }
        Err(_) => {
            let preview: String = text.chars().take(400).collect();
            eprintln!("{} response body preview: {}", label, preview);
        }
    }
}

pub fn extract_video_id(url: &str) -> Option<String> {
    let re = Regex::new(
        r"(?:youtube\.com/watch\?v=|youtu\.be/|youtube\.com/embed/|youtube\.com/shorts/|youtube\.com/live/)([0-9A-Za-z_-]{11})",
    )
    .ok()?;
    re.captures(url).map(|c| c[1].to_string())
}

fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim()
        .to_string()
}

fn extension_for_mime(mime: &str) -> &'static str {
    if mime.contains("webm") {
        "webm"
    } else if mime.contains("mp4") || mime.contains("m4a") {
        "m4a"
    } else {
        "bin"
    }
}

async fn fetch_video_page(client: &reqwest::Client, video_id: &str) -> Result<(String, String), Error> {
    let url = format!(
        "https://www.youtube.com/watch?v={}&bpctr=9999999999&has_verified=1",
        video_id
    );
    let re = Regex::new(r#""visitorData"\s*:\s*"([^"]+)""#)?;

    eprintln!("Fetching watch page (attempt 1)...");
    let resp = client
        .get(&url)
        .header("User-Agent", SAFARI_USER_AGENT)
        .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
        .header("Accept-Language", "en-us,en;q=0.5")
        .header("Sec-Fetch-Mode", "navigate")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await?;
    let html = resp.text().await?;
    let visitor_data = re
        .captures(&html)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default();
    eprintln!("Fetched watch page.");
    Ok((visitor_data, html))
}

async fn fetch_player_response(
    client: &reqwest::Client,
    video_id: &str,
    visitor_data: &str,
) -> Result<PlayerResponse, Error> {
    eprintln!("Requesting player API with ANDROID_VR client...");
    let mut ctx = serde_json::json!({
        "clientName": "ANDROID_VR",
        "clientVersion": "1.65.10",
        "deviceMake": "Oculus",
        "deviceModel": "Quest 3",
        "androidSdkVersion": 32,
        "userAgent": VR_USER_AGENT,
        "osName": "Android",
        "osVersion": "12L",
        "hl": "en",
        "gl": "US",
    });
    if !visitor_data.is_empty() {
        ctx["visitorData"] = serde_json::json!(visitor_data);
    }

    let body = serde_json::json!({
        "videoId": video_id,
        "context": { "client": ctx }
    });

    let resp = client
        .post(format!(
            "https://www.youtube.com/youtubei/v1/player?key={}",
            INNERTUBE_KEY
        ))
        .json(&body)
        .send()
        .await?;

    if resp.status() != StatusCode::OK {
        return Err(Error::Other(format!("API returned status {}", resp.status())));
    }

    Ok(resp.json().await?)
}

fn pick_audio_format<'a>(formats: &'a [AdaptiveFormat], preferred_itag: &str) -> Option<&'a AdaptiveFormat> {
    let target: u32 = preferred_itag.parse().unwrap_or(251);
    if let Some(fmt) = formats
        .iter()
        .find(|f| f.itag == target && f.mime_type.contains("audio") && (f.url.is_some() || f.signature_cipher.is_some()))
    {
        return Some(fmt);
    }
    formats
        .iter()
        .filter(|f| f.mime_type.contains("audio") && (f.url.is_some() || f.signature_cipher.is_some()))
        .max_by_key(|f| f.bitrate)
}

fn parse_signature_cipher(cipher: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(cipher.as_bytes())
        .into_owned()
        .collect()
}

fn extract_player_js_url(page_html: &str) -> Option<String> {
    let re = Regex::new(r#""jsUrl":"([^"]+)"|<script\s+src="([^"]*player[^"]+base\.js[^"]*)""#).ok()?;
    let caps = re.captures(page_html)?;
    let path = caps.get(1).or_else(|| caps.get(2))?.as_str().replace("\\/", "/");
    if path.starts_with("http") {
        Some(path)
    } else if path.starts_with("//") {
        Some(format!("https:{}", path))
    } else {
        Some(format!("https://www.youtube.com{}", path))
    }
}

async fn fetch_player_js(client: &reqwest::Client, url: &str) -> Result<String, Error> {
    Ok(client.get(url).send().await?.text().await?)
}

async fn fetch_client_config_page(
    client: &reqwest::Client,
    url: &str,
    user_agent: &str,
) -> Result<String, Error> {
    eprintln!("Fetching client config page: {}", url);
    Ok(client
        .get(url)
        .header("User-Agent", user_agent)
        .send()
        .await?
        .text()
        .await?)
}

fn run_sig_solver(player_js: &str, sig: Option<&str>, n: Option<&str>) -> Result<(Option<String>, Option<String>), Error> {
    let mut requests = Vec::new();
    if let Some(sig) = sig {
        requests.push(serde_json::json!({
            "type": "sig",
            "challenges": [sig],
        }));
    }
    if let Some(n) = n {
        requests.push(serde_json::json!({
            "type": "n",
            "challenges": [n],
        }));
    }
    if requests.is_empty() {
        return Ok((None, None));
    }

    let solver_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("js/solver.mjs");
    let input = serde_json::json!({
        "type": "player",
        "player": player_js,
        "requests": requests,
        "output_preprocessed": false,
    });
    let mut cmd = std::process::Command::new("node");
    cmd.arg(solver_path);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn()?;
    {
        let stdin = child.stdin.as_mut().ok_or_else(|| Error::NodeJs("failed to open node stdin".into()))?;
        stdin.write_all(input.to_string().as_bytes())?;
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        return Err(Error::NodeJs(String::from_utf8_lossy(&output.stderr).trim().to_string()));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    if value.get("type").and_then(|v| v.as_str()) == Some("error") {
        return Err(Error::SigCipher(
            value.get("error").and_then(|v| v.as_str()).unwrap_or("unknown solver error").to_string(),
        ));
    }
    let mut solved_sig = None;
    let mut solved_n = None;
    if let Some(responses) = value.get("responses").and_then(|v| v.as_array()) {
        for response in responses {
            if response.get("type").and_then(|v| v.as_str()) == Some("error") {
                return Err(Error::SigCipher(
                    response
                        .get("error")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown solver error")
                        .to_string(),
                ));
            }
            let Some(data) = response.get("data").and_then(|v| v.as_object()) else {
                continue;
            };
            if let Some(sig_input) = sig {
                if let Some(v) = data.get(sig_input).and_then(|v| v.as_str()) {
                    solved_sig = Some(v.to_string());
                }
            }
            if let Some(n_input) = n {
                if let Some(v) = data.get(n_input).and_then(|v| v.as_str()) {
                    solved_n = Some(v.to_string());
                }
            }
        }
    }
    Ok((solved_sig, solved_n))
}

fn resolve_format_url(
    fmt: &AdaptiveFormat,
    player_js: Option<&str>,
) -> Result<Option<String>, Error> {
    if let Some(url) = &fmt.url {
        if let Some((base, query)) = url.split_once('?') {
            let mut params: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
            if let (Some(n), Some(js)) = (params.get("n"), player_js) {
                let (_, solved_n) = run_sig_solver(js, None, Some(n.as_str()))?;
                if let Some(solved_n) = solved_n {
                    params.insert("n".into(), solved_n);
                    let rebuilt = format!(
                        "{}?{}",
                        base,
                        url::form_urlencoded::Serializer::new(String::new())
                            .extend_pairs(params.iter())
                            .finish()
                    );
                    return Ok(Some(rebuilt));
                }
            }
        }
        return Ok(Some(url.clone()));
    }

    let Some(cipher) = fmt.signature_cipher.as_deref() else {
        return Ok(None);
    };
    let Some(js) = player_js else {
        return Err(Error::SigCipher("ciphered format requires player JS".into()));
    };

    let mut params = parse_signature_cipher(cipher);
    let mut url = params
        .remove("url")
        .ok_or_else(|| Error::SigCipher("cipher missing url".into()))?;
    let sig_param = params.remove("sp").unwrap_or_else(|| "signature".into());
    let sig = params.get("s").map(String::as_str);
    let n = if let Some((_, query)) = url.split_once('?') {
        url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .find_map(|(k, v)| if k == "n" { Some(v) } else { None })
    } else {
        None
    };
    let (solved_sig, solved_n) = run_sig_solver(js, sig, n.as_deref())?;
    if let Some(sig) = solved_sig {
        let sep = if url.contains('?') { "&" } else { "?" };
        url.push_str(sep);
        url.push_str(&format!("{}={}", sig_param, urlencoding::encode(&sig)));
    }
    if let Some(solved_n) = solved_n {
        if let Some((base, query)) = url.split_once('?') {
            let mut q: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
            q.insert("n".into(), solved_n);
            url = format!(
                "{}?{}",
                base,
                url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(q.iter())
                    .finish()
            );
        }
    }
    Ok(Some(url))
}

fn solve_n_in_url(url: &str, player_js: Option<&str>) -> Result<String, Error> {
    let Some((base, query)) = url.split_once('?') else {
        return Ok(url.to_string());
    };
    let mut params: HashMap<String, String> =
        url::form_urlencoded::parse(query.as_bytes()).into_owned().collect();
    if let (Some(n), Some(js)) = (params.get("n"), player_js) {
        let (_, solved_n) = run_sig_solver(js, None, Some(n.as_str()))?;
        if let Some(solved_n) = solved_n {
            params.insert("n".into(), solved_n);
            return Ok(format!(
                "{}?{}",
                base,
                url::form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(params.iter())
                    .finish()
            ));
        }
    }
    Ok(url.to_string())
}

async fn fetch_hls_audio_stream(
    client: &reqwest::Client,
    manifest_url: &str,
    player_js: Option<&str>,
) -> Result<Option<String>, Error> {
    let manifest_url = solve_n_in_url(manifest_url, player_js)?;
    eprintln!("Fetching HLS manifest...");
    let text = client.get(&manifest_url).send().await?.text().await?;

    let mut last_stream_inf: Option<String> = None;
    let mut best_audio_url: Option<String> = None;
    let mut best_bandwidth = 0u64;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("#EXT-X-STREAM-INF:") {
            last_stream_inf = Some(line.to_string());
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(info) = last_stream_inf.take() else {
            continue;
        };
        if !info.contains("AUDIO=") {
            continue;
        }
        let bandwidth = Regex::new(r#"BANDWIDTH=(\d+)"#)
            .ok()
            .and_then(|re| re.captures(&info))
            .and_then(|caps| caps.get(1))
            .and_then(|m| m.as_str().parse::<u64>().ok())
            .unwrap_or(0);
        if bandwidth >= best_bandwidth {
            best_bandwidth = bandwidth;
            best_audio_url = Some(if line.starts_with("http://") || line.starts_with("https://") {
                line.to_string()
            } else {
                reqwest::Url::parse(&manifest_url)
                    .and_then(|base| base.join(line))
                    .map(|u| u.to_string())
                    .map_err(|e| Error::Other(format!("invalid HLS URL {}: {}", line, e)))?
            });
        }
    }

    Ok(best_audio_url)
}

async fn download_stream_file(
    client: &reqwest::Client,
    playlist_url: &str,
    output_path: &Path,
    player_js: Option<&str>,
) -> Result<(), Error> {
    let playlist_url = solve_n_in_url(playlist_url, player_js)?;
    eprintln!("Fetching media playlist...");
    let text = client.get(&playlist_url).send().await?.text().await?;
    let base_url =
        reqwest::Url::parse(&playlist_url).map_err(|e| Error::Other(format!("invalid playlist URL: {}", e)))?;
    let mut file = std::fs::File::create(output_path)?;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let segment_url = if line.starts_with("http://") || line.starts_with("https://") {
            line.to_string()
        } else {
            base_url
                .join(line)
                .map(|u| u.to_string())
                .map_err(|e| Error::Other(format!("invalid segment URL {}: {}", line, e)))?
        };
        let segment_url = solve_n_in_url(&segment_url, player_js)?;
        let bytes = client.get(&segment_url).send().await?.bytes().await?;
        file.write_all(&bytes)?;
    }

    Ok(())
}

fn make_sapisidhash(scheme: &str, sid: &str, origin: &str, user_session_id: Option<&str>) -> String {
    use sha1::{Digest, Sha1};
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let input = if let Some(user_session_id) = user_session_id {
        format!("{} {} {} {}", user_session_id, timestamp, sid, origin)
    } else {
        format!("{} {} {}", timestamp, sid, origin)
    };
    let digest = Sha1::digest(input.as_bytes());
    let hex = digest.iter().map(|b| format!("{:02x}", b)).collect::<String>();
    if user_session_id.is_some() {
        format!("{scheme} {}_{}_u", timestamp, hex)
    } else {
        format!("{scheme} {}_{}", timestamp, hex)
    }
}

fn auth_header_value(auth: &AuthContext, origin: &str) -> Option<String> {
    if auth.sapisid.is_empty() {
        return None;
    }
    let mut parts = vec![make_sapisidhash(
        "SAPISIDHASH",
        &auth.sapisid,
        origin,
        auth.user_session_id.as_deref(),
    )];
    if auth.sapisid_1p.is_some() {
        parts.push(make_sapisidhash(
            "SAPISID1PHASH",
            auth.sapisid_1p.as_deref().unwrap_or(""),
            origin,
            auth.user_session_id.as_deref(),
        ));
    }
    if auth.sapisid_3p.is_some() {
        parts.push(make_sapisidhash(
            "SAPISID3PHASH",
            auth.sapisid_3p.as_deref().unwrap_or(""),
            origin,
            auth.user_session_id.as_deref(),
        ));
    }
    Some(parts.join(" "))
}

fn maybe_add_login_header(headers: &mut reqwest::header::HeaderMap, logged_in: bool) {
    if logged_in {
        headers.insert(
            "X-Youtube-Bootstrap-Logged-In",
            reqwest::header::HeaderValue::from_static("true"),
        );
    }
}

fn maybe_add_cookie_auth_headers(headers: &mut reqwest::header::HeaderMap, auth: &AuthContext) -> Result<(), Error> {
    if let Some(page_id) = auth.delegated_session_id.as_deref() {
        headers.insert(
            "X-Goog-PageId",
            reqwest::header::HeaderValue::from_str(page_id)
                .map_err(|e| Error::Other(format!("invalid page id header: {}", e)))?,
        );
    }
    if auth.delegated_session_id.is_some() || auth.session_index.is_some() {
        let auth_user = auth.session_index.unwrap_or(0).to_string();
        headers.insert(
            "X-Goog-AuthUser",
            reqwest::header::HeaderValue::from_str(&auth_user)
                .map_err(|e| Error::Other(format!("invalid authuser header: {}", e)))?,
        );
    }
    Ok(())
}

async fn fetch_player_response_tv(
    client: &reqwest::Client,
    video_id: &str,
    auth: &AuthContext,
    cfg: &ClientConfig,
) -> Result<PlayerResponse, Error> {
    eprintln!("Requesting player API with tv_downgraded client...");
    let client_name = cfg.client_name.as_deref().unwrap_or("TVHTML5");
    let client_version = cfg.client_version.as_deref().unwrap_or(TV_CLIENT_VERSION);
    let user_agent = cfg
        .user_agent
        .as_deref()
        .unwrap_or("Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version");
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "User-Agent",
        reqwest::header::HeaderValue::from_str(user_agent)
            .map_err(|e| Error::Other(format!("invalid user-agent header: {}", e)))?,
    );
    headers.insert("Accept", reqwest::header::HeaderValue::from_static("text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"));
    headers.insert("Accept-Language", reqwest::header::HeaderValue::from_static("en-us,en;q=0.5"));
    headers.insert("Connection", reqwest::header::HeaderValue::from_static("keep-alive"));
    headers.insert("Content-Type", reqwest::header::HeaderValue::from_static("application/json"));
    headers.insert("Origin", reqwest::header::HeaderValue::from_static("https://www.youtube.com"));
    headers.insert("X-Origin", reqwest::header::HeaderValue::from_static("https://www.youtube.com"));
    headers.insert("Sec-Fetch-Mode", reqwest::header::HeaderValue::from_static("navigate"));
    headers.insert("X-YouTube-Client-Name", reqwest::header::HeaderValue::from_static("7"));
    headers.insert(
        "X-YouTube-Client-Version",
        reqwest::header::HeaderValue::from_str(client_version).map_err(|e| Error::Other(format!("invalid client version header: {}", e)))?,
    );
    if let Some(visitor) = auth.ytcfg_visitor_data.as_deref().or(auth.visitor_data.as_deref()) {
        headers.insert(
            "X-Goog-Visitor-Id",
            reqwest::header::HeaderValue::from_str(visitor).map_err(|e| Error::Other(format!("invalid visitor header: {}", e)))?,
        );
    }
    if let Some(session_index) = auth.session_index {
        headers.insert(
            "X-Goog-AuthUser",
            reqwest::header::HeaderValue::from_str(&session_index.to_string())
                .map_err(|e| Error::Other(format!("invalid authuser header: {}", e)))?,
        );
    }
    if let Some(authz) = auth_header_value(auth, "https://www.youtube.com") {
        headers.insert(
            "Authorization",
            reqwest::header::HeaderValue::from_str(&authz).map_err(|e| Error::Other(format!("invalid authorization header: {}", e)))?,
        );
    }
    maybe_add_cookie_auth_headers(&mut headers, auth)?;
    maybe_add_login_header(&mut headers, auth.logged_in);

    let mut client_ctx = serde_json::json!({
        "clientName": client_name,
        "clientVersion": client_version,
        "userAgent": user_agent,
        "hl": "en",
        "timeZone": "UTC",
        "utcOffsetMinutes": 0,
    });
    if let Some(visitor) = cfg
        .visitor_data
        .as_deref()
        .or(auth.ytcfg_visitor_data.as_deref())
        .or(auth.visitor_data.as_deref())
    {
        client_ctx["visitorData"] = serde_json::json!(visitor);
    }

    let mut body = serde_json::json!({
        "videoId": video_id,
        "context": { "client": client_ctx },
        "contentCheckOk": true,
        "racyCheckOk": true,
        "playbackContext": {
            "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS"
            }
        }
    });
    if let Some(sts) = auth.sts {
        body["playbackContext"]["contentPlaybackContext"]["signatureTimestamp"] = serde_json::json!(sts);
    }

    let resp = client
        .post(format!(
            "https://www.youtube.com/youtubei/v1/player?key={}&prettyPrint=false",
            INNERTUBE_KEY
        ))
        .headers(headers)
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    eprintln!("TV response status: {}", status);
    log_player_response_summary("TV", &text);
    Ok(serde_json::from_slice(&bytes)?)
}

async fn fetch_player_response_web_safari(
    client: &reqwest::Client,
    video_id: &str,
    auth: &AuthContext,
) -> Result<PlayerResponse, Error> {
    eprintln!("Requesting player API with web_safari client...");
    let mut ctx = serde_json::json!({
        "clientName": "WEB",
        "clientVersion": WEB_CLIENT_VERSION,
        "userAgent": "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)",
    });
    if let Some(visitor) = auth.ytcfg_visitor_data.as_deref().or(auth.visitor_data.as_deref()) {
        ctx["visitorData"] = serde_json::json!(visitor);
    }

    let mut req = client
        .post(format!("https://www.youtube.com/youtubei/v1/player?key={}", INNERTUBE_KEY))
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/15.5 Safari/605.1.15,gzip(gfe)")
        .header("Origin", "https://www.youtube.com")
        .header("X-Origin", "https://www.youtube.com")
        .header("X-YouTube-Client-Name", "1")
        .header("X-YouTube-Client-Version", WEB_CLIENT_VERSION)
        .json(&serde_json::json!({
            "videoId": video_id,
            "context": { "client": ctx },
            "contentCheckOk": true,
            "racyCheckOk": true
        }));
    if let Some(authz) = auth_header_value(auth, "https://www.youtube.com") {
        req = req.header("Authorization", authz);
    }
    if let Some(page_id) = auth.delegated_session_id.as_deref() {
        req = req.header("X-Goog-PageId", page_id);
    }
    if auth.delegated_session_id.is_some() || auth.session_index.is_some() {
        req = req.header("X-Goog-AuthUser", auth.session_index.unwrap_or(0).to_string());
    }
    if auth.logged_in {
        req = req.header("X-Youtube-Bootstrap-Logged-In", "true");
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    eprintln!("web_safari response status: {}", status);
    log_player_response_summary("web_safari", &text);
    Ok(serde_json::from_str(&text)?)
}

async fn fetch_player_response_web(
    client: &reqwest::Client,
    video_id: &str,
    auth: &AuthContext,
) -> Result<PlayerResponse, Error> {
    eprintln!("Requesting player API with WEB client...");
    let mut ctx = serde_json::json!({
        "clientName": "WEB",
        "clientVersion": WEB_CLIENT_VERSION,
        "hl": "en",
        "gl": "US",
        "userAgent": BROWSER_USER_AGENT,
        "osName": "Windows",
        "osVersion": "10.0",
        "browserName": "Chrome",
        "browserVersion": "131.0.0.0",
    });
    if let Some(visitor) = auth.ytcfg_visitor_data.as_deref().or(auth.visitor_data.as_deref()) {
        ctx["visitorData"] = serde_json::json!(visitor);
    }

    let mut req = client
        .post(format!("https://www.youtube.com/youtubei/v1/player?key={}", INNERTUBE_KEY))
        .header("User-Agent", BROWSER_USER_AGENT)
        .header("Origin", "https://www.youtube.com")
        .header("X-Origin", "https://www.youtube.com")
        .header("X-YouTube-Client-Name", "1")
        .header("X-YouTube-Client-Version", WEB_CLIENT_VERSION)
        .json(&serde_json::json!({
            "videoId": video_id,
            "context": { "client": ctx },
            "contentCheckOk": true,
            "racyCheckOk": true
        }));
    if let Some(authz) = auth_header_value(auth, "https://www.youtube.com") {
        req = req.header("Authorization", authz);
    }
    if let Some(page_id) = auth.delegated_session_id.as_deref() {
        req = req.header("X-Goog-PageId", page_id);
    }
    if auth.delegated_session_id.is_some() || auth.session_index.is_some() {
        req = req.header("X-Goog-AuthUser", auth.session_index.unwrap_or(0).to_string());
    }
    if auth.logged_in {
        req = req.header("X-Youtube-Bootstrap-Logged-In", "true");
    }
    let resp = req.send().await?;
    let status = resp.status();
    let text = resp.text().await?;
    eprintln!("WEB response status: {}", status);
    log_player_response_summary("WEB", &text);
    Ok(serde_json::from_str(&text)?)
}

async fn download_file(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    total_size: u64,
) -> Result<(), Error> {
    let existing = if path.exists() {
        let meta = std::fs::metadata(path)?;
        if meta.len() <= total_size {
            meta.len()
        } else {
            0
        }
    } else {
        0
    };

    let mut downloaded = existing;
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path)?;

    while downloaded < total_size {
        let range_start = downloaded;
        let range_end = std::cmp::min(downloaded + CHUNK_SIZE - 1, total_size - 1);

        let mut retry = 0;
        loop {
            let resp = client
                .get(url)
                .header("Range", format!("bytes={}-{}", range_start, range_end))
                .timeout(std::time::Duration::from_secs(300))
                .send()
                .await;

            let resp = match resp {
                Ok(r) => r,
                Err(e) => {
                    retry += 1;
                    if retry > 15 {
                        return Err(Error::Other(format!("connection failed after 15 retries: {}", e)));
                    }
                    tokio::time::sleep(std::time::Duration::from_secs_f64(2.0 * retry as f64)).await;
                    continue;
                }
            };

            match resp.status() {
                StatusCode::OK | StatusCode::PARTIAL_CONTENT => {
                    let expected_len = range_end - range_start + 1;
                    let mut chunk_received: u64 = 0;
                    let mut stream = resp.bytes_stream();
                    let mut failed = false;
                    while let Some(result) = stream.next().await {
                        match result {
                            Ok(data) => {
                                file.write_all(&data)?;
                                chunk_received += data.len() as u64;
                                downloaded += data.len() as u64;
                            }
                            Err(_) => {
                                failed = true;
                                break;
                            }
                        }
                    }

                    if failed || chunk_received != expected_len {
                        file.set_len(range_start)?;
                        downloaded = range_start;
                        retry += 1;
                        if retry > 15 {
                            return Err(Error::Other(format!(
                                "chunk failed after 15 retries ({}/{})",
                                chunk_received, expected_len
                            )));
                        }
                        tokio::time::sleep(std::time::Duration::from_secs_f64(2.0 * retry as f64))
                            .await;
                        continue;
                    }
                    break;
                }
                status => {
                    return Err(Error::Other(format!("download failed: {}", status)));
                }
            }
        }
    }

    Ok(())
}

async fn fetch_subtitle(client: &reqwest::Client, url: &str) -> Result<String, Error> {
    let resp = client.get(url).send().await?;
    let text = resp.text().await?;

    let mut srt = String::new();
    let mut idx = 1u32;
    let re = Regex::new(r#"<p\s+t="(\d+)"(?:\s+d="(\d+)")?[^>]*>([^<]*)</p>"#)?;

    for cap in re.captures_iter(&text) {
        let start_ms: u64 = cap[1].parse().unwrap_or(0);
        let dur_ms: u64 = cap
            .get(2)
            .and_then(|d| d.as_str().parse().ok())
            .unwrap_or(3000);
        let content = cap[3]
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace('\n', " ");

        let end_ms = start_ms + dur_ms;
        srt.push_str(&format!(
            "{}\n{:02}:{:02}:{:02},{:03} --> {:02}:{:02}:{:02},{:03}\n{}\n\n",
            idx,
            start_ms / 3600000,
            (start_ms % 3600000) / 60000,
            (start_ms % 60000) / 1000,
            start_ms % 1000,
            end_ms / 3600000,
            (end_ms % 3600000) / 60000,
            (end_ms % 60000) / 1000,
            end_ms % 1000,
            content.trim()
        ));
        idx += 1;
    }
    Ok(srt)
}

// ── Core operations ──

async fn search_youtube(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<Video>, Error> {
    let encoded = urlencoding::encode(query);
    let url = format!("https://www.youtube.com/results?search_query={}", encoded);

    let mut html = String::new();
    for attempt in 0..3 {
        let resp = client
            .get(&url)
            .header("User-Agent", BROWSER_USER_AGENT)
            .header("Accept-Language", "en-US,en;q=0.9")
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await;

        match resp {
            Ok(r) if r.status() == StatusCode::OK => {
                html = r.text().await?;
                if html.contains("ytInitialData") {
                    break;
                }
            }
            _ => {}
        }

        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt as u64 + 1))).await;
        }
    }

    if !html.contains("ytInitialData") {
        return Err(Error::Other("Failed to get search results after retries".into()));
    }

    let start = html.find("ytInitialData").unwrap() + "ytInitialData".len();
    let data_start = html[start..]
        .find('=')
        .ok_or("parse error: no = after ytInitialData")?
        + start
        + 1;
    let data_start = html[data_start..]
        .find(|c: char| !c.is_whitespace())
        .map(|i| data_start + i)
        .unwrap_or(data_start);
    let data_end = html[data_start..]
        .find(';')
        .map(|i| data_start + i)
        .ok_or("parse error: no ; to close ytInitialData")?;

    let data: serde_json::Value = serde_json::from_str(&html[data_start..data_end])?;
    let mut results = Vec::new();

    if let Some(sections) = data
        .pointer("/contents/twoColumnSearchResultsRenderer/primaryContents/sectionListRenderer/contents")
        .and_then(|v| v.as_array())
    {
        for section in sections {
            let items = match section
                .pointer("/itemSectionRenderer/contents")
                .and_then(|v| v.as_array())
            {
                Some(i) => i,
                None => continue,
            };

            for item in items {
                let vr = match item.get("videoRenderer") {
                    Some(v) => v,
                    None => continue,
                };

                let id = vr
                    .get("videoId")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if id.is_empty() {
                    continue;
                }

                results.push(Video {
                    id,
                    title: vr
                        .pointer("/title/runs/0/text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("(no title)")
                        .into(),
                    channel: vr
                        .pointer("/longBylineText/runs/0/text")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .into(),
                    duration: vr.pointer("/lengthText/simpleText").and_then(|v| v.as_str()).map(Into::into),
                    views: vr.pointer("/viewCountText/simpleText").and_then(|v| v.as_str()).map(Into::into),
                    publish_time: vr
                        .pointer("/publishedTimeText/simpleText")
                        .and_then(|v| v.as_str())
                        .map(Into::into),
                });

                if results.len() >= max_results {
                    return Ok(results);
                }
            }

            if !results.is_empty() {
                break;
            }
        }
    }

    Ok(results)
}

async fn run_download(
    yc: &YoutubeClient,
    url: &str,
    opts: DownloadOpts,
) -> Result<DownloadResult, Error> {
    let cdp_cookies = if let Some(port_str) = opts.cookies_from_browser.as_deref() {
        let port: u16 = port_str.parse().map_err(|_| {
            Error::Other(format!("--cookies-from-browser expects a CDP port number, got: {}", port_str))
        })?;
        Some(export_browser_cookies(port)?)
    } else {
        None
    };

    let video_id = extract_video_id(url).ok_or("Could not extract video ID from URL")?;

    let (visitor_data, page_html) = fetch_video_page(&yc.client, &video_id).await?;
    let pr = fetch_player_response(&yc.client, &video_id, &visitor_data).await?;

    let (pr, client, page_html) = if pr.playability_status.status != "OK" {
        let reason = pr.playability_status.reason.as_deref().unwrap_or("unknown");
        eprintln!("ANDROID_VR failed: {} — trying tv_downgraded client with cookies...", reason);

        let (cookie_store, mut auth) = if let Some(ref cookies) = cdp_cookies {
            build_cookie_store_and_auth(cookies)?
        } else if let Some(ref path) = opts.cookies {
            let auth = load_netscape_cookies(path)?;
            let store = load_cookie_store_from_netscape(path)?;
            (store, auth)
        } else {
            return Err(Error::Other(format!(
                "Video not playable ({}). Provide cookies with --cookies or --cookies-from-browser to use fallback clients.",
                reason
            )));
        };

        debug_cookie_header(
            &cookie_store,
            &format!("https://www.youtube.com/watch?v={}&bpctr=9999999999&has_verified=1", video_id),
            "watch_page",
        );
        debug_cookie_header(
            &cookie_store,
            "https://www.youtube.com/tv",
            "youtube_tv",
        );
        debug_cookie_header(
            &cookie_store,
            "https://www.youtube.com/youtubei/v1/player?key=stub",
            "player_api",
        );

        let cookie_client = {
            let mut builder = reqwest::Client::builder()
                .user_agent(BROWSER_USER_AGENT)
                .timeout(std::time::Duration::from_secs(60))
                .connect_timeout(std::time::Duration::from_secs(30))
                .cookie_provider(Arc::new(CookieStoreMutex::new(cookie_store)))
                .http1_only();
            if let Some(ref p) = yc.proxy {
                builder = builder.proxy(reqwest::Proxy::all(p)?);
            }
            builder.build()?
        };
        let (_, web_page_html) = fetch_video_page(&cookie_client, &video_id).await?;
        let page_ctx = parse_ytcfg_context(&web_page_html);
        eprintln!(
            "page_ctx: visitor={:?} session_index={:?} delegated={:?} user={:?} logged_in={:?} client_version={:?} data_sync_id={:?} sts={:?}",
            page_ctx.visitor_data,
            page_ctx.session_index,
            page_ctx.delegated_session_id,
            page_ctx.user_session_id,
            page_ctx.logged_in,
            page_ctx.ytcfg_client_version,
            page_ctx.data_sync_id,
            page_ctx.sts
        );
        if page_ctx.visitor_data.is_some() {
            auth.ytcfg_visitor_data = page_ctx.visitor_data;
        }
        if page_ctx.session_index.is_some() {
            auth.session_index = page_ctx.session_index;
        }
        if page_ctx.delegated_session_id.is_some() {
            auth.delegated_session_id = page_ctx.delegated_session_id;
        }
        if page_ctx.user_session_id.is_some() {
            auth.user_session_id = page_ctx.user_session_id;
        }
        if page_ctx.logged_in.is_some() {
            auth.logged_in = page_ctx.logged_in.unwrap_or(auth.logged_in);
        }
        if page_ctx.ytcfg_client_version.is_some() {
            auth.ytcfg_client_version = page_ctx.ytcfg_client_version;
        }
        if page_ctx.data_sync_id.is_some() {
            auth.data_sync_id = page_ctx.data_sync_id.clone();
            let (delegated, user) = parse_data_sync_id(page_ctx.data_sync_id.as_deref().unwrap_or_default());
            if auth.delegated_session_id.is_none() {
                auth.delegated_session_id = delegated;
            }
            if auth.user_session_id.is_none() {
                auth.user_session_id = user;
            }
        }
        if let Some(player_url) = extract_player_js_url(&web_page_html) {
            if let Ok(player_js) = fetch_player_js(&cookie_client, &player_url).await {
                if let Ok(re) = Regex::new(r#"(?:signatureTimestamp|sts)\s*:\s*(\d{5})"#) {
                    auth.sts = re
                        .captures(&player_js)
                        .and_then(|c| c.get(1))
                        .and_then(|m| m.as_str().parse::<u32>().ok())
                        .or(page_ctx.sts);
                }
            } else {
                auth.sts = page_ctx.sts;
            }
        } else {
            auth.sts = page_ctx.sts;
        }
        eprintln!(
            "auth_ctx: visitor={:?} session_index={:?} delegated={:?} user={:?} logged_in={} client_version={:?} data_sync_id={:?} sts={:?} sapisid_present={}",
            auth.ytcfg_visitor_data.as_deref().or(auth.visitor_data.as_deref()),
            auth.session_index,
            auth.delegated_session_id,
            auth.user_session_id,
            auth.logged_in,
            auth.ytcfg_client_version,
            auth.data_sync_id,
            auth.sts,
            !auth.sapisid.is_empty()
        );
        let tv_cfg_html = fetch_client_config_page(
            &cookie_client,
            "https://www.youtube.com/tv",
            "Mozilla/5.0 (ChromiumStylePlatform) Cobalt/Version",
        )
        .await
        .unwrap_or_default();
        let tv_cfg = parse_client_config(&tv_cfg_html);
        let tv_pr = fetch_player_response_tv(&cookie_client, &video_id, &auth, &tv_cfg).await?;
        if tv_pr.playability_status.status == "OK" {
            (tv_pr, cookie_client, web_page_html)
        } else {
            let tv_reason = tv_pr.playability_status.reason.as_deref().unwrap_or("unknown");
            eprintln!("tv_downgraded failed: {} — trying WEB client...", tv_reason);
            let web_pr = fetch_player_response_web(&cookie_client, &video_id, &auth).await?;
            if web_pr.playability_status.status == "OK" {
                (web_pr, cookie_client, web_page_html)
            } else {
                let web_reason = web_pr.playability_status.reason.as_deref().unwrap_or("unknown");
                eprintln!("WEB failed: {} — trying web_safari client...", web_reason);
                let safari_pr = fetch_player_response_web_safari(&cookie_client, &video_id, &auth).await?;
                if safari_pr.playability_status.status == "OK" {
                    (safari_pr, cookie_client, web_page_html)
                } else {
                    let safari_reason = safari_pr.playability_status.reason.unwrap_or_else(|| "unknown".into());
                    return Err(Error::Other(format!(
                        "Video not playable (tv_downgraded: {}; WEB: {}; web_safari: {})",
                        tv_reason, web_reason, safari_reason
                    )));
                }
            }
        }
    } else {
        (pr, yc.client.clone(), page_html)
    };

    let title = pr
        .video_details
        .as_ref()
        .and_then(|v| v.title.as_deref())
        .unwrap_or(&video_id);
    let safe_title = sanitize_filename(title);

    let sd = pr
        .streaming_data
        .as_ref()
        .ok_or("No streaming data available")?;

    let mut all_formats = sd.adaptive_formats.clone();
    all_formats.extend(sd.formats.clone());
    let fmt = pick_audio_format(&all_formats, &opts.itag);

    let needs_player_js = sd.hls_manifest_url.is_some()
        || fmt
            .as_ref()
            .map(|f| {
                f.signature_cipher.is_some()
                    || f.url.as_deref().map(|u| u.contains("n=")).unwrap_or(false)
            })
            .unwrap_or(false);
    let player_js = if needs_player_js {
        if let Some(player_url) = extract_player_js_url(&page_html) {
            Some(fetch_player_js(&client, &player_url).await?)
        } else {
            None
        }
    } else {
        None
    };

    let audio_path = if let Some(manifest_url) = sd.hls_manifest_url.as_deref() {
        if let Some(media_playlist_url) =
            fetch_hls_audio_stream(&client, manifest_url, player_js.as_deref()).await?
        {
            let audio_path = PathBuf::from(&opts.output_dir).join(format!("{}.m4a", safe_title));
            download_stream_file(&client, &media_playlist_url, &audio_path, player_js.as_deref()).await?;
            audio_path
        } else {
            let fmt = fmt.ok_or("No suitable audio format found")?;
            let ext = extension_for_mime(&fmt.mime_type);
            let audio_path = PathBuf::from(&opts.output_dir).join(format!("{}.{}", safe_title, ext));
            let audio_url = resolve_format_url(fmt, player_js.as_deref())?.ok_or("Format has no direct URL")?;
            let content_length = fmt.content_length.ok_or("Unknown content length")?;
            download_file(&client, &audio_url, &audio_path, content_length).await?;
            audio_path
        }
    } else {
        let fmt = fmt.ok_or("No suitable audio format found")?;
        let ext = extension_for_mime(&fmt.mime_type);
        let audio_path = PathBuf::from(&opts.output_dir).join(format!("{}.{}", safe_title, ext));
        let audio_url = resolve_format_url(fmt, player_js.as_deref())?.ok_or("Format has no direct URL")?;
        let content_length = fmt.content_length.ok_or("Unknown content length")?;
        download_file(&client, &audio_url, &audio_path, content_length).await?;
        audio_path
    };

    // Thumbnail
    let thumb_path =
        PathBuf::from(&opts.output_dir).join(format!("{}.jpg", safe_title));
    let yc = YoutubeClient { client: client.clone(), proxy: None };
    let thumbnail_path = match yc.download_thumbnail(&video_id, &thumb_path).await {
        Ok(()) => Some(thumb_path),
        Err(_) => None,
    };

    let subtitle_paths =
        fetch_subtitles(&client, &pr, &opts.output_dir, &safe_title, &opts.lang).await?;

    Ok(DownloadResult {
        audio_path,
        subtitle_paths,
        thumbnail_path,
    })
}

async fn fetch_subtitles(
    client: &reqwest::Client,
    pr: &PlayerResponse,
    output_dir: &str,
    safe_title: &str,
    lang: &Option<String>,
) -> Result<Vec<PathBuf>, Error> {
    let tracks: Vec<&CaptionTrack> = pr
        .captions
        .as_ref()
        .and_then(|c| c.player_captions_tracklist_renderer.as_ref())
        .map(|r| {
            let filtered: Vec<&CaptionTrack> = match lang {
                Some(l) => r
                    .caption_tracks
                    .iter()
                    .filter(|t| t.language_code.starts_with(l.as_str()))
                    .collect(),
                None => r.caption_tracks.first().map(|t| vec![t]).unwrap_or_default(),
            };
            if filtered.is_empty() {
                r.caption_tracks.first().map(|t| vec![t]).unwrap_or_default()
            } else {
                filtered
            }
        })
        .unwrap_or_default();

    let mut paths = Vec::new();
    for track in tracks {
        let srt_path =
            PathBuf::from(output_dir).join(format!("{}.{}.srt", safe_title, track.language_code));
        let sub_url = format!("{}&fmt=json3", track.base_url);
        match fetch_subtitle(client, &sub_url).await {
            Ok(srt_content) if !srt_content.is_empty() => {
                std::fs::write(&srt_path, &srt_content)?;
                paths.push(srt_path);
            }
            _ => {}
        }
    }

    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cdp_cookie_json_parsing() {
        let json = r#"[
            {"name":"APISID","value":"Y_JLFkQM-t4O0Xld/A2q7bLdXLrjIgAlBa","domain":".youtube.com","path":"/","secure":true,"httpOnly":false,"expires":-1,"sameSite":"Lax"},
            {"name":"SAPISID","value":"1kWiikMM3do1sRJf/ALCD7UfheVtPEIvhy","domain":".youtube.com","path":"/","secure":true,"httpOnly":false,"expires":-1,"sameSite":"Lax"},
            {"name":"__Secure-1PAPISID","value":"1kWiikMM3do1sRJf/ALCD7UfheVtPEIvhy","domain":".youtube.com","path":"/","secure":true,"httpOnly":false,"expires":-1,"sameSite":"Lax"},
            {"name":"__Secure-3PAPISID","value":"1kWiikMM3do1sRJf/ALCD7UfheVtPEIvhy","domain":".youtube.com","path":"/","secure":true,"httpOnly":false,"expires":-1,"sameSite":"Lax"},
            {"name":"PREF","value":"tz=Asia.Shanghai","domain":".youtube.com","path":"/","secure":true,"httpOnly":false,"expires":-1,"sameSite":"Lax"},
            {"name":"LOGIN_INFO","value":"AFmmF2swRA","domain":".google.com","path":"/","secure":true,"httpOnly":true,"expires":-1,"sameSite":"NoRestriction"},
            {"name":"SID","value":"g.a000k_test","domain":".youtube.com","path":"/","secure":true,"httpOnly":false,"expires":-1,"sameSite":"Lax"}
        ]"#;

        let cookies: Vec<CdpCookie> = serde_json::from_str(json).unwrap();
        assert_eq!(cookies.len(), 7);

        let (store, auth) = build_cookie_store_and_auth(&cookies).unwrap();

        assert_eq!(auth.sapisid, "1kWiikMM3do1sRJf/ALCD7UfheVtPEIvhy");
        assert_eq!(auth.sapisid_1p.as_deref(), Some("1kWiikMM3do1sRJf/ALCD7UfheVtPEIvhy"));
        assert_eq!(auth.sapisid_3p.as_deref(), Some("1kWiikMM3do1sRJf/ALCD7UfheVtPEIvhy"));
        assert!(auth.logged_in);

        let yt_url = reqwest::Url::parse("https://www.youtube.com/watch?v=dQw4w9WgXcQ").unwrap();
        let cookie_header: String = store
            .get_request_values(&yt_url)
            .map(|(n, v)| format!("{}={}", n, v))
            .collect::<Vec<_>>()
            .join("; ");
        assert!(cookie_header.contains("SAPISID="));
        assert!(cookie_header.contains("APISID="));
        assert!(cookie_header.contains("__Secure-3PAPISID="));

        let google_url = reqwest::Url::parse("https://accounts.google.com/").unwrap();
        let google_cookies: String = store
            .get_request_values(&google_url)
            .map(|(n, v)| format!("{}={}", n, v))
            .collect::<Vec<_>>()
            .join("; ");
        assert!(google_cookies.contains("LOGIN_INFO="));
    }
}

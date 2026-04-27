use futures_util::StreamExt;
use regex::Regex;
use reqwest::StatusCode;
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};

const INNERTUBE_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const VR_USER_AGENT: &str =
    "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
const BROWSER_USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
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
}

impl Default for DownloadOpts {
    fn default() -> Self {
        Self {
            itag: "251".into(),
            output_dir: ".".into(),
            lang: None,
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
}

impl YoutubeClient {
    pub fn new(proxy: Option<&str>) -> Result<Self, Error> {
        let mut builder = reqwest::Client::builder()
            .user_agent(BROWSER_USER_AGENT)
            .timeout(std::time::Duration::from_secs(30))
            .cookie_store(true)
            .connect_timeout(std::time::Duration::from_secs(15))
            .pool_max_idle_per_host(0);
        if let Some(p) = proxy {
            builder = builder.proxy(reqwest::Proxy::all(p)?);
        }
        Ok(Self {
            client: builder.build()?,
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
        run_download(&self.client, url, opts)
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
    adaptive_formats: Vec<AdaptiveFormat>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AdaptiveFormat {
    itag: u32,
    mime_type: String,
    bitrate: u64,
    url: Option<String>,
    #[serde(deserialize_with = "deserialize_string_or_number")]
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

// ── Helpers ──

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

async fn get_visitor_data(client: &reqwest::Client, video_id: &str) -> Result<String, Error> {
    let url = format!("https://www.youtube.com/watch?v={}", video_id);
    let re = Regex::new(r#""visitorData"\s*:\s*"([^"]+)""#)?;

    for attempt in 0..5 {
        let resp = client
            .get(&url)
            .header("User-Agent", BROWSER_USER_AGENT)
            .header("Accept-Language", "en-US,en;q=0.9")
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await;

        match resp {
            Ok(r) if r.status() == StatusCode::OK => {
                let html = r.text().await?;
                if let Some(caps) = re.captures(&html) {
                    return Ok(caps[1].to_string());
                }
                return Ok(String::new());
            }
            _ => {}
        }

        tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt + 1) as u64)).await;
    }
    Ok(String::new())
}

async fn fetch_player_response(
    client: &reqwest::Client,
    video_id: &str,
    visitor_data: &str,
) -> Result<PlayerResponse, Error> {
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
    if let Some(fmt) = formats.iter().find(|f| f.itag == target && f.mime_type.contains("audio")) {
        return Some(fmt);
    }
    formats
        .iter()
        .filter(|f| f.mime_type.contains("audio") && f.url.is_some())
        .max_by_key(|f| f.bitrate)
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
    client: &reqwest::Client,
    url: &str,
    opts: DownloadOpts,
) -> Result<DownloadResult, Error> {
    let video_id = extract_video_id(url).ok_or("Could not extract video ID from URL")?;

    let visitor_data = get_visitor_data(client, &video_id).await?;
    let pr = fetch_player_response(client, &video_id, &visitor_data).await?;

    if pr.playability_status.status != "OK" {
        let reason = pr.playability_status.reason.unwrap_or_else(|| "unknown".into());
        return Err(Error::Other(format!("Video not playable: {}", reason)));
    }

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

    let fmt = pick_audio_format(&sd.adaptive_formats, &opts.itag)
        .ok_or("No suitable audio format found")?;

    let ext = extension_for_mime(&fmt.mime_type);
    let audio_path = PathBuf::from(&opts.output_dir).join(format!("{}.{}", safe_title, ext));

    let audio_url = fmt.url.as_deref().ok_or("Format has no direct URL")?;
    let content_length = fmt.content_length.ok_or("Unknown content length")?;

    download_file(client, audio_url, &audio_path, content_length).await?;

    // Thumbnail
    let thumb_path =
        PathBuf::from(&opts.output_dir).join(format!("{}.jpg", safe_title));
    let yc = YoutubeClient { client: client.clone() };
    let thumbnail_path = match yc.download_thumbnail(&video_id, &thumb_path).await {
        Ok(()) => Some(thumb_path),
        Err(_) => None,
    };

    let subtitle_paths =
        fetch_subtitles(client, &pr, &opts.output_dir, &safe_title, &opts.lang).await?;

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

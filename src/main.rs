use clap::Parser;
use regex::Regex;
use reqwest::StatusCode;
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};

const INNERTUBE_KEY: &str = "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8";
const VR_USER_AGENT: &str =
    "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

#[derive(Parser)]
#[command(name = "yt-dlp-audio", about = "Download YouTube audio + subtitles")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(long, default_value = "http://127.0.0.1:1080", global = true)]
    proxy: Option<String>,

    #[arg(long, conflicts_with = "proxy", global = true)]
    no_proxy: bool,
}

#[derive(clap::Subcommand)]
enum Command {
    /// Download audio and subtitles from a YouTube URL
    Download {
        url: String,

        #[arg(short, long, default_value = "251")]
        itag: String,

        #[arg(short, long, default_value = ".")]
        output_dir: String,

        #[arg(long)]
        lang: Option<String>,
    },
    /// Search YouTube and list results
    Search {
        query: String,

        #[arg(short, long, default_value = "10")]
        max_results: usize,
    },
}

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

fn extract_video_id(url: &str) -> Option<String> {
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

fn make_client(proxy: Option<&str>) -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    let mut builder = reqwest::Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .timeout(std::time::Duration::from_secs(30))
        .cookie_store(true)
        .connect_timeout(std::time::Duration::from_secs(15))
        .pool_max_idle_per_host(0);
    if let Some(p) = proxy {
        builder = builder.proxy(reqwest::Proxy::all(p)?);
    }
    Ok(builder.build()?)
}

async fn get_visitor_data(
    client: &reqwest::Client,
    video_id: &str,
) -> Result<String, Box<dyn std::error::Error>> {
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
                // Page loaded but no visitorData (unlikely)
                return Ok(String::new());
            }
            Ok(r) => {
                eprintln!("Page fetch returned {}, retrying...", r.status());
            }
            Err(e) => {
                eprintln!("Page fetch failed: {}, retrying...", e);
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt + 1) as u64)).await;
    }
    // Last resort: return empty and hope the API works without visitorData
    Ok(String::new())
}

async fn fetch_player_response(
    client: &reqwest::Client,
    video_id: &str,
    visitor_data: &str,
) -> Result<PlayerResponse, Box<dyn std::error::Error>> {
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
        "context": {
            "client": ctx
        }
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
        return Err(format!("API returned status {}", resp.status()).into());
    }

    let pr: PlayerResponse = resp.json().await?;
    Ok(pr)
}

fn pick_audio_format<'a>(
    formats: &'a [AdaptiveFormat],
    preferred_itag: &str,
) -> Option<&'a AdaptiveFormat> {
    let target: u32 = preferred_itag.parse().unwrap_or(251);

    // Try exact itag match
    if let Some(fmt) = formats
        .iter()
        .find(|f| f.itag == target && f.mime_type.contains("audio"))
    {
        return Some(fmt);
    }

    // Fall back to best bitrate audio
    formats
        .iter()
        .filter(|f| f.mime_type.contains("audio") && f.url.is_some())
        .max_by_key(|f| f.bitrate)
}

const CHUNK_SIZE: u64 = 10 * 1024 * 1024; // 10 MB per request, like yt-dlp default

async fn download_file(
    client: &reqwest::Client,
    url: &str,
    path: &Path,
    label: &str,
    total_size: Option<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    use futures_util::StreamExt;

    let total = total_size.ok_or("Unknown content length")?;
    let existing = if path.exists() {
        let meta = std::fs::metadata(path)?;
        if meta.len() <= total {
            meta.len()
        } else {
            0
        }
    } else {
        0
    };

    if existing > 0 {
        eprintln!("{}: resuming from {:.1} MB", label, existing as f64 / 1_048_576.0);
    }

    let mut downloaded = existing;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;

    while downloaded < total {
        let range_start = downloaded;
        let range_end = std::cmp::min(downloaded + CHUNK_SIZE - 1, total - 1);

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
                        return Err(format!("{}: connection failed after {} retries: {}", label, 15, e).into());
                    }
                    eprintln!("\n{}: connection error, retry {}...", label, retry);
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

                                let pct = (downloaded as f64 / total as f64) * 100.0;
                                eprint!(
                                    "\r{}: {:.1}% ({:.1}/{:.1} MB)",
                                    label,
                                    pct,
                                    downloaded as f64 / 1_048_576.0,
                                    total as f64 / 1_048_576.0
                                );
                            }
                            Err(_) => {
                                failed = true;
                                break;
                            }
                        }
                    }

                    if failed {
                        file.set_len(range_start)?;
                        downloaded = range_start;
                        retry += 1;
                        if retry > 15 {
                            return Err(format!("{}: chunk failed after {} retries", label, 15).into());
                        }
                        eprintln!("\n{}: chunk failed, retry {}...", label, retry);
                        tokio::time::sleep(std::time::Duration::from_secs_f64(2.0 * retry as f64)).await;
                        continue;
                    }

                    if chunk_received != expected_len {
                        file.set_len(range_start)?;
                        downloaded = range_start;
                        retry += 1;
                        if retry > 15 {
                            return Err(format!("{}: short read after {} retries", label, 15).into());
                        }
                        eprintln!("\n{}: short read ({}/{} bytes), retry {}...", label, chunk_received, expected_len, retry);
                        tokio::time::sleep(std::time::Duration::from_secs_f64(2.0 * retry as f64)).await;
                        continue;
                    }
                    break;
                }
                status => {
                    return Err(format!("{} download failed: {}", label, status).into());
                }
            }
        }
    }

    eprintln!();
    Ok(())
}

async fn fetch_subtitle(client: &reqwest::Client, url: &str) -> Result<String, Box<dyn std::error::Error>> {
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
            (start_ms / 3600000),
            ((start_ms % 3600000) / 60000),
            ((start_ms % 60000) / 1000),
            start_ms % 1000,
            (end_ms / 3600000),
            ((end_ms % 3600000) / 60000),
            ((end_ms % 60000) / 1000),
            end_ms % 1000,
            content.trim()
        ));
        idx += 1;
    }
    Ok(srt)
}

struct SearchResult {
    id: String,
    title: String,
    channel: String,
    duration: Option<String>,
    views: Option<String>,
    publish_time: Option<String>,
}

async fn search_youtube(
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
) -> Result<Vec<SearchResult>, Box<dyn std::error::Error>> {
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
                eprintln!("No ytInitialData in response, retrying...");
            }
            Ok(r) => eprintln!("Search returned status {}, retrying...", r.status()),
            Err(e) => eprintln!("Search request failed: {}, retrying...", e),
        }

        if attempt < 2 {
            tokio::time::sleep(std::time::Duration::from_secs(2 * (attempt as u64 + 1)))
                .await;
        }
    }

    if !html.contains("ytInitialData") {
        return Err("Failed to get search results after retries".into());
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
    let json_str = &html[data_start..data_end];

    let data: serde_json::Value = serde_json::from_str(json_str)?;

    let mut results = Vec::new();

    if let Some(sections) = data
        .pointer("/contents/twoColumnSearchResultsRenderer/primaryContents/sectionListRenderer/contents")
        .and_then(|v| v.as_array())
    {
        for section in sections {
            let items = section
                .pointer("/itemSectionRenderer/contents")
                .and_then(|v| v.as_array());
            let items = match items {
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

                let title = vr
                    .pointer("/title/runs/0/text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(no title)")
                    .to_string();

                let channel = vr
                    .pointer("/longBylineText/runs/0/text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                let duration = vr
                    .pointer("/lengthText/simpleText")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let views = vr
                    .pointer("/viewCountText/simpleText")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                let publish_time = vr
                    .pointer("/publishedTimeText/simpleText")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());

                results.push(SearchResult {
                    id,
                    title,
                    channel,
                    duration,
                    views,
                    publish_time,
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
    cli: &Cli,
    url: &str,
    itag: &str,
    output_dir: &str,
    lang: &Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let video_id = extract_video_id(url).ok_or("Could not extract video ID from URL")?;

    let proxy = if cli.no_proxy { None } else { cli.proxy.as_deref() };
    let client = make_client(proxy)?;

    eprintln!("Fetching info for {}...", video_id);

    let visitor_data = get_visitor_data(&client, &video_id).await?;
    let pr = fetch_player_response(&client, &video_id, &visitor_data).await?;

    if pr.playability_status.status != "OK" {
        let reason = pr
            .playability_status
            .reason
            .unwrap_or_else(|| "unknown".into());
        eprintln!("Video unavailable: {}", reason);
        return Err(format!("Video not playable: {}", reason).into());
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

    let fmt = pick_audio_format(&sd.adaptive_formats, itag)
        .ok_or("No suitable audio format found")?;

    let ext = extension_for_mime(&fmt.mime_type);
    let audio_path = PathBuf::from(output_dir).join(format!("{}.{}", safe_title, ext));

    eprintln!(
        "Downloading audio: itag={} {} (~{:.2} MB)",
        fmt.itag,
        ext,
        fmt.content_length.unwrap_or(0) as f64 / 1_048_576.0
    );

    let audio_url = fmt.url.as_deref().ok_or("Format has no direct URL")?;

    if let Err(e) =
        download_file(&client, audio_url, &audio_path, "Audio", fmt.content_length).await
    {
        return Err(e);
    }

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

    for track in tracks {
        let lang_code = &track.language_code;
        let srt_path =
            PathBuf::from(output_dir).join(format!("{}.{}.srt", safe_title, lang_code));

        eprintln!("Downloading subtitles: {}...", lang_code);
        let sub_url = format!("{}&fmt=json3", track.base_url);
        match fetch_subtitle(&client, &sub_url).await {
            Ok(srt_content) if !srt_content.is_empty() => {
                std::fs::write(&srt_path, &srt_content)?;
                eprintln!("Saved: {}", srt_path.display());
            }
            Ok(_) => eprintln!("No subtitle content for {}", lang_code),
            Err(e) => eprintln!("Subtitle download failed for {}: {}", lang_code, e),
        }
    }

    eprintln!("Done! Audio: {}", audio_path.display());
    Ok(())
}

async fn run_search(
    cli: &Cli,
    query: &str,
    max_results: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let proxy = if cli.no_proxy { None } else { cli.proxy.as_deref() };
    let client = make_client(proxy)?;

    eprintln!("Searching for \"{}\"...", query);
    let results = search_youtube(&client, query, max_results).await?;

    if results.is_empty() {
        eprintln!("No results found.");
        return Ok(());
    }

    for (i, r) in results.iter().enumerate() {
        let dur = r
            .duration
            .as_deref()
            .unwrap_or("LIVE");
        let views = r
            .views
            .as_deref()
            .unwrap_or("");
        let time = r
            .publish_time
            .as_deref()
            .unwrap_or("");
        let meta = if !views.is_empty() && !time.is_empty() {
            format!(" [{} · {} · {}]", views, time, dur)
        } else if !views.is_empty() {
            format!(" [{} · {}]", views, dur)
        } else {
            format!(" [{}]", dur)
        };
        println!(
            "[{}] {}{}",
            i + 1,
            &r.title,
            meta
        );
        println!("    {} — https://youtube.com/watch?v={}", &r.channel, &r.id);
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Command::Download {
            ref url,
            ref itag,
            ref output_dir,
            ref lang,
        } => run_download(&cli, url, itag, output_dir, lang).await,
        Command::Search {
            ref query,
            max_results,
        } => run_search(&cli, query, max_results).await,
    }
}

use clap::Parser;
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use ytdl_audio::{
    DownloadOpts, DownloadProgressEvent, DownloadProgressPhase, DownloadProgressReporter,
    DownloadRequest, StdFileWriter, YoutubeClient, convert_audio,
};

#[derive(Parser)]
#[command(name = "ytdl-audio", about = "Download YouTube audio + subtitles")]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(long, global = true)]
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

        /// Netscape cookie file for bot-detected videos
        #[arg(long)]
        cookies: Option<String>,

        /// CDP port of a running Chrome instance (e.g. 9222). Requires Chrome started with --remote-debugging-port=PORT.
        #[arg(long)]
        cookies_from_browser: Option<String>,

        /// Convert to format via the embedding application's audio pipeline
        #[arg(short, long)]
        format: Option<String>,

        /// Embed cover art in output file through the embedding application's audio pipeline
        #[arg(long)]
        embed_cover: bool,
    },
    /// Search YouTube and list results
    Search {
        query: String,

        #[arg(short, long, default_value = "10")]
        max_results: usize,
    },
}

#[derive(Default)]
struct CliProgressState {
    last_phase: Option<DownloadProgressPhase>,
    drew_inline: bool,
}

struct CliProgressReporter {
    state: Mutex<CliProgressState>,
}

impl CliProgressReporter {
    fn new() -> Self {
        Self {
            state: Mutex::new(CliProgressState::default()),
        }
    }
}

impl DownloadProgressReporter for CliProgressReporter {
    fn emit(&self, event: DownloadProgressEvent) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };

        let mut stdout = io::stdout().lock();
        let phase_changed = state.last_phase.as_ref() != Some(&event.phase);

        if matches!(event.phase, DownloadProgressPhase::Downloading) {
            if state.drew_inline && !phase_changed {
                let _ = write!(stdout, "\r");
            }
            match event.percent {
                Some(percent) => {
                    let _ = write!(
                        stdout,
                        "[{}] {:>3}% {}",
                        event.source, percent, event.message
                    );
                }
                None => {
                    let _ = write!(stdout, "[{}] {}", event.source, event.message);
                }
            }
            if let Some(detail) = &event.detail {
                let _ = write!(stdout, " ({detail})");
            }
            let _ = stdout.flush();
            state.drew_inline = true;
        } else {
            if state.drew_inline {
                let _ = writeln!(stdout);
                state.drew_inline = false;
            }
            let _ = write!(
                stdout,
                "[{}] {:?}: {}",
                event.source, event.phase, event.message
            );
            if let Some(detail) = &event.detail {
                let _ = write!(stdout, " ({detail})");
            }
            let _ = writeln!(stdout);
            let _ = stdout.flush();
        }

        state.last_phase = Some(event.phase);
    }
}

fn next_job_id(prefix: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{ts}")
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let proxy = if cli.no_proxy {
        None
    } else {
        cli.proxy.as_deref()
    };
    let client = YoutubeClient::new(proxy)?;

    match cli.command {
        Command::Download {
            url,
            itag,
            output_dir,
            lang,
            cookies,
            cookies_from_browser,
            format,
            embed_cover,
        } => {
            eprintln!("Downloading {}...", url);
            let request = DownloadRequest {
                job_id: next_job_id("youtube"),
                url: url.clone(),
                opts: DownloadOpts {
                    itag,
                    output_dir: output_dir.clone(),
                    lang,
                    cookies,
                    cookies_from_browser,
                },
            };
            let result = client
                .download_with_progress(&request, std::sync::Arc::new(CliProgressReporter::new()))
                .await?;

            let final_audio = if format.is_some() || embed_cover {
                let ext = format.as_deref().unwrap_or_else(|| {
                    result
                        .audio_path
                        .extension()
                        .and_then(|e| e.to_str())
                        .unwrap_or("m4a")
                });
                let output_name = result
                    .audio_path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let converted: PathBuf =
                    PathBuf::from(&output_dir).join(format!("{}.{}", output_name, ext));

                let cover = if embed_cover {
                    result.thumbnail_path.as_deref()
                } else {
                    None
                };

                eprintln!("Converting to {}...", converted.display());
                let fw = StdFileWriter;
                convert_audio(&result.audio_path, &converted, cover, &fw)?;

                // Clean up original if we converted to a different file
                if converted != result.audio_path {
                    let _ = std::fs::remove_file(&result.audio_path);
                }
                converted
            } else {
                result.audio_path
            };

            eprintln!("Audio: {}", final_audio.display());
            for p in &result.subtitle_paths {
                eprintln!("Subtitle: {}", p.display());
            }
            if let Some(t) = &result.thumbnail_path {
                eprintln!("Thumbnail: {}", t.display());
            }
        }
        Command::Search { query, max_results } => {
            eprintln!("Searching \"{}\"...", query);
            let videos = client.search(&query, max_results).await?;
            if videos.is_empty() {
                eprintln!("No results found.");
                return Ok(());
            }
            for (i, v) in videos.iter().enumerate() {
                let dur = v.duration.as_deref().unwrap_or("LIVE");
                let views = v.views.as_deref().unwrap_or("");
                let time = v.publish_time.as_deref().unwrap_or("");
                let meta = if !views.is_empty() && !time.is_empty() {
                    format!(" [{} · {} · {}]", views, time, dur)
                } else if !views.is_empty() {
                    format!(" [{} · {}]", views, dur)
                } else {
                    format!(" [{}]", dur)
                };
                println!("[{}] {}{}", i + 1, &v.title, meta);
                println!("    {} — https://youtube.com/watch?v={}", &v.channel, &v.id);
            }
        }
    }

    Ok(())
}

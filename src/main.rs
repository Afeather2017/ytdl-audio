use clap::Parser;
use std::path::PathBuf;
use yt_dlp_audio::{convert_audio, DownloadOpts, YoutubeClient};

#[derive(Parser)]
#[command(name = "yt-dlp-audio", about = "Download YouTube audio + subtitles")]
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

        /// Convert to format via ffmpeg (e.g. ogg, m4a, mp3)
        #[arg(short, long)]
        format: Option<String>,

        /// Embed cover art in output file (requires ffmpeg)
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let proxy = if cli.no_proxy { None } else { cli.proxy.as_deref() };
    let client = YoutubeClient::new(proxy)?;

    match cli.command {
        Command::Download {
            url,
            itag,
            output_dir,
            lang,
            format,
            embed_cover,
        } => {
            eprintln!("Downloading {}...", url);
            let result = client
                .download(&url, DownloadOpts { itag, output_dir: output_dir.clone(), lang })
                .await?;

            let final_audio = if format.is_some() || embed_cover {
                let ext = format.as_deref().unwrap_or_else(|| {
                    result.audio_path.extension().and_then(|e| e.to_str()).unwrap_or("m4a")
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
                convert_audio(&result.audio_path, &converted, cover)?;

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
        Command::Search {
            query,
            max_results,
        } => {
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
                println!(
                    "    {} — https://youtube.com/watch?v={}",
                    &v.channel, &v.id
                );
            }
        }
    }

    Ok(())
}

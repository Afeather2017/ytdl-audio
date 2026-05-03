# yt-dlp-audio

A minimal YouTube audio + subtitle downloader and search tool in Rust, with a small Node.js helper for YouTube's cipher and `n` challenge.

## Build

```bash
cargo build --release
```

Binary size: ~7.3 MB.

Dependencies: `reqwest`, `tokio`, `serde`, `serde_json`, `regex`, `clap`, `futures-util`, `urlencoding`, `sha1`, `reqwest_cookie_store`.
Requires: `node` for the YouTube cipher solver.

## Usage

```
yt-dlp-audio [OPTIONS] <COMMAND>

Commands:
  download  Download audio and subtitles from a YouTube URL
  search    Search YouTube and list results

Options:
      --proxy <PROXY>    HTTP proxy [default: http://127.0.0.1:1080]
      --no-proxy         Disable proxy
  -h, --help             Print help
```

### Search

```bash
# Search YouTube
yt-dlp-audio search "HAPPY SOULS"

# Limit results
yt-dlp-audio search --max-results 3 "lofi beats"
```

Output:
```
[1] HAPPY SOULS [33,904,186 views · 9 years ago · 15:31]
    Jameserton — https://youtube.com/watch?v=2kr7KDCsIws
[2] Happy Souls 2 : Gameplay trailer [951,417 views · 4 years ago · 2:25]
    Gameserton — https://youtube.com/watch?v=_277J8ZJxSU
...
```

### Download

```bash
# Best quality audio (opus) + auto subtitles
yt-dlp-audio download "https://youtube.com/watch?v=VIDEO_ID"

# AAC format instead of opus
yt-dlp-audio download -i 140 "URL"

# Save to specific directory, prefer Japanese subtitles
yt-dlp-audio download -o ~/Music --lang ja "URL"

# No proxy (direct connection)
yt-dlp-audio download --no-proxy "URL"

# Custom proxy
yt-dlp-audio download --proxy socks5://127.0.0.1:9050 "URL"

# Use browser cookies for bot-detected videos
yt-dlp-audio download --cookies-from-browser chrome "URL"

# Resume a partial download (just run the same command again)
yt-dlp-audio download -o /tmp "URL"
```

#### Download Options

| Option | Default | Description |
|--------|---------|-------------|
| `-i, --itag` | 251 | Audio format itag |
| `-o, --output-dir` | `.` | Output directory |
| `--lang` | (first available) | Preferred subtitle language |
| `--cookies` | (none) | Netscape cookie file |
| `--cookies-from-browser` | (none) | Export cookies from a local browser profile |

#### Audio Formats

| itag | Codec | Bitrate | Container |
|------|-------|---------|-----------|
| 251 | Opus | 128 kbps | WebM |
| 250 | Opus | 70 kbps | WebM |
| 249 | Opus | 50 kbps | WebM |
| 140 | AAC | 128 kbps | M4A |
| 139 | AAC | 48 kbps | M4A |

### Output

- Audio: `{title}.webm` or `{title}.m4a` depending on format
- Subtitles: `{title}.{lang}.srt` in SRT format
- Thumbnail: `{title}.jpg` (1280x720)
- Re-running with the same output path resumes partial downloads

### Convert and Cover Art

Use `--format` to remux via ffmpeg and `--embed-cover` to embed the thumbnail:

```bash
# Remux WebM/Opus to OGG (stream copy, ~1s)
yt-dlp-audio download --format ogg "URL"

# Embed cover art in M4A
yt-dlp-audio download -i 140 --embed-cover "URL"

# Convert to MKV with cover art (instant, same container family as WebM)
yt-dlp-audio download --format mkv --embed-cover "URL"
```

Requires `ffmpeg` on PATH. Downloads are audio-only WebM, so remux to OGG/MKV is fast (~1s). Cover art only works with M4A and MKV containers (not OGG).

## How It Works

The downloader now uses a hybrid flow:

1. It first tries the `ANDROID_VR` player, which is the simplest path when YouTube returns direct audio URLs.
2. If YouTube bot-detects that client, it retries with cookies.
3. With cookies, it can fall back to the `TV` / `web_safari` / `WEB` player clients.
4. When YouTube returns `signatureCipher` or an `n` challenge, it invokes the local Node.js solver in `js/` using the vendored `yt-dlp` player logic.
5. The resolved audio URL is downloaded with resume support, then subtitles and thumbnail are fetched.

### Flow

```
watch page -> player API -> format selection -> Node JS solver if needed -> download
                  |                |
                  |                +-- decrypt signatureCipher / n
                  +-- retry with cookies + SAPISIDHASH when bot-detected
```

### Cookie Support

You can pass a Netscape cookie file with `--cookies`, or export cookies from Chrome with `--cookies-from-browser chrome`.

### Node Solver

`js/solver.mjs` wraps the vendored `yt-dlp` cipher solver and runs it under Node.js. This is what makes the Rust refactor work on YouTube responses that need cipher decryption.

## Limitations

- **Login-required videos** may still need fresh cookies from Chrome
- **No video download** — audio only
- **Node.js is required** for the cipher solver
- **Subtitle language filtering** uses prefix matching (`--lang ja` matches `ja`, but falls back to the first available track if no match)

## Comparison with yt-dlp

This tool is intentionally minimal. Use yt-dlp if you need:

- Video download or format merging
- Playlist/channel bulk downloads
- Live stream recording
- Thumbnails, metadata, NFO files
- Format selection (`-f bestaudio`)
- Post-processing (ffmpeg conversion, embedding thumbnails)
- OAuth login for member content
- SponsorBlock, description formatting, etc.

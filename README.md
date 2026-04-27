# yt-dlp-audio

A minimal YouTube audio + subtitle downloader and search tool written in pure Rust. No Python, no JS runtime, no yt-dlp dependency.

## Build

```bash
cargo build --release
```

Binary size: ~7.3 MB.

Dependencies: `reqwest`, `tokio`, `serde`, `serde_json`, `regex`, `clap`, `futures-util`, `urlencoding`.

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

# Resume a partial download (just run the same command again)
yt-dlp-audio download -o /tmp "URL"
```

#### Download Options

| Option | Default | Description |
|--------|---------|-------------|
| `-i, --itag` | 251 | Audio format itag |
| `-o, --output-dir` | `.` | Output directory |
| `--lang` | (first available) | Preferred subtitle language |

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

The tool exploits a key insight: YouTube's **ANDROID_VR** client (`REQUIRE_JS_PLAYER: false`) returns direct streaming URLs with no signature cipher. This eliminates the hardest part of YouTube downloading — running the obfuscated player JavaScript to decrypt stream URLs.

### Architecture

```
┌──────────────────────────────────────────────────────────┐
│  1. URL Parsing                                           │
│     youtube.com/watch?v=ID, youtu.be/ID, shorts/, live/   │
│     Extract 11-char video ID via regex                     │
├──────────────────────────────────────────────────────────┤
│  2. Anti-Bot: Fetch visitorData                           │
│     GET /watch?v=ID  (browser UA, cookies enabled)        │
│     Parse "visitorData":"..." from HTML                    │
│     Retries 5 times with exponential backoff              │
├──────────────────────────────────────────────────────────┤
│  3. Player API Call                                       │
│     POST /youtubei/v1/player?key=API_KEY                  │
│     Client: ANDROID_VR (Quest 3, v1.65.10)                │
│     Includes visitorData to avoid bot detection            │
│     Returns: formats, subtitles, video metadata           │
├──────────────────────────────────────────────────────────┤
│  4. Format Selection                                      │
│     Filter adaptiveFormats by mime type "audio/*"         │
│     Prefer requested itag (default: 251 = opus 128kbps)   │
│     Fallback: highest bitrate audio format                │
├──────────────────────────────────────────────────────────┤
│  5. Range-Based Download (the critical part)              │
│     Download in 10 MB chunks with HTTP Range headers      │
│     Resume from existing partial file                     │
│     On failure: truncate to chunk boundary, retry        │
│     Up to 15 retries per chunk, exponential backoff       │
│     This is how yt-dlp handles unreliable connections    │
├──────────────────────────────────────────────────────────┤
│  6. Subtitle Download                                    │
│     Fetch from YouTube timedtext API                     │
│     Parse XML <p t="ms" d="ms">text</p> format            │
│     Convert to SRT with proper timestamps                │
└──────────────────────────────────────────────────────────┘

┌──────────────────────────────────────────────────────────┐
│  Search (separate subcommand)                             │
│     GET /results?search_query=<encoded>                   │
│     Extract ytInitialData JSON from HTML                  │
│     Parse videoRenderer entries: title, channel, ID,     │
│       duration, views, publish time                      │
└──────────────────────────────────────────────────────────┘
```

### Why ANDROID_VR Works

YouTube serves different stream data depending on the client. Most clients (web, iOS, tv) return `signatureCipher` — an encrypted URL parameter that requires running YouTube's minified player JavaScript to decrypt. The ANDROID_VR client is special:

- Configured with `REQUIRE_JS_PLAYER: false`
- Returns direct `url` fields in the format list (no cipher)
- Supports adaptive formats (separate audio/video streams)
- Works without authentication

The trade-off: YouTube may block this client in the future, and it cannot access age-restricted or login-required videos.

### Why No JavaScript Runtime

The original plan was to use Boa (a Rust JS engine) to interpret YouTube's cipher functions. This turned out to be unnecessary because:

1. The ANDROID_VR client bypasses the cipher entirely
2. The cipher functions change frequently and would need constant maintenance
3. A JS engine adds ~3-5MB to binary size and complexity

If YouTube ever removes the ANDROID_VR direct URL behavior, the cipher code would need to be ported.

## Limitations

- **Login-required videos** (age-gated, members-only, private) cannot be downloaded
- **No video download** — audio only
- **ANDROID_VR client version** may be blocked by YouTube in the future; when that happens, the client version in `src/main.rs` (currently `1.65.10`) needs to be updated to match yt-dlp's current version
- **No PO token support** — YouTube's newer anti-bot system; most videos work without it
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

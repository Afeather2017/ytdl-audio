# ytdl-demo

Small Tauri demo for YouTube login and `yt-dlp-rs` testing.

## Setup

Install both the demo frontend dependencies and the shared solver dependencies from the parent `ytdl-audio` crate:

```bash
npm ci
cd ../../js && npm ci
```

The second step is required because the underlying `ytdl-audio` crate uses `js/solver.mjs` for YouTube cipher and `n` solving. Without those packages, CLI downloads and any Node-backed solver path will fail at runtime.

## Flow

1. Open YouTube in the current webview and sign in.
2. Export cookies.
3. Cookies are saved to the app data directory and restored on the next launch.
4. Returning to the app shell keeps the exported cookie jar on disk.
5. Downloads are written to the same app data directory.

## Devtools

On desktop builds, the `Devtools` button opens the Tauri webview devtools for the current window.

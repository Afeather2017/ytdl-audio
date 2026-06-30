<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

type DownloadProgressPhase =
  | "queued"
  | "preparing"
  | "resolving_meta"
  | "downloading"
  | "post_processing"
  | "embedding_cover"
  | "saving_lyrics"
  | "refreshing_library"
  | "completed"
  | "failed";

type DownloadProgressSnapshot = {
  job_id: string;
  source: string;
  state: string;
  phase: DownloadProgressPhase;
  percent: number | null;
  message: string;
  detail: string | null;
  filename: string | null;
  warning: string | null;
  error: string | null;
};

const videoUrl = ref("https://www.youtube.com/watch?v=fywVL3hh1xo");
const proxy = ref("");
const cookieJar = ref("");
const outputDir = ref("");
const status = ref("Open the YouTube window and log in.");
const progress = ref<DownloadProgressSnapshot | null>(null);
let unlistenProgress: UnlistenFn | null = null;

onMounted(async () => {
  unlistenProgress = await listen<DownloadProgressSnapshot>(
    "download-progress",
    (event) => {
      progress.value = event.payload;
      const percent =
        event.payload.percent == null ? "" : ` ${event.payload.percent}%`;
      const detail = event.payload.detail ? ` (${event.payload.detail})` : "";
      status.value = `[${event.payload.phase}]${percent} ${event.payload.message}${detail}`;
    },
  );
});

onBeforeUnmount(() => {
  if (unlistenProgress) {
    unlistenProgress();
    unlistenProgress = null;
  }
});

async function openBrowser() {
  status.value = "Navigating current webview to YouTube...";
  try {
    await invoke("open_browser");
    status.value = "YouTube loaded in the current webview.";
  } catch (error) {
    status.value = `Open browser failed: ${String(error)}`;
    console.error("open_browser failed", error);
  }
}

async function openBrowserDevtools() {
  status.value = "Opening browser devtools...";
  try {
    await invoke("open_browser_devtools");
    status.value = "Browser devtools opened.";
  } catch (error) {
    status.value = `Devtools failed: ${String(error)}`;
    console.error("open_browser_devtools failed", error);
  }
}

async function exportCookies() {
  status.value = "Exporting cookies...";
  console.log("export_browser_cookies invoked");
  try {
    cookieJar.value = await invoke<string>("export_browser_cookies");
    status.value = `Cookie jar saved: ${cookieJar.value}`;
    console.log("export_browser_cookies ok", cookieJar.value);
  } catch (error) {
    status.value = `Export cookies failed: ${String(error)}`;
    console.error("export_browser_cookies failed", error);
  }
}

async function closeBrowser() {
  status.value = "Returning to app shell...";
  try {
    await invoke("close_browser");
    status.value = "App shell restored.";
  } catch (error) {
    status.value = `Close browser failed: ${String(error)}`;
    console.error("close_browser failed", error);
  }
}

async function testDownload() {
  status.value = "Starting yt-dlp-rs download...";
  progress.value = null;
  try {
    const result = await invoke<{
      audio_path: string;
      subtitle_paths: string[];
      thumbnail_path: string | null;
      cookie_jar: string;
      output_dir: string;
    }>("test_download", {
      url: videoUrl.value,
      proxy: proxy.value.trim() || null,
    });
    cookieJar.value = result.cookie_jar;
    outputDir.value = result.output_dir;
    status.value = `Downloaded: ${result.audio_path}`;
  } catch (error) {
    status.value = `Download failed: ${String(error)}`;
    console.error("test_download failed", error);
  }
}
</script>

<template>
  <main class="shell">
    <section class="panel hero">
      <p class="eyebrow">Tauri demo</p>
      <h1>YouTube cookie test harness</h1>
      <p class="lead">
        Open YouTube in the current webview, log in, export the live cookie
        jar, then test yt-dlp-rs against it.
      </p>
      <div class="actions">
        <button @click="openBrowser">Open browser</button>
        <button class="secondary" @click="openBrowserDevtools">Devtools</button>
        <button class="secondary" @click="exportCookies">Export cookies</button>
        <button class="secondary" @click="closeBrowser">Close browser</button>
        <button class="primary" @click="testDownload">Test download</button>
      </div>
    </section>

    <section class="panel form">
      <label>
        Video URL
        <input v-model="videoUrl" />
      </label>
      <label>
        Proxy
        <input v-model="proxy" placeholder="http://127.0.0.1:1080" />
      </label>
    </section>

    <section class="panel status">
      <div><strong>Status:</strong> {{ status }}</div>
      <div v-if="progress">
        <strong>Progress:</strong>
        {{ progress.phase }}
        <template v-if="progress.percent !== null"> · {{ progress.percent }}%</template>
      </div>
      <div v-if="progress?.detail"><strong>Detail:</strong> {{ progress.detail }}</div>
      <div v-if="progress?.error"><strong>Error:</strong> {{ progress.error }}</div>
      <div v-if="outputDir"><strong>App dir:</strong> {{ outputDir }}</div>
      <div v-if="cookieJar"><strong>Cookie jar:</strong> {{ cookieJar }}</div>
    </section>
  </main>
</template>

<style scoped>
:global(body) {
  margin: 0;
}

.shell {
  min-height: 100vh;
  padding: 24px;
  background:
    radial-gradient(circle at top left, rgba(88, 130, 255, 0.2), transparent 30%),
    linear-gradient(180deg, #0e1320, #090c14);
  color: #eef2ff;
  display: grid;
  gap: 16px;
  grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
  font-family: Inter, Avenir, Helvetica, Arial, sans-serif;
}

.panel {
  background: rgba(16, 22, 38, 0.82);
  border: 1px solid rgba(148, 163, 184, 0.18);
  border-radius: 20px;
  padding: 20px;
  backdrop-filter: blur(16px);
  box-shadow: 0 20px 60px rgba(0, 0, 0, 0.28);
}

.hero {
  grid-column: 1 / -1;
}

.eyebrow {
  margin: 0 0 8px;
  text-transform: uppercase;
  letter-spacing: 0.18em;
  color: #7dd3fc;
  font-size: 12px;
}

h1 {
  margin: 0 0 12px;
  font-size: clamp(2rem, 4vw, 3.5rem);
  line-height: 1.05;
}

.lead {
  margin: 0 0 18px;
  color: #cbd5e1;
  max-width: 60ch;
}

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
}

button,
input {
  border-radius: 12px;
  border: 1px solid rgba(148, 163, 184, 0.2);
  padding: 0.8rem 1rem;
  background: rgba(15, 23, 42, 0.9);
  color: #f8fafc;
  font: inherit;
}

button {
  cursor: pointer;
}

button.primary {
  background: linear-gradient(135deg, #38bdf8, #2563eb);
}

button.secondary {
  background: rgba(30, 41, 59, 0.95);
}

.form {
  display: grid;
  gap: 12px;
}

label {
  display: grid;
  gap: 6px;
  color: #cbd5e1;
}

.status {
  grid-column: 1 / -1;
  color: #e2e8f0;
  line-height: 1.7;
}
</style>

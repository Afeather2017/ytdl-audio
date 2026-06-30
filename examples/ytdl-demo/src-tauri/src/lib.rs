use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;
#[cfg(target_os = "android")]
use std::sync::OnceLock;
use tauri::{Emitter, Manager};
#[cfg(not(target_os = "android"))]
use tauri::webview::PageLoadPayload;
#[cfg(not(target_os = "android"))]
use tauri::WebviewUrl;
#[cfg(not(target_os = "android"))]
use tauri::WebviewWindow;
use ytdl_audio::{
    DownloadOpts, DownloadProgressEvent, DownloadProgressPhase, DownloadProgressReporter,
    DownloadProgressSnapshot, DownloadRequest, JsRunner, YoutubeClient, apply_progress_event,
};

#[cfg(target_os = "android")]
use jni::objects::{JObject, JString, JValue};
#[cfg(target_os = "android")]
use jni::JavaVM;
#[cfg(not(target_os = "android"))]
use std::sync::mpsc;
#[cfg(target_os = "android")]
static ANDROID_EXTERNAL_DATA_DIR: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();
#[cfg(target_os = "android")]
static ANDROID_ACTIVITY_VM: OnceLock<Mutex<Option<JavaVM>>> = OnceLock::new();
#[cfg(target_os = "android")]
static ANDROID_ACTIVITY_GLOBAL: OnceLock<Mutex<Option<jni::objects::GlobalRef>>> = OnceLock::new();
#[cfg(not(target_os = "android"))]
static SOLVER_WINDOW_READY: std::sync::OnceLock<std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>> =
    std::sync::OnceLock::new();
const MAIN_WINDOW_LABEL: &str = "main";
#[cfg(not(target_os = "android"))]
const SOLVER_WINDOW_LABEL: &str = "solver";
const YOUTUBE_URL: &str = "https://www.youtube.com";
const COOKIE_FILE_NAME: &str = "ytdl-demo-youtube-cookies.txt";
const RUNTIME_LOG_FILE_NAME: &str = "ytdl-demo-runtime.log";
const BUILD_TRACE_MARKER: &str = "trace-2026-05-11-pathflow-01";
const DOWNLOAD_PROGRESS_EVENT: &str = "download-progress";

#[derive(Serialize)]
struct DownloadOutcome {
    audio_path: String,
    subtitle_paths: Vec<String>,
    thumbnail_path: Option<String>,
    cookie_jar: String,
    output_dir: String,
}

struct TauriProgressReporter {
    app: tauri::AppHandle,
    snapshot: Mutex<Option<DownloadProgressSnapshot>>,
}

impl TauriProgressReporter {
    fn new(app: tauri::AppHandle) -> Self {
        Self {
            app,
            snapshot: Mutex::new(None),
        }
    }
}

impl DownloadProgressReporter for TauriProgressReporter {
    fn emit(&self, event: DownloadProgressEvent) {
        let Ok(mut snapshot_guard) = self.snapshot.lock() else {
            return;
        };

        let mut snapshot = apply_progress_event(snapshot_guard.take(), event.clone());
        match event.phase {
            DownloadProgressPhase::Completed => {
                snapshot.filename = event.detail.clone();
                snapshot.error = None;
            }
            DownloadProgressPhase::Failed => {
                snapshot.error = event.detail.clone().or_else(|| Some(event.message.clone()));
            }
            _ => {}
        }

        let _ = self.app.emit(DOWNLOAD_PROGRESS_EVENT, snapshot.clone());
        *snapshot_guard = Some(snapshot);
    }
}

struct WebviewJsRunner {
    #[cfg(not(target_os = "android"))]
    app: tauri::AppHandle,
}

impl JsRunner for WebviewJsRunner {
    fn run(&self, input: &str) -> Result<String, ytdl_audio::Error> {
        #[cfg(target_os = "android")]
        {
            return run_hidden_android_solver(input);
        }

        #[cfg(not(target_os = "android"))]
        {
        let window = solver_window(&self.app).map_err(ytdl_audio::Error::Other)?;
        wait_for_solver_window_ready(&self.app)?;
        runtime_log(&self.app, &format!("webview js runner: input_json={input}"));
        self.ensure_solver_ready(&window)?;
        let (tx, rx) = mpsc::channel::<Result<String, String>>();
        let script = format!(
            r#"(() => {{
  try {{
    const input = JSON.parse({input:?});
    return window.__ytdlSolve(input);
  }} catch (error) {{
    return {{"type":"error","error": String(error && error.message ? error.message : error)}};
  }}
}})()"#
        );
        window
            .eval_with_callback(script, move |value| {
                eprintln!("webview js runner: callback_json={value}");
                let _ = tx.send(Ok(value));
            })
            .map_err(|e| ytdl_audio::Error::Other(e.to_string()))?;
        rx.recv_timeout(std::time::Duration::from_secs(30))
            .map_err(|e| ytdl_audio::Error::Other(format!("webview js runner timeout: {}", e)))?
            .map_err(ytdl_audio::Error::Other)
        }
    }
}

impl WebviewJsRunner {
    #[cfg(not(target_os = "android"))]
    fn ensure_solver_ready(&self, window: &WebviewWindow) -> Result<(), ytdl_audio::Error> {
        let init_script = format!(
            r#"(function() {{
  if (window.__ytdlSolveReady) {{
    return;
  }}
  try {{
    if (!window.__ytdlSolverBootstrapping) {{
      window.__ytdlSolverBootstrapping = true;
      const loadScript = (src) => new Promise((resolve, reject) => {{
        const existing = document.querySelector(`script[data-ytdl-src="${{src}}"]`);
        if (existing) {{
          existing.addEventListener('load', () => resolve(), {{ once: true }});
          existing.addEventListener('error', () => reject(new Error(`failed to load ${{src}}`)), {{ once: true }});
          return;
        }}
        const script = document.createElement('script');
        script.src = src;
        script.async = false;
        script.dataset.ytdlSrc = src;
        script.onload = () => resolve();
        script.onerror = () => reject(new Error(`failed to load ${{src}}`));
        document.head.appendChild(script);
      }});
      Promise.resolve()
        .then(() => loadScript('https://cdn.jsdelivr.net/npm/meriyah@6.1.4/dist/meriyah.umd.min.js'))
        .then(() => loadScript('https://cdn.jsdelivr.net/npm/astring@1.9.0/dist/astring.min.js'))
        .then(() => {{
          globalThis.meriyah = globalThis.meriyah || window.meriyah;
          globalThis.astring = globalThis.astring || window.astring;
          const coreCode = {core_code:?};
          window.__ytdlSolve = eval(`${{coreCode}}\n; jsc;`);
          window.__ytdlSolveReady = true;
          window.__ytdlSolveError = null;
        }})
        .catch((error) => {{
          window.__ytdlSolveError = String(error && error.message ? error.message : error);
        }})
        .finally(() => {{
          window.__ytdlSolverBootstrapping = false;
        }});
    }}
  }} catch (error) {{
    window.__ytdlSolveError = String(error && error.message ? error.message : error);
  }}
}})()"#,
            core_code = include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../../js/yt.solver.core.js"
            ))
        );
        window
            .eval(init_script)
            .map_err(|e| ytdl_audio::Error::Other(e.to_string()))?;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let (tx, rx) = mpsc::channel::<Result<String, String>>();
                window
                .eval_with_callback(
                    r#"(() => ({
  ready: !!window.__ytdlSolveReady,
  error: window.__ytdlSolveError || null
}))()"#,
                    move |value| {
                        let _ = tx.send(Ok(value));
                    },
                )
                .map_err(|e| ytdl_audio::Error::Other(e.to_string()))?;
            let status = rx
                .recv_timeout(std::time::Duration::from_secs(2))
                .map_err(|e| ytdl_audio::Error::Other(format!("webview solver init timeout: {}", e)))?
                .map_err(ytdl_audio::Error::Other)?;
            eprintln!("webview js runner: init_status_json={status}");
            let parsed: serde_json::Value = serde_json::from_str(&status)?;
            if parsed.get("ready").and_then(|v| v.as_bool()) == Some(true) {
                return Ok(());
            }
            if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
                return Err(ytdl_audio::Error::Other(format!("webview solver init failed: {err}")));
            }
            if std::time::Instant::now() >= deadline {
                return Err(ytdl_audio::Error::Other("webview solver init timed out".into()));
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
}

#[cfg(target_os = "android")]
fn run_hidden_android_solver(input: &str) -> Result<String, ytdl_audio::Error> {
    let vm_guard = ANDROID_ACTIVITY_VM
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| ytdl_audio::Error::Other("android vm mutex poisoned".into()))?;
    let vm = vm_guard
        .as_ref()
        .ok_or_else(|| ytdl_audio::Error::Other("android vm not initialized".into()))?;
    let activity_guard = ANDROID_ACTIVITY_GLOBAL
        .get_or_init(|| Mutex::new(None))
        .lock()
        .map_err(|_| ytdl_audio::Error::Other("android activity mutex poisoned".into()))?;
    let activity = activity_guard
        .as_ref()
        .ok_or_else(|| ytdl_audio::Error::Other("android activity not initialized".into()))?;

    let mut env = vm
        .attach_current_thread()
        .map_err(|e| ytdl_audio::Error::Other(format!("attach_current_thread failed: {}", e)))?;
    let input_java = env
        .new_string(input)
        .map_err(|e| ytdl_audio::Error::Other(format!("failed to allocate solver input string: {}", e)))?;
    let core_java = env
        .new_string(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../js/yt.solver.core.js"
        )))
        .map_err(|e| ytdl_audio::Error::Other(format!("failed to allocate solver core string: {}", e)))?;
    let result = env
        .call_method(
            activity.as_obj(),
            "runHiddenSolver",
            "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;",
            &[
                JValue::Object(&JObject::from(input_java)),
                JValue::Object(&JObject::from(core_java)),
            ],
        )
        .and_then(|v| v.l())
        .map_err(|e| ytdl_audio::Error::Other(format!("runHiddenSolver failed: {}", e)))?;
    if result.is_null() {
        return Err(ytdl_audio::Error::Other(
            "hidden android solver returned null".into(),
        ));
    }
    let result = JString::from(result);
    env.get_string(&result)
        .map(|s| s.to_string_lossy().into_owned())
        .map_err(|e| ytdl_audio::Error::Other(format!("failed to decode solver result: {}", e)))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            open_browser,
            open_browser_devtools,
            close_browser,
            export_browser_cookies,
            test_download
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn main_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    app.get_webview_window(MAIN_WINDOW_LABEL)
        .ok_or_else(|| "main window not found".to_string())
}

#[cfg(not(target_os = "android"))]
fn solver_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    if let Some(window) = app.get_webview_window(SOLVER_WINDOW_LABEL) {
        return Ok(window);
    }
    *solver_ready_state()
        .0
        .lock()
        .map_err(|_| "solver ready mutex poisoned".to_string())? = false;
    tauri::WebviewWindowBuilder::new(app, SOLVER_WINDOW_LABEL, WebviewUrl::App("index.html".into()))
        .visible(false)
        .title("solver")
        .on_page_load(|_, payload: PageLoadPayload<'_>| {
            let url = payload.url().as_str();
            eprintln!("solver window: page_load url={url}");
            if url.starts_with("tauri://")
                || url.starts_with("http://tauri.localhost")
                || url.starts_with("http://localhost:")
                || url.starts_with("http://127.0.0.1:")
            {
                let state = solver_ready_state();
                if let Ok(mut ready) = state.0.lock() {
                    *ready = true;
                    state.1.notify_all();
                }
            }
        })
        .build()
        .map_err(|e| format!("failed to create solver window: {}", e))
}

#[cfg(not(target_os = "android"))]
fn solver_ready_state() -> &'static std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)> {
    SOLVER_WINDOW_READY.get_or_init(|| std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new())))
}

#[cfg(not(target_os = "android"))]
fn wait_for_solver_window_ready(app: &tauri::AppHandle) -> Result<(), ytdl_audio::Error> {
    let _ = solver_window(app).map_err(ytdl_audio::Error::Other)?;
    let state = solver_ready_state();
    let ready = state
        .0
        .lock()
        .map_err(|_| ytdl_audio::Error::Other("solver ready mutex poisoned".into()))?;
    let (ready, timeout) = state
        .1
        .wait_timeout_while(ready, std::time::Duration::from_secs(10), |ready| !*ready)
        .map_err(|_| ytdl_audio::Error::Other("solver ready wait poisoned".into()))?;
    if *ready {
        return Ok(());
    }
    if timeout.timed_out() {
        return Err(ytdl_audio::Error::Other("solver window did not finish loading".into()));
    }
    Err(ytdl_audio::Error::Other("solver window did not become ready".into()))
}

fn ensure_browser_window(app: &tauri::AppHandle) -> Result<(), Box<dyn std::error::Error>> {
    let window = main_window(app).map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    window.set_focus().ok();
    window.navigate(url::Url::parse(YOUTUBE_URL)?)?;
    runtime_log(app, "browser replace: navigated main webview to YouTube");
    Ok(())
}

fn export_browser_cookie_jar(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let path = cookie_store_path(app)?;
    export_browser_cookie_jar_to_path(app, &path)?;
    Ok(path)
}

fn export_browser_cookie_jar_to_path(app: &tauri::AppHandle, path: &PathBuf) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        return export_android_cookie_jar_to_path(app, path);
    }

    #[cfg(not(target_os = "android"))]
    {
        let browser = main_window(app)?;
        let cookies = browser
            .cookies()
            .map_err(|e| format!("failed to read browser cookies: {}", e))?;

        let mut lines = vec![
            "# Netscape HTTP Cookie File".to_string(),
            "# Exported from Tauri webview".to_string(),
        ];
        let mut seen = HashSet::new();
        let mut exported = 0usize;

        for cookie in cookies {
            let name = cookie.name().to_string();
            let value = cookie.value().to_string();
            let domain = cookie
                .domain_raw()
                .or_else(|| cookie.domain())
                .unwrap_or("")
                .to_string();
            let path_part = cookie.path().unwrap_or("/").to_string();
            if name.is_empty() || value.is_empty() || domain.is_empty() {
                continue;
            }

            let domain_lower = domain.to_ascii_lowercase();
            if !domain_lower.contains("youtube.com") && !domain_lower.contains("google.com") {
                continue;
            }

            if !seen.insert((name.clone(), domain.clone(), path_part.clone())) {
                continue;
            }

            let secure = if cookie.secure().unwrap_or(false) { "TRUE" } else { "FALSE" };
            let include_subdomains = if domain.starts_with('.') { "TRUE" } else { "FALSE" };
            let expires = cookie.expires_datetime().map(|t| t.unix_timestamp()).unwrap_or(0);
            lines.push(format!(
                "{domain}\t{include_subdomains}\t{path_part}\t{secure}\t{expires}\t{name}\t{value}"
            ));
            exported += 1;
        }

        if exported == 0 {
            return Err("no YouTube/Google cookies found".to_string());
        }

        fs::write(path, lines.join("\n") + "\n")
            .map_err(|e| format!("failed to write cookie jar: {}", e))?;
        runtime_log(app, &format!("cookie export: wrote {}", path.display()));
        Ok(())
    }
}

#[cfg(target_os = "android")]
fn export_android_cookie_jar_to_path(app: &tauri::AppHandle, path: &PathBuf) -> Result<(), String> {
    let sources = [
        ("https://www.youtube.com/", ".youtube.com"),
        ("https://m.youtube.com/", ".youtube.com"),
        ("https://music.youtube.com/", ".youtube.com"),
        ("https://studio.youtube.com/", ".youtube.com"),
        ("https://accounts.google.com/", ".google.com"),
        ("https://accounts.youtube.com/", ".youtube.com"),
    ];
    let mut lines = vec![
        "# Netscape HTTP Cookie File".to_string(),
        "# Exported from Android CookieManager".to_string(),
    ];
    for (url, _) in sources {
        let header = android_cookie_header(url)?;
        let domain = if url.contains("google.com") {
            ".google.com"
        } else {
            ".youtube.com"
        };
        if header.is_empty() {
            continue;
        }
        lines.extend(cookie_header_to_netscape_lines(domain, &header));
    }

    if lines.len() <= 2 {
        return Err("no YouTube/Google cookies found".to_string());
    }

    fs::write(path, lines.join("\n") + "\n")
        .map_err(|e| format!("failed to write cookie jar: {}", e))?;
    runtime_log(app, &format!("cookie export: wrote {}", path.display()));
    Ok(())
}

#[cfg(target_os = "android")]
fn cookie_header_to_netscape_lines(domain: &str, header: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = HashSet::new();
    for pair in header.split(';') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() || value.is_empty() {
            continue;
        }
        let path = "/".to_string();
        if !seen.insert((name.to_string(), domain.to_string(), path.clone())) {
            continue;
        }
        lines.push(format!("{domain}\tTRUE\t{path}\tTRUE\t0\t{name}\t{value}"));
    }
    lines
}

#[cfg(target_os = "android")]
fn android_cookie_header(url: &str) -> Result<String, String> {
    let ctx = ndk_context::android_context();
    let vm = unsafe { JavaVM::from_raw(ctx.vm() as *mut _) }
        .map_err(|e| format!("Failed to get JavaVM: {}", e))?;
    let mut env = vm
        .attach_current_thread()
        .map_err(|e| format!("Failed to attach thread: {}", e))?;

    let cookie_manager_class = env
        .find_class("android/webkit/CookieManager")
        .map_err(|e| format!("Failed to find CookieManager: {}", e))?;
    let cookie_manager = env
        .call_static_method(
            cookie_manager_class,
            "getInstance",
            "()Landroid/webkit/CookieManager;",
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("Failed to get CookieManager instance: {}", e))?;

    let url_string = env
        .new_string(url)
        .map_err(|e| format!("Failed to allocate Java URL string: {}", e))?;
    let value = env
        .call_method(
            &cookie_manager,
            "getCookie",
            "(Ljava/lang/String;)Ljava/lang/String;",
            &[JValue::Object(&JObject::from(url_string))],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("Failed to read cookies for {url}: {}", e))?;

    if value.is_null() {
        return Ok(String::new());
    }

    env.get_string(&value.into())
        .map(|s| s.to_string_lossy().into_owned())
        .map_err(|e| format!("Failed to decode cookie header: {}", e))
}

fn cookie_store_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut dir = app_work_dir(app)?;
    dir.push(COOKIE_FILE_NAME);
    Ok(dir)
}

fn download_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    app_work_dir(app)
}

fn runtime_log_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let mut dir = app_work_dir(app)?;
    dir.push(RUNTIME_LOG_FILE_NAME);
    Ok(dir)
}

fn app_work_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    #[cfg(target_os = "android")]
    {
        let _ = app;
        let dir = ANDROID_EXTERNAL_DATA_DIR
            .get_or_init(|| Mutex::new(None))
            .lock()
            .map_err(|_| "android external data dir mutex poisoned".to_string())?
            .clone()
            .ok_or_else(|| "android external data dir not initialized".to_string())?;
        fs::create_dir_all(&dir).map_err(|e| format!("failed to create app work dir: {}", e))?;
        Ok(dir)
    }

    #[cfg(not(target_os = "android"))]
    {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("failed to resolve app data dir: {}", e))?;
        fs::create_dir_all(&dir).map_err(|e| format!("failed to create app data dir: {}", e))?;
        Ok(dir)
    }
}

fn runtime_log(app: &tauri::AppHandle, message: &str) {
    if let Ok(path) = runtime_log_path(app) {
        let _ = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| std::io::Write::write_all(&mut file, format!("{message}\n").as_bytes()));
    }
    eprintln!("{message}");
}

fn next_job_id(prefix: &str) -> String {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{prefix}-{ts}")
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_afeather_ytdl_1demo_MainActivity_nativeInitAndroidContext(
    mut env: jni::JNIEnv,
    this: JObject,
) {
    let raw_this = this.into_raw();
    let this_for_path = unsafe { JObject::from_raw(raw_this) };
    let this_for_global = unsafe { JObject::from_raw(raw_this) };
    if let Ok(vm) = env.get_java_vm() {
        let vm_ptr = vm.get_java_vm_pointer();
        if let Ok(mut guard) = ANDROID_ACTIVITY_VM
            .get_or_init(|| Mutex::new(None))
            .lock()
        {
            *guard = Some(vm);
        }
        unsafe {
            ndk_context::initialize_android_context(
                vm_ptr as *mut _,
                raw_this as *mut _,
            );
        }
    }
    if let Ok(global_ref) = env.new_global_ref(&this_for_global) {
        if let Ok(mut guard) = ANDROID_ACTIVITY_GLOBAL
            .get_or_init(|| Mutex::new(None))
            .lock()
        {
            *guard = Some(global_ref);
        }
    }
    if let Ok(dir) = external_files_dir(&mut env, &this_for_path) {
        let slot = ANDROID_EXTERNAL_DATA_DIR.get_or_init(|| Mutex::new(None));
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(dir);
        }
    }
}

#[cfg(target_os = "android")]
#[no_mangle]
pub extern "system" fn Java_com_afeather_ytdl_1demo_MainActivity_nativeReleaseAndroidContext(
    _env: jni::JNIEnv,
    _this: JObject,
) {
    unsafe {
        let _ = std::panic::catch_unwind(|| ndk_context::release_android_context());
    }
    if let Some(slot) = ANDROID_EXTERNAL_DATA_DIR.get() {
        if let Ok(mut guard) = slot.lock() {
            *guard = None;
        }
    }
    if let Some(slot) = ANDROID_ACTIVITY_GLOBAL.get() {
        if let Ok(mut guard) = slot.lock() {
            *guard = None;
        }
    }
    if let Some(slot) = ANDROID_ACTIVITY_VM.get() {
        if let Ok(mut guard) = slot.lock() {
            *guard = None;
        }
    }
}

#[cfg(target_os = "android")]
fn external_files_dir(env: &mut jni::JNIEnv<'_>, this: &JObject<'_>) -> Result<PathBuf, String> {
    let null_dir = env
        .call_method(
            this,
            "getExternalFilesDir",
            "(Ljava/lang/String;)Ljava/io/File;",
            &[JValue::Object(&JObject::null())],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("Failed to get external files dir: {}", e))?;
    let path_obj = env
        .call_method(
            &null_dir,
            "getAbsolutePath",
            "()Ljava/lang/String;",
            &[],
        )
        .and_then(|v| v.l())
        .map_err(|e| format!("Failed to resolve external files path: {}", e))?;
    let path = env
        .get_string(&path_obj.into())
        .map_err(|e| format!("Failed to decode external files path: {}", e))?
        .to_string_lossy()
        .into_owned();
    Ok(PathBuf::from(path))
}

#[tauri::command]
fn open_browser(app: tauri::AppHandle) -> Result<(), String> {
    runtime_log(&app, "open browser: requested");
    match ensure_browser_window(&app) {
        Ok(()) => {
            runtime_log(&app, "open browser: ok");
            Ok(())
        }
        Err(err) => {
            let message = err.to_string();
            runtime_log(&app, &format!("open browser: error {message}"));
            Err(message)
        }
    }
}

#[tauri::command]
fn open_browser_devtools(app: tauri::AppHandle) -> Result<(), String> {
    runtime_log(&app, "open browser devtools: requested");
    ensure_browser_window(&app).map_err(|e| e.to_string())?;
    #[cfg(not(target_os = "android"))]
    {
        let window = main_window(&app)?;
        window.open_devtools();
        runtime_log(&app, "open browser devtools: ok");
        return Ok(());
    }

    #[cfg(target_os = "android")]
    {
        let message = "devtools are not available on Android".to_string();
        runtime_log(&app, &format!("open browser devtools: error {message}"));
        Err(message)
    }
}

#[tauri::command]
fn close_browser(app: tauri::AppHandle) -> Result<(), String> {
    runtime_log(&app, "close browser: requested");
    let window = main_window(&app)?;
    match window.navigate(url::Url::parse("tauri://localhost").map_err(|e| e.to_string())?) {
        Ok(()) => {
            runtime_log(&app, "close browser: ok");
            Ok(())
        }
        Err(err) => {
            let message = err.to_string();
            runtime_log(&app, &format!("close browser: error {message}"));
            Err(message)
        }
    }
}

#[tauri::command]
fn export_browser_cookies(app: tauri::AppHandle) -> Result<String, String> {
    runtime_log(&app, "export cookies: requested");
    match export_browser_cookie_jar(&app) {
        Ok(path) => {
            let path_str = path.to_string_lossy().to_string();
            runtime_log(&app, &format!("export cookies: ok path={path_str}"));
            Ok(path_str)
        }
        Err(err) => {
            runtime_log(&app, &format!("export cookies: error {err}"));
            Err(err)
        }
    }
}

#[tauri::command]
async fn test_download(
    app: tauri::AppHandle,
    url: String,
    proxy: Option<String>,
) -> Result<DownloadOutcome, String> {
    runtime_log(
        &app,
        &format!("download: enter marker={} url={} proxy={:?}", BUILD_TRACE_MARKER, url, proxy),
    );
    runtime_log(&app, &format!("download: start url={}", url));
    let cookie_jar = export_browser_cookie_jar(&app).map_err(|err| {
        runtime_log(&app, &format!("download: cookie export error {err}"));
        err
    })?;
    runtime_log(&app, "download: cookie export complete");
    let cookie_jar_str = cookie_jar.to_string_lossy().to_string();
    let output_dir = download_dir(&app)?;
    let output_dir_str = output_dir.to_string_lossy().to_string();
    runtime_log(
        &app,
        &format!("download: output_dir={} cookie_jar={}", output_dir_str, cookie_jar_str),
    );
    let client = YoutubeClient::new(proxy.as_deref()).map_err(|e| e.to_string())?;
    let mut client = client;
    #[cfg(not(target_os = "android"))]
    client.set_js_runner(WebviewJsRunner { app: app.clone() });
    #[cfg(target_os = "android")]
    client.set_js_runner(WebviewJsRunner {});
    runtime_log(&app, "download: js runner set to webview");
    runtime_log(&app, "download: calling yt-dlp-rs download_with_progress()");
    let request = DownloadRequest {
        job_id: next_job_id("youtube"),
        url: url.clone(),
        opts: DownloadOpts {
            output_dir: output_dir_str.clone(),
            cookies: Some(cookie_jar_str.clone()),
            ..Default::default()
        },
    };
    let reporter = std::sync::Arc::new(TauriProgressReporter::new(app.clone()));
    let result = client
        .download_with_progress(&request, reporter)
        .await
        .map_err(|e| {
            let msg = e.to_string();
            runtime_log(&app, &format!("download: yt-dlp-rs error {msg}"));
            msg
        })?;
    runtime_log(&app, &format!("download: finished audio={}", result.audio_path.display()));

    Ok(DownloadOutcome {
        audio_path: result.audio_path.to_string_lossy().to_string(),
        subtitle_paths: result
            .subtitle_paths
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect(),
        thumbnail_path: result.thumbnail_path.map(|p| p.to_string_lossy().to_string()),
        cookie_jar: cookie_jar_str,
        output_dir: output_dir_str,
    })
}

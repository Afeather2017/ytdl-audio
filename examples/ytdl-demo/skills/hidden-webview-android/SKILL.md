---
name: hidden-webview-android
description: Use when implementing Android-only hidden WebView execution from Rust or Tauri to run browser-bound JavaScript such as YouTube solver code without blocking or depending on the visible app WebView.
---

# Hidden WebView Android

Use this pattern when Android logic needs a real `android.webkit.WebView` JS environment, but the visible app webview must stay uninvolved.

## When to use

- JS depends on browser/WebView runtime behavior
- Work should not run in the visible Tauri/UI webview
- Rust needs to call Android-side JS execution
- Release builds may minify reflective entrypoints

## Architecture

For `ytdl-demo`, the split is:

- Rust owns downloader flow and calls a `JsRunner`
- Android owns a hidden native `WebView`
- JNI bridges Rust to `MainActivity.runHiddenSolver(...)`
- The hidden WebView loads, initializes solver JS, then evaluates requests

Relevant files:

- [src-tauri/src/lib.rs](/home/afeather/Codes/yt-dlp/ytdl-demo/src-tauri/src/lib.rs)
- [src-tauri/gen/android/app/src/main/java/com/afeather/ytdl_demo/MainActivity.kt](/home/afeather/Codes/yt-dlp/ytdl-demo/src-tauri/gen/android/app/src/main/java/com/afeather/ytdl_demo/MainActivity.kt)
- [src-tauri/gen/android/app/proguard-rules.pro](/home/afeather/Codes/yt-dlp/ytdl-demo/src-tauri/gen/android/app/proguard-rules.pro)

## Implementation pattern

1. Keep Android-only global JNI state in Rust.
   Store `JavaVM` and a `GlobalRef` to the activity during `nativeInitAndroidContext`.

2. Expose a normal Java/Kotlin instance method on the activity.
   Example: `fun runHiddenSolver(inputJson: String, coreCode: String): String?`

3. In that activity, create one offscreen `WebView`.
   Requirements:
   - create it on the main thread
   - keep it unattached to the view hierarchy
   - track `ready/loading` with a lock
   - destroy it in `onDestroy`

4. Load a tiny HTML shell with `loadDataWithBaseURL(...)`.
   Use a YouTube-like base URL when same-origin behavior matters.

5. Evaluate JS through `evaluateJavascript(...)`.
   Convert the callback result into a blocking `CompletableFuture`/timeout wrapper.

6. From Rust, call the Java method through JNI inside the Android `JsRunner`.
   Pass:
   - the solver request JSON
   - the solver core JS source

7. Protect the reflective entrypoint from R8/ProGuard.
   Keep `runHiddenSolver` explicitly or release builds will crash with `NoSuchMethodError`.

## Critical details

- Do not create or drive `WebView` off the Android main thread.
- Use a `GlobalRef` for the activity; a local JNI ref is not sufficient.
- If Rust reaches Java through `call_method`, minification can strip the target method.
- Hidden WebView readiness must be separate from solver readiness.
- Time out JS evaluation. Do not block forever waiting on WebView callbacks.

## Release-build rule

Keep reflective JNI targets:

```pro
-keepclassmembers class com.afeather.ytdl_demo.MainActivity {
    public java.lang.String runHiddenSolver(java.lang.String, java.lang.String);
}
```

## Tauri-specific notes

- On desktop, `ytdl-demo` can still use a Tauri webview window solver path.
- On Android, prefer the hidden native `WebView` path instead of the visible Tauri webview.
- This keeps solver work isolated from the user-facing browser state.

## Current limitation in `ytdl-demo`

The hidden solver currently loads `meriyah` and `astring` from CDN URLs at runtime.

If you need reliability offline or under restricted networks:

- vendor those JS files into the APK
- load them from app assets instead of the network

## Validation

Use:

```bash
cd ytdl-demo/src-tauri
cargo check
```

Then build/install:

```bash
cd ytdl-demo
./build-android.sh
```

If release crashes with missing-method errors, check shrinker rules first.

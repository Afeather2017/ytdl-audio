package com.afeather.ytdl_demo

import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.webkit.WebResourceError
import android.webkit.WebResourceRequest
import android.webkit.WebResourceResponse
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.activity.enableEdgeToEdge
import org.json.JSONObject
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

class MainActivity : TauriActivity() {
  private companion object {
    private const val HIDDEN_SOLVER_USER_AGENT =
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " +
        "(KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36"
    private const val HIDDEN_SOLVER_HTML = """
      <!DOCTYPE html>
      <html lang="en">
      <head>
        <meta charset="utf-8" />
        <script src="https://cdn.jsdelivr.net/npm/meriyah@6.1.4/dist/meriyah.umd.min.js"></script>
        <script src="https://cdn.jsdelivr.net/npm/astring@1.9.0/dist/astring.min.js"></script>
      </head>
      <body></body>
      </html>
    """
  }

  private external fun nativeInitAndroidContext()
  private external fun nativeReleaseAndroidContext()
  private val mainHandler = Handler(Looper.getMainLooper())
  private val hiddenSolverLock = Object()
  private var hiddenSolverWebView: WebView? = null
  private var hiddenSolverReady = false
  private var hiddenSolverLoading = false

  fun runHiddenSolver(inputJson: String, coreCode: String): String? {
    if (!awaitHiddenSolverReady(30_000L)) {
      return null
    }
    val initStatus = evaluateHiddenSolver(
      """
        (() => {
          if (window.__ytdlSolveReady) {
            return { ready: true, error: null };
          }
          if (!window.meriyah || !window.astring) {
            return { ready: false, error: "solver dependencies not loaded" };
          }
          try {
            const coreCode = ${JSONObject.quote(coreCode)};
            window.__ytdlSolve = eval(`${'$'}{coreCode}\n; jsc;`);
            window.__ytdlSolveReady = true;
            window.__ytdlSolveError = null;
            return { ready: true, error: null };
          } catch (error) {
            const message = String(error && error.stack ? error.stack : error);
            window.__ytdlSolveError = message;
            return { ready: false, error: message };
          }
        })()
      """.trimIndent(),
      30_000L,
    ) ?: return null
    val statusJson = JSONObject(initStatus)
    if (!statusJson.optBoolean("ready")) {
      return null
    }
    return evaluateHiddenSolver(
      """
        (() => {
          try {
            const input = JSON.parse(${JSONObject.quote(inputJson)});
            return window.__ytdlSolve(input);
          } catch (error) {
            return {
              type: "error",
              error: String(error && error.stack ? error.stack : error),
            };
          }
        })()
      """.trimIndent(),
      30_000L,
    )
  }

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    nativeInitAndroidContext()
  }

  override fun onDestroy() {
    destroyHiddenSolverWebView()
    nativeReleaseAndroidContext()
    super.onDestroy()
  }

  private fun awaitHiddenSolverReady(timeoutMs: Long): Boolean {
    if (Looper.myLooper() == Looper.getMainLooper()) {
      ensureHiddenSolverLoadedOnMainThread()
      synchronized(hiddenSolverLock) {
        return hiddenSolverReady && hiddenSolverWebView != null
      }
    }
    val deadline = SystemClock.uptimeMillis() + timeoutMs
    synchronized(hiddenSolverLock) {
      mainHandler.post { ensureHiddenSolverLoadedOnMainThread() }
      while (!hiddenSolverReady || hiddenSolverWebView == null) {
        val remainingMs = deadline - SystemClock.uptimeMillis()
        if (remainingMs <= 0L) {
          return false
        }
        try {
          hiddenSolverLock.wait(remainingMs)
        } catch (_: InterruptedException) {
          Thread.currentThread().interrupt()
          return false
        }
      }
      return true
    }
  }

  private fun ensureHiddenSolverLoadedOnMainThread() {
    check(Looper.myLooper() == Looper.getMainLooper())
    val solverWebView: WebView
    synchronized(hiddenSolverLock) {
      if (hiddenSolverWebView == null) {
        hiddenSolverWebView = WebView(this)
        configureHiddenSolverWebView(hiddenSolverWebView!!)
      }
      if (hiddenSolverReady || hiddenSolverLoading) {
        return
      }
      solverWebView = hiddenSolverWebView!!
      hiddenSolverReady = false
      hiddenSolverLoading = true
    }
    solverWebView.loadDataWithBaseURL(
      "https://www.youtube.com",
      HIDDEN_SOLVER_HTML,
      "text/html",
      "utf-8",
      null,
    )
  }

  private fun configureHiddenSolverWebView(webView: WebView) {
    val settings = webView.settings
    settings.javaScriptEnabled = true
    settings.domStorageEnabled = false
    settings.databaseEnabled = false
    settings.cacheMode = WebSettings.LOAD_DEFAULT
    settings.userAgentString = HIDDEN_SOLVER_USER_AGENT
    settings.blockNetworkLoads = false
    webView.setWillNotDraw(true)
    webView.webViewClient = object : WebViewClient() {
      override fun onPageFinished(view: WebView, url: String) {
        super.onPageFinished(view, url)
        synchronized(hiddenSolverLock) {
          if (view !== hiddenSolverWebView) {
            return
          }
          hiddenSolverLoading = false
          hiddenSolverReady = true
          hiddenSolverLock.notifyAll()
        }
      }

      override fun onReceivedHttpError(
        view: WebView,
        request: WebResourceRequest,
        errorResponse: WebResourceResponse,
      ) {
        super.onReceivedHttpError(view, request, errorResponse)
        if (request.isForMainFrame) {
          onHiddenSolverLoadFailed(view)
        }
      }

      override fun onReceivedError(
        view: WebView,
        request: WebResourceRequest,
        error: WebResourceError,
      ) {
        super.onReceivedError(view, request, error)
        if (request.isForMainFrame) {
          onHiddenSolverLoadFailed(view)
        }
      }
    }
  }

  private fun onHiddenSolverLoadFailed(view: WebView) {
    synchronized(hiddenSolverLock) {
      if (view !== hiddenSolverWebView) {
        return
      }
      hiddenSolverLoading = false
      hiddenSolverReady = false
      hiddenSolverLock.notifyAll()
    }
  }

  private fun evaluateHiddenSolver(script: String, timeoutMs: Long): String? {
    val future = CompletableFuture<String?>()
    mainHandler.post {
      val solverWebView = synchronized(hiddenSolverLock) {
        hiddenSolverWebView.takeIf { hiddenSolverReady }
      }
      if (solverWebView == null) {
        future.complete(null)
        return@post
      }
      solverWebView.evaluateJavascript(script) { value ->
        future.complete(value)
      }
    }
    return try {
      future.get(timeoutMs, TimeUnit.MILLISECONDS)
    } catch (_: Exception) {
      future.cancel(true)
      null
    }
  }

  private fun destroyHiddenSolverWebView() {
    if (Looper.myLooper() == Looper.getMainLooper()) {
      destroyHiddenSolverWebViewOnMainThread()
      return
    }
    mainHandler.post { destroyHiddenSolverWebViewOnMainThread() }
  }

  private fun destroyHiddenSolverWebViewOnMainThread() {
    val solverWebView = synchronized(hiddenSolverLock) {
      val view = hiddenSolverWebView
      hiddenSolverWebView = null
      hiddenSolverReady = false
      hiddenSolverLoading = false
      hiddenSolverLock.notifyAll()
      view
    }
    solverWebView?.apply {
      stopLoading()
      destroy()
    }
  }
}

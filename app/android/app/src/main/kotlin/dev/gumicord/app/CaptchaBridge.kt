package dev.gumicord.app

import android.webkit.JavascriptInterface

// Answer object for the captcha challenge page (ADR-0015). The page calls
// post() with `type:payload`; Rust polls takeMessage() from its nested pump
// and parses it there. Judgement stays on the Rust side; this only carries
// strings between the WebView's thread and the UI thread.
class CaptchaBridge {
    private val queue: ArrayDeque<String> = ArrayDeque()

    @JavascriptInterface
    fun post(message: String) {
        synchronized(queue) { queue.add(message) }
    }

    @Synchronized
    fun pollMessage(): String? = if (queue.isEmpty()) null else queue.removeFirst()
}

package dev.veloxdesk.input

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.util.Log

/**
 * Bidirectional Android clipboard synchronization controller with echo-loop suppression
 * and resilient handling of Android 10+ background clipboard access restrictions.
 */
class ClipboardController(
    context: Context,
    private val onLocalTextChanged: (String) -> Unit,
) {
    private val appContext = context.applicationContext
    private val clipboardManager: ClipboardManager? =
        appContext.getSystemService(Context.CLIPBOARD_SERVICE) as? ClipboardManager

    private val lock = Any()
    private var started: Boolean = false
    private var enabled: Boolean = true
    private var lastSyncedText: String? = null

    private val clipChangedListener = ClipboardManager.OnPrimaryClipChangedListener {
        val textToSend: String? = synchronized(lock) {
            if (!started || !enabled) return@synchronized null
            val text = readPrimaryClipText() ?: return@synchronized null
            if (text == lastSyncedText) {
                return@synchronized null
            }
            lastSyncedText = text
            text
        }
        if (textToSend != null) {
            try {
                onLocalTextChanged(textToSend)
            } catch (t: Throwable) {
                Log.w(TAG, "onLocalTextChanged threw: ${t.message}")
            }
        }
    }

    fun start() {
        val cm = clipboardManager ?: return
        synchronized(lock) {
            if (started) return
            started = true
            // Seed lastSyncedText with current clip so we don't fire stale clipboard content immediately.
            lastSyncedText = readPrimaryClipText()
            try {
                cm.addPrimaryClipChangedListener(clipChangedListener)
            } catch (t: Throwable) {
                started = false
                Log.w(TAG, "Failed registering clipboard listener: ${t.message}")
            }
        }
    }

    fun stop() {
        val cm = clipboardManager ?: return
        synchronized(lock) {
            if (!started) return
            started = false
            try {
                cm.removePrimaryClipChangedListener(clipChangedListener)
            } catch (t: Throwable) {
                Log.w(TAG, "Failed removing clipboard listener: ${t.message}")
            }
        }
    }

    fun setEnabled(enabled: Boolean) {
        synchronized(lock) {
            this.enabled = enabled
        }
    }

    fun onRemoteClipboardText(text: String) {
        val cm = clipboardManager ?: return
        synchronized(lock) {
            if (!enabled) return
            if (text == lastSyncedText) return
            // Update echo-suppression cache before mutating system clipboard so our listener ignores it.
            lastSyncedText = text
        }
        try {
            cm.setPrimaryClip(ClipData.newPlainText(CLIP_LABEL, text))
        } catch (se: SecurityException) {
            Log.w(TAG, "Clipboard write blocked by Android security policy: ${se.message}")
        } catch (t: Throwable) {
            Log.w(TAG, "Clipboard write failed: ${t.message}")
        }
    }

    private fun readPrimaryClipText(): String? {
        val cm = clipboardManager ?: return null
        return try {
            val clip = cm.primaryClip ?: return null
            if (clip.itemCount <= 0) return null
            clip.getItemAt(0)?.coerceToText(appContext)?.toString()
        } catch (se: SecurityException) {
            Log.d(TAG, "Clipboard read blocked in background: ${se.message}")
            null
        } catch (t: Throwable) {
            Log.w(TAG, "Clipboard read failed: ${t.message}")
            null
        }
    }

    companion object {
        private const val TAG = "ClipboardController"
        private const val CLIP_LABEL = "VeloxDesk"
    }
}

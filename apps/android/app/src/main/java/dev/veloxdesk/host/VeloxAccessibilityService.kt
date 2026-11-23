package dev.veloxdesk.host

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.graphics.Path
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.util.DisplayMetrics
import android.util.Log
import android.view.WindowManager
import android.view.accessibility.AccessibilityEvent
import android.view.accessibility.AccessibilityNodeInfo

/**
 * Accessibility service enabling remote touch gestures, scrolling, navigation (Back/Home/Recents),
 * and text input when this Android device is hosting a VeloxDesk session.
 *
 * View-only screen sharing works without enabling this service; remote control requires the user
 * to explicitly enable VeloxDesk in Android Accessibility Settings.
 */
class VeloxAccessibilityService : AccessibilityService() {
    private val mainHandler = Handler(Looper.getMainLooper())
    private val mapper = HostInputMapper()

    override fun onServiceConnected() {
        super.onServiceConnected()
        instance = this
        HostController.onAccessibilityStateChanged(true)
        Log.i(TAG, "VeloxAccessibilityService connected")
    }

    override fun onAccessibilityEvent(event: AccessibilityEvent?) {
        // We only inject gestures and actions on demand; no passive event collection.
    }

    override fun onInterrupt() {
        mapper.reset()
    }

    override fun onUnbind(intent: Intent?): Boolean {
        if (instance === this) {
            instance = null
        }
        mapper.reset()
        HostController.onAccessibilityStateChanged(false)
        Log.i(TAG, "VeloxAccessibilityService disconnected")
        return super.onUnbind(intent)
    }

    override fun onDestroy() {
        if (instance === this) {
            instance = null
        }
        mapper.reset()
        HostController.onAccessibilityStateChanged(false)
        super.onDestroy()
    }

    private fun handleRemoteInput(
        kind: Int,
        x: Int,
        y: Int,
        code: Int,
        down: Boolean,
        modifiers: Int,
        dx: Int,
        dy: Int,
        text: String,
        srcWidth: Int,
        srcHeight: Int,
    ) {
        val (screenW, screenH) = currentPhysicalScreenSize()
        val nowMs = SystemClock.uptimeMillis()
        val actions = mapper.onEvent(
            kind = kind,
            x = x,
            y = y,
            code = code,
            down = down,
            modifiers = modifiers,
            dx = dx,
            dy = dy,
            text = text,
            srcWidth = srcWidth,
            srcHeight = srcHeight,
            screenWidth = screenW,
            screenHeight = screenH,
            nowMs = nowMs,
        )
        for (action in actions) {
            executeAction(action)
        }
    }

    private fun executeAction(action: HostAccessibilityAction) {
        try {
            when (action) {
                is HostAccessibilityAction.Tap -> {
                    val path = Path().apply { moveTo(action.x, action.y) }
                    val stroke = GestureDescription.StrokeDescription(path, 0L, action.durationMs)
                    val gesture = GestureDescription.Builder().addStroke(stroke).build()
                    dispatchGesture(gesture, null, null)
                }

                is HostAccessibilityAction.Drag -> {
                    if (action.points.size < 2) return
                    val path = Path().apply {
                        val first = action.points.first()
                        moveTo(first.x, first.y)
                        for (i in 1 until action.points.size) {
                            val pt = action.points[i]
                            lineTo(pt.x, pt.y)
                        }
                    }
                    val stroke = GestureDescription.StrokeDescription(path, 0L, action.durationMs)
                    val gesture = GestureDescription.Builder().addStroke(stroke).build()
                    dispatchGesture(gesture, null, null)
                }

                is HostAccessibilityAction.Scroll -> {
                    val path = Path().apply {
                        moveTo(action.startX, action.startY)
                        lineTo(action.endX, action.endY)
                    }
                    val stroke = GestureDescription.StrokeDescription(path, 0L, action.durationMs)
                    val gesture = GestureDescription.Builder().addStroke(stroke).build()
                    dispatchGesture(gesture, null, null)
                }

                is HostAccessibilityAction.Global -> {
                    val globalCode = when (action.action) {
                        HostGlobalAction.BACK -> GLOBAL_ACTION_BACK
                        HostGlobalAction.HOME -> GLOBAL_ACTION_HOME
                        HostGlobalAction.RECENTS -> GLOBAL_ACTION_RECENTS
                    }
                    performGlobalAction(globalCode)
                }

                is HostAccessibilityAction.InsertText,
                HostAccessibilityAction.Backspace,
                -> applyEditableNodeText(action)

                HostAccessibilityAction.Enter -> applyEditableNodeEnter()
            }
        } catch (t: Throwable) {
            Log.w(TAG, "Failed executing accessibility action $action: ${t.message}")
        }
    }

    private fun applyEditableNodeText(action: HostAccessibilityAction) {
        val node = findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: return
        try {
            if (!node.isEditable) return
            val existing = node.text?.toString() ?: ""
            // Avoid accidentally appending onto placeholder hint text.
            val current = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && node.isShowingHintText) {
                ""
            } else {
                existing
            }
            val updated = HostInputMapper.applyTextEdit(current, action) ?: return
            val args = Bundle().apply {
                putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, updated)
            }
            node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
        } finally {
            @Suppress("DEPRECATION")
            node.recycle()
        }
    }

    private fun applyEditableNodeEnter() {
        val node = findFocus(AccessibilityNodeInfo.FOCUS_INPUT) ?: return
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                val ok = node.performAction(AccessibilityNodeInfo.AccessibilityAction.ACTION_IME_ENTER.id)
                if (ok) return
            }
            if (node.isEditable) {
                val existing = node.text?.toString() ?: ""
                val current = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && node.isShowingHintText) {
                    ""
                } else {
                    existing
                }
                val args = Bundle().apply {
                    putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, "$current\n")
                }
                node.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, args)
            }
        } finally {
            @Suppress("DEPRECATION")
            node.recycle()
        }
    }

    private fun currentPhysicalScreenSize(): Pair<Int, Int> {
        val wm = getSystemService(Context.WINDOW_SERVICE) as? WindowManager
            ?: return Pair(1080, 1920)
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val bounds = wm.currentWindowMetrics.bounds
            Pair(bounds.width().coerceAtLeast(1), bounds.height().coerceAtLeast(1))
        } else {
            val dm = DisplayMetrics()
            @Suppress("DEPRECATION")
            wm.defaultDisplay?.getRealMetrics(dm)
            Pair(dm.widthPixels.coerceAtLeast(1), dm.heightPixels.coerceAtLeast(1))
        }
    }

    companion object {
        private const val TAG = "VeloxAccessibility"

        @Volatile
        private var instance: VeloxAccessibilityService? = null

        fun isEnabled(context: Context): Boolean {
            if (instance != null) return true
            return try {
                val expected = ComponentName(context, VeloxAccessibilityService::class.java)
                val enabled = Settings.Secure.getString(
                    context.contentResolver,
                    Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES,
                ) ?: return false
                enabled.split(':').any { raw ->
                    val comp = ComponentName.unflattenFromString(raw.trim())
                    comp == expected
                }
            } catch (_: Throwable) {
                false
            }
        }

        fun openSettings(context: Context) {
            try {
                val intent = Intent(Settings.ACTION_ACCESSIBILITY_SETTINGS).apply {
                    addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                }
                context.startActivity(intent)
            } catch (t: Throwable) {
                Log.w(TAG, "Failed opening accessibility settings: ${t.message}")
            }
        }

        fun resetState() {
            val svc = instance ?: return
            svc.mainHandler.post { svc.mapper.reset() }
        }

        fun dispatchRemoteInput(
            kind: Int,
            x: Int,
            y: Int,
            code: Int,
            down: Boolean,
            modifiers: Int,
            dx: Int,
            dy: Int,
            text: String,
            srcWidth: Int,
            srcHeight: Int,
        ) {
            val svc = instance ?: return
            svc.mainHandler.post {
                svc.handleRemoteInput(kind, x, y, code, down, modifiers, dx, dy, text, srcWidth, srcHeight)
            }
        }
    }
}

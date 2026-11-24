package dev.veloxdesk.host

import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.input.KeyMapper
import kotlin.math.hypot
import kotlin.math.sign

data class ScreenPoint(val x: Float, val y: Float)

enum class HostGlobalAction {
    BACK,
    HOME,
    RECENTS,
}

sealed interface HostAccessibilityAction {
    data class Tap(val x: Float, val y: Float, val durationMs: Long) : HostAccessibilityAction
    data class Drag(val points: List<ScreenPoint>, val durationMs: Long) : HostAccessibilityAction
    data class Scroll(
        val startX: Float,
        val startY: Float,
        val endX: Float,
        val endY: Float,
        val durationMs: Long,
    ) : HostAccessibilityAction
    data class Global(val action: HostGlobalAction) : HostAccessibilityAction
    data class InsertText(val text: String) : HostAccessibilityAction
    data object Backspace : HostAccessibilityAction
    data object Enter : HostAccessibilityAction
}

/**
 * Pure Kotlin state machine converting remote VeloxDesk input events (encoded-frame coordinates,
 * mouse buttons, scroll wheel, USB HID usage codes, and UTF-8 text) into Android Accessibility
 * gesture, global-action, and text-editing commands.
 */
class HostInputMapper {
    private var cursorX: Float = 0f
    private var cursorY: Float = 0f
    private var cursorInitialized: Boolean = false

    private var leftDown: Boolean = false
    private var downTimeMs: Long = 0L
    private var downPoint: ScreenPoint = ScreenPoint(0f, 0f)
    private val dragPoints = mutableListOf<ScreenPoint>()

    fun reset() {
        leftDown = false
        dragPoints.clear()
        cursorInitialized = false
    }

    fun onEvent(
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
        screenWidth: Int,
        screenHeight: Int,
        nowMs: Long,
    ): List<HostAccessibilityAction> {
        val sw = screenWidth.coerceAtLeast(1)
        val sh = screenHeight.coerceAtLeast(1)
        if (!cursorInitialized) {
            cursorX = sw * 0.5f
            cursorY = sh * 0.5f
            cursorInitialized = true
        }

        return when (kind) {
            BridgeConstants.HOST_INPUT_MOUSE_MOVE -> {
                val mapped = mapToScreen(x, y, srcWidth, srcHeight, sw, sh)
                cursorX = mapped.x
                cursorY = mapped.y
                if (leftDown) {
                    appendDragPoint(mapped)
                }
                emptyList()
            }

            BridgeConstants.HOST_INPUT_MOUSE_RELATIVE -> {
                cursorX = (cursorX + dx).coerceIn(0f, (sw - 1).coerceAtLeast(0).toFloat())
                cursorY = (cursorY + dy).coerceIn(0f, (sh - 1).coerceAtLeast(0).toFloat())
                if (leftDown) {
                    appendDragPoint(ScreenPoint(cursorX, cursorY))
                }
                emptyList()
            }

            BridgeConstants.HOST_INPUT_MOUSE_BUTTON -> {
                handleMouseButton(code, down, nowMs)
            }

            BridgeConstants.HOST_INPUT_MOUSE_WHEEL -> {
                handleMouseWheel(dx, dy, sw, sh)
            }

            BridgeConstants.HOST_INPUT_KEY -> {
                handleKey(code, down, modifiers)
            }

            BridgeConstants.HOST_INPUT_TEXT -> {
                if (text.isNotEmpty()) {
                    listOf(HostAccessibilityAction.InsertText(text))
                } else {
                    emptyList()
                }
            }

            else -> emptyList()
        }
    }

    private fun appendDragPoint(point: ScreenPoint) {
        val last = dragPoints.lastOrNull()
        if (last == null || hypot((point.x - last.x).toDouble(), (point.y - last.y).toDouble()) >= 2.0) {
            if (dragPoints.size < MAX_DRAG_POINTS) {
                dragPoints.add(point)
            } else {
                dragPoints[dragPoints.lastIndex] = point
            }
        }
    }

    private fun handleMouseButton(code: Int, down: Boolean, nowMs: Long): List<HostAccessibilityAction> {
        return when (code) {
            BridgeConstants.HOST_MOUSE_BTN_LEFT -> {
                if (down) {
                    leftDown = true
                    downTimeMs = nowMs
                    downPoint = ScreenPoint(cursorX, cursorY)
                    dragPoints.clear()
                    dragPoints.add(downPoint)
                    emptyList()
                } else if (leftDown) {
                    leftDown = false
                    val upPoint = ScreenPoint(cursorX, cursorY)
                    if (dragPoints.isEmpty() || dragPoints.last() != upPoint) {
                        dragPoints.add(upPoint)
                    }
                    val elapsedMs = (nowMs - downTimeMs).coerceIn(1L, 2_000L)
                    val maxDist = dragPoints.maxOfOrNull {
                        hypot((it.x - downPoint.x).toDouble(), (it.y - downPoint.y).toDouble())
                    } ?: 0.0
                    val snapshot = dragPoints.toList()
                    dragPoints.clear()
                    if (maxDist < TAP_SLOP_PX) {
                        val tapDuration = if (elapsedMs >= LONG_PRESS_THRESHOLD_MS) {
                            elapsedMs.coerceIn(500L, 1_200L)
                        } else {
                            DEFAULT_TAP_MS
                        }
                        listOf(HostAccessibilityAction.Tap(upPoint.x, upPoint.y, tapDuration))
                    } else {
                        val dragDuration = elapsedMs.coerceIn(MIN_DRAG_MS, MAX_DRAG_MS)
                        listOf(HostAccessibilityAction.Drag(snapshot, dragDuration))
                    }
                } else {
                    emptyList()
                }
            }

            BridgeConstants.HOST_MOUSE_BTN_RIGHT -> {
                if (down) listOf(HostAccessibilityAction.Global(HostGlobalAction.BACK)) else emptyList()
            }

            BridgeConstants.HOST_MOUSE_BTN_MIDDLE -> {
                if (down) listOf(HostAccessibilityAction.Global(HostGlobalAction.HOME)) else emptyList()
            }

            else -> emptyList()
        }
    }

    private fun handleMouseWheel(dx: Int, dy: Int, screenW: Int, screenH: Int): List<HostAccessibilityAction> {
        if (dy == 0 && dx == 0) return emptyList()
        val maxY = (screenH - 1).coerceAtLeast(1).toFloat()
        val maxX = (screenW - 1).coerceAtLeast(1).toFloat()
        val marginY = (screenH * 0.08f).coerceAtLeast(16f).coerceAtMost(maxY * 0.4f)
        val marginX = (screenW * 0.08f).coerceAtLeast(16f).coerceAtMost(maxX * 0.4f)
        val cx = cursorX.coerceIn(marginX, (maxX - marginX).coerceAtLeast(marginX))
        val cy = cursorY.coerceIn(marginY, (maxY - marginY).coerceAtLeast(marginY))

        if (dy != 0) {
            val units = (kotlin.math.abs(dy).toFloat() / 120f).coerceIn(0.5f, 3.0f)
            val dist = (screenH * 0.16f * units).coerceIn(80f, screenH * 0.35f)
            // Positive wheel dy scrolls content up => finger swipes downward (+y).
            val dir = sign(dy.toFloat())
            val half = dist * 0.5f
            val startY = (cy - dir * half).coerceIn(marginY, (maxY - marginY).coerceAtLeast(marginY))
            val endY = (cy + dir * half).coerceIn(marginY, (maxY - marginY).coerceAtLeast(marginY))
            if (kotlin.math.abs(endY - startY) < 4f) return emptyList()
            return listOf(HostAccessibilityAction.Scroll(cx, startY, cx, endY, SCROLL_DURATION_MS))
        }

        val units = (kotlin.math.abs(dx).toFloat() / 120f).coerceIn(0.5f, 3.0f)
        val dist = (screenW * 0.16f * units).coerceIn(80f, screenW * 0.35f)
        val dir = sign(dx.toFloat())
        val half = dist * 0.5f
        val startX = (cx - dir * half).coerceIn(marginX, (maxX - marginX).coerceAtLeast(marginX))
        val endX = (cx + dir * half).coerceIn(marginX, (maxX - marginX).coerceAtLeast(marginX))
        if (kotlin.math.abs(endX - startX) < 4f) return emptyList()
        return listOf(HostAccessibilityAction.Scroll(startX, cy, endX, cy, SCROLL_DURATION_MS))
    }

    private fun handleKey(hidUsage: Int, down: Boolean, modifiers: Int): List<HostAccessibilityAction> {
        if (!down) return emptyList()
        val hasAlt = (modifiers and BridgeConstants.MOD_ALT) != 0
        val hasCtrl = (modifiers and BridgeConstants.MOD_CTRL) != 0
        val hasMeta = (modifiers and BridgeConstants.MOD_META) != 0
        val hasShift = (modifiers and BridgeConstants.MOD_SHIFT) != 0

        return when (hidUsage) {
            KeyMapper.HID_ESCAPE -> listOf(HostAccessibilityAction.Global(HostGlobalAction.BACK))
            KeyMapper.HID_LEFT_GUI, KeyMapper.HID_RIGHT_GUI ->
                listOf(HostAccessibilityAction.Global(HostGlobalAction.HOME))
            KeyMapper.HID_TAB -> {
                if (hasAlt) {
                    listOf(HostAccessibilityAction.Global(HostGlobalAction.RECENTS))
                } else {
                    emptyList()
                }
            }
            KeyMapper.HID_BACKSPACE -> listOf(HostAccessibilityAction.Backspace)
            KeyMapper.HID_ENTER -> listOf(HostAccessibilityAction.Enter)
            else -> {
                if (hasCtrl || hasAlt || hasMeta) {
                    emptyList()
                } else {
                    hidUsageToChar(hidUsage, hasShift)?.let {
                        listOf(HostAccessibilityAction.InsertText(it.toString()))
                    } ?: emptyList()
                }
            }
        }
    }

    companion object {
        private const val TAP_SLOP_PX = 12.0
        private const val LONG_PRESS_THRESHOLD_MS = 450L
        private const val DEFAULT_TAP_MS = 50L
        private const val MIN_DRAG_MS = 80L
        private const val MAX_DRAG_MS = 1_000L
        private const val SCROLL_DURATION_MS = 160L
        private const val MAX_DRAG_POINTS = 32

        fun mapToScreen(
            x: Int,
            y: Int,
            srcWidth: Int,
            srcHeight: Int,
            screenWidth: Int,
            screenHeight: Int,
        ): ScreenPoint {
            val dstW = screenWidth.coerceAtLeast(1)
            val dstH = screenHeight.coerceAtLeast(1)
            val sx = if (srcWidth > 0) x.toFloat() * dstW.toFloat() / srcWidth.toFloat() else x.toFloat()
            val sy = if (srcHeight > 0) y.toFloat() * dstH.toFloat() / srcHeight.toFloat() else y.toFloat()
            return ScreenPoint(
                x = sx.coerceIn(0f, (dstW - 1).coerceAtLeast(0).toFloat()),
                y = sy.coerceIn(0f, (dstH - 1).coerceAtLeast(0).toFloat()),
            )
        }

        fun hidUsageToChar(hidUsage: Int, shift: Boolean): Char? = when (hidUsage) {
            in 0x04..0x1D -> {
                val base = ('a'.code + (hidUsage - 0x04)).toChar()
                if (shift) base.uppercaseChar() else base
            }
            0x1E -> if (shift) '!' else '1'
            0x1F -> if (shift) '@' else '2'
            0x20 -> if (shift) '#' else '3'
            0x21 -> if (shift) '$' else '4'
            0x22 -> if (shift) '%' else '5'
            0x23 -> if (shift) '^' else '6'
            0x24 -> if (shift) '&' else '7'
            0x25 -> if (shift) '*' else '8'
            0x26 -> if (shift) '(' else '9'
            0x27 -> if (shift) ')' else '0'
            0x2C -> ' '
            0x2D -> if (shift) '_' else '-'
            0x2E -> if (shift) '+' else '='
            0x2F -> if (shift) '{' else '['
            0x30 -> if (shift) '}' else ']'
            0x31 -> if (shift) '|' else '\\'
            0x33 -> if (shift) ':' else ';'
            0x34 -> if (shift) '"' else '\''
            0x35 -> if (shift) '~' else '`'
            0x36 -> if (shift) '<' else ','
            0x37 -> if (shift) '>' else '.'
            0x38 -> if (shift) '?' else '/'
            else -> null
        }

        /** Apply a text mutation (`InsertText` or `Backspace`) cleanly to an existing string. */
        fun applyTextEdit(current: String, action: HostAccessibilityAction): String? = when (action) {
            is HostAccessibilityAction.InsertText -> current + action.text
            HostAccessibilityAction.Backspace -> if (current.isNotEmpty()) current.dropLast(1) else ""
            else -> null
        }
    }
}

package dev.veloxdesk.input

import android.os.Handler
import android.os.Looper
import android.view.MotionEvent
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.TouchMode
import kotlin.math.abs
import kotlin.math.floor
import kotlin.math.hypot
import kotlin.math.min
import kotlin.math.truncate

data class ViewportTransform(
    val zoom: Float = 1f,
    val panX: Float = 0f,
    val panY: Float = 0f,
)

interface InputSink {
    fun sendMouseMove(x: Int, y: Int, display: Int)
    fun sendMouseRelative(dx: Int, dy: Int)
    fun sendMouseButton(button: Int, down: Boolean)
    fun sendMouseWheel(dx: Int, dy: Int)
}

internal data class HostPoint(val x: Int, val y: Int)

internal data class TrackpadDelta(
    val dx: Int,
    val dy: Int,
    val residualX: Float,
    val residualY: Float,
)

/**
 * Pure coordinate-transformation and trackpad-acceleration math helpers,
 * extracted for deterministic JVM unit testing without Android framework mocks.
 */
internal object TouchCoordinateMath {
    const val MIN_ZOOM = 1.0f
    const val MAX_ZOOM = 4.0f

    fun clampTransform(
        transform: ViewportTransform,
        viewWidth: Int,
        viewHeight: Int,
        minZoom: Float = MIN_ZOOM,
        maxZoom: Float = MAX_ZOOM,
    ): ViewportTransform {
        val safeZoom = if (transform.zoom.isFinite()) {
            transform.zoom.coerceIn(minZoom, maxZoom)
        } else {
            minZoom
        }
        if (safeZoom <= 1.0f || viewWidth <= 0 || viewHeight <= 0) {
            return ViewportTransform(zoom = safeZoom, panX = 0f, panY = 0f)
        }
        val maxPanX = viewWidth * (safeZoom - 1.0f) * 0.5f
        val maxPanY = viewHeight * (safeZoom - 1.0f) * 0.5f
        val safePanX = if (transform.panX.isFinite()) transform.panX.coerceIn(-maxPanX, maxPanX) else 0f
        val safePanY = if (transform.panY.isFinite()) transform.panY.coerceIn(-maxPanY, maxPanY) else 0f
        return ViewportTransform(zoom = safeZoom, panX = safePanX, panY = safePanY)
    }

    /**
     * Maps a touch point `(viewX, viewY)` in view pixels through the active [ViewportTransform]
     * (centered scale + translation) and [ScaleMode] into clamped host pixel coordinates
     * `(0 until hostWidth, 0 until hostHeight)`.
     */
    fun mapViewToHost(
        viewX: Float,
        viewY: Float,
        viewWidth: Int,
        viewHeight: Int,
        hostWidth: Int,
        hostHeight: Int,
        scaleMode: ScaleMode,
        transform: ViewportTransform,
    ): HostPoint? {
        if (viewWidth <= 0 || viewHeight <= 0 || hostWidth <= 0 || hostHeight <= 0) {
            return null
        }

        val clamped = clampTransform(transform, viewWidth, viewHeight)
        val halfW = viewWidth * 0.5f
        val halfH = viewHeight * 0.5f

        // Inverse of Compose graphicsLayer(scaleX=zoom, scaleY=zoom, translationX=panX, translationY=panY)
        // with TransformOrigin.Center.
        val contentX = (viewX - halfW - clamped.panX) / clamped.zoom + halfW
        val contentY = (viewY - halfH - clamped.panY) / clamped.zoom + halfH

        val hostXf: Float
        val hostYf: Float
        when (scaleMode) {
            ScaleMode.FIT -> {
                val scale = min(viewWidth.toFloat() / hostWidth.toFloat(), viewHeight.toFloat() / hostHeight.toFloat())
                val dispW = hostWidth * scale
                val dispH = hostHeight * scale
                val offsetX = (viewWidth - dispW) * 0.5f
                val offsetY = (viewHeight - dispH) * 0.5f
                hostXf = (contentX - offsetX) / scale
                hostYf = (contentY - offsetY) / scale
            }
            ScaleMode.STRETCH -> {
                hostXf = (contentX / viewWidth.toFloat()) * hostWidth.toFloat()
                hostYf = (contentY / viewHeight.toFloat()) * hostHeight.toFloat()
            }
            ScaleMode.ORIGINAL -> {
                val offsetX = (viewWidth - hostWidth) * 0.5f
                val offsetY = (viewHeight - hostHeight) * 0.5f
                hostXf = contentX - offsetX
                hostYf = contentY - offsetY
            }
        }

        val hx = floor(hostXf).toInt().coerceIn(0, hostWidth - 1)
        val hy = floor(hostYf).toInt().coerceIn(0, hostHeight - 1)
        return HostPoint(hx, hy)
    }

    /**
     * Applies a pinch-scale and focal-point pan gesture around `(prevFocusX, prevFocusY) -> (currFocusX, currFocusY)`,
     * keeping the content beneath the fingers anchored while clamping to viewport bounds.
     */
    fun applyPinchPan(
        current: ViewportTransform,
        prevFocusX: Float,
        prevFocusY: Float,
        currFocusX: Float,
        currFocusY: Float,
        scaleFactor: Float,
        viewWidth: Int,
        viewHeight: Int,
    ): ViewportTransform {
        if (viewWidth <= 0 || viewHeight <= 0) return current
        val safeFactor = if (scaleFactor.isFinite() && scaleFactor > 0f) scaleFactor else 1f
        val oldZoom = current.zoom.coerceIn(MIN_ZOOM, MAX_ZOOM)
        val newZoom = (oldZoom * safeFactor).coerceIn(MIN_ZOOM, MAX_ZOOM)
        val zoomRatio = newZoom / oldZoom

        val halfW = viewWidth * 0.5f
        val halfH = viewHeight * 0.5f
        val newPanX = currFocusX - halfW - (prevFocusX - halfW - current.panX) * zoomRatio
        val newPanY = currFocusY - halfH - (prevFocusY - halfH - current.panY) * zoomRatio

        return clampTransform(
            ViewportTransform(zoom = newZoom, panX = newPanX, panY = newPanY),
            viewWidth = viewWidth,
            viewHeight = viewHeight,
        )
    }

    /**
     * Applies subtle velocity-based pointer acceleration with a sub-pixel residual accumulator
     * so slow trackpad precision moves never get lost to integer truncation.
     */
    fun computeTrackpadDelta(
        rawDx: Float,
        rawDy: Float,
        residualX: Float,
        residualY: Float,
        sensitivity: Float = 1.35f,
    ): TrackpadDelta {
        val speed = hypot(rawDx.toDouble(), rawDy.toDouble()).toFloat()
        val accel = 1.0f + min(speed / 30.0f, 1.6f) * 0.65f
        val scaledX = rawDx * sensitivity * accel + residualX
        val scaledY = rawDy * sensitivity * accel + residualY

        val intX = truncate(scaledX).toInt()
        val intY = truncate(scaledY).toInt()
        return TrackpadDelta(
            dx = intX,
            dy = intY,
            residualX = scaledX - intX,
            residualY = scaledY - intY,
        )
    }
}

class TouchInputController(
    private val sink: InputSink,
    private val onViewportChanged: (ViewportTransform) -> Unit,
) {
    private data class HostResolution(val width: Int, val height: Int)

    @Volatile
    var touchMode: TouchMode = TouchMode.TRACKPAD

    @Volatile
    var scaleMode: ScaleMode = ScaleMode.FIT

    @Volatile
    var activeDisplay: Int = 0

    @Volatile
    private var hostResolution: HostResolution = HostResolution(1920, 1080)
    private var viewWidth: Int = 0
    private var viewHeight: Int = 0
    private var viewport: ViewportTransform = ViewportTransform()

    private val mainHandler: Handler? = try {
        Looper.getMainLooper()?.let { Handler(it) }
    } catch (_: Throwable) {
        null
    }

    // Single-finger tracking
    private var activePointerId: Int = MotionEvent.INVALID_POINTER_ID
    private var downTimeMs: Long = 0L
    private var downX: Float = 0f
    private var downY: Float = 0f
    private var lastX: Float = 0f
    private var lastY: Float = 0f
    private var movedBeyondSlop: Boolean = false
    private var residualX: Float = 0f
    private var residualY: Float = 0f

    // Trackpad double-tap & drag
    private var lastTapUpTimeMs: Long = 0L
    private var lastTapUpX: Float = 0f
    private var lastTapUpY: Float = 0f
    private var trackpadDraggingLeft: Boolean = false

    // Direct touch drag & long-press right-click
    private var directDraggingLeft: Boolean = false
    private var longPressTriggered: Boolean = false
    private var pendingLongPress: Runnable? = null

    // Multi-touch (2-finger tap / scroll / pinch-zoom)
    private enum class TwoFingerMode { UNDECIDED, SCROLL, PINCH_ZOOM }

    private var hadMultiTouch: Boolean = false
    private var twoFingerMode: TwoFingerMode = TwoFingerMode.UNDECIDED
    private var twoFingerStartTimeMs: Long = 0L
    private var twoFingerStartFocusX: Float = 0f
    private var twoFingerStartFocusY: Float = 0f
    private var twoFingerStartSpan: Float = 0f
    private var prevFocusX: Float = 0f
    private var prevFocusY: Float = 0f
    private var prevSpan: Float = 0f
    private var twoFingerTapEmitted: Boolean = false
    private var wheelResidualX: Float = 0f
    private var wheelResidualY: Float = 0f

    fun setHostResolution(width: Int, height: Int) {
        if (width > 0 && height > 0) {
            hostResolution = HostResolution(width, height)
        }
    }

    fun setViewSize(width: Int, height: Int) {
        if (width > 0 && height > 0) {
            viewWidth = width
            viewHeight = height
            val clamped = TouchCoordinateMath.clampTransform(viewport, viewWidth, viewHeight)
            if (clamped != viewport) {
                viewport = clamped
                onViewportChanged(viewport)
            }
        }
    }

    fun resetZoom() {
        val reset = ViewportTransform(1f, 0f, 0f)
        if (viewport != reset) {
            viewport = reset
            onViewportChanged(viewport)
        }
    }

    fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN -> handleActionDown(event)
            MotionEvent.ACTION_POINTER_DOWN -> handlePointerDown(event)
            MotionEvent.ACTION_MOVE -> handleActionMove(event)
            MotionEvent.ACTION_POINTER_UP -> handlePointerUp(event)
            MotionEvent.ACTION_UP -> handleActionUp(event)
            MotionEvent.ACTION_CANCEL -> handleActionCancel()
        }
        return true
    }

    private fun handleActionDown(event: MotionEvent) {
        cancelLongPress()
        activePointerId = event.getPointerId(0)
        downTimeMs = event.eventTime
        downX = event.x
        downY = event.y
        lastX = event.x
        lastY = event.y
        movedBeyondSlop = false
        residualX = 0f
        residualY = 0f
        hadMultiTouch = false
        twoFingerMode = TwoFingerMode.UNDECIDED
        twoFingerTapEmitted = false
        directDraggingLeft = false
        longPressTriggered = false

        when (touchMode) {
            TouchMode.TRACKPAD -> {
                val withinWindow = (event.eventTime - lastTapUpTimeMs) in 0..DOUBLE_TAP_WINDOW_MS
                val withinSlop = hypot((downX - lastTapUpX).toDouble(), (downY - lastTapUpY).toDouble()) <= DOUBLE_TAP_SLOP_PX
                if (withinWindow && withinSlop) {
                    trackpadDraggingLeft = true
                    lastTapUpTimeMs = 0L
                    sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, true)
                } else {
                    trackpadDraggingLeft = false
                }
            }
            TouchMode.DIRECT -> {
                scheduleDirectLongPress(downX, downY)
            }
        }
    }

    private fun handlePointerDown(event: MotionEvent) {
        cancelLongPress()
        if (trackpadDraggingLeft) {
            sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
            trackpadDraggingLeft = false
        }
        if (directDraggingLeft) {
            sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
            directDraggingLeft = false
        }

        if (event.pointerCount == 2) {
            hadMultiTouch = true
            twoFingerMode = TwoFingerMode.UNDECIDED
            twoFingerTapEmitted = false
            twoFingerStartTimeMs = event.eventTime
            val x0 = event.getX(0)
            val y0 = event.getY(0)
            val x1 = event.getX(1)
            val y1 = event.getY(1)
            twoFingerStartFocusX = (x0 + x1) * 0.5f
            twoFingerStartFocusY = (y0 + y1) * 0.5f
            twoFingerStartSpan = hypot((x1 - x0).toDouble(), (y1 - y0).toDouble()).toFloat()
            prevFocusX = twoFingerStartFocusX
            prevFocusY = twoFingerStartFocusY
            prevSpan = twoFingerStartSpan
            wheelResidualX = 0f
            wheelResidualY = 0f
        }
    }

    private fun handleActionMove(event: MotionEvent) {
        if (event.pointerCount >= 2) {
            handleTwoFingerMove(event)
            return
        }
        if (hadMultiTouch) return

        val idx = event.findPointerIndex(activePointerId)
        if (idx < 0) return

        val x = event.getX(idx)
        val y = event.getY(idx)
        val dx = x - lastX
        val dy = y - lastY
        lastX = x
        lastY = y

        val totalDist = hypot((x - downX).toDouble(), (y - downY).toDouble()).toFloat()
        if (!movedBeyondSlop && totalDist > TAP_SLOP_PX) {
            movedBeyondSlop = true
            cancelLongPress()
            if (touchMode == TouchMode.DIRECT && !longPressTriggered && !directDraggingLeft) {
                mapToHost(downX, downY)?.let { pt ->
                    sink.sendMouseMove(pt.x, pt.y, activeDisplay)
                    sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, true)
                    directDraggingLeft = true
                }
            }
        }

        when (touchMode) {
            TouchMode.TRACKPAD -> {
                if (dx != 0f || dy != 0f) {
                    val delta = TouchCoordinateMath.computeTrackpadDelta(dx, dy, residualX, residualY)
                    residualX = delta.residualX
                    residualY = delta.residualY
                    if (delta.dx != 0 || delta.dy != 0) {
                        sink.sendMouseRelative(delta.dx, delta.dy)
                    }
                }
            }
            TouchMode.DIRECT -> {
                if (directDraggingLeft) {
                    mapToHost(x, y)?.let { pt ->
                        sink.sendMouseMove(pt.x, pt.y, activeDisplay)
                    }
                }
            }
        }
    }

    private fun handleTwoFingerMove(event: MotionEvent) {
        val x0 = event.getX(0)
        val y0 = event.getY(0)
        val x1 = event.getX(1)
        val y1 = event.getY(1)

        val focusX = (x0 + x1) * 0.5f
        val focusY = (y0 + y1) * 0.5f
        val span = hypot((x1 - x0).toDouble(), (y1 - y0).toDouble()).toFloat()

        if (twoFingerMode == TwoFingerMode.UNDECIDED) {
            val spanDelta = abs(span - twoFingerStartSpan)
            val ratioDelta = if (twoFingerStartSpan > 1f) abs(span / twoFingerStartSpan - 1f) else 0f
            val focusDist = hypot(
                (focusX - twoFingerStartFocusX).toDouble(),
                (focusY - twoFingerStartFocusY).toDouble(),
            ).toFloat()

            if (spanDelta > PINCH_SPAN_SLOP_PX || ratioDelta > PINCH_RATIO_THRESHOLD) {
                twoFingerMode = TwoFingerMode.PINCH_ZOOM
            } else if (focusDist > TWO_FINGER_MOVE_SLOP_PX) {
                twoFingerMode = TwoFingerMode.SCROLL
            }
        }

        when (twoFingerMode) {
            TwoFingerMode.PINCH_ZOOM -> {
                val scaleFactor = if (prevSpan > 1f) span / prevSpan else 1f
                val updated = TouchCoordinateMath.applyPinchPan(
                    current = viewport,
                    prevFocusX = prevFocusX,
                    prevFocusY = prevFocusY,
                    currFocusX = focusX,
                    currFocusY = focusY,
                    scaleFactor = scaleFactor,
                    viewWidth = viewWidth,
                    viewHeight = viewHeight,
                )
                if (updated != viewport) {
                    viewport = updated
                    onViewportChanged(viewport)
                }
            }
            TwoFingerMode.SCROLL -> {
                val deltaX = (focusX - prevFocusX) * WHEEL_UNITS_PER_PX + wheelResidualX
                val deltaY = (focusY - prevFocusY) * WHEEL_UNITS_PER_PX + wheelResidualY
                val wheelX = truncate(deltaX).toInt()
                val wheelY = truncate(deltaY).toInt()
                wheelResidualX = deltaX - wheelX
                wheelResidualY = deltaY - wheelY
                if (wheelX != 0 || wheelY != 0) {
                    sink.sendMouseWheel(wheelX, wheelY)
                }
            }
            TwoFingerMode.UNDECIDED -> Unit
        }

        prevFocusX = focusX
        prevFocusY = focusY
        prevSpan = span
    }

    private fun handlePointerUp(event: MotionEvent) {
        if (event.pointerCount == 2 && hadMultiTouch && twoFingerMode == TwoFingerMode.UNDECIDED && !twoFingerTapEmitted) {
            val elapsed = event.eventTime - twoFingerStartTimeMs
            if (elapsed in 0..TWO_FINGER_TAP_TIMEOUT_MS) {
                twoFingerTapEmitted = true
                if (touchMode == TouchMode.DIRECT) {
                    mapToHost(twoFingerStartFocusX, twoFingerStartFocusY)?.let { pt ->
                        sink.sendMouseMove(pt.x, pt.y, activeDisplay)
                    }
                }
                sink.sendMouseButton(BridgeConstants.MOUSE_RIGHT, true)
                sink.sendMouseButton(BridgeConstants.MOUSE_RIGHT, false)
            }
        }

        // Keep activePointerId valid if primary pointer lifts.
        val liftedIdx = event.actionIndex
        if (event.getPointerId(liftedIdx) == activePointerId) {
            val newIdx = if (liftedIdx == 0) 1 else 0
            if (newIdx < event.pointerCount) {
                activePointerId = event.getPointerId(newIdx)
                lastX = event.getX(newIdx)
                lastY = event.getY(newIdx)
            }
        }
    }

    private fun handleActionUp(event: MotionEvent) {
        cancelLongPress()
        if (hadMultiTouch) {
            resetTouchState()
            return
        }

        val elapsed = event.eventTime - downTimeMs
        when (touchMode) {
            TouchMode.TRACKPAD -> {
                if (trackpadDraggingLeft) {
                    sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
                    trackpadDraggingLeft = false
                } else if (!movedBeyondSlop && elapsed in 0..TAP_TIMEOUT_MS) {
                    sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, true)
                    sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
                    lastTapUpTimeMs = event.eventTime
                    lastTapUpX = event.x
                    lastTapUpY = event.y
                }
            }
            TouchMode.DIRECT -> {
                if (directDraggingLeft) {
                    mapToHost(event.x, event.y)?.let { pt ->
                        sink.sendMouseMove(pt.x, pt.y, activeDisplay)
                    }
                    sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
                    directDraggingLeft = false
                } else if (!longPressTriggered && !movedBeyondSlop) {
                    mapToHost(event.x, event.y)?.let { pt ->
                        sink.sendMouseMove(pt.x, pt.y, activeDisplay)
                        sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, true)
                        sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
                    }
                }
            }
        }
        resetTouchState()
    }

    private fun handleActionCancel() {
        cancelLongPress()
        if (trackpadDraggingLeft) {
            sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
            trackpadDraggingLeft = false
        }
        if (directDraggingLeft) {
            sink.sendMouseButton(BridgeConstants.MOUSE_LEFT, false)
            directDraggingLeft = false
        }
        resetTouchState()
    }

    private fun resetTouchState() {
        activePointerId = MotionEvent.INVALID_POINTER_ID
        hadMultiTouch = false
        twoFingerMode = TwoFingerMode.UNDECIDED
        movedBeyondSlop = false
        longPressTriggered = false
        wheelResidualX = 0f
        wheelResidualY = 0f
    }

    private fun scheduleDirectLongPress(x: Float, y: Float) {
        val handler = mainHandler ?: return
        val runnable = Runnable {
            if (!hadMultiTouch && !movedBeyondSlop && !directDraggingLeft) {
                longPressTriggered = true
                mapToHost(x, y)?.let { pt ->
                    sink.sendMouseMove(pt.x, pt.y, activeDisplay)
                    sink.sendMouseButton(BridgeConstants.MOUSE_RIGHT, true)
                    sink.sendMouseButton(BridgeConstants.MOUSE_RIGHT, false)
                }
            }
        }
        pendingLongPress = runnable
        handler.postDelayed(runnable, LONG_PRESS_TIMEOUT_MS)
    }

    private fun cancelLongPress() {
        val runnable = pendingLongPress ?: return
        pendingLongPress = null
        mainHandler?.removeCallbacks(runnable)
    }

    private fun mapToHost(viewX: Float, viewY: Float): HostPoint? {
        val res = hostResolution
        return TouchCoordinateMath.mapViewToHost(
            viewX = viewX,
            viewY = viewY,
            viewWidth = viewWidth,
            viewHeight = viewHeight,
            hostWidth = res.width,
            hostHeight = res.height,
            scaleMode = scaleMode,
            transform = viewport,
        )
    }

    companion object {
        private const val TAP_SLOP_PX = 18f
        private const val DOUBLE_TAP_SLOP_PX = 48f
        private const val TAP_TIMEOUT_MS = 250L
        private const val DOUBLE_TAP_WINDOW_MS = 280L
        private const val TWO_FINGER_TAP_TIMEOUT_MS = 280L
        private const val LONG_PRESS_TIMEOUT_MS = 450L
        private const val PINCH_SPAN_SLOP_PX = 24f
        private const val PINCH_RATIO_THRESHOLD = 0.08f
        private const val TWO_FINGER_MOVE_SLOP_PX = 16f
        private const val WHEEL_UNITS_PER_PX = 4.0f
    }
}

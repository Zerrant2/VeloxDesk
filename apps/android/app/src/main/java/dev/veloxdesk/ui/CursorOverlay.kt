package dev.veloxdesk.ui

import androidx.compose.foundation.Canvas
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.FilterQuality
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.Fill
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.translate
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.unit.IntOffset
import androidx.compose.ui.unit.IntSize
import androidx.compose.ui.unit.dp
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.TouchMode
import dev.veloxdesk.input.TouchCoordinateMath
import dev.veloxdesk.input.ViewportTransform
import kotlin.math.min
import kotlin.math.roundToInt

data class CursorShapeBitmap(
    val shapeId: Long,
    val width: Int,
    val height: Int,
    val hotspotX: Int,
    val hotspotY: Int,
    val imageBitmap: ImageBitmap?,
)

data class CursorOverlayState(
    val hostX: Float = 0f,
    val hostY: Float = 0f,
    val visible: Boolean = false,
    val hostReportedVisibility: Boolean = false,
    val hasPosition: Boolean = false,
    val shapeId: Long = 0L,
    val shape: CursorShapeBitmap? = null,
)

internal data class ScreenCursorTransform(
    val screenX: Float,
    val screenY: Float,
    val videoScale: Float,
)

/**
 * Converts RGBA8888 byte pixels (`[R, G, B, A, ...]`) into Android packed `ARGB_8888` ints
 * (`0xAARRGGBB`) for `Bitmap.createBitmap`.
 */
internal fun rgbaBytesToArgbInts(rgba: ByteArray): IntArray {
    val pixelCount = rgba.size / 4
    val out = IntArray(pixelCount)
    var src = 0
    for (i in 0 until pixelCount) {
        val r = rgba[src].toInt() and 0xFF
        val g = rgba[src + 1].toInt() and 0xFF
        val b = rgba[src + 2].toInt() and 0xFF
        val a = rgba[src + 3].toInt() and 0xFF
        out[i] = (a shl 24) or (r shl 16) or (g shl 8) or b
        src += 4
    }
    return out
}

/**
 * Maps host pixel coordinates `(hostX, hostY)` to Android view pixel coordinates using the exact
 * forward inverse of [TouchCoordinateMath.mapViewToHost] (`ScaleMode` + centered `graphicsLayer`
 * zoom & pan).
 */
internal fun mapHostToView(
    hostX: Float,
    hostY: Float,
    viewWidth: Int,
    viewHeight: Int,
    hostWidth: Int,
    hostHeight: Int,
    scaleMode: ScaleMode,
    transform: ViewportTransform,
): ScreenCursorTransform? {
    if (viewWidth <= 0 || viewHeight <= 0 || hostWidth <= 0 || hostHeight <= 0) {
        return null
    }
    if (!hostX.isFinite() || !hostY.isFinite()) {
        return null
    }

    val clamped = TouchCoordinateMath.clampTransform(transform, viewWidth, viewHeight)
    val contentX: Float
    val contentY: Float
    val baseScale: Float

    when (scaleMode) {
        ScaleMode.FIT -> {
            baseScale = min(
                viewWidth.toFloat() / hostWidth.toFloat(),
                viewHeight.toFloat() / hostHeight.toFloat(),
            )
            val dispW = hostWidth * baseScale
            val dispH = hostHeight * baseScale
            val offsetX = (viewWidth - dispW) * 0.5f
            val offsetY = (viewHeight - dispH) * 0.5f
            contentX = offsetX + hostX * baseScale
            contentY = offsetY + hostY * baseScale
        }
        ScaleMode.STRETCH -> {
            val scaleX = viewWidth.toFloat() / hostWidth.toFloat()
            val scaleY = viewHeight.toFloat() / hostHeight.toFloat()
            baseScale = min(scaleX, scaleY)
            contentX = hostX * scaleX
            contentY = hostY * scaleY
        }
        ScaleMode.ORIGINAL -> {
            baseScale = 1f
            val offsetX = (viewWidth - hostWidth) * 0.5f
            val offsetY = (viewHeight - hostHeight) * 0.5f
            contentX = offsetX + hostX
            contentY = offsetY + hostY
        }
    }

    val halfW = viewWidth * 0.5f
    val halfH = viewHeight * 0.5f
    val screenX = (contentX - halfW) * clamped.zoom + halfW + clamped.panX
    val screenY = (contentY - halfH) * clamped.zoom + halfH + clamped.panY

    return ScreenCursorTransform(
        screenX = screenX,
        screenY = screenY,
        videoScale = baseScale * clamped.zoom,
    )
}

/**
 * Computes the cursor draw scale factor so host bitmaps stay proportional to the video while
 * guaranteeing a comfortable minimum readable size on small phone screens.
 */
internal fun computeCursorDrawScale(
    videoScale: Float,
    shapeWidth: Int,
    shapeHeight: Int,
    minCursorSizePx: Float,
): Float {
    val maxDim = maxOf(shapeWidth, shapeHeight).coerceAtLeast(1).toFloat()
    val safeVideoScale = if (videoScale.isFinite() && videoScale > 0f) videoScale else 1f
    val minScale = if (minCursorSizePx > 0f) minCursorSizePx / maxDim else 0f
    return maxOf(safeVideoScale, minScale)
}

/**
 * Non-interactive Compose overlay drawing the remote host hardware cursor (or a crisp fallback
 * pointer arrow) aligned 1:1 with the zoomed/panned video surface.
 */
@Composable
fun CursorOverlayCanvas(
    cursor: CursorOverlayState,
    touchMode: TouchMode,
    videoWidth: Int,
    videoHeight: Int,
    scaleMode: ScaleMode,
    viewport: ViewportTransform,
    modifier: Modifier = Modifier,
) {
    val shouldDraw = videoWidth > 0 && videoHeight > 0 &&
        if (cursor.hostReportedVisibility) {
            cursor.visible
        } else {
            cursor.visible || (touchMode == TouchMode.TRACKPAD && cursor.hasPosition)
        }
    if (!shouldDraw) return

    val density = LocalDensity.current
    val minCursorPx = with(density) { 20.dp.toPx() }
    val fallbackArrowHeightPx = with(density) { 22.dp.toPx() }
    val outlineStrokePx = with(density) { 1.75.dp.toPx() }

    Canvas(modifier = modifier) {
        val viewW = size.width.roundToInt()
        val viewH = size.height.roundToInt()
        val mapped = mapHostToView(
            hostX = cursor.hostX,
            hostY = cursor.hostY,
            viewWidth = viewW,
            viewHeight = viewH,
            hostWidth = videoWidth,
            hostHeight = videoHeight,
            scaleMode = scaleMode,
            transform = viewport,
        ) ?: return@Canvas

        val shape = cursor.shape
        val bitmap = shape?.imageBitmap
        if (shape != null && bitmap != null && shape.width > 0 && shape.height > 0) {
            val drawScale = computeCursorDrawScale(
                videoScale = mapped.videoScale,
                shapeWidth = shape.width,
                shapeHeight = shape.height,
                minCursorSizePx = minCursorPx,
            )
            val dstX = (mapped.screenX - shape.hotspotX * drawScale).roundToInt()
            val dstY = (mapped.screenY - shape.hotspotY * drawScale).roundToInt()
            val dstW = (shape.width * drawScale).roundToInt().coerceAtLeast(1)
            val dstH = (shape.height * drawScale).roundToInt().coerceAtLeast(1)

            drawImage(
                image = bitmap,
                dstOffset = IntOffset(dstX, dstY),
                dstSize = IntSize(dstW, dstH),
                filterQuality = FilterQuality.Medium,
            )
        } else {
            // Crisp high-contrast white pointer arrow with dark outline (hotspot at exact tip)
            val s = fallbackArrowHeightPx / 24f
            val arrowPath = Path().apply {
                moveTo(0f, 0f)
                lineTo(0f, 20.5f * s)
                lineTo(5.2f * s, 15.6f * s)
                lineTo(8.8f * s, 23.4f * s)
                lineTo(12.2f * s, 21.8f * s)
                lineTo(8.6f * s, 14.2f * s)
                lineTo(15.2f * s, 14.2f * s)
                close()
            }
            translate(left = mapped.screenX, top = mapped.screenY) {
                drawPath(
                    path = arrowPath,
                    color = Color(0xF00F172A),
                    style = Stroke(width = outlineStrokePx * 2f, join = StrokeJoin.Round),
                )
                drawPath(
                    path = arrowPath,
                    color = Color.White,
                    style = Fill,
                )
            }
        }
    }
}

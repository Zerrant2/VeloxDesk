package dev.veloxdesk.ui

import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.input.TouchCoordinateMath
import dev.veloxdesk.input.ViewportTransform
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Test

class CursorOverlayTest {
    @Test
    fun rgbaBytesToArgbInts_packsUnsignedBytesCorrectly() {
        val rgba = byteArrayOf(
            // Pixel 0: opaque red (255, 0, 0, 255)
            0xFF.toByte(), 0x00, 0x00, 0xFF.toByte(),
            // Pixel 1: 50% green (0, 255, 0, 128)
            0x00, 0xFF.toByte(), 0x00, 0x80.toByte(),
            // Pixel 2: translucent blue (17, 34, 51, 68)
            0x11, 0x22, 0x33, 0x44,
            // Pixel 3: fully transparent white (255, 255, 255, 0)
            0xFF.toByte(), 0xFF.toByte(), 0xFF.toByte(), 0x00,
        )
        val expected = intArrayOf(
            0xFFFF0000.toInt(),
            0x8000FF00.toInt(),
            0x44112233,
            0x00FFFFFF,
        )
        assertArrayEquals(expected, rgbaBytesToArgbInts(rgba))
        assertArrayEquals(IntArray(0), rgbaBytesToArgbInts(ByteArray(0)))
    }

    @Test
    fun mapHostToView_fitLetterboxAndPillarboxExactOffsets() {
        // Letterbox: 1920x1080 host on 1000x1000 square view -> scale = 1000/1920, dispH = 562.5, offsetY = 218.75
        val lbTopLeft = mapHostToView(
            hostX = 0f,
            hostY = 0f,
            viewWidth = 1000,
            viewHeight = 1000,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = ViewportTransform(),
        )
        assertNotNull(lbTopLeft)
        assertEquals(0f, lbTopLeft!!.screenX, 0.01f)
        assertEquals(218.75f, lbTopLeft.screenY, 0.01f)
        assertEquals(1000f / 1920f, lbTopLeft.videoScale, 0.0001f)

        val lbCenter = mapHostToView(
            hostX = 960f,
            hostY = 540f,
            viewWidth = 1000,
            viewHeight = 1000,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = ViewportTransform(),
        )
        assertNotNull(lbCenter)
        assertEquals(500f, lbCenter!!.screenX, 0.01f)
        assertEquals(500f, lbCenter.screenY, 0.01f)

        // Pillarbox: 1920x1080 host on 2400x1080 ultrawide phone -> scale = 1.0, offsetX = 240, offsetY = 0
        val pbTopLeft = mapHostToView(
            hostX = 0f,
            hostY = 0f,
            viewWidth = 2400,
            viewHeight = 1080,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = ViewportTransform(),
        )
        assertNotNull(pbTopLeft)
        assertEquals(240f, pbTopLeft!!.screenX, 0.01f)
        assertEquals(0f, pbTopLeft.screenY, 0.01f)
        assertEquals(1.0f, pbTopLeft.videoScale, 0.0001f)

        val pbBottomRight = mapHostToView(
            hostX = 1920f,
            hostY = 1080f,
            viewWidth = 2400,
            viewHeight = 1080,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = ViewportTransform(),
        )
        assertNotNull(pbBottomRight)
        assertEquals(2160f, pbBottomRight!!.screenX, 0.01f)
        assertEquals(1080f, pbBottomRight.screenY, 0.01f)
    }

    @Test
    fun mapHostToView_zoomAndPanMatchesInverseOfTouchCoordinateMath() {
        val viewW = 2400
        val viewH = 1080
        val hostW = 1920
        val hostH = 1080
        val transform = ViewportTransform(zoom = 2.5f, panX = 180f, panY = -120f)

        for (mode in ScaleMode.entries) {
            val testPoints = listOf(
                100f to 120f,
                960f to 540f,
                1700f to 950f,
            )
            for ((hx, hy) in testPoints) {
                val mapped = mapHostToView(
                    hostX = hx + 0.5f,
                    hostY = hy + 0.5f,
                    viewWidth = viewW,
                    viewHeight = viewH,
                    hostWidth = hostW,
                    hostHeight = hostH,
                    scaleMode = mode,
                    transform = transform,
                )
                assertNotNull(mapped)
                val roundTrip = TouchCoordinateMath.mapViewToHost(
                    viewX = mapped!!.screenX,
                    viewY = mapped.screenY,
                    viewWidth = viewW,
                    viewHeight = viewH,
                    hostWidth = hostW,
                    hostHeight = hostH,
                    scaleMode = mode,
                    transform = transform,
                )
                assertNotNull(roundTrip)
                assertEquals("mode=$mode x", hx.toInt(), roundTrip!!.x)
                assertEquals("mode=$mode y", hy.toInt(), roundTrip.y)
            }
        }
    }

    @Test
    fun mapHostToView_rejectsInvalidDimensionsAndNonFiniteCoords() {
        assertNull(
            mapHostToView(10f, 10f, 0, 1080, 1920, 1080, ScaleMode.FIT, ViewportTransform()),
        )
        assertNull(
            mapHostToView(Float.NaN, 10f, 1920, 1080, 1920, 1080, ScaleMode.FIT, ViewportTransform()),
        )
    }

    @Test
    fun computeCursorDrawScale_enforcesMinimumReadableSizeAndScalesWhenZoomed() {
        // Small video scale (0.4x) on 32x32 cursor with 60px min size -> boosted to 60/32 = 1.875x
        assertEquals(1.875f, computeCursorDrawScale(0.4f, 32, 32, 60f), 0.001f)
        // High zoom (3.0x) on 32x32 cursor -> keeps proportional 3.0x (96px > 60px)
        assertEquals(3.0f, computeCursorDrawScale(3.0f, 32, 32, 60f), 0.001f)
    }
}

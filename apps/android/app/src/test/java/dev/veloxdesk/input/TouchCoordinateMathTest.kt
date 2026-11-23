package dev.veloxdesk.input

import dev.veloxdesk.bridge.ScaleMode
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class TouchCoordinateMathTest {

    @Test
    fun fitModeLetterboxMapsVideoAreaAndClampsBars() {
        // 1920x1080 host displayed in 1080x1920 portrait view:
        // scale = min(1080/1920, 1920/1080) = 0.5625
        // dispW = 1080, dispH = 607.5, offsetX = 0, offsetY = (1920 - 607.5)/2 = 656.25
        val identity = ViewportTransform()

        // Center of view -> center of host
        val center = TouchCoordinateMath.mapViewToHost(
            viewX = 540f,
            viewY = 960f,
            viewWidth = 1080,
            viewHeight = 1920,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = identity,
        )
        assertEquals(HostPoint(960, 540), center)

        // Top black letterbox bar clamps Y to 0
        val topBar = TouchCoordinateMath.mapViewToHost(
            viewX = 0f,
            viewY = 100f,
            viewWidth = 1080,
            viewHeight = 1920,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = identity,
        )
        assertEquals(HostPoint(0, 0), topBar)

        // Bottom black letterbox bar clamps Y to hostHeight - 1
        val bottomBar = TouchCoordinateMath.mapViewToHost(
            viewX = 1080f,
            viewY = 1800f,
            viewWidth = 1080,
            viewHeight = 1920,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = identity,
        )
        assertEquals(HostPoint(1919, 1079), bottomBar)
    }

    @Test
    fun fitModePillarboxMapsLandscapeUltrawideCorrectly() {
        // 1920x1080 host in 2400x1080 view -> scale = 1.0, dispW = 1920, offsetX = 240
        val identity = ViewportTransform()

        val leftEdge = TouchCoordinateMath.mapViewToHost(
            viewX = 240f,
            viewY = 0f,
            viewWidth = 2400,
            viewHeight = 1080,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = identity,
        )
        assertEquals(HostPoint(0, 0), leftEdge)

        val rightEdge = TouchCoordinateMath.mapViewToHost(
            viewX = 2159.5f,
            viewY = 1079.5f,
            viewWidth = 2400,
            viewHeight = 1080,
            hostWidth = 1920,
            hostHeight = 1080,
            scaleMode = ScaleMode.FIT,
            transform = identity,
        )
        assertEquals(HostPoint(1919, 1079), rightEdge)
    }

    @Test
    fun stretchModeMapsFullViewToHost() {
        val identity = ViewportTransform()

        assertEquals(
            HostPoint(0, 0),
            TouchCoordinateMath.mapViewToHost(0f, 0f, 1000, 500, 1920, 1080, ScaleMode.STRETCH, identity),
        )
        assertEquals(
            HostPoint(960, 540),
            TouchCoordinateMath.mapViewToHost(500f, 250f, 1000, 500, 1920, 1080, ScaleMode.STRETCH, identity),
        )
        assertEquals(
            HostPoint(1919, 1079),
            TouchCoordinateMath.mapViewToHost(1000f, 500f, 1000, 500, 1920, 1080, ScaleMode.STRETCH, identity),
        )
    }

    @Test
    fun originalModeMapsOneToOneCentered() {
        val identity = ViewportTransform()
        // View 1000x600, Host 1920x1080 -> offsetX = -460, offsetY = -240
        // View (0, 0) -> Host (460, 240); View (500, 300) -> Host (960, 540)
        assertEquals(
            HostPoint(460, 240),
            TouchCoordinateMath.mapViewToHost(0f, 0f, 1000, 600, 1920, 1080, ScaleMode.ORIGINAL, identity),
        )
        assertEquals(
            HostPoint(960, 540),
            TouchCoordinateMath.mapViewToHost(500f, 300f, 1000, 600, 1920, 1080, ScaleMode.ORIGINAL, identity),
        )
    }

    @Test
    fun zoomAndPanTransformMapsTopLeftAccurately() {
        // View 1000x500, Host 1000x500 (1:1), zoom = 2.0, maxPanX = 500, maxPanY = 250.
        // Panning by (+500, +250) brings content (0, 0) to view (0, 0).
        val zoomedTopLeft = ViewportTransform(zoom = 2f, panX = 500f, panY = 250f)
        assertEquals(
            HostPoint(0, 0),
            TouchCoordinateMath.mapViewToHost(0f, 0f, 1000, 500, 1000, 500, ScaleMode.STRETCH, zoomedTopLeft),
        )
        // View center (500, 250) should map to content (250, 125).
        assertEquals(
            HostPoint(250, 125),
            TouchCoordinateMath.mapViewToHost(500f, 250f, 1000, 500, 1000, 500, ScaleMode.STRETCH, zoomedTopLeft),
        )
    }

    @Test
    fun clampTransformEnforcesZoomAndPanBounds() {
        val tooSmall = TouchCoordinateMath.clampTransform(ViewportTransform(0.5f, 100f, -100f), 1000, 800)
        assertEquals(ViewportTransform(1f, 0f, 0f), tooSmall)

        val tooBig = TouchCoordinateMath.clampTransform(ViewportTransform(9f, 5000f, -5000f), 1000, 800)
        // maxZoom = 4f -> maxPanX = 1000 * 3 / 2 = 1500, maxPanY = 800 * 3 / 2 = 1200
        assertEquals(ViewportTransform(4f, 1500f, -1200f), tooBig)

        val nonFinite = TouchCoordinateMath.clampTransform(
            ViewportTransform(Float.NaN, Float.POSITIVE_INFINITY, Float.NEGATIVE_INFINITY),
            1000,
            800,
        )
        assertEquals(ViewportTransform(1f, 0f, 0f), nonFinite)
    }

    @Test
    fun applyPinchPanKeepsFocalPointStationary() {
        val start = ViewportTransform(1f, 0f, 0f)
        // Zoom 2x around top-left corner (0, 0) in a 1000x500 view -> pan should move to (+500, +250).
        val after = TouchCoordinateMath.applyPinchPan(
            current = start,
            prevFocusX = 0f,
            prevFocusY = 0f,
            currFocusX = 0f,
            currFocusY = 0f,
            scaleFactor = 2f,
            viewWidth = 1000,
            viewHeight = 500,
        )
        assertEquals(2f, after.zoom, 1e-4f)
        assertEquals(500f, after.panX, 1e-3f)
        assertEquals(250f, after.panY, 1e-3f)
    }

    @Test
    fun trackpadResidualAccumulatesSubPixelMoves() {
        var rx = 0f
        var ry = 0f
        var totalDx = 0
        repeat(10) {
            val d = TouchCoordinateMath.computeTrackpadDelta(0.2f, 0f, rx, ry)
            rx = d.residualX
            ry = d.residualY
            totalDx += d.dx
        }
        assertTrue("Expected sub-pixel moves to accumulate to > 0, got $totalDx", totalDx >= 2)
    }

    @Test
    fun invalidDimensionsReturnNull() {
        assertNull(
            TouchCoordinateMath.mapViewToHost(10f, 10f, 0, 100, 1920, 1080, ScaleMode.FIT, ViewportTransform()),
        )
        assertNull(
            TouchCoordinateMath.mapViewToHost(10f, 10f, 100, 100, 0, 1080, ScaleMode.FIT, ViewportTransform()),
        )
    }
}

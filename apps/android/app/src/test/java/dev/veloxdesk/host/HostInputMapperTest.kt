package dev.veloxdesk.host

import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.HostPasswordMode
import dev.veloxdesk.bridge.HostSavedTrustedPeer
import dev.veloxdesk.input.KeyMapper
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class HostInputMapperTest {

    @Test
    fun mapsScaledEncodedCoordsToPhysicalScreenPixelsAndClamps() {
        val center = HostInputMapper.mapToScreen(
            x = 250,
            y = 540,
            srcWidth = 500,
            srcHeight = 1080,
            screenWidth = 1440,
            screenHeight = 3120,
        )
        assertEquals(720f, center.x, 0.5f)
        assertEquals(1560f, center.y, 0.5f)

        val clampedNeg = HostInputMapper.mapToScreen(-20, -10, 500, 1080, 1440, 3120)
        assertEquals(0f, clampedNeg.x, 0.01f)
        assertEquals(0f, clampedNeg.y, 0.01f)

        val clampedOver = HostInputMapper.mapToScreen(9999, 9999, 500, 1080, 1440, 3120)
        assertEquals(1439f, clampedOver.x, 0.01f)
        assertEquals(3119f, clampedOver.y, 0.01f)
    }

    @Test
    fun leftClickTapLongPressAndDragGestures() {
        val mapper = HostInputMapper()

        // Quick tap
        assertTrue(
            mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_MOVE, 540, 960, 0, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 100L).isEmpty(),
        )
        assertTrue(
            mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_LEFT, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 100L).isEmpty(),
        )
        val tapActions = mapper.onEvent(
            BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_LEFT, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 140L,
        )
        assertEquals(1, tapActions.size)
        val tap = tapActions.first() as HostAccessibilityAction.Tap
        assertEquals(540f, tap.x, 0.5f)
        assertEquals(960f, tap.y, 0.5f)
        assertEquals(50L, tap.durationMs)

        // Long press (>= 450 ms)
        mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_LEFT, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 200L)
        val lpActions = mapper.onEvent(
            BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_LEFT, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 850L,
        )
        val lp = lpActions.first() as HostAccessibilityAction.Tap
        assertEquals(650L, lp.durationMs)

        // Drag gesture
        mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_MOVE, 100, 200, 0, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 1000L)
        mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_LEFT, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 1000L)
        mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_MOVE, 200, 400, 0, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 1100L)
        mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_MOVE, 350, 600, 0, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 1200L)
        val dragActions = mapper.onEvent(
            BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_LEFT, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 1250L,
        )
        val drag = dragActions.first() as HostAccessibilityAction.Drag
        assertEquals(3, drag.points.size)
        assertEquals(100f, drag.points.first().x, 0.5f)
        assertEquals(350f, drag.points.last().x, 0.5f)
        assertEquals(250L, drag.durationMs)
    }

    @Test
    fun wheelScrollsVerticallyWithDirectionAndGlobalActionsMapCorrectly() {
        val mapper = HostInputMapper()
        mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_MOVE, 500, 900, 0, false, 0, 0, 0, "", 1080, 1920, 1080, 1920, 10L)

        // Positive wheel dy => finger swipes downward (endY > startY)
        val scrollUp = mapper.onEvent(
            BridgeConstants.HOST_INPUT_MOUSE_WHEEL, 0, 0, 0, false, 0, 0, 120, "", 1080, 1920, 1080, 1920, 20L,
        ).first() as HostAccessibilityAction.Scroll
        assertTrue("expected endY > startY, got $scrollUp", scrollUp.endY > scrollUp.startY)

        // Negative wheel dy => finger swipes upward (endY < startY)
        val scrollDown = mapper.onEvent(
            BridgeConstants.HOST_INPUT_MOUSE_WHEEL, 0, 0, 0, false, 0, 0, -120, "", 1080, 1920, 1080, 1920, 30L,
        ).first() as HostAccessibilityAction.Scroll
        assertTrue("expected endY < startY, got $scrollDown", scrollDown.endY < scrollDown.startY)

        // Right-click & Escape -> BACK
        assertEquals(
            listOf(HostAccessibilityAction.Global(HostGlobalAction.BACK)),
            mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_RIGHT, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 40L),
        )
        assertEquals(
            listOf(HostAccessibilityAction.Global(HostGlobalAction.BACK)),
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, KeyMapper.HID_ESCAPE, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 50L),
        )

        // Middle-click & MetaLeft -> HOME
        assertEquals(
            listOf(HostAccessibilityAction.Global(HostGlobalAction.HOME)),
            mapper.onEvent(BridgeConstants.HOST_INPUT_MOUSE_BUTTON, 0, 0, BridgeConstants.HOST_MOUSE_BTN_MIDDLE, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 60L),
        )
        assertEquals(
            listOf(HostAccessibilityAction.Global(HostGlobalAction.HOME)),
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, KeyMapper.HID_LEFT_GUI, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 70L),
        )

        // Alt+Tab -> RECENTS; plain Tab -> ignored
        assertEquals(
            listOf(HostAccessibilityAction.Global(HostGlobalAction.RECENTS)),
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, KeyMapper.HID_TAB, true, BridgeConstants.MOD_ALT, 0, 0, "", 1080, 1920, 1080, 1920, 80L),
        )
        assertTrue(
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, KeyMapper.HID_TAB, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 90L).isEmpty(),
        )
    }

    @Test
    fun textAndKeyboardEditingActions() {
        val mapper = HostInputMapper()

        assertEquals(
            listOf(HostAccessibilityAction.InsertText("Hello")),
            mapper.onEvent(BridgeConstants.HOST_INPUT_TEXT, 0, 0, 0, false, 0, 0, 0, "Hello", 1080, 1920, 1080, 1920, 10L),
        )
        assertEquals(
            listOf(HostAccessibilityAction.InsertText("A")),
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, 0x04, true, BridgeConstants.MOD_SHIFT, 0, 0, "", 1080, 1920, 1080, 1920, 20L),
        )
        // Ctrl+C must not inject literal 'c'
        assertTrue(
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, 0x06, true, BridgeConstants.MOD_CTRL, 0, 0, "", 1080, 1920, 1080, 1920, 30L).isEmpty(),
        )
        assertEquals(
            listOf(HostAccessibilityAction.Backspace),
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, KeyMapper.HID_BACKSPACE, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 40L),
        )
        assertEquals(
            listOf(HostAccessibilityAction.Enter),
            mapper.onEvent(BridgeConstants.HOST_INPUT_KEY, 0, 0, KeyMapper.HID_ENTER, true, 0, 0, 0, "", 1080, 1920, 1080, 1920, 50L),
        )

        assertEquals("abc", HostInputMapper.applyTextEdit("ab", HostAccessibilityAction.InsertText("c")))
        assertEquals("a", HostInputMapper.applyTextEdit("ab", HostAccessibilityAction.Backspace))
        assertEquals("", HostInputMapper.applyTextEdit("", HostAccessibilityAction.Backspace))
    }

    @Test
    fun encoderScalingAndSpsPpsKeyframes() {
        val (pw, ph) = ScreenCaptureEncoder.computeScaledDimensions(1440, 3120, maxLongEdge = 1080)
        assertTrue(ph <= 1088 && pw % 16 == 0 && ph % 16 == 0)
        assertTrue(ph > pw)

        val (lw, lh) = ScreenCaptureEncoder.computeScaledDimensions(3840, 2160, maxLongEdge = 1080)
        assertTrue(lw <= 1088 && lw % 16 == 0 && lh % 16 == 0)
        assertTrue(lw > lh)

        val csd = byteArrayOf(0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x68, 0x10)
        val idrOnly = byteArrayOf(0, 0, 0, 1, 0x65, 0x88.toByte(), 0x99.toByte())
        val merged = ScreenCaptureEncoder.ensureSpsPpsOnKeyframe(idrOnly, csd)
        assertTrue(ScreenCaptureEncoder.containsH264Sps(merged))
        assertArrayEquals(csd + idrOnly, merged)
        // Must not duplicate when SPS is already present
        assertArrayEquals(merged, ScreenCaptureEncoder.ensureSpsPpsOnKeyframe(merged, csd))
    }

    @Test
    fun networkIpClassifierAndTrustedPeerJsonRoundTrip() {
        assertEquals(IpKind.TAILSCALE, NetworkIpDetector.classifyIpv4("100.64.12.34"))
        assertEquals(IpKind.TAILSCALE, NetworkIpDetector.classifyIpv4("100.127.255.1"))
        assertEquals(IpKind.OTHER, NetworkIpDetector.classifyIpv4("100.63.0.1"))
        assertEquals(IpKind.LAN, NetworkIpDetector.classifyIpv4("192.168.1.42"))
        assertEquals(IpKind.LAN, NetworkIpDetector.classifyIpv4("10.0.0.5"))
        assertEquals(IpKind.LAN, NetworkIpDetector.classifyIpv4("172.16.4.9"))
        assertEquals(IpKind.OTHER, NetworkIpDetector.classifyIpv4("8.8.8.8"))

        val detected = NetworkIpDetector.partitionIps(
            listOf("127.0.0.1", "8.8.8.8", "10.0.0.8", "100.88.1.9", "192.168.1.50"),
        )
        assertEquals(listOf("192.168.1.50", "10.0.0.8"), detected.lanIps)
        assertEquals("100.88.1.9", detected.tailscaleIp)

        val peer = HostSavedTrustedPeer(
            publicKeyHex = "aabbccdd",
            deviceId = "123456789",
            name = "Gaming PC",
            fingerprint = "ab:cd:ef",
            allowInput = true,
            allowClipboard = true,
            allowAudio = false,
            allowPrivacyMode = true,
        )
        val json = HostSavedTrustedPeer.listToJson(listOf(peer))
        val parsed = HostSavedTrustedPeer.listFromJson(json)
        assertEquals(listOf(peer), parsed)
        assertNotNull(HostSavedTrustedPeer.fromJson(peer.toJsonObject().toString()))
        assertNull(HostSavedTrustedPeer.fromJson("{}"))
        assertEquals(HostPasswordMode.BOTH, HostPasswordMode.fromId(2))
        assertFalse(ScreenCaptureEncoder.containsH264Sps(byteArrayOf(0, 0, 1, 0x41, 0x01)))

        // Defense-in-depth auth sanitization
        assertEquals(
            Pair(HostPasswordMode.ONE_TIME, ""),
            HostController.sanitizeAuthParams(HostPasswordMode.ONE_TIME, "\$argon2id\$v=19\$fake"),
        )
        assertEquals(
            Pair(HostPasswordMode.ONE_TIME, ""),
            HostController.sanitizeAuthParams(HostPasswordMode.PERMANENT, "   "),
        )
        assertEquals(
            Pair(HostPasswordMode.PERMANENT, "\$argon2id\$v=19\$valid"),
            HostController.sanitizeAuthParams(HostPasswordMode.PERMANENT, " \$argon2id\$v=19\$valid "),
        )
    }
}

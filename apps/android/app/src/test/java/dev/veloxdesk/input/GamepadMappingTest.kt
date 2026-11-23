package dev.veloxdesk.input

import android.view.KeyEvent
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class GamepadMappingTest {

    @Test
    fun allXInputButtonsMatchProtocolBitmask() {
        val expected = mapOf(
            KeyEvent.KEYCODE_DPAD_UP to 0x0001,
            KeyEvent.KEYCODE_DPAD_DOWN to 0x0002,
            KeyEvent.KEYCODE_DPAD_LEFT to 0x0004,
            KeyEvent.KEYCODE_DPAD_RIGHT to 0x0008,
            KeyEvent.KEYCODE_BUTTON_START to 0x0010,
            KeyEvent.KEYCODE_BUTTON_SELECT to 0x0020,
            KeyEvent.KEYCODE_BUTTON_THUMBL to 0x0040,
            KeyEvent.KEYCODE_BUTTON_THUMBR to 0x0080,
            KeyEvent.KEYCODE_BUTTON_L1 to 0x0100,
            KeyEvent.KEYCODE_BUTTON_R1 to 0x0200,
            KeyEvent.KEYCODE_BUTTON_MODE to 0x0400,
            KeyEvent.KEYCODE_BUTTON_A to 0x1000,
            KeyEvent.KEYCODE_BUTTON_B to 0x2000,
            KeyEvent.KEYCODE_BUTTON_X to 0x4000,
            KeyEvent.KEYCODE_BUTTON_Y to 0x8000,
        )

        for ((keyCode, bit) in expected) {
            assertEquals("Mismatch for keyCode=$keyCode", bit, GamepadMapper.androidKeyCodeToXInputButton(keyCode))
        }
        assertNull(GamepadMapper.androidKeyCodeToXInputButton(KeyEvent.KEYCODE_VOLUME_UP))
    }

    @Test
    fun hatAxesMapToDpadBits() {
        assertEquals(0, GamepadMapper.hatAxesToDpadBits(0f, 0f))
        assertEquals(XInputButtons.DPAD_LEFT, GamepadMapper.hatAxesToDpadBits(-1f, 0f))
        assertEquals(XInputButtons.DPAD_RIGHT, GamepadMapper.hatAxesToDpadBits(1f, 0f))
        assertEquals(XInputButtons.DPAD_UP, GamepadMapper.hatAxesToDpadBits(0f, -1f))
        assertEquals(XInputButtons.DPAD_DOWN, GamepadMapper.hatAxesToDpadBits(0f, 1f))
        assertEquals(
            XInputButtons.DPAD_UP or XInputButtons.DPAD_RIGHT,
            GamepadMapper.hatAxesToDpadBits(0.8f, -0.8f),
        )
    }

    @Test
    fun triggerScalingClampsToByteRange() {
        assertEquals(0, GamepadMapper.scaleTrigger(-0.5f))
        assertEquals(0, GamepadMapper.scaleTrigger(0f))
        assertEquals(128, GamepadMapper.scaleTrigger(0.5f))
        assertEquals(255, GamepadMapper.scaleTrigger(1f))
        assertEquals(255, GamepadMapper.scaleTrigger(1.5f))
        assertEquals(0, GamepadMapper.scaleTrigger(Float.NaN))
    }

    @Test
    fun stickAxesApplyDeadzoneAndInvertY() {
        // Deadzone filters small noise around 0
        assertEquals(0, GamepadMapper.scaleStickAxis(0f, invert = false))
        assertEquals(0, GamepadMapper.scaleStickAxis(0.05f, invert = false))
        assertEquals(0, GamepadMapper.scaleStickAxis(-0.07f, invert = true))
        assertEquals(0, GamepadMapper.scaleStickAxis(Float.NaN, invert = false))

        // X axis: non-inverted
        assertEquals(-32768, GamepadMapper.scaleStickAxis(-1f, invert = false))
        assertEquals(32767, GamepadMapper.scaleStickAxis(1f, invert = false))

        // Y axis: inverted (Android -1 = up -> XInput +32767; Android +1 = down -> XInput -32768)
        assertEquals(32767, GamepadMapper.scaleStickAxis(-1f, invert = true))
        assertEquals(-32768, GamepadMapper.scaleStickAxis(1f, invert = true))

        // Mid-deflection produces positive/negative intermediate values
        val midRight = GamepadMapper.scaleStickAxis(0.54f, invert = false)
        assertTrue("Expected midRight near 16383, got $midRight", midRight in 15000..17500)
        val midUp = GamepadMapper.scaleStickAxis(-0.54f, invert = true)
        assertTrue("Expected midUp near 16383, got $midUp", midUp in 15000..17500)
    }

    @Test
    fun rumbleAmplitudeScales16BitToAndroidByteRange() {
        assertEquals(0, GamepadMapper.scaleRumbleAmplitude(0))
        assertEquals(0, GamepadMapper.scaleRumbleAmplitude(-10))
        assertEquals(1, GamepadMapper.scaleRumbleAmplitude(1))
        assertEquals(128, GamepadMapper.scaleRumbleAmplitude(32768))
        assertEquals(255, GamepadMapper.scaleRumbleAmplitude(65535))
        assertEquals(255, GamepadMapper.scaleRumbleAmplitude(99999))
    }

    @Test
    fun slotManagerAllocatesUpToFourPadsAndDisconnectsAll() {
        val manager = GamepadSlotManager()

        val p0 = manager.getOrAllocate(101)
        val p1 = manager.getOrAllocate(102)
        val p2 = manager.getOrAllocate(103)
        val p3 = manager.getOrAllocate(104)
        assertNotNull(p0)
        assertNotNull(p1)
        assertNotNull(p2)
        assertNotNull(p3)
        assertEquals(0, p0!!.padId)
        assertEquals(1, p1!!.padId)
        assertEquals(2, p2!!.padId)
        assertEquals(3, p3!!.padId)
        assertEquals(4, manager.connectedCount())

        // Existing device reuses its slot
        assertEquals(1, manager.getOrAllocate(102)!!.padId)

        // 5th distinct device rejected when all 4 slots are full
        assertNull(manager.getOrAllocate(105))

        // Verify button/hat merge and trigger fallback
        p0.keyButtons = XInputButtons.A
        p0.hatButtons = XInputButtons.DPAD_UP
        p0.digitalLeftTrigger = true
        p0.analogRightTrigger = 200
        val snap = p0.toSnapshot()
        assertEquals(XInputButtons.A or XInputButtons.DPAD_UP, snap.buttons)
        assertEquals(255, snap.leftTrigger)
        assertEquals(200, snap.rightTrigger)

        // disconnectAll clears all slots and emits connected=false snapshots
        val disconnected = manager.disconnectAll()
        assertEquals(4, disconnected.size)
        assertEquals(listOf(0, 1, 2, 3), disconnected.map { it.padId })
        assertTrue(disconnected.all { !it.connected && it.buttons == 0 })
        assertEquals(0, manager.connectedCount())

        // Slots can be allocated again cleanly
        assertEquals(0, manager.getOrAllocate(200)!!.padId)
        assertFalse(manager.deviceIdForPad(0) == null)
    }
}

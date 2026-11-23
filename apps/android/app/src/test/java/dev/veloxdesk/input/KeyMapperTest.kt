package dev.veloxdesk.input

import android.view.KeyEvent
import dev.veloxdesk.bridge.BridgeConstants
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyMapperTest {

    @Test
    fun primarySpecialKeysHaveExpectedModifiersAndHidUsages() {
        val byId = KeyMapper.primarySpecialKeys.associateBy { it.id }

        val ctrl = byId.getValue("ctrl")
        assertTrue(ctrl.isToggleModifier)
        assertEquals(0xE0, ctrl.hidUsage)
        assertEquals(BridgeConstants.MOD_CTRL, ctrl.modifierBit)

        val alt = byId.getValue("alt")
        assertTrue(alt.isToggleModifier)
        assertEquals(0xE2, alt.hidUsage)
        assertEquals(BridgeConstants.MOD_ALT, alt.modifierBit)

        val shift = byId.getValue("shift")
        assertTrue(shift.isToggleModifier)
        assertEquals(0xE1, shift.hidUsage)
        assertEquals(BridgeConstants.MOD_SHIFT, shift.modifierBit)

        val win = byId.getValue("win")
        assertTrue(win.isToggleModifier)
        assertEquals(0xE3, win.hidUsage)
        assertEquals(BridgeConstants.MOD_META, win.modifierBit)

        assertEquals(0x29, byId.getValue("esc").hidUsage)
        assertEquals(0x2B, byId.getValue("tab").hidUsage)
        assertEquals(0x4C, byId.getValue("del").hidUsage)
        assertEquals(0x4A, byId.getValue("home").hidUsage)
        assertEquals(0x4D, byId.getValue("end").hidUsage)
        assertEquals(0x50, byId.getValue("left").hidUsage)
        assertEquals(0x52, byId.getValue("up").hidUsage)
        assertEquals(0x51, byId.getValue("down").hidUsage)
        assertEquals(0x4F, byId.getValue("right").hidUsage)

        assertFalse(byId.getValue("esc").isToggleModifier)
    }

    @Test
    fun functionKeysCoverF1ThroughF12() {
        assertEquals(12, KeyMapper.functionKeys.size)
        for (i in 1..12) {
            val fk = KeyMapper.functionKeys[i - 1]
            assertEquals("f$i", fk.id)
            assertEquals("F$i", fk.label)
            assertEquals(0x39 + i, fk.hidUsage)
            assertFalse(fk.isToggleModifier)
        }
    }

    @Test
    fun androidKeyCodesMapToExactUsbHidUsages() {
        // Letters A..Z -> 0x04..0x1D
        assertEquals(0x04, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_A))
        assertEquals(0x1D, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_Z))
        for (i in 0..25) {
            assertEquals(0x04 + i, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_A + i))
        }

        // Digits 1..9 -> 0x1E..0x26, 0 -> 0x27
        for (i in 0..8) {
            assertEquals(0x1E + i, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_1 + i))
        }
        assertEquals(0x27, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_0))

        // Control & navigation
        assertEquals(0x28, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_ENTER))
        assertEquals(0x29, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_ESCAPE))
        assertEquals(0x2A, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_DEL)) // Backspace
        assertEquals(0x2B, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_TAB))
        assertEquals(0x2C, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_SPACE))
        assertEquals(0x49, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_INSERT))
        assertEquals(0x4A, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_MOVE_HOME))
        assertEquals(0x4B, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_PAGE_UP))
        assertEquals(0x4C, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_FORWARD_DEL)) // Delete
        assertEquals(0x4D, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_MOVE_END))
        assertEquals(0x4E, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_PAGE_DOWN))

        // Arrows
        assertEquals(0x4F, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_DPAD_RIGHT))
        assertEquals(0x50, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_DPAD_LEFT))
        assertEquals(0x51, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_DPAD_DOWN))
        assertEquals(0x52, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_DPAD_UP))

        // F1..F12
        for (i in 0..11) {
            assertEquals(0x3A + i, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_F1 + i))
        }

        // Modifiers
        assertEquals(0xE0, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_CTRL_LEFT))
        assertEquals(0xE1, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_SHIFT_LEFT))
        assertEquals(0xE2, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_ALT_LEFT))
        assertEquals(0xE3, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_META_LEFT))
        assertEquals(0xE4, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_CTRL_RIGHT))
        assertEquals(0xE5, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_SHIFT_RIGHT))
        assertEquals(0xE6, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_ALT_RIGHT))
        assertEquals(0xE7, KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_META_RIGHT))

        // Unknown returns null
        assertNull(KeyMapper.androidKeyCodeToHid(KeyEvent.KEYCODE_UNKNOWN))
    }

    @Test
    fun metaStateTranslatesToProtocolModifierBitmask() {
        assertEquals(0, KeyMapper.androidMetaStateToModifiers(0))
        assertEquals(BridgeConstants.MOD_SHIFT, KeyMapper.androidMetaStateToModifiers(KeyEvent.META_SHIFT_ON))
        assertEquals(BridgeConstants.MOD_CTRL, KeyMapper.androidMetaStateToModifiers(KeyEvent.META_CTRL_ON))
        assertEquals(BridgeConstants.MOD_ALT, KeyMapper.androidMetaStateToModifiers(KeyEvent.META_ALT_ON))
        assertEquals(BridgeConstants.MOD_META, KeyMapper.androidMetaStateToModifiers(KeyEvent.META_META_ON))
        assertEquals(BridgeConstants.MOD_CAPS_LOCK, KeyMapper.androidMetaStateToModifiers(KeyEvent.META_CAPS_LOCK_ON))
        assertEquals(BridgeConstants.MOD_NUM_LOCK, KeyMapper.androidMetaStateToModifiers(KeyEvent.META_NUM_LOCK_ON))

        val combined = KeyMapper.androidMetaStateToModifiers(
            KeyEvent.META_CTRL_ON or KeyEvent.META_ALT_ON or KeyEvent.META_SHIFT_ON,
        )
        assertEquals(
            BridgeConstants.MOD_CTRL or BridgeConstants.MOD_ALT or BridgeConstants.MOD_SHIFT,
            combined,
        )
    }

    @Test
    fun sendCtrlAltDelEmitsOrderedPressAndReleaseSequence() {
        data class SentKey(val hid: Int, val down: Boolean, val mods: Int)
        val events = mutableListOf<SentKey>()

        KeyMapper.sendCtrlAltDel { hid, down, mods ->
            events.add(SentKey(hid, down, mods))
        }

        val ctrl = BridgeConstants.MOD_CTRL
        val ctrlAlt = BridgeConstants.MOD_CTRL or BridgeConstants.MOD_ALT
        assertEquals(
            listOf(
                SentKey(0xE0, true, ctrl),
                SentKey(0xE2, true, ctrlAlt),
                SentKey(0x4C, true, ctrlAlt),
                SentKey(0x4C, false, ctrlAlt),
                SentKey(0xE2, false, ctrl),
                SentKey(0xE0, false, 0),
            ),
            events,
        )
    }
}

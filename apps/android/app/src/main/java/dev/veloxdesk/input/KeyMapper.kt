package dev.veloxdesk.input

import android.view.KeyEvent
import dev.veloxdesk.bridge.BridgeConstants

data class SpecialKeyDef(
    val id: String,
    val label: String,
    val hidUsage: Int,
    val modifierBit: Int = 0,
    val isToggleModifier: Boolean = false,
)

/**
 * Maps Android [KeyEvent] keycodes and meta states to USB HID Usage Page 0x07 codes
 * matching `crates/input/src/keymap.rs`.
 */
object KeyMapper {
    const val HID_ENTER = 0x28
    const val HID_ESCAPE = 0x29
    const val HID_BACKSPACE = 0x2A
    const val HID_TAB = 0x2B
    const val HID_SPACE = 0x2C
    const val HID_DELETE = 0x4C
    const val HID_LEFT_CTRL = 0xE0
    const val HID_LEFT_SHIFT = 0xE1
    const val HID_LEFT_ALT = 0xE2
    const val HID_LEFT_GUI = 0xE3
    const val HID_RIGHT_CTRL = 0xE4
    const val HID_RIGHT_SHIFT = 0xE5
    const val HID_RIGHT_ALT = 0xE6
    const val HID_RIGHT_GUI = 0xE7

    val primarySpecialKeys: List<SpecialKeyDef> = listOf(
        SpecialKeyDef(
            id = "ctrl",
            label = "Ctrl",
            hidUsage = HID_LEFT_CTRL,
            modifierBit = BridgeConstants.MOD_CTRL,
            isToggleModifier = true,
        ),
        SpecialKeyDef(
            id = "alt",
            label = "Alt",
            hidUsage = HID_LEFT_ALT,
            modifierBit = BridgeConstants.MOD_ALT,
            isToggleModifier = true,
        ),
        SpecialKeyDef(
            id = "shift",
            label = "Shift",
            hidUsage = HID_LEFT_SHIFT,
            modifierBit = BridgeConstants.MOD_SHIFT,
            isToggleModifier = true,
        ),
        SpecialKeyDef(
            id = "win",
            label = "Win",
            hidUsage = HID_LEFT_GUI,
            modifierBit = BridgeConstants.MOD_META,
            isToggleModifier = true,
        ),
        SpecialKeyDef(id = "esc", label = "Esc", hidUsage = HID_ESCAPE),
        SpecialKeyDef(id = "tab", label = "Tab", hidUsage = HID_TAB),
        SpecialKeyDef(id = "del", label = "Del", hidUsage = HID_DELETE),
        SpecialKeyDef(id = "home", label = "Home", hidUsage = 0x4A),
        SpecialKeyDef(id = "end", label = "End", hidUsage = 0x4D),
        SpecialKeyDef(id = "left", label = "←", hidUsage = 0x50),
        SpecialKeyDef(id = "up", label = "↑", hidUsage = 0x52),
        SpecialKeyDef(id = "down", label = "↓", hidUsage = 0x51),
        SpecialKeyDef(id = "right", label = "→", hidUsage = 0x4F),
    )

    val functionKeys: List<SpecialKeyDef> = (1..12).map { num ->
        SpecialKeyDef(
            id = "f$num",
            label = "F$num",
            hidUsage = 0x39 + num, // F1 = 0x3A .. F12 = 0x45
        )
    }

    fun androidKeyCodeToHid(keyCode: Int): Int? = when (keyCode) {
        // Letters A..Z -> 0x04..0x1D
        in KeyEvent.KEYCODE_A..KeyEvent.KEYCODE_Z -> 0x04 + (keyCode - KeyEvent.KEYCODE_A)

        // Digits 1..9 -> 0x1E..0x26, 0 -> 0x27
        in KeyEvent.KEYCODE_1..KeyEvent.KEYCODE_9 -> 0x1E + (keyCode - KeyEvent.KEYCODE_1)
        KeyEvent.KEYCODE_0 -> 0x27

        // Control & whitespace
        KeyEvent.KEYCODE_ENTER -> HID_ENTER
        KeyEvent.KEYCODE_ESCAPE -> HID_ESCAPE
        KeyEvent.KEYCODE_DEL -> HID_BACKSPACE // Android KEYCODE_DEL is Backspace
        KeyEvent.KEYCODE_TAB -> HID_TAB
        KeyEvent.KEYCODE_SPACE -> HID_SPACE

        // Punctuation
        KeyEvent.KEYCODE_MINUS -> 0x2D
        KeyEvent.KEYCODE_EQUALS -> 0x2E
        KeyEvent.KEYCODE_LEFT_BRACKET -> 0x2F
        KeyEvent.KEYCODE_RIGHT_BRACKET -> 0x30
        KeyEvent.KEYCODE_BACKSLASH -> 0x31
        KeyEvent.KEYCODE_SEMICOLON -> 0x33
        KeyEvent.KEYCODE_APOSTROPHE -> 0x34
        KeyEvent.KEYCODE_GRAVE -> 0x35
        KeyEvent.KEYCODE_COMMA -> 0x36
        KeyEvent.KEYCODE_PERIOD -> 0x37
        KeyEvent.KEYCODE_SLASH -> 0x38
        KeyEvent.KEYCODE_CAPS_LOCK -> 0x39

        // Function keys F1..F12 -> 0x3A..0x45
        in KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12 -> 0x3A + (keyCode - KeyEvent.KEYCODE_F1)

        // System & navigation
        KeyEvent.KEYCODE_SYSRQ -> 0x46
        KeyEvent.KEYCODE_SCROLL_LOCK -> 0x47
        KeyEvent.KEYCODE_BREAK -> 0x48
        KeyEvent.KEYCODE_INSERT -> 0x49
        KeyEvent.KEYCODE_MOVE_HOME -> 0x4A
        KeyEvent.KEYCODE_PAGE_UP -> 0x4B
        KeyEvent.KEYCODE_FORWARD_DEL -> HID_DELETE
        KeyEvent.KEYCODE_MOVE_END -> 0x4D
        KeyEvent.KEYCODE_PAGE_DOWN -> 0x4E
        KeyEvent.KEYCODE_DPAD_RIGHT -> 0x4F
        KeyEvent.KEYCODE_DPAD_LEFT -> 0x50
        KeyEvent.KEYCODE_DPAD_DOWN -> 0x51
        KeyEvent.KEYCODE_DPAD_UP -> 0x52

        // Numpad
        KeyEvent.KEYCODE_NUM_LOCK -> 0x53
        KeyEvent.KEYCODE_NUMPAD_DIVIDE -> 0x54
        KeyEvent.KEYCODE_NUMPAD_MULTIPLY -> 0x55
        KeyEvent.KEYCODE_NUMPAD_SUBTRACT -> 0x56
        KeyEvent.KEYCODE_NUMPAD_ADD -> 0x57
        KeyEvent.KEYCODE_NUMPAD_ENTER -> 0x58
        in KeyEvent.KEYCODE_NUMPAD_1..KeyEvent.KEYCODE_NUMPAD_9 -> 0x59 + (keyCode - KeyEvent.KEYCODE_NUMPAD_1)
        KeyEvent.KEYCODE_NUMPAD_0 -> 0x62
        KeyEvent.KEYCODE_NUMPAD_DOT -> 0x63
        KeyEvent.KEYCODE_MENU -> 0x65
        KeyEvent.KEYCODE_NUMPAD_EQUALS -> 0x67
        KeyEvent.KEYCODE_NUMPAD_COMMA -> 0x85

        // Volume / Media
        KeyEvent.KEYCODE_VOLUME_MUTE -> 0x7F
        KeyEvent.KEYCODE_VOLUME_UP -> 0x80
        KeyEvent.KEYCODE_VOLUME_DOWN -> 0x81

        // Modifiers
        KeyEvent.KEYCODE_CTRL_LEFT -> HID_LEFT_CTRL
        KeyEvent.KEYCODE_SHIFT_LEFT -> HID_LEFT_SHIFT
        KeyEvent.KEYCODE_ALT_LEFT -> HID_LEFT_ALT
        KeyEvent.KEYCODE_META_LEFT -> HID_LEFT_GUI
        KeyEvent.KEYCODE_CTRL_RIGHT -> HID_RIGHT_CTRL
        KeyEvent.KEYCODE_SHIFT_RIGHT -> HID_RIGHT_SHIFT
        KeyEvent.KEYCODE_ALT_RIGHT -> HID_RIGHT_ALT
        KeyEvent.KEYCODE_META_RIGHT -> HID_RIGHT_GUI

        else -> null
    }

    fun androidMetaStateToModifiers(metaState: Int): Int {
        var mods = 0
        if ((metaState and KeyEvent.META_SHIFT_ON) != 0) mods = mods or BridgeConstants.MOD_SHIFT
        if ((metaState and KeyEvent.META_CTRL_ON) != 0) mods = mods or BridgeConstants.MOD_CTRL
        if ((metaState and KeyEvent.META_ALT_ON) != 0) mods = mods or BridgeConstants.MOD_ALT
        if ((metaState and KeyEvent.META_META_ON) != 0) mods = mods or BridgeConstants.MOD_META
        if ((metaState and KeyEvent.META_CAPS_LOCK_ON) != 0) mods = mods or BridgeConstants.MOD_CAPS_LOCK
        if ((metaState and KeyEvent.META_NUM_LOCK_ON) != 0) mods = mods or BridgeConstants.MOD_NUM_LOCK
        return mods
    }

    fun sendCtrlAltDel(sendKey: (hidUsage: Int, down: Boolean, modifiers: Int) -> Unit) {
        val ctrl = BridgeConstants.MOD_CTRL
        val ctrlAlt = BridgeConstants.MOD_CTRL or BridgeConstants.MOD_ALT
        sendKey(HID_LEFT_CTRL, true, ctrl)
        sendKey(HID_LEFT_ALT, true, ctrlAlt)
        sendKey(HID_DELETE, true, ctrlAlt)
        sendKey(HID_DELETE, false, ctrlAlt)
        sendKey(HID_LEFT_ALT, false, ctrl)
        sendKey(HID_LEFT_CTRL, false, 0)
    }
}

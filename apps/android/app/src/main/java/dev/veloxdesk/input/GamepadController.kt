package dev.veloxdesk.input

import android.content.Context
import android.os.Build
import android.os.CombinedVibration
import android.os.VibrationEffect
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import kotlin.math.abs
import kotlin.math.max
import kotlin.math.roundToInt
import kotlin.math.sign

interface GamepadSink {
    fun sendGamepad(
        padId: Int,
        connected: Boolean,
        buttons: Int,
        leftTrigger: Int,
        rightTrigger: Int,
        leftX: Int,
        leftY: Int,
        rightX: Int,
        rightY: Int,
    )
}

/**
 * Standard XInput `wButtons` bitflags matching `crates/input/src/gamepad.rs`.
 */
internal object XInputButtons {
    const val DPAD_UP = 0x0001
    const val DPAD_DOWN = 0x0002
    const val DPAD_LEFT = 0x0004
    const val DPAD_RIGHT = 0x0008
    const val START = 0x0010
    const val BACK = 0x0020
    const val L_THUMB = 0x0040
    const val R_THUMB = 0x0080
    const val L_SHOULDER = 0x0100
    const val R_SHOULDER = 0x0200
    const val GUIDE = 0x0400
    const val A = 0x1000
    const val B = 0x2000
    const val X = 0x4000
    const val Y = 0x8000
}

internal data class GamepadSnapshot(
    val padId: Int,
    val connected: Boolean,
    val buttons: Int,
    val leftTrigger: Int,
    val rightTrigger: Int,
    val leftX: Int,
    val leftY: Int,
    val rightX: Int,
    val rightY: Int,
)

/**
 * Pure Android-to-XInput mapping helpers, extracted for JVM unit testing.
 */
internal object GamepadMapper {
    const val MAX_PADS = 4
    const val DEFAULT_DEADZONE = 0.08f
    private const val HAT_THRESHOLD = 0.5f

    fun androidKeyCodeToXInputButton(keyCode: Int): Int? = when (keyCode) {
        KeyEvent.KEYCODE_DPAD_UP -> XInputButtons.DPAD_UP
        KeyEvent.KEYCODE_DPAD_DOWN -> XInputButtons.DPAD_DOWN
        KeyEvent.KEYCODE_DPAD_LEFT -> XInputButtons.DPAD_LEFT
        KeyEvent.KEYCODE_DPAD_RIGHT -> XInputButtons.DPAD_RIGHT
        KeyEvent.KEYCODE_BUTTON_START -> XInputButtons.START
        KeyEvent.KEYCODE_BUTTON_SELECT -> XInputButtons.BACK
        KeyEvent.KEYCODE_BUTTON_THUMBL -> XInputButtons.L_THUMB
        KeyEvent.KEYCODE_BUTTON_THUMBR -> XInputButtons.R_THUMB
        KeyEvent.KEYCODE_BUTTON_L1 -> XInputButtons.L_SHOULDER
        KeyEvent.KEYCODE_BUTTON_R1 -> XInputButtons.R_SHOULDER
        KeyEvent.KEYCODE_BUTTON_MODE -> XInputButtons.GUIDE
        KeyEvent.KEYCODE_BUTTON_A -> XInputButtons.A
        KeyEvent.KEYCODE_BUTTON_B -> XInputButtons.B
        KeyEvent.KEYCODE_BUTTON_X -> XInputButtons.X
        KeyEvent.KEYCODE_BUTTON_Y -> XInputButtons.Y
        else -> null
    }

    fun hatAxesToDpadBits(hatX: Float, hatY: Float): Int {
        var bits = 0
        if (hatX <= -HAT_THRESHOLD) bits = bits or XInputButtons.DPAD_LEFT
        if (hatX >= HAT_THRESHOLD) bits = bits or XInputButtons.DPAD_RIGHT
        if (hatY <= -HAT_THRESHOLD) bits = bits or XInputButtons.DPAD_UP
        if (hatY >= HAT_THRESHOLD) bits = bits or XInputButtons.DPAD_DOWN
        return bits
    }

    fun scaleTrigger(rawValue: Float): Int {
        if (!rawValue.isFinite() || rawValue <= 0f) return 0
        if (rawValue >= 1f) return 255
        return (rawValue * 255f).roundToInt().coerceIn(0, 255)
    }

    /**
     * Converts a normalized Android stick axis (`-1.0..1.0`) to XInput signed 16-bit (`-32768..32767`)
     * with radial/linear deadzone rescaling and optional Y-axis inversion (Android +Y is down, XInput +Y is up).
     */
    fun scaleStickAxis(
        rawValue: Float,
        invert: Boolean,
        deadzone: Float = DEFAULT_DEADZONE,
    ): Int {
        if (!rawValue.isFinite()) return 0
        val clamped = rawValue.coerceIn(-1f, 1f)
        val magnitude = abs(clamped)
        if (magnitude < deadzone) return 0

        val normalized = ((magnitude - deadzone) / (1f - deadzone)).coerceIn(0f, 1f)
        val signed = sign(clamped) * normalized
        val directed = if (invert) -signed else signed

        return if (directed >= 0f) {
            (directed * 32767f).roundToInt().coerceIn(0, 32767)
        } else {
            (directed * 32768f).roundToInt().coerceIn(-32768, 0)
        }
    }

    fun scaleRumbleAmplitude(freq16: Int): Int {
        val clamped = freq16.coerceIn(0, 65535)
        if (clamped == 0) return 0
        return ((clamped * 255 + 32767) / 65535).coerceIn(1, 255)
    }
}

/**
 * Pure slot allocator & per-pad state tracker supporting up to 4 concurrent gamepads.
 */
internal class GamepadSlotManager(private val maxPads: Int = GamepadMapper.MAX_PADS) {
    internal data class PadState(
        val padId: Int,
        val deviceId: Int,
        var keyButtons: Int = 0,
        var hatButtons: Int = 0,
        var analogLeftTrigger: Int = 0,
        var analogRightTrigger: Int = 0,
        var digitalLeftTrigger: Boolean = false,
        var digitalRightTrigger: Boolean = false,
        var leftX: Int = 0,
        var leftY: Int = 0,
        var rightX: Int = 0,
        var rightY: Int = 0,
    ) {
        fun toSnapshot(): GamepadSnapshot {
            val lt = if (analogLeftTrigger > 0) analogLeftTrigger else if (digitalLeftTrigger) 255 else 0
            val rt = if (analogRightTrigger > 0) analogRightTrigger else if (digitalRightTrigger) 255 else 0
            return GamepadSnapshot(
                padId = padId,
                connected = true,
                buttons = (keyButtons or hatButtons) and 0xFFFF,
                leftTrigger = lt,
                rightTrigger = rt,
                leftX = leftX,
                leftY = leftY,
                rightX = rightX,
                rightY = rightY,
            )
        }
    }

    private val slots = arrayOfNulls<PadState>(maxPads)

    fun getOrAllocate(deviceId: Int): PadState? {
        for (slot in slots) {
            if (slot != null && slot.deviceId == deviceId) return slot
        }
        for (i in slots.indices) {
            if (slots[i] == null) {
                val created = PadState(padId = i, deviceId = deviceId)
                slots[i] = created
                return created
            }
        }
        return null
    }

    fun deviceIdForPad(padId: Int): Int? {
        if (padId !in slots.indices) return null
        return slots[padId]?.deviceId
    }

    fun disconnectAll(): List<GamepadSnapshot> {
        val disconnected = ArrayList<GamepadSnapshot>(maxPads)
        for (i in slots.indices) {
            if (slots[i] != null) {
                slots[i] = null
                disconnected.add(
                    GamepadSnapshot(
                        padId = i,
                        connected = false,
                        buttons = 0,
                        leftTrigger = 0,
                        rightTrigger = 0,
                        leftX = 0,
                        leftY = 0,
                        rightX = 0,
                        rightY = 0,
                    ),
                )
            }
        }
        return disconnected
    }

    fun connectedCount(): Int = slots.count { it != null }
}

class GamepadController(private val sink: GamepadSink) {
    private val lock = Any()
    private val slotManager = GamepadSlotManager()

    fun isGamepadDevice(device: InputDevice?): Boolean {
        if (device == null) return false
        val sources = device.sources
        val hasGamepad = (sources and InputDevice.SOURCE_GAMEPAD) == InputDevice.SOURCE_GAMEPAD
        val hasJoystick = (sources and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK
        return hasGamepad || hasJoystick
    }

    fun onKeyEvent(event: KeyEvent): Boolean {
        if (!isGamepadDevice(event.device)) return false
        val action = event.action
        if (action != KeyEvent.ACTION_DOWN && action != KeyEvent.ACTION_UP) return false

        val isDown = action == KeyEvent.ACTION_DOWN
        val keyCode = event.keyCode
        val buttonBit = GamepadMapper.androidKeyCodeToXInputButton(keyCode)
        val isDigitalL2 = keyCode == KeyEvent.KEYCODE_BUTTON_L2
        val isDigitalR2 = keyCode == KeyEvent.KEYCODE_BUTTON_R2

        if (buttonBit == null && !isDigitalL2 && !isDigitalR2) {
            return false
        }
        if (isDown && event.repeatCount > 0) {
            return true
        }

        val snapshot = synchronized(lock) {
            val pad = slotManager.getOrAllocate(event.deviceId) ?: return false
            if (buttonBit != null) {
                pad.keyButtons = if (isDown) {
                    pad.keyButtons or buttonBit
                } else {
                    pad.keyButtons and buttonBit.inv()
                }
            } else if (isDigitalL2) {
                pad.digitalLeftTrigger = isDown
            } else if (isDigitalR2) {
                pad.digitalRightTrigger = isDown
            }
            pad.toSnapshot()
        }

        emitSnapshot(snapshot)
        return true
    }

    fun onGenericMotionEvent(event: MotionEvent): Boolean {
        val isJoystickSource = (event.source and InputDevice.SOURCE_JOYSTICK) == InputDevice.SOURCE_JOYSTICK
        if (!isJoystickSource && !isGamepadDevice(event.device)) return false
        if (event.action != MotionEvent.ACTION_MOVE) return false

        val device = event.device
        val hatX = event.getAxisValue(MotionEvent.AXIS_HAT_X)
        val hatY = event.getAxisValue(MotionEvent.AXIS_HAT_Y)

        val lTriggerRaw = max(
            event.getAxisValue(MotionEvent.AXIS_LTRIGGER),
            event.getAxisValue(MotionEvent.AXIS_BRAKE),
        )
        val rTriggerRaw = max(
            event.getAxisValue(MotionEvent.AXIS_RTRIGGER),
            event.getAxisValue(MotionEvent.AXIS_GAS),
        )

        val lxRaw = event.getAxisValue(MotionEvent.AXIS_X)
        val lyRaw = event.getAxisValue(MotionEvent.AXIS_Y)

        val hasZRange = device?.getMotionRange(MotionEvent.AXIS_Z, event.source) != null ||
            device?.getMotionRange(MotionEvent.AXIS_RZ, event.source) != null
        val hasRxRange = device?.getMotionRange(MotionEvent.AXIS_RX, event.source) != null ||
            device?.getMotionRange(MotionEvent.AXIS_RY, event.source) != null

        val rxRaw: Float
        val ryRaw: Float
        if (hasZRange || !hasRxRange) {
            rxRaw = event.getAxisValue(MotionEvent.AXIS_Z)
            ryRaw = event.getAxisValue(MotionEvent.AXIS_RZ)
        } else {
            rxRaw = event.getAxisValue(MotionEvent.AXIS_RX)
            ryRaw = event.getAxisValue(MotionEvent.AXIS_RY)
        }

        val snapshot = synchronized(lock) {
            val pad = slotManager.getOrAllocate(event.deviceId) ?: return false
            pad.hatButtons = GamepadMapper.hatAxesToDpadBits(hatX, hatY)
            pad.analogLeftTrigger = GamepadMapper.scaleTrigger(lTriggerRaw)
            pad.analogRightTrigger = GamepadMapper.scaleTrigger(rTriggerRaw)
            pad.leftX = GamepadMapper.scaleStickAxis(lxRaw, invert = false)
            pad.leftY = GamepadMapper.scaleStickAxis(lyRaw, invert = true)
            pad.rightX = GamepadMapper.scaleStickAxis(rxRaw, invert = false)
            pad.rightY = GamepadMapper.scaleStickAxis(ryRaw, invert = true)
            pad.toSnapshot()
        }

        emitSnapshot(snapshot)
        return true
    }

    fun applyRumble(context: Context, padId: Int, lowFreq: Int, highFreq: Int) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return
        val deviceId = synchronized(lock) { slotManager.deviceIdForPad(padId) } ?: return
        val device = InputDevice.getDevice(deviceId) ?: return

        try {
            val vm = device.vibratorManager
            val vibratorIds = vm.vibratorIds
            if (vibratorIds.isEmpty()) return

            val lowAmp = GamepadMapper.scaleRumbleAmplitude(lowFreq)
            val highAmp = GamepadMapper.scaleRumbleAmplitude(highFreq)

            if (lowAmp == 0 && highAmp == 0) {
                vm.cancel()
                return
            }

            if (vibratorIds.size >= 2) {
                val parallel = CombinedVibration.startParallel()
                var added = false
                if (lowAmp > 0) {
                    parallel.addVibrator(vibratorIds[0], VibrationEffect.createOneShot(RUMBLE_DURATION_MS, lowAmp))
                    added = true
                }
                if (highAmp > 0) {
                    parallel.addVibrator(vibratorIds[1], VibrationEffect.createOneShot(RUMBLE_DURATION_MS, highAmp))
                    added = true
                }
                if (added) {
                    vm.vibrate(parallel.combine())
                } else {
                    vm.cancel()
                }
            } else {
                val amp = max(lowAmp, highAmp)
                if (amp > 0) {
                    val effect = CombinedVibration.createParallel(
                        VibrationEffect.createOneShot(RUMBLE_DURATION_MS, amp),
                    )
                    vm.vibrate(effect)
                } else {
                    vm.cancel()
                }
            }
        } catch (_: Throwable) {
            // Gracefully ignore missing vibrator hardware or permission errors.
        }
    }

    fun disconnectAll() {
        val snapshots = synchronized(lock) {
            slotManager.disconnectAll()
        }
        for (snap in snapshots) {
            emitSnapshot(snap)
        }
    }

    private fun emitSnapshot(s: GamepadSnapshot) {
        sink.sendGamepad(
            padId = s.padId,
            connected = s.connected,
            buttons = s.buttons,
            leftTrigger = s.leftTrigger,
            rightTrigger = s.rightTrigger,
            leftX = s.leftX,
            leftY = s.leftY,
            rightX = s.rightX,
            rightY = s.rightY,
        )
    }

    companion object {
        private const val RUMBLE_DURATION_MS = 250L
    }
}

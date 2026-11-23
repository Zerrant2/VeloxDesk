package dev.veloxdesk.bridge

/**
 * Shared protocol and bridge constants matching `veloxdesk-protocol` and `crates/android-bridge`.
 */
object BridgeConstants {
    // SessionMode (matches veloxdesk_protocol::control::SessionMode)
    const val MODE_DESKTOP = 0
    const val MODE_GAMING = 1

    // VideoCodec (matches veloxdesk_protocol::control::VideoCodec)
    const val CODEC_UNSPECIFIED = 0
    const val CODEC_H264 = 1
    const val CODEC_H265 = 2
    const val CODEC_AV1 = 3
    const val CODEC_VP9 = 4

    // PathKind (matches veloxdesk_core_traits::transport::PathKind)
    const val PATH_UNKNOWN = 0
    const val PATH_DIRECT = 1
    const val PATH_P2P_HOLE_PUNCH = 2
    const val PATH_RELAY = 3

    // StatusCode emitted by Rust -> onStatus(statusCode, detail)
    const val STATUS_CONNECTING = 0
    const val STATUS_WAITING_FOR_HOST = 1
    const val STATUS_AUTHENTICATING = 2
    const val STATUS_WAITING_FOR_ACCEPT = 3
    const val STATUS_CONNECTED = 4
    const val STATUS_RECONNECTING = 5

    // MouseButton (matches veloxdesk_protocol::control::MouseButton)
    const val MOUSE_LEFT = 0
    const val MOUSE_RIGHT = 1
    const val MOUSE_MIDDLE = 2
    const val MOUSE_BACK = 3
    const val MOUSE_FORWARD = 4

    // Keyboard modifier bitmask (matches veloxdesk_protocol::control::KeyEvent.modifiers)
    const val MOD_SHIFT = 1
    const val MOD_CTRL = 2
    const val MOD_ALT = 4
    const val MOD_META = 8
    const val MOD_CAPS_LOCK = 16
    const val MOD_NUM_LOCK = 32

    // CodecCapsMask passed to nativeConnect
    const val CAP_H264_HW = 1 shl 0
    const val CAP_H265_HW = 1 shl 1
    const val CAP_AV1_HW = 1 shl 2
    const val CAP_H264_SW = 1 shl 3
    const val CAP_H265_SW = 1 shl 4
    const val CAP_AV1_SW = 1 shl 5

    // Default ports (matches veloxdesk-protocol)
    const val DEFAULT_RENDEZVOUS_PORT = 21116
    const val DEFAULT_RELAY_PORT = 21117
    const val DEFAULT_DIRECT_PORT = 21118
}

enum class ScaleMode(val id: Int) {
    FIT(0),
    STRETCH(1),
    ORIGINAL(2);

    companion object {
        fun fromId(id: Int): ScaleMode = entries.firstOrNull { it.id == id } ?: FIT
    }
}

enum class TouchMode(val id: Int) {
    TRACKPAD(0),
    DIRECT(1);

    companion object {
        fun fromId(id: Int): TouchMode = entries.firstOrNull { it.id == id } ?: TRACKPAD
    }
}

data class DisplayDescriptor(
    val index: Int,
    val name: String,
    val x: Int,
    val y: Int,
    val width: Int,
    val height: Int,
    val refreshHz: Int,
    val primary: Boolean,
)

data class SessionInfo(
    val hostName: String,
    val hostOs: String,
    val mode: Int,
    val activeDisplay: Int,
    val videoCodec: Int,
    val width: Int,
    val height: Int,
    val fps: Int,
    val encoderName: String,
    val hwEncoder: Boolean,
    val audioSampleRate: Int,
    val audioChannels: Int,
    val audioFrameUs: Int,
    val displays: List<DisplayDescriptor>,
)

data class HudStats(
    val fps: Float = 0f,
    val bitrateKbps: Int = 0,
    val rttMs: Float = 0f,
    val lossPct: Float = 0f,
    val fecPct: Float = 100f,
    val captureMs: Float = 0f,
    val encodeMs: Float = 0f,
    val netMs: Float = 0f,
    val decodeMs: Float = 0f,
    val totalMs: Float = 0f,
    val pathKind: Int = BridgeConstants.PATH_UNKNOWN,
)

data class AuthPromptData(
    val hostDeviceId: String,
    val hostFingerprint: String,
    val allowPerm: Boolean,
    val allowOtp: Boolean,
)

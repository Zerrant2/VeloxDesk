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

    // Host input event kinds emitted by Rust -> VeloxHostCallback.onHostInputEvent
    const val HOST_INPUT_MOUSE_MOVE = 0
    const val HOST_INPUT_MOUSE_BUTTON = 1
    const val HOST_INPUT_MOUSE_WHEEL = 2
    const val HOST_INPUT_KEY = 3
    const val HOST_INPUT_TEXT = 4

    // Mouse button codes in onHostInputEvent(kind = HOST_INPUT_MOUSE_BUTTON)
    const val HOST_MOUSE_BTN_LEFT = 1
    const val HOST_MOUSE_BTN_RIGHT = 2
    const val HOST_MOUSE_BTN_MIDDLE = 3
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

enum class HostPasswordMode(val id: Int) {
    ONE_TIME(0),
    PERMANENT(1),
    BOTH(2);

    companion object {
        fun fromId(id: Int): HostPasswordMode =
            entries.firstOrNull { it.id == id } ?: ONE_TIME
    }
}

data class HostSavedTrustedPeer(
    val publicKeyHex: String,
    val deviceId: String,
    val name: String,
    val fingerprint: String,
    val allowInput: Boolean = true,
    val allowClipboard: Boolean = true,
    val allowFileTransfer: Boolean = false,
    val allowAudio: Boolean = true,
    val allowTerminal: Boolean = false,
    val allowTunnel: Boolean = false,
    val allowPrivacyMode: Boolean = false,
) {
    fun toJsonObject(): org.json.JSONObject = org.json.JSONObject().apply {
        put("publicKeyHex", publicKeyHex)
        put("deviceId", deviceId)
        put("name", name)
        put("fingerprint", fingerprint)
        put("allowInput", allowInput)
        put("allowClipboard", allowClipboard)
        put("allowFileTransfer", allowFileTransfer)
        put("allowAudio", allowAudio)
        put("allowTerminal", allowTerminal)
        put("allowTunnel", allowTunnel)
        put("allowPrivacyMode", allowPrivacyMode)
    }

    companion object {
        fun fromJsonObject(obj: org.json.JSONObject): HostSavedTrustedPeer? {
            val pk = obj.optString("publicKeyHex", "").trim()
            if (pk.isEmpty()) return null
            return HostSavedTrustedPeer(
                publicKeyHex = pk,
                deviceId = obj.optString("deviceId", ""),
                name = obj.optString("name", ""),
                fingerprint = obj.optString("fingerprint", ""),
                allowInput = obj.optBoolean("allowInput", true),
                allowClipboard = obj.optBoolean("allowClipboard", true),
                allowFileTransfer = obj.optBoolean("allowFileTransfer", false),
                allowAudio = obj.optBoolean("allowAudio", true),
                allowTerminal = obj.optBoolean("allowTerminal", false),
                allowTunnel = obj.optBoolean("allowTunnel", false),
                allowPrivacyMode = obj.optBoolean("allowPrivacyMode", false),
            )
        }

        fun fromJson(json: String): HostSavedTrustedPeer? = try {
            fromJsonObject(org.json.JSONObject(json))
        } catch (_: Throwable) {
            null
        }

        fun listToJson(peers: List<HostSavedTrustedPeer>): String {
            val arr = org.json.JSONArray()
            peers.forEach { arr.put(it.toJsonObject()) }
            return arr.toString()
        }

        fun listFromJson(json: String?): List<HostSavedTrustedPeer> {
            if (json.isNullOrBlank()) return emptyList()
            return try {
                val arr = org.json.JSONArray(json)
                buildList(arr.length()) {
                    for (i in 0 until arr.length()) {
                        val obj = arr.optJSONObject(i) ?: continue
                        fromJsonObject(obj)?.let(::add)
                    }
                }
            } catch (_: Throwable) {
                emptyList()
            }
        }
    }
}

data class HostApprovalRequest(
    val requestId: Long,
    val peerDeviceId: String,
    val peerName: String,
    val peerFingerprint: String,
    val peerPubKeyHex: String,
)

data class HostUiState(
    val isRunning: Boolean = false,
    val isStarting: Boolean = false,
    val deviceId: String = "",
    val otp: String = "",
    val port: Int = BridgeConstants.DEFAULT_DIRECT_PORT,
    val lanIps: List<String> = emptyList(),
    val tailscaleIp: String? = null,
    val activePeerDeviceId: String? = null,
    val activePeerName: String? = null,
    val captureWidth: Int = 0,
    val captureHeight: Int = 0,
    val captureFps: Int = 30,
    val accessibilityEnabled: Boolean = false,
    val error: String? = null,
)

package dev.veloxdesk.bridge

/**
 * Frozen JNI gateway to `libveloxdesk_android.so` (`crates/android-bridge`).
 *
 * All methods are `@JvmStatic` so Rust JNI symbols map deterministically to
 * `Java_dev_veloxdesk_bridge_VeloxNative_<methodName>`.
 */
object VeloxNative {
    @Volatile
    var isLoaded: Boolean = false
        private set

    @Volatile
    var loadError: String? = null
        private set

    init {
        try {
            System.loadLibrary("veloxdesk_android")
            isLoaded = true
        } catch (t: Throwable) {
            isLoaded = false
            loadError = t.message ?: t.javaClass.simpleName
        }
    }

    @JvmStatic
    external fun nativeInitLogging()

    @JvmStatic
    external fun nativeGetBuildDisplay(): String

    @JvmStatic
    external fun nativeInitIdentity(filesDir: String): String

    @JvmStatic
    external fun nativeGetFingerprint(filesDir: String): String

    @JvmStatic
    external fun nativeConnect(
        filesDir: String,
        target: String,
        rendezvousAddr: String,
        relayOnly: Boolean,
        password: String?,
        mode: Int,
        maxBitrateKbps: Int,
        audioEnabled: Boolean,
        clipboardEnabled: Boolean,
        viewerWidth: Int,
        viewerHeight: Int,
        viewerRefreshHz: Int,
        deviceName: String,
        codecCapsMask: Int,
        callback: VeloxSessionCallback,
    ): Long

    @JvmStatic
    external fun nativeSubmitPassword(handle: Long, password: String)

    @JvmStatic
    external fun nativeCancelAuth(handle: Long)

    @JvmStatic
    external fun nativeDisconnect(handle: Long)

    @JvmStatic
    external fun nativeRequestKeyframe(handle: Long)

    @JvmStatic
    external fun nativeReportDecodeTimeUs(handle: Long, decodeUs: Int, renderUs: Int)

    @JvmStatic
    external fun nativeSetMaxBitrate(handle: Long, bitrateKbps: Int)

    @JvmStatic
    external fun nativeSwitchDisplay(handle: Long, displayIndex: Int)

    @JvmStatic
    external fun nativeSetAudioEnabled(handle: Long, enabled: Boolean)

    @JvmStatic
    external fun nativeSetClipboardEnabled(handle: Long, enabled: Boolean)

    @JvmStatic
    external fun nativeSendMouseMove(handle: Long, x: Int, y: Int, display: Int)

    @JvmStatic
    external fun nativeSendMouseRelative(handle: Long, dx: Int, dy: Int)

    @JvmStatic
    external fun nativeSendMouseButton(handle: Long, button: Int, down: Boolean)

    @JvmStatic
    external fun nativeSendMouseWheel(handle: Long, dx: Int, dy: Int)

    @JvmStatic
    external fun nativeSendKey(handle: Long, hidUsage: Int, down: Boolean, modifiers: Int)

    @JvmStatic
    external fun nativeSendText(handle: Long, text: String)

    @JvmStatic
    external fun nativeSendGamepad(
        handle: Long,
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

    @JvmStatic
    external fun nativeSendClipboardText(handle: Long, text: String)

    @JvmStatic
    external fun nativeSendChat(handle: Long, senderName: String, text: String): Long

    // ───────────────────────────── Host / Password Hashing (v0.4.0) ─────────────────────────────

    @JvmStatic
    external fun nativeHashPassword(password: String): String

    @JvmStatic
    external fun nativeIsValidPhcHash(phc: String): Boolean

    @JvmStatic
    external fun nativeHostStart(
        filesDir: String,
        rendezvousAddr: String,
        deviceName: String,
        width: Int,
        height: Int,
        fps: Int,
        passwordMode: Int,
        permanentHashPhc: String,
        requireAccept: Boolean,
        trustedPeersJson: String,
        callback: VeloxHostCallback,
    ): Long

    @JvmStatic
    external fun nativeHostStop(handle: Long)

    @JvmStatic
    external fun nativeHostPushVideoFrame(
        handle: Long,
        data: ByteArray,
        isKeyframe: Boolean,
        ptsUs: Long,
        width: Int,
        height: Int,
    )

    @JvmStatic
    external fun nativeHostPushAudioPcm(
        handle: Long,
        pcm16: ShortArray,
        sampleRate: Int,
        channels: Int,
    )

    @JvmStatic
    external fun nativeHostUpdateDisplay(handle: Long, width: Int, height: Int, fps: Int)

    @JvmStatic
    external fun nativeHostRotateOtp(handle: Long)

    @JvmStatic
    external fun nativeHostUpdateAuth(
        handle: Long,
        passwordMode: Int,
        permanentHashPhc: String,
        requireAccept: Boolean,
    )

    @JvmStatic
    external fun nativeHostRemoveTrustedPeer(handle: Long, pubKeyHex: String)

    @JvmStatic
    external fun nativeHostRespondApproval(
        handle: Long,
        requestId: Long,
        accepted: Boolean,
        rememberPeer: Boolean,
        allowInput: Boolean,
        allowClipboard: Boolean,
        allowAudio: Boolean,
    )

    @JvmStatic
    external fun nativeHostSendClipboardText(handle: Long, text: String)

    @JvmStatic
    external fun nativeHostSendChat(handle: Long, senderName: String, text: String): Long
}

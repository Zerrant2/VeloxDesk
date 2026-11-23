package dev.veloxdesk.host

import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjection
import android.os.Build
import android.util.DisplayMetrics
import android.util.Log
import android.view.WindowManager
import androidx.core.content.ContextCompat
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.ChatState
import dev.veloxdesk.bridge.HostApprovalRequest
import dev.veloxdesk.bridge.HostPasswordMode
import dev.veloxdesk.bridge.HostSavedTrustedPeer
import dev.veloxdesk.bridge.HostUiState
import dev.veloxdesk.bridge.VeloxHostCallback
import dev.veloxdesk.bridge.VeloxNative
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.input.ClipboardController
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/**
 * Configuration passed when starting Android Host screen sharing.
 */
data class HostStartConfig(
    val rendezvousAddr: String = "",
    val deviceName: String = Build.MODEL ?: "Android",
    val fps: Int = 30,
    val passwordMode: HostPasswordMode = HostPasswordMode.ONE_TIME,
    val permanentHashPhc: String = "",
    val requireAccept: Boolean = true,
    val trustedPeers: List<HostSavedTrustedPeer> = emptyList(),
)

internal data class PendingHostStart(
    val resultCode: Int,
    val projectionData: Intent,
    val config: HostStartConfig,
)

/**
 * Process-wide coordinator for Android Host Mode ("Share This Device").
 *
 * Bridges Compose UI (`@sys-worker`), [VeloxHostService], [ScreenCaptureEncoder],
 * [VeloxAccessibilityService], [ClipboardController], and the Rust JNI host engine.
 */
object HostController {
    private const val TAG = "HostController"

    private val lock = Any()

    private val _uiState = MutableStateFlow(HostUiState())
    val uiState: StateFlow<HostUiState> = _uiState.asStateFlow()

    private val _pendingApproval = MutableStateFlow<HostApprovalRequest?>(null)
    val pendingApproval: StateFlow<HostApprovalRequest?> = _pendingApproval.asStateFlow()

    /**
     * Hook set by the UI/Preferences layer so newly trusted peers ("Remember this device")
     * are immediately persisted to `AppPreferences`.
     */
    @Volatile
    var onTrustedPeerAdded: ((HostSavedTrustedPeer) -> Unit)? = null

    @Volatile
    private var pendingStart: PendingHostStart? = null

    @Volatile
    private var nativeHandle: Long = 0L

    @Volatile
    private var encoder: ScreenCaptureEncoder? = null

    @Volatile
    private var clipboard: ClipboardController? = null

    @Volatile
    private var appContextRef: Context? = null

    private val hostCallback = object : VeloxHostCallback {
        override fun onHostListening(deviceId: String, otp: String, port: Int) {
            val ips = NetworkIpDetector.detectLocalAndTailscaleIps()
            _uiState.update {
                it.copy(
                    isRunning = true,
                    isStarting = false,
                    deviceId = deviceId,
                    otp = otp,
                    port = port,
                    lanIps = ips.lanIps,
                    tailscaleIp = ips.tailscaleIp,
                    error = null,
                )
            }
        }

        override fun onHostOtpRotated(otp: String) {
            _uiState.update { it.copy(otp = otp) }
        }

        override fun onHostApprovalRequested(
            requestId: Long,
            peerDeviceId: String,
            peerName: String,
            peerFingerprint: String,
            peerPubKeyHex: String,
        ) {
            _pendingApproval.value = HostApprovalRequest(
                requestId = requestId,
                peerDeviceId = peerDeviceId,
                peerName = peerName,
                peerFingerprint = peerFingerprint,
                peerPubKeyHex = peerPubKeyHex,
            )
        }

        override fun onHostTrustedPeerAdded(peerJson: String) {
            val parsed = HostSavedTrustedPeer.fromJson(peerJson) ?: return
            // Persist directly at application level so newly trusted peers survive even when
            // MainActivity has been destroyed in the background.
            appContextRef?.let { ctx ->
                try {
                    AppPreferences(ctx).upsertHostTrustedPeer(parsed)
                } catch (t: Throwable) {
                    Log.w(TAG, "Failed persisting trusted peer to AppPreferences: ${t.message}")
                }
            }
            try {
                onTrustedPeerAdded?.invoke(parsed)
            } catch (t: Throwable) {
                Log.w(TAG, "onTrustedPeerAdded callback threw: ${t.message}")
            }
        }

        override fun onHostSessionStarted(
            peerDeviceId: String,
            peerName: String,
            requestedBitrateKbps: Int,
            requestedFps: Int,
        ) {
            _pendingApproval.value = null
            _uiState.update {
                it.copy(
                    activePeerDeviceId = peerDeviceId,
                    activePeerName = peerName.ifBlank { peerDeviceId },
                    chat = ChatState(),
                )
            }
            val enc = encoder
            if (enc != null) {
                if (requestedBitrateKbps > 0) {
                    enc.updateBitrate(requestedBitrateKbps)
                }
                enc.requestKeyframe()
            }
            VeloxAccessibilityService.resetState()
        }

        override fun onHostSessionEnded(peerDeviceId: String, reason: String) {
            _pendingApproval.value = null
            _uiState.update {
                it.copy(
                    activePeerDeviceId = null,
                    activePeerName = null,
                    chat = ChatState(),
                )
            }
            VeloxAccessibilityService.resetState()
        }

        override fun onHostBitrateChanged(bitrateKbps: Int) {
            encoder?.updateBitrate(bitrateKbps)
        }

        override fun onHostKeyframeRequested() {
            encoder?.requestKeyframe()
        }

        override fun onHostInputEvent(
            kind: Int,
            x: Int,
            y: Int,
            code: Int,
            down: Boolean,
            modifiers: Int,
            dx: Int,
            dy: Int,
            text: String,
        ) {
            val enc = encoder
            val srcW = enc?.encodedWidth ?: _uiState.value.captureWidth
            val srcH = enc?.encodedHeight ?: _uiState.value.captureHeight
            VeloxAccessibilityService.dispatchRemoteInput(
                kind = kind,
                x = x,
                y = y,
                code = code,
                down = down,
                modifiers = modifiers,
                dx = dx,
                dy = dy,
                text = text,
                srcWidth = srcW,
                srcHeight = srcH,
            )
        }

        override fun onHostClipboardText(text: String) {
            clipboard?.onRemoteClipboardText(text)
        }

        override fun onHostChatMessage(
            peerDeviceId: String,
            id: Long,
            senderName: String,
            text: String,
            tsMs: Long,
        ) {
            val resolvedSender = senderName.trim().ifEmpty {
                _uiState.value.activePeerName?.ifBlank { null } ?: peerDeviceId.ifBlank { "Viewer" }
            }
            _uiState.update { state ->
                state.copy(chat = state.chat.onIncomingReceived(id, resolvedSender, text, tsMs))
            }
        }

        override fun onHostChatAck(peerDeviceId: String, id: Long) {
            _uiState.update { state ->
                state.copy(chat = state.chat.onAckReceived(id))
            }
        }

        override fun onHostError(message: String) {
            Log.e(TAG, "Rust host error: $message")
            _uiState.update { it.copy(error = message, isStarting = false) }
        }

        override fun onHostStopped() {
            _pendingApproval.value = null
            _uiState.update {
                it.copy(
                    isRunning = false,
                    isStarting = false,
                    activePeerDeviceId = null,
                    activePeerName = null,
                    chat = ChatState(),
                )
            }
        }
    }

    /** Refresh local LAN/Tailscale IPs, accessibility status, and persistent Device ID. */
    fun refreshStatus(context: Context) {
        val appCtx = context.applicationContext
        appContextRef = appCtx
        val ips = NetworkIpDetector.detectLocalAndTailscaleIps()
        val a11y = VeloxAccessibilityService.isEnabled(appCtx)
        val deviceId = if (VeloxNative.isLoaded) {
            try {
                VeloxNative.nativeInitIdentity(appCtx.filesDir.absolutePath)
            } catch (_: Throwable) {
                _uiState.value.deviceId
            }
        } else {
            _uiState.value.deviceId
        }
        _uiState.update {
            it.copy(
                deviceId = deviceId,
                lanIps = ips.lanIps,
                tailscaleIp = ips.tailscaleIp,
                accessibilityEnabled = a11y,
            )
        }
    }

    internal fun onAccessibilityStateChanged(enabled: Boolean) {
        _uiState.update { it.copy(accessibilityEnabled = enabled) }
    }

    /**
     * Request starting Android Host screen sharing with the user-granted `MediaProjection`
     * permission intent. Launches [VeloxHostService] as a `mediaProjection` foreground service.
     */
    fun startSharing(
        context: Context,
        resultCode: Int,
        projectionData: Intent,
        config: HostStartConfig,
    ) {
        val appCtx = context.applicationContext
        appContextRef = appCtx
        if (!VeloxNative.isLoaded) {
            _uiState.update {
                it.copy(
                    isRunning = false,
                    isStarting = false,
                    error = "Native engine unavailable: ${VeloxNative.loadError ?: "unknown"}",
                )
            }
            return
        }
        pendingStart = PendingHostStart(resultCode, projectionData, config)
        _uiState.update { it.copy(isStarting = true, error = null) }
        val intent = Intent(appCtx, VeloxHostService::class.java).apply {
            action = VeloxHostService.ACTION_START_SHARING
        }
        ContextCompat.startForegroundService(appCtx, intent)
    }

    /** Stop Android Host screen sharing and tear down [VeloxHostService]. */
    fun stopSharing(context: Context) {
        val appCtx = context.applicationContext
        teardownEngine()
        try {
            appCtx.stopService(Intent(appCtx, VeloxHostService::class.java))
        } catch (_: Throwable) {
        }
    }

    fun rotateOtp() {
        val handle = nativeHandle
        if (handle != 0L && VeloxNative.isLoaded) {
            try {
                VeloxNative.nativeHostRotateOtp(handle)
            } catch (t: Throwable) {
                Log.w(TAG, "nativeHostRotateOtp failed: ${t.message}")
            }
        }
    }

    fun updateAuth(
        passwordMode: HostPasswordMode,
        permanentHashPhc: String,
        requireAccept: Boolean,
    ) {
        val (safeMode, safePhc) = sanitizeAuthParams(passwordMode, permanentHashPhc)
        val handle = nativeHandle
        if (handle != 0L && VeloxNative.isLoaded) {
            try {
                VeloxNative.nativeHostUpdateAuth(handle, safeMode.id, safePhc, requireAccept)
            } catch (t: Throwable) {
                Log.w(TAG, "nativeHostUpdateAuth failed: ${t.message}")
            }
        }
    }

    /**
     * Defense-in-depth auth invariant matching Desktop v0.3.0:
     * - `ONE_TIME` never passes a permanent hash to the host engine.
     * - `PERMANENT` without a non-blank PHC hash falls back safely to `ONE_TIME`.
     */
    internal fun sanitizeAuthParams(
        passwordMode: HostPasswordMode,
        permanentHashPhc: String,
    ): Pair<HostPasswordMode, String> {
        val trimmed = permanentHashPhc.trim()
        return when (passwordMode) {
            HostPasswordMode.ONE_TIME -> Pair(HostPasswordMode.ONE_TIME, "")
            HostPasswordMode.PERMANENT -> if (trimmed.isEmpty()) {
                Pair(HostPasswordMode.ONE_TIME, "")
            } else {
                Pair(HostPasswordMode.PERMANENT, trimmed)
            }
            HostPasswordMode.BOTH -> Pair(HostPasswordMode.BOTH, trimmed)
        }
    }

    fun removeTrustedPeer(pubKeyHex: String) {
        val handle = nativeHandle
        if (handle != 0L && VeloxNative.isLoaded) {
            try {
                VeloxNative.nativeHostRemoveTrustedPeer(handle, pubKeyHex)
            } catch (t: Throwable) {
                Log.w(TAG, "nativeHostRemoveTrustedPeer failed: ${t.message}")
            }
        }
    }

    fun respondApproval(
        requestId: Long,
        accepted: Boolean,
        rememberPeer: Boolean,
        allowInput: Boolean = true,
        allowClipboard: Boolean = true,
        allowAudio: Boolean = true,
    ) {
        if (_pendingApproval.value?.requestId == requestId) {
            _pendingApproval.value = null
        }
        val handle = nativeHandle
        if (handle != 0L && VeloxNative.isLoaded) {
            try {
                VeloxNative.nativeHostRespondApproval(
                    handle = handle,
                    requestId = requestId,
                    accepted = accepted,
                    rememberPeer = rememberPeer,
                    allowInput = allowInput,
                    allowClipboard = allowClipboard,
                    allowAudio = allowAudio,
                )
            } catch (t: Throwable) {
                Log.w(TAG, "nativeHostRespondApproval failed: ${t.message}")
            }
        }
    }

    fun clearError() {
        _uiState.update { it.copy(error = null) }
    }

    fun setChatOpen(open: Boolean) {
        _uiState.update { it.copy(chat = it.chat.setOpen(open)) }
    }

    fun sendChatMessage(rawText: String) {
        val text = rawText.trim()
        if (text.isEmpty()) return
        val handle = nativeHandle
        if (handle == 0L || !VeloxNative.isLoaded) return
        val sender = (Build.MODEL ?: "Android").trim().ifEmpty { "Android" }
        val id = try {
            VeloxNative.nativeHostSendChat(handle, sender, text)
        } catch (t: Throwable) {
            Log.w(TAG, "nativeHostSendChat failed: ${t.message}")
            0L
        }
        if (id > 0L) {
            val nowMs = System.currentTimeMillis()
            _uiState.update { state ->
                state.copy(chat = state.chat.onOutgoingSent(id, sender, text, nowMs))
            }
        }
    }

    internal fun consumePendingStart(): PendingHostStart? {
        val p = pendingStart
        pendingStart = null
        return p
    }

    /**
     * Called by [VeloxHostService] strictly AFTER `startForeground(..., MEDIA_PROJECTION)` so
     * Android 14/15 allows obtaining and starting the [MediaProjection].
     */
    internal fun startEngineWithProjection(
        service: VeloxHostService,
        projection: MediaProjection,
        config: HostStartConfig,
    ): Boolean {
        synchronized(lock) {
            appContextRef = service.applicationContext
            teardownEngineLocked()
            val (rawW, rawH, dpi) = currentDisplayMetrics(service)
            val fps = config.fps.coerceIn(15, 60)
            val (encW, encH) = ScreenCaptureEncoder.computeScaledDimensions(rawW, rawH)

            val (safeMode, safePhc) = sanitizeAuthParams(config.passwordMode, config.permanentHashPhc)
            val trustedJson = HostSavedTrustedPeer.listToJson(config.trustedPeers)
            val handle = try {
                VeloxNative.nativeHostStart(
                    filesDir = service.filesDir.absolutePath,
                    rendezvousAddr = config.rendezvousAddr.trim(),
                    deviceName = config.deviceName.ifBlank { Build.MODEL ?: "Android" },
                    width = encW,
                    height = encH,
                    fps = fps,
                    passwordMode = safeMode.id,
                    permanentHashPhc = safePhc,
                    requireAccept = config.requireAccept,
                    trustedPeersJson = trustedJson,
                    callback = hostCallback,
                )
            } catch (t: Throwable) {
                Log.e(TAG, "nativeHostStart failed: ${t.message}", t)
                _uiState.update {
                    it.copy(
                        isRunning = false,
                        isStarting = false,
                        error = "Host start failed: ${t.message ?: t.javaClass.simpleName}",
                    )
                }
                try {
                    projection.stop()
                } catch (_: Throwable) {
                }
                return false
            }

            if (handle == 0L) {
                _uiState.update {
                    it.copy(
                        isRunning = false,
                        isStarting = false,
                        error = "Host engine failed to start",
                    )
                }
                try {
                    projection.stop()
                } catch (_: Throwable) {
                }
                return false
            }
            nativeHandle = handle

            val enc = ScreenCaptureEncoder(
                mediaProjection = projection,
                onEncodedFrame = { data, isKeyframe, ptsUs, w, h ->
                    val hnd = nativeHandle
                    if (hnd != 0L) {
                        VeloxNative.nativeHostPushVideoFrame(hnd, data, isKeyframe, ptsUs, w, h)
                    }
                },
                onProjectionRevoked = {
                    stopSharing(service)
                },
                onError = { msg ->
                    _uiState.update { it.copy(error = msg) }
                    stopSharing(service)
                },
            )
            try {
                val (actualW, actualH) = enc.start(rawW, rawH, dpi, fps)
                encoder = enc
                val clip = ClipboardController(service) { localText ->
                    val hnd = nativeHandle
                    if (hnd != 0L) {
                        VeloxNative.nativeHostSendClipboardText(hnd, localText)
                    }
                }.also { it.start() }
                clipboard = clip

                val ips = NetworkIpDetector.detectLocalAndTailscaleIps()
                _uiState.update {
                    it.copy(
                        isRunning = true,
                        isStarting = false,
                        captureWidth = actualW,
                        captureHeight = actualH,
                        captureFps = fps,
                        lanIps = ips.lanIps,
                        tailscaleIp = ips.tailscaleIp,
                        accessibilityEnabled = VeloxAccessibilityService.isEnabled(service),
                        error = null,
                    )
                }
                return true
            } catch (t: Throwable) {
                Log.e(TAG, "ScreenCaptureEncoder start failed: ${t.message}", t)
                teardownEngineLocked()
                _uiState.update {
                    it.copy(
                        isRunning = false,
                        isStarting = false,
                        error = "Screen capture failed: ${t.message ?: t.javaClass.simpleName}",
                    )
                }
                return false
            }
        }
    }

    internal fun onDisplayMetricsChanged(context: Context) {
        val enc = encoder ?: return
        val (rawW, rawH, dpi) = currentDisplayMetrics(context)
        val updated = try {
            enc.reconfigureForRotation(rawW, rawH, dpi)
        } catch (t: Throwable) {
            Log.w(TAG, "Rotation reconfigure failed: ${t.message}")
            null
        } ?: return

        val (newW, newH) = updated
        val fps = enc.targetFps
        _uiState.update { it.copy(captureWidth = newW, captureHeight = newH) }
        val handle = nativeHandle
        if (handle != 0L && VeloxNative.isLoaded) {
            try {
                VeloxNative.nativeHostUpdateDisplay(handle, newW, newH, fps)
            } catch (t: Throwable) {
                Log.w(TAG, "nativeHostUpdateDisplay failed: ${t.message}")
            }
        }
    }

    internal fun failStart(message: String) {
        teardownEngine()
        _uiState.update { it.copy(error = message, isRunning = false, isStarting = false) }
    }

    internal fun teardownEngine() {
        synchronized(lock) {
            teardownEngineLocked()
        }
    }

    private fun teardownEngineLocked() {
        val enc = encoder
        encoder = null
        val clip = clipboard
        clipboard = null
        val handle = nativeHandle
        nativeHandle = 0L

        try {
            clip?.stop()
        } catch (_: Throwable) {
        }
        try {
            enc?.stop()
        } catch (_: Throwable) {
        }
        if (handle != 0L && VeloxNative.isLoaded) {
            try {
                VeloxNative.nativeHostStop(handle)
            } catch (t: Throwable) {
                Log.w(TAG, "nativeHostStop failed: ${t.message}")
            }
        }
        _pendingApproval.value = null
        _uiState.update {
            it.copy(
                isRunning = false,
                isStarting = false,
                activePeerDeviceId = null,
                activePeerName = null,
                chat = ChatState(),
            )
        }
        VeloxAccessibilityService.resetState()
    }

    private fun currentDisplayMetrics(context: Context): Triple<Int, Int, Int> {
        val wm = context.getSystemService(Context.WINDOW_SERVICE) as? WindowManager
            ?: return Triple(1080, 1920, 320)
        val dpi = context.resources.displayMetrics.densityDpi.coerceAtLeast(120)
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val b = wm.currentWindowMetrics.bounds
            Triple(b.width().coerceAtLeast(256), b.height().coerceAtLeast(256), dpi)
        } else {
            val dm = DisplayMetrics()
            @Suppress("DEPRECATION")
            wm.defaultDisplay?.getRealMetrics(dm)
            Triple(dm.widthPixels.coerceAtLeast(256), dm.heightPixels.coerceAtLeast(256), dm.densityDpi.coerceAtLeast(120))
        }
    }
}

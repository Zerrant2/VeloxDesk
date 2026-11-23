package dev.veloxdesk.ui

import android.app.Application
import android.graphics.Bitmap
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.Surface
import androidx.compose.ui.graphics.asImageBitmap
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.veloxdesk.BuildConfig
import dev.veloxdesk.bridge.AuthPromptData
import dev.veloxdesk.bridge.BridgeConstants
import dev.veloxdesk.bridge.ChatState
import dev.veloxdesk.bridge.DisplayDescriptor
import dev.veloxdesk.bridge.HudStats
import dev.veloxdesk.bridge.ScaleMode
import dev.veloxdesk.bridge.SessionInfo
import dev.veloxdesk.bridge.TouchMode
import dev.veloxdesk.bridge.VeloxNative
import dev.veloxdesk.bridge.VeloxSessionCallback
import dev.veloxdesk.data.AppPreferences
import dev.veloxdesk.data.BitratePreset
import dev.veloxdesk.data.StreamProfile
import dev.veloxdesk.input.ClipboardController
import dev.veloxdesk.input.GamepadController
import dev.veloxdesk.input.GamepadSink
import dev.veloxdesk.input.InputSink
import dev.veloxdesk.input.KeyMapper
import dev.veloxdesk.input.SpecialKeyDef
import dev.veloxdesk.input.TouchInputController
import dev.veloxdesk.input.ViewportTransform
import dev.veloxdesk.media.AudioPlayerEngine
import dev.veloxdesk.media.CodecCapsProber
import dev.veloxdesk.media.VideoDecoderEngine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import org.json.JSONArray
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong

data class DeviceIdentityUiState(
    val ownDeviceId: String = "",
    val ownFingerprint: String = "",
    val buildDisplay: String = "v${BuildConfig.VERSION_NAME} (${BuildConfig.GIT_SHA})",
)

data class SessionUiState(
    val isActive: Boolean = false,
    val target: String = "",
    val mode: Int = BridgeConstants.MODE_DESKTOP,
    val statusCode: Int = BridgeConstants.STATUS_CONNECTING,
    val statusDetail: String = "",
    val authPrompt: AuthPromptData? = null,
    val sessionInfo: SessionInfo? = null,
    val displays: List<DisplayDescriptor> = emptyList(),
    val activeDisplay: Int = 0,
    val videoWidth: Int = 0,
    val videoHeight: Int = 0,
    val hasFirstVideoFrame: Boolean = false,
    val decoderName: String = "",
    val decoderHardware: Boolean = true,
    val hudStats: HudStats = HudStats(),
    val showHud: Boolean = false,
    val touchMode: TouchMode = TouchMode.TRACKPAD,
    val scaleMode: ScaleMode = ScaleMode.FIT,
    val viewport: ViewportTransform = ViewportTransform(),
    val audioEnabled: Boolean = true,
    val clipboardEnabled: Boolean = true,
    val bitrateKbps: Int = BitratePreset.BALANCED.kbps,
    val stickyModifiers: Int = 0,
    val cursor: CursorOverlayState = CursorOverlayState(),
    val chat: ChatState = ChatState(),
    val lastEndedReason: String? = null,
    val lastEndedWasError: Boolean = false,
)

/**
 * Coordinates the active VeloxDesk remote session: Rust JNI handle, MediaCodec zero-copy
 * VideoDecoderEngine, low-latency AudioPlayerEngine, touch/keyboard/gamepad/clipboard engines.
 */
class SessionViewModel(application: Application) : AndroidViewModel(application) {
    private val appContext = application.applicationContext
    private val mainHandler = Handler(Looper.getMainLooper())
    private val filesDirPath: String = appContext.filesDir.absolutePath

    private val activeHandle = AtomicLong(0L)
    private val sessionGen = AtomicLong(0L)
    private val firstFrameSeen = AtomicBoolean(false)
    private val initialModeUpdateSent = AtomicBoolean(false)

    @Volatile
    private var activeProfile: StreamProfile = StreamProfile.defaultDesktop()

    private val cursorCacheLock = Any()
    private val cursorShapeCache = object : LinkedHashMap<Long, CursorShapeBitmap>(
        MAX_CURSOR_CACHE_ENTRIES,
        0.75f,
        true,
    ) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<Long, CursorShapeBitmap>?): Boolean =
            size > MAX_CURSOR_CACHE_ENTRIES
    }

    private val _uiState = MutableStateFlow(SessionUiState())
    val uiState: StateFlow<SessionUiState> = _uiState.asStateFlow()

    private val _deviceIdentity = MutableStateFlow(DeviceIdentityUiState())
    val deviceIdentity: StateFlow<DeviceIdentityUiState> = _deviceIdentity.asStateFlow()

    val nativeLoaded: Boolean = VeloxNative.isLoaded
    val nativeLoadError: String? = VeloxNative.loadError

    private val videoDecoder = VideoDecoderEngine(
        onRequestKeyframe = {
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded) {
                VeloxNative.nativeRequestKeyframe(h)
            }
        },
        onDecodeTimingUs = { decodeUs, renderUs ->
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded) {
                VeloxNative.nativeReportDecodeTimeUs(h, decodeUs, renderUs)
            }
        },
        onDecoderInfo = { name, hardware ->
            _uiState.update { it.copy(decoderName = name, decoderHardware = hardware) }
        },
    )

    private val audioPlayer = AudioPlayerEngine()

    private val inputSink = object : InputSink {
        override fun sendMouseMove(x: Int, y: Int, display: Int) {
            updateLocalCursorPosition(x.toFloat(), y.toFloat())
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded) {
                VeloxNative.nativeSendMouseMove(h, x, y, display)
            }
        }

        override fun sendMouseRelative(dx: Int, dy: Int) {
            applyLocalCursorDelta(dx, dy)
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded) {
                VeloxNative.nativeSendMouseRelative(h, dx, dy)
            }
        }

        override fun sendMouseButton(button: Int, down: Boolean) {
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded) {
                VeloxNative.nativeSendMouseButton(h, button, down)
            }
        }

        override fun sendMouseWheel(dx: Int, dy: Int) {
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded) {
                VeloxNative.nativeSendMouseWheel(h, dx, dy)
            }
        }
    }

    val touchController = TouchInputController(
        sink = inputSink,
        onViewportChanged = { transform ->
            _uiState.update { it.copy(viewport = transform) }
        },
    )

    private val gamepadSink = object : GamepadSink {
        override fun sendGamepad(
            padId: Int,
            connected: Boolean,
            buttons: Int,
            leftTrigger: Int,
            rightTrigger: Int,
            leftX: Int,
            leftY: Int,
            rightX: Int,
            rightY: Int,
        ) {
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded) {
                VeloxNative.nativeSendGamepad(
                    handle = h,
                    padId = padId,
                    connected = connected,
                    buttons = buttons,
                    leftTrigger = leftTrigger,
                    rightTrigger = rightTrigger,
                    leftX = leftX,
                    leftY = leftY,
                    rightX = rightX,
                    rightY = rightY,
                )
            }
        }
    }

    private val gamepadController = GamepadController(gamepadSink)

    private val clipboardController = ClipboardController(
        context = appContext,
        onLocalTextChanged = { text ->
            val h = activeHandle.get()
            if (h != 0L && VeloxNative.isLoaded && _uiState.value.clipboardEnabled) {
                VeloxNative.nativeSendClipboardText(h, text)
            }
        },
    )

    private var onSessionConnectedRecord: ((target: String, hostName: String, mode: Int) -> Unit)? = null

    private val localDeviceName: String = buildString {
        val manu = Build.MANUFACTURER.orEmpty().trim()
        val model = Build.MODEL.orEmpty().trim()
        if (model.startsWith(manu, ignoreCase = true)) {
            append(model)
        } else {
            append(manu).append(' ').append(model)
        }
    }.trim().ifEmpty { "Android" }

    init {
        if (VeloxNative.isLoaded) {
            viewModelScope.launch(Dispatchers.IO) {
                runCatching { VeloxNative.nativeInitLogging() }
                val deviceId = runCatching { VeloxNative.nativeInitIdentity(filesDirPath) }.getOrDefault("")
                val fingerprint = runCatching { VeloxNative.nativeGetFingerprint(filesDirPath) }.getOrDefault("")
                val nativeBuild = runCatching { VeloxNative.nativeGetBuildDisplay() }.getOrDefault("")
                val display = nativeBuild.ifBlank { "v${BuildConfig.VERSION_NAME} (${BuildConfig.GIT_SHA})" }
                _deviceIdentity.value = DeviceIdentityUiState(
                    ownDeviceId = deviceId,
                    ownFingerprint = fingerprint,
                    buildDisplay = display,
                )
            }
        }
    }

    fun connect(
        target: String,
        mode: Int,
        rendezvousServer: String,
        relayOnly: Boolean,
        profile: StreamProfile,
        onConnectedHost: (target: String, hostName: String, mode: Int) -> Unit,
    ) {
        if (!VeloxNative.isLoaded) {
            _uiState.update {
                it.copy(
                    isActive = false,
                    lastEndedReason = VeloxNative.loadError ?: "Native library not loaded",
                    lastEndedWasError = true,
                )
            }
            return
        }

        // Tear down any lingering previous session cleanly before starting a new one.
        disconnectInternal(recordCloseReason = false)

        val normalizedTarget = AppPreferences.normalizeTarget(target)
        val gen = sessionGen.incrementAndGet()
        firstFrameSeen.set(false)
        initialModeUpdateSent.set(false)
        activeProfile = profile
        synchronized(cursorCacheLock) { cursorShapeCache.clear() }
        onSessionConnectedRecord = onConnectedHost

        touchController.touchMode = profile.touchMode
        touchController.scaleMode = profile.scaleMode
        touchController.activeDisplay = 0
        touchController.resetZoom()
        audioPlayer.setMuted(!profile.audioEnabled)
        clipboardController.setEnabled(profile.clipboardEnabled)
        if (profile.clipboardEnabled) {
            clipboardController.start()
        }

        _uiState.value = SessionUiState(
            isActive = true,
            target = normalizedTarget,
            mode = mode,
            statusCode = BridgeConstants.STATUS_CONNECTING,
            statusDetail = "",
            showHud = profile.showHudOnConnect,
            touchMode = profile.touchMode,
            scaleMode = profile.scaleMode,
            viewport = ViewportTransform(),
            audioEnabled = profile.audioEnabled,
            clipboardEnabled = profile.clipboardEnabled,
            bitrateKbps = profile.bitrateKbps,
            stickyModifiers = 0,
        )

        val metrics = appContext.resources.displayMetrics
        val viewerWidth = maxOf(metrics.widthPixels, metrics.heightPixels).coerceAtLeast(1280)
        val viewerHeight = minOf(metrics.widthPixels, metrics.heightPixels).coerceAtLeast(720)
        val deviceName = localDeviceName

        val codecCaps = CodecCapsProber.probeCodecCapsMask()
        val callback = createSessionCallback(gen, normalizedTarget, mode)

        val handle = try {
            VeloxNative.nativeConnect(
                filesDir = filesDirPath,
                target = normalizedTarget,
                rendezvousAddr = rendezvousServer.trim(),
                relayOnly = relayOnly,
                password = null,
                mode = mode,
                maxBitrateKbps = profile.bitrateKbps,
                audioEnabled = profile.audioEnabled,
                clipboardEnabled = profile.clipboardEnabled,
                viewerWidth = viewerWidth,
                viewerHeight = viewerHeight,
                viewerRefreshHz = profile.fpsCap,
                deviceName = deviceName,
                codecCapsMask = codecCaps,
                callback = callback,
            )
        } catch (t: Throwable) {
            _uiState.update {
                it.copy(
                    isActive = false,
                    lastEndedReason = t.message ?: t.javaClass.simpleName,
                    lastEndedWasError = true,
                )
            }
            return
        }

        if (handle == 0L) {
            clipboardController.stop()
            _uiState.update {
                it.copy(
                    isActive = false,
                    lastEndedReason = "Failed to initialize native session",
                    lastEndedWasError = true,
                )
            }
            return
        }
        activeHandle.set(handle)
        runCatching {
            VeloxNative.nativeSetAudioProcessing(
                handle = handle,
                echoCancellation = profile.echoCancellation,
                noiseSuppression = profile.noiseSuppression,
            )
        }
    }

    private fun createSessionCallback(
        gen: Long,
        target: String,
        requestedMode: Int,
    ): VeloxSessionCallback = object : VeloxSessionCallback {
        override fun onStatus(statusCode: Int, detail: String) {
            if (sessionGen.get() != gen) return
            _uiState.update { state ->
                state.copy(
                    statusCode = statusCode,
                    statusDetail = detail,
                    authPrompt = if (statusCode == BridgeConstants.STATUS_CONNECTED) null else state.authPrompt,
                )
            }
        }

        override fun onAuthRequired(
            hostDeviceId: String,
            hostFingerprint: String,
            allowPerm: Boolean,
            allowOtp: Boolean,
        ) {
            if (sessionGen.get() != gen) return
            _uiState.update {
                it.copy(
                    statusCode = BridgeConstants.STATUS_AUTHENTICATING,
                    authPrompt = AuthPromptData(
                        hostDeviceId = hostDeviceId,
                        hostFingerprint = hostFingerprint,
                        allowPerm = allowPerm,
                        allowOtp = allowOtp,
                    ),
                )
            }
        }

        override fun onSessionConfig(
            hostName: String,
            hostOs: String,
            mode: Int,
            activeDisplay: Int,
            videoCodec: Int,
            width: Int,
            height: Int,
            fps: Int,
            encoderName: String,
            hwEncoder: Boolean,
            audioSampleRate: Int,
            audioChannels: Int,
            audioFrameUs: Int,
            displaysJson: String,
        ) {
            if (sessionGen.get() != gen) return
            val displays = parseDisplaysJson(displaysJson)
            if (audioSampleRate > 0 && audioChannels > 0) {
                audioPlayer.configure(audioSampleRate, audioChannels)
            }
            if (width > 0 && height > 0) {
                touchController.setHostResolution(width, height)
            }
            touchController.activeDisplay = activeDisplay

            val info = SessionInfo(
                hostName = hostName,
                hostOs = hostOs,
                mode = mode,
                activeDisplay = activeDisplay,
                videoCodec = videoCodec,
                width = width,
                height = height,
                fps = fps,
                encoderName = encoderName,
                hwEncoder = hwEncoder,
                audioSampleRate = audioSampleRate,
                audioChannels = audioChannels,
                audioFrameUs = audioFrameUs,
                displays = displays,
            )

            _uiState.update { state ->
                val initialCursor = if (!state.cursor.hasPosition && width > 0 && height > 0) {
                    state.cursor.copy(
                        hostX = width * 0.5f,
                        hostY = height * 0.5f,
                        hasPosition = true,
                        visible = state.touchMode == TouchMode.TRACKPAD,
                    )
                } else {
                    state.cursor
                }
                state.copy(
                    statusCode = BridgeConstants.STATUS_CONNECTED,
                    authPrompt = null,
                    sessionInfo = info,
                    displays = displays,
                    activeDisplay = activeDisplay,
                    videoWidth = width,
                    videoHeight = height,
                    cursor = initialCursor,
                )
            }
            mainHandler.post {
                onSessionConnectedRecord?.invoke(target, hostName, requestedMode)
            }
            if (initialModeUpdateSent.compareAndSet(false, true)) {
                val h = activeHandle.get()
                val prof = activeProfile
                if (h != 0L && VeloxNative.isLoaded) {
                    runCatching {
                        VeloxNative.nativeUpdateSessionMode(
                            handle = h,
                            mode = requestedMode,
                            maxBitrateKbps = prof.bitrateKbps,
                            maxFps = prof.fpsCap,
                            audioFrameUs = prof.protocolAudioFrameUs(),
                            preferYuv444 = prof.preferYuv444,
                        )
                    }
                }
            }
        }

        override fun onDisplayList(displaysJson: String) {
            if (sessionGen.get() != gen) return
            val displays = parseDisplaysJson(displaysJson)
            _uiState.update { it.copy(displays = displays) }
        }

        override fun onVideoFrame(
            codec: Int,
            width: Int,
            height: Int,
            frameId: Int,
            captureTsUs: Long,
            keyframe: Boolean,
            discontinuity: Boolean,
            data: ByteArray,
        ) {
            if (sessionGen.get() != gen) return
            // Stay on Rust video background thread — zero Main-thread hop!
            videoDecoder.onFrame(
                codec = codec,
                width = width,
                height = height,
                frameId = frameId,
                captureTsUs = captureTsUs,
                keyframe = keyframe,
                discontinuity = discontinuity,
                data = data,
            )
            if (firstFrameSeen.compareAndSet(false, true)) {
                if (width > 0 && height > 0) {
                    touchController.setHostResolution(width, height)
                }
                _uiState.update {
                    it.copy(
                        hasFirstVideoFrame = true,
                        videoWidth = if (width > 0) width else it.videoWidth,
                        videoHeight = if (height > 0) height else it.videoHeight,
                    )
                }
            } else {
                val cur = _uiState.value
                if (width > 0 && height > 0 && (width != cur.videoWidth || height != cur.videoHeight)) {
                    touchController.setHostResolution(width, height)
                    _uiState.update { it.copy(videoWidth = width, videoHeight = height) }
                }
            }
        }

        override fun onAudioPcm(sampleRate: Int, channels: Int, pcm16: ShortArray) {
            if (sessionGen.get() != gen) return
            // Stay on Rust audio background thread — never bounce through Main thread!
            audioPlayer.writePcm(sampleRate, channels, pcm16)
        }

        override fun onStats(
            fps: Float,
            bitrateKbps: Int,
            rttMs: Float,
            lossPct: Float,
            fecPct: Float,
            captureMs: Float,
            encodeMs: Float,
            netMs: Float,
            decodeMs: Float,
            totalMs: Float,
            pathKind: Int,
        ) {
            if (sessionGen.get() != gen) return
            _uiState.update {
                it.copy(
                    hudStats = HudStats(
                        fps = fps,
                        bitrateKbps = bitrateKbps,
                        rttMs = rttMs,
                        lossPct = lossPct,
                        fecPct = fecPct,
                        captureMs = captureMs,
                        encodeMs = encodeMs,
                        netMs = netMs,
                        decodeMs = decodeMs,
                        totalMs = totalMs,
                        pathKind = pathKind,
                    ),
                )
            }
        }

        override fun onClipboardText(text: String) {
            if (sessionGen.get() != gen) return
            if (!_uiState.value.clipboardEnabled) return
            mainHandler.post {
                if (sessionGen.get() == gen) {
                    clipboardController.onRemoteClipboardText(text)
                }
            }
        }

        override fun onRumble(padId: Int, lowFreq: Int, highFreq: Int) {
            if (sessionGen.get() != gen) return
            gamepadController.applyRumble(appContext, padId, lowFreq, highFreq)
        }

        override fun onCursorShape(
            shapeId: Long,
            width: Int,
            height: Int,
            hotspotX: Int,
            hotspotY: Int,
            rgba: ByteArray,
        ) {
            if (sessionGen.get() != gen) return
            if (width !in 1..MAX_CURSOR_DIM || height !in 1..MAX_CURSOR_DIM) return
            if (rgba.size != width * height * 4) return

            val argb = rgbaBytesToArgbInts(rgba)
            val bitmap = runCatching {
                Bitmap.createBitmap(argb, width, height, Bitmap.Config.ARGB_8888).asImageBitmap()
            }.getOrNull()

            val shape = CursorShapeBitmap(
                shapeId = shapeId,
                width = width,
                height = height,
                hotspotX = hotspotX.coerceIn(0, width - 1),
                hotspotY = hotspotY.coerceIn(0, height - 1),
                imageBitmap = bitmap,
            )
            synchronized(cursorCacheLock) {
                cursorShapeCache[shapeId] = shape
            }

            _uiState.update { state ->
                val cur = state.cursor
                if (cur.shapeId == shapeId || (cur.shape == null && shapeId != 0L)) {
                    state.copy(cursor = cur.copy(shapeId = shapeId, shape = shape))
                } else {
                    state
                }
            }
        }

        override fun onCursorPosition(x: Int, y: Int, visible: Boolean, shapeId: Long) {
            if (sessionGen.get() != gen) return
            val cachedShape = if (shapeId != 0L) {
                synchronized(cursorCacheLock) { cursorShapeCache[shapeId] }
            } else {
                null
            }
            _uiState.update { state ->
                val cur = state.cursor
                val resolvedShapeId = if (shapeId != 0L) shapeId else cur.shapeId
                val resolvedShape = when {
                    shapeId != 0L -> cachedShape ?: if (cur.shape?.shapeId == shapeId) cur.shape else null
                    else -> cur.shape
                }
                state.copy(
                    cursor = cur.copy(
                        hostX = x.toFloat(),
                        hostY = y.toFloat(),
                        visible = visible,
                        hostReportedVisibility = true,
                        hasPosition = true,
                        shapeId = resolvedShapeId,
                        shape = resolvedShape,
                    ),
                )
            }
        }

        override fun onChatMessage(id: Long, senderName: String, text: String, tsMs: Long) {
            if (sessionGen.get() != gen) return
            val resolvedSender = senderName.trim().ifEmpty {
                _uiState.value.sessionInfo?.hostName?.ifBlank { null } ?: "Host"
            }
            _uiState.update { state ->
                state.copy(chat = state.chat.onIncomingReceived(id, resolvedSender, text, tsMs))
            }
        }

        override fun onChatAck(id: Long) {
            if (sessionGen.get() != gen) return
            _uiState.update { state ->
                state.copy(chat = state.chat.onAckReceived(id))
            }
        }

        override fun onClosed(reason: String, isError: Boolean) {
            if (sessionGen.get() != gen) return
            activeHandle.set(0L)
            mainHandler.post {
                if (sessionGen.get() != gen) return@post
                cleanupLocalEngines()
                _uiState.update {
                    it.copy(
                        isActive = false,
                        authPrompt = null,
                        stickyModifiers = 0,
                        lastEndedReason = reason.ifBlank { null },
                        lastEndedWasError = isError,
                    )
                }
            }
        }
    }

    fun submitAuthPassword(password: String) {
        val h = activeHandle.get()
        if (h != 0L && VeloxNative.isLoaded) {
            _uiState.update { it.copy(authPrompt = null, statusCode = BridgeConstants.STATUS_AUTHENTICATING) }
            VeloxNative.nativeSubmitPassword(h, password)
        }
    }

    fun cancelAuth() {
        val h = activeHandle.get()
        _uiState.update { it.copy(authPrompt = null) }
        if (h != 0L && VeloxNative.isLoaded) {
            VeloxNative.nativeCancelAuth(h)
        }
        disconnect()
    }

    fun disconnect() {
        disconnectInternal(recordCloseReason = true)
    }

    private fun disconnectInternal(recordCloseReason: Boolean) {
        sessionGen.incrementAndGet()
        releaseStickyModifiers()
        gamepadController.disconnectAll()
        val h = activeHandle.getAndSet(0L)
        if (h != 0L && VeloxNative.isLoaded) {
            runCatching { VeloxNative.nativeDisconnect(h) }
        }
        cleanupLocalEngines()
        _uiState.update {
            it.copy(
                isActive = false,
                authPrompt = null,
                stickyModifiers = 0,
                lastEndedReason = if (recordCloseReason) null else it.lastEndedReason,
                lastEndedWasError = false,
            )
        }
    }

    private fun cleanupLocalEngines() {
        clipboardController.stop()
        videoDecoder.resetSession()
        videoDecoder.detachSurface()
        audioPlayer.stop()
        firstFrameSeen.set(false)
        synchronized(cursorCacheLock) { cursorShapeCache.clear() }
    }

    fun onAppBackgrounded() {
        if (!_uiState.value.isActive) return
        releaseStickyModifiers()
        gamepadController.disconnectAll()
    }

    fun clearEndedBanner() {
        _uiState.update { it.copy(lastEndedReason = null, lastEndedWasError = false) }
    }

    // ── Surface & Viewport ───────────────────────────────────────────────────

    fun attachSurface(surface: Surface) {
        videoDecoder.attachSurface(surface)
    }

    fun detachSurface() {
        videoDecoder.detachSurface()
    }

    fun onVideoViewSizeChanged(width: Int, height: Int) {
        if (width > 0 && height > 0) {
            touchController.setViewSize(width, height)
        }
    }

    fun onTouchEvent(event: MotionEvent): Boolean = touchController.onTouchEvent(event)

    /**
     * Immediately updates the local cursor overlay coordinates in host pixels for 0 ms visual
     * feedback during trackpad drags or direct touches (authoritatively synced by `onCursorPosition`).
     */
    fun updateLocalCursorPosition(hostX: Float, hostY: Float) {
        if (!hostX.isFinite() || !hostY.isFinite()) return
        _uiState.update { state ->
            val maxX = (state.videoWidth - 1).coerceAtLeast(0).toFloat()
            val maxY = (state.videoHeight - 1).coerceAtLeast(0).toFloat()
            val clampedX = if (maxX > 0f) hostX.coerceIn(0f, maxX) else hostX.coerceAtLeast(0f)
            val clampedY = if (maxY > 0f) hostY.coerceIn(0f, maxY) else hostY.coerceAtLeast(0f)
            val isAndroidHost = state.sessionInfo?.hostOs.equals("android", ignoreCase = true)
            val showCursor = if (state.cursor.hostReportedVisibility) {
                state.cursor.visible
            } else {
                state.touchMode == TouchMode.TRACKPAD || state.cursor.visible || !isAndroidHost
            }
            state.copy(
                cursor = state.cursor.copy(
                    hostX = clampedX,
                    hostY = clampedY,
                    visible = showCursor,
                    hasPosition = true,
                ),
            )
        }
    }

    private fun applyLocalCursorDelta(dx: Int, dy: Int) {
        if (dx == 0 && dy == 0) return
        _uiState.update { state ->
            val w = state.videoWidth.coerceAtLeast(1)
            val h = state.videoHeight.coerceAtLeast(1)
            val baseX = if (state.cursor.hasPosition) state.cursor.hostX else w * 0.5f
            val baseY = if (state.cursor.hasPosition) state.cursor.hostY else h * 0.5f
            val nextX = (baseX + dx).coerceIn(0f, (w - 1).toFloat())
            val nextY = (baseY + dy).coerceIn(0f, (h - 1).toFloat())
            val showCursor = if (state.cursor.hostReportedVisibility) state.cursor.visible else true
            state.copy(
                cursor = state.cursor.copy(
                    hostX = nextX,
                    hostY = nextY,
                    visible = showCursor,
                    hasPosition = true,
                ),
            )
        }
    }

    fun resetZoom() {
        touchController.resetZoom()
    }

    // ── Live Session Controls ────────────────────────────────────────────────

    fun toggleTouchMode() {
        val next = if (_uiState.value.touchMode == TouchMode.TRACKPAD) {
            TouchMode.DIRECT
        } else {
            TouchMode.TRACKPAD
        }
        touchController.touchMode = next
        _uiState.update { it.copy(touchMode = next) }
    }

    fun cycleScaleMode() {
        val next = when (_uiState.value.scaleMode) {
            ScaleMode.FIT -> ScaleMode.STRETCH
            ScaleMode.STRETCH -> ScaleMode.ORIGINAL
            ScaleMode.ORIGINAL -> ScaleMode.FIT
        }
        touchController.scaleMode = next
        touchController.resetZoom()
        _uiState.update { it.copy(scaleMode = next, viewport = ViewportTransform()) }
    }

    fun toggleAudio() {
        val next = !_uiState.value.audioEnabled
        audioPlayer.setMuted(!next)
        val h = activeHandle.get()
        if (h != 0L && VeloxNative.isLoaded) {
            VeloxNative.nativeSetAudioEnabled(h, next)
        }
        _uiState.update { it.copy(audioEnabled = next) }
    }

    fun toggleClipboard() {
        val next = !_uiState.value.clipboardEnabled
        clipboardController.setEnabled(next)
        if (next) clipboardController.start() else clipboardController.stop()
        val h = activeHandle.get()
        if (h != 0L && VeloxNative.isLoaded) {
            VeloxNative.nativeSetClipboardEnabled(h, next)
        }
        _uiState.update { it.copy(clipboardEnabled = next) }
    }

    fun toggleHud() {
        _uiState.update { it.copy(showHud = !it.showHud) }
    }

    fun setChatOpen(open: Boolean) {
        _uiState.update { it.copy(chat = it.chat.setOpen(open)) }
    }

    fun toggleChat() {
        _uiState.update { it.copy(chat = it.chat.setOpen(!it.chat.isOpen)) }
    }

    fun sendChatMessage(rawText: String) {
        val text = rawText.trim()
        if (text.isEmpty()) return
        val h = activeHandle.get()
        if (h == 0L || !VeloxNative.isLoaded) return
        val sender = localDeviceName
        val id = runCatching { VeloxNative.nativeSendChat(h, sender, text) }.getOrDefault(0L)
        if (id > 0L) {
            val nowMs = System.currentTimeMillis()
            _uiState.update { state ->
                state.copy(chat = state.chat.onOutgoingSent(id, sender, text, nowMs))
            }
        }
    }

    fun setMaxBitrate(bitrateKbps: Int) {
        val h = activeHandle.get()
        if (h != 0L && VeloxNative.isLoaded) {
            VeloxNative.nativeSetMaxBitrate(h, bitrateKbps)
        }
        _uiState.update { it.copy(bitrateKbps = bitrateKbps) }
    }

    fun updateSessionMode(mode: Int, profile: StreamProfile) {
        activeProfile = profile
        val h = activeHandle.get()
        if (h != 0L && VeloxNative.isLoaded) {
            runCatching {
                VeloxNative.nativeUpdateSessionMode(
                    handle = h,
                    mode = mode,
                    maxBitrateKbps = profile.bitrateKbps,
                    maxFps = profile.fpsCap,
                    audioFrameUs = profile.protocolAudioFrameUs(),
                    preferYuv444 = profile.preferYuv444,
                )
            }
        }
        _uiState.update { it.copy(mode = mode, bitrateKbps = profile.bitrateKbps) }
    }

    fun updateAudioProcessing(echoCancellation: Boolean, noiseSuppression: Boolean) {
        activeProfile = activeProfile.copy(
            echoCancellation = echoCancellation,
            noiseSuppression = noiseSuppression,
        )
        val h = activeHandle.get()
        if (h != 0L && VeloxNative.isLoaded) {
            runCatching {
                VeloxNative.nativeSetAudioProcessing(h, echoCancellation, noiseSuppression)
            }
        }
    }

    fun switchDisplay(displayIndex: Int) {
        val h = activeHandle.get()
        if (h != 0L && VeloxNative.isLoaded) {
            VeloxNative.nativeSwitchDisplay(h, displayIndex)
        }
        touchController.activeDisplay = displayIndex
        _uiState.update { it.copy(activeDisplay = displayIndex) }
    }

    // ── Special Keys, Soft Keyboard, Hardware Keyboard & Gamepad ─────────────

    fun onSpecialKeyPressed(key: SpecialKeyDef) {
        val h = activeHandle.get()
        if (h == 0L || !VeloxNative.isLoaded) return

        if (key.isToggleModifier && key.modifierBit != 0) {
            val currentMods = _uiState.value.stickyModifiers
            val isLatched = (currentMods and key.modifierBit) != 0
            val nextMods = if (isLatched) {
                currentMods and key.modifierBit.inv()
            } else {
                currentMods or key.modifierBit
            }
            VeloxNative.nativeSendKey(h, key.hidUsage, !isLatched, nextMods)
            _uiState.update { it.copy(stickyModifiers = nextMods) }
        } else {
            val mods = _uiState.value.stickyModifiers
            VeloxNative.nativeSendKey(h, key.hidUsage, true, mods)
            VeloxNative.nativeSendKey(h, key.hidUsage, false, mods)
            if (mods != 0) {
                releaseStickyModifiers()
            }
        }
    }

    fun sendCtrlAltDel() {
        val h = activeHandle.get()
        if (h == 0L || !VeloxNative.isLoaded) return
        releaseStickyModifiers()
        KeyMapper.sendCtrlAltDel { hidUsage, down, modifiers ->
            VeloxNative.nativeSendKey(h, hidUsage, down, modifiers)
        }
    }

    fun sendSoftKeyboardText(text: String) {
        val h = activeHandle.get()
        if (h == 0L || !VeloxNative.isLoaded || text.isEmpty()) return

        val mods = _uiState.value.stickyModifiers
        // If user latched Ctrl/Alt/Win and tapped a single ASCII letter/digit, emit a HID chord.
        if (mods != 0 && text.length == 1) {
            val hid = asciiCharToHid(text[0])
            if (hid != null) {
                VeloxNative.nativeSendKey(h, hid, true, mods)
                VeloxNative.nativeSendKey(h, hid, false, mods)
                releaseStickyModifiers()
                return
            }
        }
        VeloxNative.nativeSendText(h, text)
    }

    fun sendSoftBackspace() {
        val h = activeHandle.get()
        if (h == 0L || !VeloxNative.isLoaded) return
        val mods = _uiState.value.stickyModifiers
        VeloxNative.nativeSendKey(h, HID_BACKSPACE, true, mods)
        VeloxNative.nativeSendKey(h, HID_BACKSPACE, false, mods)
        if (mods != 0) {
            releaseStickyModifiers()
        }
    }

    fun sendSoftEnter() {
        val h = activeHandle.get()
        if (h == 0L || !VeloxNative.isLoaded) return
        val mods = _uiState.value.stickyModifiers
        VeloxNative.nativeSendKey(h, HID_ENTER, true, mods)
        VeloxNative.nativeSendKey(h, HID_ENTER, false, mods)
        if (mods != 0) {
            releaseStickyModifiers()
        }
    }

    fun dispatchHardwareKeyEvent(event: KeyEvent): Boolean {
        if (!_uiState.value.isActive) return false
        if (_uiState.value.authPrompt != null) return false
        if (_uiState.value.chat.isOpen) return false

        if (gamepadController.isGamepadDevice(event.device)) {
            if (gamepadController.onKeyEvent(event)) return true
        }

        // Preserve Android system volume / power / back keys
        when (event.keyCode) {
            KeyEvent.KEYCODE_VOLUME_UP,
            KeyEvent.KEYCODE_VOLUME_DOWN,
            KeyEvent.KEYCODE_VOLUME_MUTE,
            KeyEvent.KEYCODE_POWER,
            KeyEvent.KEYCODE_BACK,
            -> return false
        }

        val h = activeHandle.get()
        if (h == 0L || !VeloxNative.isLoaded) return false

        val hid = KeyMapper.androidKeyCodeToHid(event.keyCode) ?: return false
        val down = when (event.action) {
            KeyEvent.ACTION_DOWN -> true
            KeyEvent.ACTION_UP -> false
            else -> return false
        }
        val modifiers = KeyMapper.androidMetaStateToModifiers(event.metaState) or _uiState.value.stickyModifiers
        VeloxNative.nativeSendKey(h, hid, down, modifiers)
        return true
    }

    fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        if (!_uiState.value.isActive) return false
        if (gamepadController.isGamepadDevice(event.device)) {
            return gamepadController.onGenericMotionEvent(event)
        }
        return false
    }

    private fun releaseStickyModifiers() {
        val h = activeHandle.get()
        val mods = _uiState.value.stickyModifiers
        if (mods == 0) return
        if (h != 0L && VeloxNative.isLoaded) {
            for (key in KeyMapper.primarySpecialKeys) {
                if (key.isToggleModifier && (mods and key.modifierBit) != 0) {
                    VeloxNative.nativeSendKey(h, key.hidUsage, false, 0)
                }
            }
        }
        _uiState.update { it.copy(stickyModifiers = 0) }
    }

    override fun onCleared() {
        disconnectInternal(recordCloseReason = false)
        videoDecoder.release()
        audioPlayer.release()
        super.onCleared()
    }

    companion object {
        private const val HID_ENTER = 0x28
        private const val HID_BACKSPACE = 0x2A
        private const val MAX_CURSOR_CACHE_ENTRIES = 16
        private const val MAX_CURSOR_DIM = 256

        internal fun parseDisplaysJson(json: String): List<DisplayDescriptor> {
            if (json.isBlank()) return emptyList()
            return try {
                val arr = JSONArray(json)
                buildList {
                    for (i in 0 until arr.length()) {
                        val obj = arr.optJSONObject(i) ?: continue
                        add(
                            DisplayDescriptor(
                                index = obj.optInt("index", i),
                                name = obj.optString("name", "Display ${i + 1}"),
                                x = obj.optInt("x", 0),
                                y = obj.optInt("y", 0),
                                width = obj.optInt("width", 1920),
                                height = obj.optInt("height", 1080),
                                refreshHz = obj.optInt("refresh_hz", obj.optInt("refreshHz", 60)),
                                primary = obj.optBoolean("primary", i == 0),
                            ),
                        )
                    }
                }
            } catch (_: Throwable) {
                emptyList()
            }
        }

        private fun asciiCharToHid(ch: Char): Int? {
            val lower = ch.lowercaseChar()
            return when (lower) {
                in 'a'..'z' -> 0x04 + (lower - 'a')
                in '1'..'9' -> 0x1E + (lower - '1')
                '0' -> 0x27
                ' ' -> 0x2C
                else -> null
            }
        }
    }
}

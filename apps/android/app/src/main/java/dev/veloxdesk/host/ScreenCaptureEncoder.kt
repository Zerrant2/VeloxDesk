package dev.veloxdesk.host

import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaFormat
import android.media.projection.MediaProjection
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.Surface
import java.nio.ByteBuffer
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Hardware zero-copy Android screen capture & H.264 Annex-B encoder.
 *
 * Uses `MediaProjection` -> `VirtualDisplay` -> `MediaCodec.createInputSurface()` so frames stay
 * on the GPU without CPU readback. Caches SPS/PPS from `BUFFER_FLAG_CODEC_CONFIG` and prepends
 * them to every IDR keyframe so remote PC/Android viewers can start decoding immediately.
 */
class ScreenCaptureEncoder(
    private val mediaProjection: MediaProjection,
    private val onEncodedFrame: (data: ByteArray, isKeyframe: Boolean, ptsUs: Long, width: Int, height: Int) -> Unit,
    private val onProjectionRevoked: () -> Unit,
    private val onError: (String) -> Unit,
) {
    private val lock = Any()
    private val running = AtomicBoolean(false)
    private val mainHandler = Handler(Looper.getMainLooper())

    private var codec: MediaCodec? = null
    private var inputSurface: Surface? = null
    private var virtualDisplay: VirtualDisplay? = null
    private var drainThread: Thread? = null

    @Volatile
    var encodedWidth: Int = 0
        private set

    @Volatile
    var encodedHeight: Int = 0
        private set

    @Volatile
    var targetFps: Int = 30
        private set

    @Volatile
    private var currentBitrateKbps: Int = DEFAULT_BITRATE_KBPS

    @Volatile
    private var cachedSpsPps: ByteArray? = null

    private var callbackRegistered: Boolean = false

    private val projectionCallback = object : MediaProjection.Callback() {
        override fun onStop() {
            Log.i(TAG, "MediaProjection revoked by system/user")
            stop()
            onProjectionRevoked()
        }
    }

    fun start(rawWidth: Int, rawHeight: Int, densityDpi: Int, fps: Int, bitrateKbps: Int = DEFAULT_BITRATE_KBPS): Pair<Int, Int> {
        synchronized(lock) {
            stopPipelineLocked()
            if (!callbackRegistered) {
                // Android 14+ requires registering a callback BEFORE createVirtualDisplay().
                mediaProjection.registerCallback(projectionCallback, mainHandler)
                callbackRegistered = true
            }
            targetFps = fps.coerceIn(15, 60)
            currentBitrateKbps = bitrateKbps.coerceIn(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
            val (w, h) = computeScaledDimensions(rawWidth, rawHeight, MAX_LONG_EDGE)
            startPipelineLocked(w, h, densityDpi.coerceAtLeast(120), targetFps, currentBitrateKbps)
            return Pair(w, h)
        }
    }

    fun reconfigureForRotation(rawWidth: Int, rawHeight: Int, densityDpi: Int): Pair<Int, Int>? {
        synchronized(lock) {
            if (!running.get()) return null
            val (w, h) = computeScaledDimensions(rawWidth, rawHeight, MAX_LONG_EDGE)
            if (w == encodedWidth && h == encodedHeight) return null
            Log.i(TAG, "Reconfiguring screen capture encoder for rotation: ${encodedWidth}x$encodedHeight -> ${w}x$h")
            val dpi = densityDpi.coerceAtLeast(120)
            val existingDisplay = virtualDisplay
            if (existingDisplay != null) {
                // Android 14/15: NEVER release & recreate VirtualDisplay on MediaProjection during
                // rotation (single-use projection token). Detach surface, recreate MediaCodec only,
                // resize the existing VirtualDisplay, and attach the new input surface.
                try {
                    existingDisplay.setSurface(null)
                } catch (t: Throwable) {
                    Log.w(TAG, "VirtualDisplay.setSurface(null) failed: ${t.message}")
                }
                stopCodecOnlyLocked()
                startCodecOnExistingDisplayLocked(existingDisplay, w, h, dpi, targetFps, currentBitrateKbps)
            } else {
                stopPipelineLocked()
                startPipelineLocked(w, h, dpi, targetFps, currentBitrateKbps)
            }
            return Pair(w, h)
        }
    }

    fun updateBitrate(kbps: Int) {
        val clamped = kbps.coerceIn(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
        currentBitrateKbps = clamped
        synchronized(lock) {
            val c = codec ?: return
            try {
                val params = Bundle().apply {
                    putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, clamped * 1000)
                }
                c.setParameters(params)
            } catch (t: Throwable) {
                Log.w(TAG, "Failed updating encoder bitrate to ${clamped}kbps: ${t.message}")
            }
        }
    }

    fun requestKeyframe() {
        synchronized(lock) {
            val c = codec ?: return
            try {
                val params = Bundle().apply {
                    putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
                }
                c.setParameters(params)
            } catch (t: Throwable) {
                Log.w(TAG, "Failed requesting sync frame: ${t.message}")
            }
        }
    }

    fun stop() {
        synchronized(lock) {
            stopPipelineLocked()
            if (callbackRegistered) {
                try {
                    mediaProjection.unregisterCallback(projectionCallback)
                } catch (_: Throwable) {
                }
                callbackRegistered = false
            }
            try {
                mediaProjection.stop()
            } catch (_: Throwable) {
            }
        }
    }

    private fun startPipelineLocked(width: Int, height: Int, densityDpi: Int, fps: Int, bitrateKbps: Int) {
        cachedSpsPps = null
        encodedWidth = width
        encodedHeight = height

        val encoder = createAndConfigureCodec(width, height, fps, bitrateKbps)
        val surface = encoder.createInputSurface()
        encoder.start()

        val display = mediaProjection.createVirtualDisplay(
            VIRTUAL_DISPLAY_NAME,
            width,
            height,
            densityDpi,
            DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
            surface,
            null,
            null,
        )

        codec = encoder
        inputSurface = surface
        virtualDisplay = display
        running.set(true)

        val thread = Thread({ drainLoop(encoder, width, height) }, "vdk-android-host-enc").apply {
            priority = Thread.MAX_PRIORITY - 1
            start()
        }
        drainThread = thread
    }

    private fun startCodecOnExistingDisplayLocked(
        display: VirtualDisplay,
        width: Int,
        height: Int,
        densityDpi: Int,
        fps: Int,
        bitrateKbps: Int,
    ) {
        cachedSpsPps = null
        encodedWidth = width
        encodedHeight = height

        val encoder = createAndConfigureCodec(width, height, fps, bitrateKbps)
        val surface = encoder.createInputSurface()
        encoder.start()

        display.resize(width, height, densityDpi)
        display.setSurface(surface)

        codec = encoder
        inputSurface = surface
        running.set(true)

        val thread = Thread({ drainLoop(encoder, width, height) }, "vdk-android-host-enc").apply {
            priority = Thread.MAX_PRIORITY - 1
            start()
        }
        drainThread = thread
    }

    private fun stopCodecOnlyLocked() {
        running.set(false)
        val thread = drainThread
        drainThread = null
        val c = codec
        codec = null
        val surf = inputSurface
        inputSurface = null

        if (thread != null && thread !== Thread.currentThread()) {
            try {
                thread.join(500)
            } catch (_: InterruptedException) {
            }
        }
        try {
            c?.stop()
        } catch (_: Throwable) {
        }
        try {
            c?.release()
        } catch (_: Throwable) {
        }
        try {
            surf?.release()
        } catch (_: Throwable) {
        }
    }

    private fun stopPipelineLocked() {
        val vd = virtualDisplay
        virtualDisplay = null
        try {
            vd?.setSurface(null)
        } catch (_: Throwable) {
        }
        try {
            vd?.release()
        } catch (_: Throwable) {
        }
        stopCodecOnlyLocked()
    }

    private fun createAndConfigureCodec(width: Int, height: Int, fps: Int, bitrateKbps: Int): MediaCodec {
        val attempts = listOf(true, false)
        var lastError: Throwable? = null
        for (useLowLatencyExtras in attempts) {
            val encoder = MediaCodec.createEncoderByType(MediaFormat.MIMETYPE_VIDEO_AVC)
            try {
                val fmt = MediaFormat.createVideoFormat(MediaFormat.MIMETYPE_VIDEO_AVC, width, height).apply {
                    setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
                    setInteger(MediaFormat.KEY_BIT_RATE, bitrateKbps * 1000)
                    setInteger(MediaFormat.KEY_FRAME_RATE, fps)
                    setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 60)
                    setLong(MediaFormat.KEY_REPEAT_PREVIOUS_FRAME_AFTER, REPEAT_PREV_FRAME_US)
                    if (useLowLatencyExtras) {
                        setInteger(MediaFormat.KEY_PRIORITY, 0)
                        setInteger(
                            MediaFormat.KEY_BITRATE_MODE,
                            MediaCodecInfo.EncoderCapabilities.BITRATE_MODE_CBR,
                        )
                        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                            setInteger(MediaFormat.KEY_LATENCY, 0)
                        }
                    }
                }
                encoder.configure(fmt, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                return encoder
            } catch (t: Throwable) {
                lastError = t
                try {
                    encoder.release()
                } catch (_: Throwable) {
                }
                Log.w(TAG, "MediaCodec configure (lowLatency=$useLowLatencyExtras) failed: ${t.message}")
            }
        }
        throw IllegalStateException("Failed to configure H.264 hardware encoder: ${lastError?.message}", lastError)
    }

    private fun drainLoop(encoder: MediaCodec, width: Int, height: Int) {
        val info = MediaCodec.BufferInfo()
        while (running.get()) {
            try {
                val outIdx = encoder.dequeueOutputBuffer(info, DEQUEUE_TIMEOUT_US)
                if (outIdx == MediaCodec.INFO_TRY_AGAIN_LATER) {
                    continue
                }
                if (outIdx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED) {
                    extractCsdFromFormat(encoder.outputFormat)?.let { csd ->
                        cachedSpsPps = csd
                    }
                    continue
                }
                if (outIdx < 0) continue

                try {
                    val buf = encoder.getOutputBuffer(outIdx) ?: continue
                    if (info.size > 0) {
                        buf.position(info.offset)
                        buf.limit(info.offset + info.size)
                        val raw = ByteArray(info.size)
                        buf.get(raw)

                        val isConfig = (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0
                        if (isConfig) {
                            cachedSpsPps = raw
                        } else {
                            val isKeyframe = (info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0
                            val payload = if (isKeyframe) {
                                ensureSpsPpsOnKeyframe(raw, cachedSpsPps)
                            } else {
                                raw
                            }
                            onEncodedFrame(payload, isKeyframe, info.presentationTimeUs, width, height)
                        }
                    }
                } finally {
                    encoder.releaseOutputBuffer(outIdx, false)
                }
            } catch (t: Throwable) {
                if (running.get()) {
                    Log.e(TAG, "Encoder drain loop error: ${t.message}", t)
                    onError("Screen encoder error: ${t.message ?: t.javaClass.simpleName}")
                }
                break
            }
        }
    }

    private fun extractCsdFromFormat(fmt: MediaFormat): ByteArray? {
        val csd0 = fmt.getByteBuffer("csd-0") ?: return null
        val csd1 = fmt.getByteBuffer("csd-1")
        val b0 = bufferToBytes(csd0)
        val b1 = if (csd1 != null) bufferToBytes(csd1) else ByteArray(0)
        return if (b1.isEmpty()) b0 else b0 + b1
    }

    private fun bufferToBytes(bb: ByteBuffer): ByteArray {
        val dup = bb.duplicate()
        val out = ByteArray(dup.remaining())
        dup.get(out)
        return out
    }

    companion object {
        private const val TAG = "ScreenCaptureEncoder"
        private const val VIRTUAL_DISPLAY_NAME = "VeloxDeskHost"
        private const val MAX_LONG_EDGE = 1080
        private const val DEFAULT_BITRATE_KBPS = 6_000
        private const val MIN_BITRATE_KBPS = 500
        private const val MAX_BITRATE_KBPS = 25_000
        private const val DEQUEUE_TIMEOUT_US = 10_000L
        private const val REPEAT_PREV_FRAME_US = 100_000L

        /**
         * Scale raw physical display dimensions so the longest edge is at most [maxLongEdge]
         * while preserving aspect ratio and aligning both dimensions to even multiples of 16
         * (with safe floor of 256px) for universal mobile H.264 MediaCodec compatibility.
         */
        fun computeScaledDimensions(rawWidth: Int, rawHeight: Int, maxLongEdge: Int = MAX_LONG_EDGE): Pair<Int, Int> {
            val w = rawWidth.coerceAtLeast(256)
            val h = rawHeight.coerceAtLeast(256)
            val longEdge = maxOf(w, h)
            val scale = if (longEdge > maxLongEdge) maxLongEdge.toFloat() / longEdge.toFloat() else 1f
            val scaledW = alignEven((w * scale).toInt(), 16).coerceAtLeast(256)
            val scaledH = alignEven((h * scale).toInt(), 16).coerceAtLeast(256)
            return Pair(scaledW, scaledH)
        }

        private fun alignEven(value: Int, align: Int): Int {
            val a = align.coerceAtLeast(2)
            return ((value + a / 2) / a) * a
        }

        /**
         * Ensure Annex-B H.264 keyframe contains SPS (NAL type 7) by prepending cached CSD when absent.
         */
        fun ensureSpsPpsOnKeyframe(frame: ByteArray, codecConfig: ByteArray?): ByteArray {
            if (codecConfig == null || codecConfig.isEmpty()) return frame
            if (containsH264Sps(frame)) return frame
            val combined = ByteArray(codecConfig.size + frame.size)
            System.arraycopy(codecConfig, 0, combined, 0, codecConfig.size)
            System.arraycopy(frame, 0, combined, codecConfig.size, frame.size)
            return combined
        }

        fun containsH264Sps(annexB: ByteArray): Boolean {
            var i = 0
            while (i + 3 < annexB.size) {
                if (annexB[i].toInt() == 0 && annexB[i + 1].toInt() == 0) {
                    val nalStart = when {
                        annexB[i + 2].toInt() == 1 -> i + 3
                        i + 4 < annexB.size && annexB[i + 2].toInt() == 0 && annexB[i + 3].toInt() == 1 -> i + 4
                        else -> -1
                    }
                    if (nalStart in annexB.indices) {
                        val nalType = annexB[nalStart].toInt() and 0x1F
                        if (nalType == 7) return true
                        i = nalStart + 1
                        continue
                    }
                }
                i++
            }
            return false
        }
    }
}

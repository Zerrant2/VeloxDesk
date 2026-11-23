package dev.veloxdesk.media

import android.media.MediaCodec
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.util.Log
import android.view.Surface
import dev.veloxdesk.bridge.BridgeConstants
import java.nio.ByteBuffer

/**
 * Zero-copy hardware-accelerated [MediaCodec] video decoder rendering directly to a [Surface].
 *
 * Thread-safe: [onFrame] is invoked from the native Rust video thread while [attachSurface],
 * [detachSurface], and [release] are called from the Android UI thread.
 */
class VideoDecoderEngine(
    private val onRequestKeyframe: () -> Unit,
    private val onDecodeTimingUs: (decodeUs: Int, renderUs: Int) -> Unit,
    private val onDecoderInfo: (name: String, hardware: Boolean) -> Unit,
) {
    private val lock = Any()

    private var surface: Surface? = null
    private var codec: MediaCodec? = null
    private var activeCodecType: Int = BridgeConstants.CODEC_UNSPECIFIED
    private var activeWidth: Int = 0
    private var activeHeight: Int = 0
    private var activeDecoderName: String = ""
    private var activeIsHardware: Boolean = false
    private var forceSoftware: Boolean = false

    private var waitingForKeyframe: Boolean = true
    private var released: Boolean = false
    private var lastKeyframeRequestNs: Long = 0L
    private var lastPtsUs: Long = 0L

    private var cachedVps: ByteArray? = null
    private var cachedSps: ByteArray? = null
    private var cachedPps: ByteArray? = null

    private val enqueueTimesNs = LinkedHashMap<Long, Long>(32)
    private val bufferInfo = MediaCodec.BufferInfo()

    fun attachSurface(surface: Surface) {
        var shouldRequestKeyframe = false
        synchronized(lock) {
            if (released) return
            if (this.surface === surface && codec != null) {
                return
            }
            this.surface = surface
            if (codec != null) {
                releaseCodecLocked()
            }
            waitingForKeyframe = true
            shouldRequestKeyframe = true
            lastKeyframeRequestNs = System.nanoTime()
        }
        if (shouldRequestKeyframe) {
            safeInvoke(onRequestKeyframe)
        }
    }

    fun detachSurface() {
        synchronized(lock) {
            surface = null
            releaseCodecLocked()
            waitingForKeyframe = true
        }
    }

    /**
     * Resets all per-session decoder state so this engine can be cleanly reused across
     * consecutive sessions without carrying stale parameter sets, timestamps, or software fallback.
     */
    fun resetSession() {
        synchronized(lock) {
            if (released) return
            releaseCodecLocked()
            cachedVps = null
            cachedSps = null
            cachedPps = null
            lastPtsUs = 0L
            lastKeyframeRequestNs = 0L
            forceSoftware = false
            activeCodecType = BridgeConstants.CODEC_UNSPECIFIED
            activeWidth = 0
            activeHeight = 0
            activeDecoderName = ""
            activeIsHardware = false
            waitingForKeyframe = true
        }
    }

    fun onFrame(
        codec: Int,
        width: Int,
        height: Int,
        frameId: Int,
        captureTsUs: Long,
        keyframe: Boolean,
        discontinuity: Boolean,
        data: ByteArray,
    ) {
        if (data.isEmpty() || width <= 0 || height <= 0) return
        val mime = codecToMime(codec) ?: return

        var shouldRequestKeyframe = false
        var newDecoderInfo: Pair<String, Boolean>? = null
        var timings: MutableList<Pair<Int, Int>>? = null

        synchronized(lock) {
            if (released) return

            // Reset cached parameter sets & software fallback if codec type changed.
            if (activeCodecType != BridgeConstants.CODEC_UNSPECIFIED && activeCodecType != codec) {
                cachedVps = null
                cachedSps = null
                cachedPps = null
                forceSoftware = false
            }

            val parsed = AnnexBParser.parseAccessUnit(codec, data)
            updateCachedCsdLocked(parsed)
            val isKeyframe = keyframe || parsed.isKeyframe

            val currentSurface = surface
            if (currentSurface == null || !currentSurface.isValid) {
                return
            }

            if (discontinuity && !isKeyframe) {
                waitingForKeyframe = true
                flushCodecLocked()
                shouldRequestKeyframe = markKeyframeRequestIfDueLocked(force = false)
                return@synchronized
            }

            val needsReconfigure = this.codec == null ||
                activeCodecType != codec ||
                activeWidth != width ||
                activeHeight != height

            if (needsReconfigure) {
                if (!isKeyframe) {
                    waitingForKeyframe = true
                    shouldRequestKeyframe = markKeyframeRequestIfDueLocked(force = false)
                    return@synchronized
                }
                val info = configureDecoderLocked(
                    codecType = codec,
                    mime = mime,
                    width = width,
                    height = height,
                    targetSurface = currentSurface,
                )
                if (info == null) {
                    waitingForKeyframe = true
                    shouldRequestKeyframe = markKeyframeRequestIfDueLocked(force = false)
                    return@synchronized
                }
                newDecoderInfo = info
                waitingForKeyframe = false
            } else if (waitingForKeyframe) {
                if (!isKeyframe) {
                    shouldRequestKeyframe = markKeyframeRequestIfDueLocked(force = false)
                    return@synchronized
                }
                waitingForKeyframe = false
            }

            val mc = this.codec ?: return@synchronized
            val payload = if (isKeyframe && (codec == BridgeConstants.CODEC_H264 || codec == BridgeConstants.CODEC_H265)) {
                AnnexBParser.prependMissingParameterSets(
                    codec = codec,
                    data = data,
                    parsed = parsed,
                    cachedVps = cachedVps,
                    cachedSps = cachedSps,
                    cachedPps = cachedPps,
                )
            } else {
                data
            }

            try {
                // Drain any completed outputs first so input buffer pool stays free.
                timings = drainOutputsLocked(mc, currentSurface, timings)

                val inIdx = mc.dequeueInputBuffer(INPUT_DEQUEUE_TIMEOUT_US)
                if (inIdx >= 0) {
                    val inputBuf = mc.getInputBuffer(inIdx)
                    if (inputBuf != null) {
                        inputBuf.clear()
                        if (inputBuf.capacity() < payload.size) {
                            // Reconfigure with larger KEY_MAX_INPUT_SIZE if needed.
                            mc.queueInputBuffer(inIdx, 0, 0, 0L, 0)
                            releaseCodecLocked()
                            waitingForKeyframe = true
                            shouldRequestKeyframe = markKeyframeRequestIfDueLocked(force = true)
                            return@synchronized
                        }
                        inputBuf.put(payload)
                        val ptsUs = nextPtsUsLocked(captureTsUs)
                        recordEnqueueTimeLocked(ptsUs, System.nanoTime())
                        val flags = if (isKeyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
                        mc.queueInputBuffer(inIdx, 0, payload.size, ptsUs, flags)
                    } else {
                        mc.queueInputBuffer(inIdx, 0, 0, 0L, 0)
                    }
                } else {
                    // Input queue saturated: try draining outputs once more.
                    timings = drainOutputsLocked(mc, currentSurface, timings)
                }

                // Non-blocking drain after queuing.
                timings = drainOutputsLocked(mc, currentSurface, timings)
            } catch (e: Exception) {
                Log.w(TAG, "Decoder error on $activeDecoderName (hw=$activeIsHardware): ${e.message}")
                val wasHardware = activeIsHardware
                releaseCodecLocked()
                if (wasHardware) {
                    forceSoftware = true
                }
                waitingForKeyframe = true
                shouldRequestKeyframe = markKeyframeRequestIfDueLocked(force = true)
            }
        }

        // Dispatch callbacks outside lock to prevent deadlocks with UI or JNI threads.
        newDecoderInfo?.let { (name, hw) ->
            safeInvoke { onDecoderInfo(name, hw) }
        }
        timings?.forEach { (decodeUs, renderUs) ->
            safeInvoke { onDecodeTimingUs(decodeUs, renderUs) }
        }
        if (shouldRequestKeyframe) {
            safeInvoke(onRequestKeyframe)
        }
    }

    fun release() {
        synchronized(lock) {
            released = true
            surface = null
            releaseCodecLocked()
            cachedVps = null
            cachedSps = null
            cachedPps = null
            enqueueTimesNs.clear()
        }
    }

    private fun updateCachedCsdLocked(parsed: AnnexBParser.ParsedAccessUnit): Boolean {
        var changed = false
        parsed.vps?.let {
            if (!it.contentEquals(cachedVps)) {
                cachedVps = it
                changed = true
            }
        }
        parsed.sps?.let {
            if (!it.contentEquals(cachedSps)) {
                cachedSps = it
                changed = true
            }
        }
        parsed.pps?.let {
            if (!it.contentEquals(cachedPps)) {
                cachedPps = it
                changed = true
            }
        }
        return changed
    }

    private fun configureDecoderLocked(
        codecType: Int,
        mime: String,
        width: Int,
        height: Int,
        targetSurface: Surface,
    ): Pair<String, Boolean>? {
        releaseCodecLocked()

        val candidates = findDecoderCandidates(mime, preferHardware = !forceSoftware)
        for (candidate in candidates) {
            val created = tryStartDecoder(
                decoderName = candidate.name,
                isHardware = candidate.hardware,
                codecType = codecType,
                mime = mime,
                width = width,
                height = height,
                targetSurface = targetSurface,
            )
            if (created != null) {
                codec = created
                activeCodecType = codecType
                activeWidth = width
                activeHeight = height
                activeDecoderName = candidate.name
                activeIsHardware = candidate.hardware
                if (!candidate.hardware) {
                    forceSoftware = true
                }
                return candidate.name to candidate.hardware
            }
        }

        // Final fallback via generic MediaCodec.createDecoderByType
        try {
            val generic = MediaCodec.createDecoderByType(mime)
            val name = try {
                generic.name
            } catch (_: Throwable) {
                mime
            }
            val isHw = !CodecCapsProber.isSoftwareCodecName(name)
            val format = buildMediaFormat(codecType, mime, width, height, includeOperatingRate = false)
            generic.configure(format, targetSurface, null, 0)
            generic.start()
            codec = generic
            activeCodecType = codecType
            activeWidth = width
            activeHeight = height
            activeDecoderName = name
            activeIsHardware = isHw
            return name to isHw
        } catch (e: Exception) {
            Log.e(TAG, "All decoders failed for $mime ${width}x$height: ${e.message}")
            return null
        }
    }

    private fun tryStartDecoder(
        decoderName: String,
        isHardware: Boolean,
        codecType: Int,
        mime: String,
        width: Int,
        height: Int,
        targetSurface: Surface,
    ): MediaCodec? {
        // First try with full low-latency operating rate; if rejected by a vendor codec, retry without operating rate.
        for (includeOpRate in booleanArrayOf(true, false)) {
            var mc: MediaCodec? = null
            try {
                mc = MediaCodec.createByCodecName(decoderName)
                val format = buildMediaFormat(codecType, mime, width, height, includeOperatingRate = includeOpRate)
                mc.configure(format, targetSurface, null, 0)
                mc.start()
                return mc
            } catch (e: Exception) {
                Log.w(
                    TAG,
                    "Failed starting $decoderName (hw=$isHardware, opRate=$includeOpRate): ${e.message}",
                )
                try {
                    mc?.release()
                } catch (_: Throwable) {
                }
            }
        }
        return null
    }

    private fun buildMediaFormat(
        codecType: Int,
        mime: String,
        width: Int,
        height: Int,
        includeOperatingRate: Boolean,
    ): MediaFormat {
        val format = MediaFormat.createVideoFormat(mime, width, height)
        format.setInteger(MediaFormat.KEY_PRIORITY, 0)
        if (includeOperatingRate) {
            format.setInteger(MediaFormat.KEY_OPERATING_RATE, Short.MAX_VALUE.toInt())
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            format.setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
        }
        val maxInputSize = (width.toLong() * height.toLong() * 3L / 2L)
            .coerceIn(MIN_INPUT_BUFFER_BYTES.toLong(), MAX_INPUT_BUFFER_BYTES.toLong())
            .toInt()
        format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, maxInputSize)

        AnnexBParser.buildCsd0(codecType, cachedVps, cachedSps, cachedPps)?.let { csd0 ->
            format.setByteBuffer("csd-0", ByteBuffer.wrap(csd0))
        }
        AnnexBParser.buildCsd1(codecType, cachedPps)?.let { csd1 ->
            format.setByteBuffer("csd-1", ByteBuffer.wrap(csd1))
        }
        return format
    }

    private fun drainOutputsLocked(
        mc: MediaCodec,
        currentSurface: Surface,
        timingsIn: MutableList<Pair<Int, Int>>?,
    ): MutableList<Pair<Int, Int>>? {
        var timings = timingsIn
        while (true) {
            val outIdx = mc.dequeueOutputBuffer(bufferInfo, 0L)
            when {
                outIdx >= 0 -> {
                    val renderStartNs = System.nanoTime()
                    val shouldRender = bufferInfo.size > 0 && currentSurface.isValid
                    mc.releaseOutputBuffer(outIdx, shouldRender)
                    val renderEndNs = System.nanoTime()

                    val enqueueNs = enqueueTimesNs.remove(bufferInfo.presentationTimeUs)
                    if (enqueueNs != null && shouldRender) {
                        val decodeUs = ((renderStartNs - enqueueNs) / 1000L)
                            .coerceIn(0L, Int.MAX_VALUE.toLong())
                            .toInt()
                        val renderUs = ((renderEndNs - renderStartNs) / 1000L)
                            .coerceIn(0L, Int.MAX_VALUE.toLong())
                            .toInt()
                        if (timings == null) {
                            timings = ArrayList(4)
                        }
                        timings.add(decodeUs to renderUs)
                    }
                }
                outIdx == MediaCodec.INFO_OUTPUT_FORMAT_CHANGED -> {
                    // Format change handled transparently by Surface rendering.
                }
                outIdx == MediaCodec.INFO_TRY_AGAIN_LATER -> {
                    break
                }
                else -> {
                    break
                }
            }
        }
        return timings
    }

    private fun nextPtsUsLocked(captureTsUs: Long): Long {
        val candidate = if (captureTsUs > 0L) captureTsUs else (lastPtsUs + 16_667L)
        val pts = if (candidate > lastPtsUs) candidate else (lastPtsUs + 1L)
        lastPtsUs = pts
        return pts
    }

    private fun recordEnqueueTimeLocked(ptsUs: Long, nowNs: Long) {
        if (enqueueTimesNs.size >= MAX_TIMING_ENTRIES) {
            val oldest = enqueueTimesNs.keys.firstOrNull()
            if (oldest != null) {
                enqueueTimesNs.remove(oldest)
            }
        }
        enqueueTimesNs[ptsUs] = nowNs
    }

    private fun markKeyframeRequestIfDueLocked(force: Boolean): Boolean {
        val nowNs = System.nanoTime()
        if (force || nowNs - lastKeyframeRequestNs >= KEYFRAME_REQUEST_COOLDOWN_NS) {
            lastKeyframeRequestNs = nowNs
            return true
        }
        return false
    }

    private fun flushCodecLocked() {
        val mc = codec ?: return
        try {
            mc.flush()
        } catch (_: Throwable) {
            releaseCodecLocked()
        }
        enqueueTimesNs.clear()
    }

    private fun releaseCodecLocked() {
        val mc = codec
        codec = null
        enqueueTimesNs.clear()
        if (mc != null) {
            try {
                mc.stop()
            } catch (_: Throwable) {
            }
            try {
                mc.release()
            } catch (_: Throwable) {
            }
        }
    }

    private data class DecoderCandidate(val name: String, val hardware: Boolean)

    private fun findDecoderCandidates(mime: String, preferHardware: Boolean): List<DecoderCandidate> {
        val hw = ArrayList<DecoderCandidate>(4)
        val sw = ArrayList<DecoderCandidate>(4)
        val codecInfos = try {
            MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
        } catch (_: Throwable) {
            return emptyList()
        }

        for (info in codecInfos) {
            if (info.isEncoder) continue
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && info.isAlias) continue
            if (info.supportedTypes.none { it.equals(mime, ignoreCase = true) }) continue

            val isHw = CodecCapsProber.isHardwareDecoder(info)
            val candidate = DecoderCandidate(info.name, isHw)
            if (isHw) hw.add(candidate) else sw.add(candidate)
        }

        return if (preferHardware) {
            hw + sw
        } else {
            sw
        }
    }

    private fun codecToMime(codecType: Int): String? = when (codecType) {
        BridgeConstants.CODEC_H264 -> CodecCapsProber.MIME_H264
        BridgeConstants.CODEC_H265 -> CodecCapsProber.MIME_H265
        BridgeConstants.CODEC_AV1 -> CodecCapsProber.MIME_AV1
        else -> null
    }

    private inline fun safeInvoke(block: () -> Unit) {
        try {
            block()
        } catch (t: Throwable) {
            Log.w(TAG, "Callback threw: ${t.message}")
        }
    }

    companion object {
        private const val TAG = "VideoDecoderEngine"
        private const val INPUT_DEQUEUE_TIMEOUT_US = 8_000L
        private const val KEYFRAME_REQUEST_COOLDOWN_NS = 250_000_000L
        private const val MAX_TIMING_ENTRIES = 64
        private const val MIN_INPUT_BUFFER_BYTES = 512 * 1024
        private const val MAX_INPUT_BUFFER_BYTES = 16 * 1024 * 1024
    }
}

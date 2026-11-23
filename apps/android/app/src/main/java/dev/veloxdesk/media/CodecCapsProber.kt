package dev.veloxdesk.media

import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.os.Build
import dev.veloxdesk.bridge.BridgeConstants

/**
 * Probes Android [MediaCodecList] for supported hardware and software video decoders
 * and encodes capabilities into the [BridgeConstants] `CAP_*` bitmask sent during handshake.
 */
object CodecCapsProber {
    internal const val MIME_H264 = "video/avc"
    internal const val MIME_H265 = "video/hevc"
    internal const val MIME_AV1 = "video/av01"

    fun probeCodecCapsMask(): Int {
        var mask = BridgeConstants.CAP_H264_SW // Always include H.264 SW fallback

        val codecInfos = try {
            MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
        } catch (_: Throwable) {
            return mask
        }

        for (info in codecInfos) {
            if (info.isEncoder) continue
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && info.isAlias) continue

            val isHw = isHardwareDecoder(info)
            for (mime in info.supportedTypes) {
                when {
                    mime.equals(MIME_H264, ignoreCase = true) -> {
                        mask = mask or if (isHw) BridgeConstants.CAP_H264_HW else BridgeConstants.CAP_H264_SW
                    }
                    mime.equals(MIME_H265, ignoreCase = true) -> {
                        mask = mask or if (isHw) BridgeConstants.CAP_H265_HW else BridgeConstants.CAP_H265_SW
                    }
                    mime.equals(MIME_AV1, ignoreCase = true) -> {
                        mask = mask or if (isHw) BridgeConstants.CAP_AV1_HW else BridgeConstants.CAP_AV1_SW
                    }
                }
            }
        }

        return mask
    }

    internal fun isHardwareDecoder(info: MediaCodecInfo): Boolean {
        val name = info.name ?: ""
        if (isSoftwareCodecName(name)) return false
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            info.isHardwareAccelerated && !info.isSoftwareOnly
        } else {
            true
        }
    }

    internal fun isSoftwareCodecName(name: String): Boolean {
        val lower = name.lowercase()
        return lower.startsWith("omx.google.") ||
            lower.startsWith("c2.android.") ||
            lower.startsWith("c2.google.") ||
            lower.startsWith("omx.ffmpeg.") ||
            lower.startsWith("omx.sec.avc.sw.") ||
            lower.startsWith("omx.sec.hevc.sw.") ||
            lower.contains(".sw.") ||
            lower.contains("software")
    }
}

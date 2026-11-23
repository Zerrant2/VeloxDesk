package dev.veloxdesk.media

import dev.veloxdesk.bridge.BridgeConstants
import java.io.ByteArrayOutputStream

/**
 * Lightweight Annex-B NAL unit scanner and parameter-set (VPS/SPS/PPS) helper
 * for H.264 (AVC) and H.265 (HEVC) elementary bitstreams.
 */
internal object AnnexBParser {
    const val H264_NAL_IDR = 5
    const val H264_NAL_SPS = 7
    const val H264_NAL_PPS = 8

    const val H265_NAL_BLA_W_LP = 16
    const val H265_NAL_CRA_NUT = 21
    const val H265_NAL_VPS = 32
    const val H265_NAL_SPS = 33
    const val H265_NAL_PPS = 34

    data class NalUnit(
        val startCodeOffset: Int,
        val payloadOffset: Int,
        val payloadLength: Int,
        val headerByte: Int,
    ) {
        val startCodeLength: Int get() = payloadOffset - startCodeOffset
        val totalLength: Int get() = startCodeLength + payloadLength
        val h264Type: Int get() = headerByte and 0x1F
        val h265Type: Int get() = (headerByte ushr 1) and 0x3F
    }

    data class ParsedAccessUnit(
        val codec: Int,
        val nalUnits: List<NalUnit>,
        val vps: ByteArray?,
        val sps: ByteArray?,
        val pps: ByteArray?,
        val isKeyframe: Boolean,
    ) {
        val hasParameterSets: Boolean
            get() = when (codec) {
                BridgeConstants.CODEC_H264 -> sps != null && pps != null
                BridgeConstants.CODEC_H265 -> vps != null && sps != null && pps != null
                else -> false
            }
    }

    /**
     * Scans an Annex-B byte buffer for all 3-byte (`00 00 01`) and 4-byte (`00 00 00 01`) NAL units.
     */
    fun parseNalUnits(data: ByteArray): List<NalUnit> {
        if (data.size < 4) return emptyList()

        val starts = ArrayList<Int>(8)
        val payloads = ArrayList<Int>(8)

        var i = 0
        val limit = data.size - 3
        while (i <= limit) {
            if (data[i].toInt() == 0 && data[i + 1].toInt() == 0) {
                if (i + 3 < data.size && data[i + 2].toInt() == 0 && data[i + 3].toInt() == 1) {
                    starts.add(i)
                    payloads.add(i + 4)
                    i += 4
                    continue
                } else if (data[i + 2].toInt() == 1) {
                    starts.add(i)
                    payloads.add(i + 3)
                    i += 3
                    continue
                }
            }
            i++
        }

        if (starts.isEmpty()) return emptyList()

        val result = ArrayList<NalUnit>(starts.size)
        for (idx in starts.indices) {
            val scOffset = starts[idx]
            val pOffset = payloads[idx]
            val endOffset = if (idx + 1 < starts.size) starts[idx + 1] else data.size
            val pLen = endOffset - pOffset
            if (pLen > 0) {
                result.add(
                    NalUnit(
                        startCodeOffset = scOffset,
                        payloadOffset = pOffset,
                        payloadLength = pLen,
                        headerByte = data[pOffset].toInt() and 0xFF,
                    ),
                )
            }
        }
        return result
    }

    /**
     * Parses an H.264 or H.265 Annex-B access unit, extracting parameter-set NAL slices
     * (including their Annex-B start codes) and detecting IDR/keyframe NALs.
     */
    fun parseAccessUnit(codec: Int, data: ByteArray): ParsedAccessUnit {
        if (codec != BridgeConstants.CODEC_H264 && codec != BridgeConstants.CODEC_H265) {
            return ParsedAccessUnit(
                codec = codec,
                nalUnits = emptyList(),
                vps = null,
                sps = null,
                pps = null,
                isKeyframe = false,
            )
        }

        val nals = parseNalUnits(data)
        var vps: ByteArray? = null
        var sps: ByteArray? = null
        var pps: ByteArray? = null
        var keyframe = false

        for (nal in nals) {
            when (codec) {
                BridgeConstants.CODEC_H264 -> {
                    when (nal.h264Type) {
                        H264_NAL_SPS -> sps = sliceNal(data, nal)
                        H264_NAL_PPS -> pps = sliceNal(data, nal)
                        H264_NAL_IDR -> keyframe = true
                    }
                }
                BridgeConstants.CODEC_H265 -> {
                    when (nal.h265Type) {
                        H265_NAL_VPS -> vps = sliceNal(data, nal)
                        H265_NAL_SPS -> sps = sliceNal(data, nal)
                        H265_NAL_PPS -> pps = sliceNal(data, nal)
                        in H265_NAL_BLA_W_LP..H265_NAL_CRA_NUT -> keyframe = true
                    }
                }
            }
        }

        return ParsedAccessUnit(
            codec = codec,
            nalUnits = nals,
            vps = vps,
            sps = sps,
            pps = pps,
            isKeyframe = keyframe,
        )
    }

    /**
     * Prepends cached parameter sets (VPS/SPS/PPS) in front of [data] when the current
     * keyframe access unit is missing any of them. Returns [data] directly if nothing needs prepending.
     */
    fun prependMissingParameterSets(
        codec: Int,
        data: ByteArray,
        parsed: ParsedAccessUnit,
        cachedVps: ByteArray?,
        cachedSps: ByteArray?,
        cachedPps: ByteArray?,
    ): ByteArray {
        val prefixVps = if (codec == BridgeConstants.CODEC_H265 && parsed.vps == null) cachedVps else null
        val prefixSps = if (parsed.sps == null) cachedSps else null
        val prefixPps = if (parsed.pps == null) cachedPps else null

        if (prefixVps == null && prefixSps == null && prefixPps == null) {
            return data
        }

        val totalLen = (prefixVps?.size ?: 0) + (prefixSps?.size ?: 0) + (prefixPps?.size ?: 0) + data.size
        val out = ByteArrayOutputStream(totalLen)
        prefixVps?.let { out.write(it) }
        prefixSps?.let { out.write(it) }
        prefixPps?.let { out.write(it) }
        out.write(data)
        return out.toByteArray()
    }

    /**
     * Builds the `csd-0` buffer for Android [android.media.MediaFormat]:
     * - H.264: SPS (with start code)
     * - H.265: VPS + SPS + PPS concatenated (each with start code)
     */
    fun buildCsd0(codec: Int, vps: ByteArray?, sps: ByteArray?, pps: ByteArray?): ByteArray? {
        return when (codec) {
            BridgeConstants.CODEC_H264 -> sps
            BridgeConstants.CODEC_H265 -> {
                if (vps == null && sps == null && pps == null) return null
                val out = ByteArrayOutputStream((vps?.size ?: 0) + (sps?.size ?: 0) + (pps?.size ?: 0))
                vps?.let { out.write(it) }
                sps?.let { out.write(it) }
                pps?.let { out.write(it) }
                out.toByteArray()
            }
            else -> null
        }
    }

    /**
     * Builds the `csd-1` buffer for Android [android.media.MediaFormat] (H.264 PPS only).
     */
    fun buildCsd1(codec: Int, pps: ByteArray?): ByteArray? {
        return if (codec == BridgeConstants.CODEC_H264) pps else null
    }

    private fun sliceNal(data: ByteArray, nal: NalUnit): ByteArray {
        return data.copyOfRange(nal.startCodeOffset, nal.startCodeOffset + nal.totalLength)
    }
}

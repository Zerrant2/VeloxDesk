package dev.veloxdesk.media

import dev.veloxdesk.bridge.BridgeConstants
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test

class AnnexBParserTest {

    @Test
    fun parsesH264SpsPpsIdrWithFourByteStartCodes() {
        val sps = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E)
        val pps = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x68, 0xCE.toByte(), 0x3C, 0x80.toByte())
        val idr = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x65, 0x88.toByte(), 0x84.toByte(), 0x21)
        val frame = sps + pps + idr

        val parsed = AnnexBParser.parseAccessUnit(BridgeConstants.CODEC_H264, frame)
        assertEquals(3, parsed.nalUnits.size)
        assertEquals(AnnexBParser.H264_NAL_SPS, parsed.nalUnits[0].h264Type)
        assertEquals(AnnexBParser.H264_NAL_PPS, parsed.nalUnits[1].h264Type)
        assertEquals(AnnexBParser.H264_NAL_IDR, parsed.nalUnits[2].h264Type)

        assertNull(parsed.vps)
        assertArrayEquals(sps, parsed.sps)
        assertArrayEquals(pps, parsed.pps)
        assertTrue(parsed.isKeyframe)
        assertTrue(parsed.hasParameterSets)

        assertArrayEquals(sps, AnnexBParser.buildCsd0(BridgeConstants.CODEC_H264, null, parsed.sps, parsed.pps))
        assertArrayEquals(pps, AnnexBParser.buildCsd1(BridgeConstants.CODEC_H264, parsed.pps))
    }

    @Test
    fun parsesH264MixedThreeAndFourByteStartCodesWithoutTrailingZeroLeak() {
        val sps = byteArrayOf(0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x28)
        val pps = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x68, 0xEB.toByte(), 0xEF.toByte())
        val slice = byteArrayOf(0x00, 0x00, 0x01, 0x41, 0x9A.toByte(), 0x01)
        val frame = sps + pps + slice

        val parsed = AnnexBParser.parseAccessUnit(BridgeConstants.CODEC_H264, frame)
        assertEquals(3, parsed.nalUnits.size)
        assertEquals(3, parsed.nalUnits[0].startCodeLength)
        assertEquals(4, parsed.nalUnits[1].startCodeLength)
        assertEquals(3, parsed.nalUnits[2].startCodeLength)

        assertArrayEquals(sps, parsed.sps)
        assertArrayEquals(pps, parsed.pps)
        assertFalse(parsed.isKeyframe)
    }

    @Test
    fun parsesH265VpsSpsPpsAndIdr() {
        // H.265 NAL header byte: type = (byte >>> 1) & 0x3F
        // VPS (32) -> 0x40, SPS (33) -> 0x42, PPS (34) -> 0x44, IDR_W_RADL (19) -> 0x26
        val vps = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0x0C, 0x01)
        val sps = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0x01, 0x01)
        val pps = byteArrayOf(0x00, 0x00, 0x01, 0x44, 0x01, 0xC1.toByte(), 0x72)
        val idr = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xAF.toByte())
        val frame = vps + sps + pps + idr

        val parsed = AnnexBParser.parseAccessUnit(BridgeConstants.CODEC_H265, frame)
        assertEquals(4, parsed.nalUnits.size)
        assertEquals(AnnexBParser.H265_NAL_VPS, parsed.nalUnits[0].h265Type)
        assertEquals(AnnexBParser.H265_NAL_SPS, parsed.nalUnits[1].h265Type)
        assertEquals(AnnexBParser.H265_NAL_PPS, parsed.nalUnits[2].h265Type)
        assertEquals(19, parsed.nalUnits[3].h265Type)

        assertArrayEquals(vps, parsed.vps)
        assertArrayEquals(sps, parsed.sps)
        assertArrayEquals(pps, parsed.pps)
        assertTrue(parsed.isKeyframe)
        assertTrue(parsed.hasParameterSets)

        val csd0 = AnnexBParser.buildCsd0(BridgeConstants.CODEC_H265, parsed.vps, parsed.sps, parsed.pps)
        assertNotNull(csd0)
        assertArrayEquals(vps + sps + pps, csd0)
        assertNull(AnnexBParser.buildCsd1(BridgeConstants.CODEC_H265, parsed.pps))
    }

    @Test
    fun parsesH265NonIdrTrailSlice() {
        // TRAIL_R (type 1) -> header 0x02
        val trail = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0x55, 0x66)
        val parsed = AnnexBParser.parseAccessUnit(BridgeConstants.CODEC_H265, trail)
        assertEquals(1, parsed.nalUnits.size)
        assertEquals(1, parsed.nalUnits[0].h265Type)
        assertFalse(parsed.isKeyframe)
        assertFalse(parsed.hasParameterSets)
    }

    @Test
    fun prependsCachedParameterSetsOnlyWhenMissing() {
        val sps = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E)
        val pps = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x68, 0xCE.toByte())
        val idrOnly = byteArrayOf(0x00, 0x00, 0x00, 0x01, 0x65, 0x88.toByte())

        val parsedIdrOnly = AnnexBParser.parseAccessUnit(BridgeConstants.CODEC_H264, idrOnly)
        val patched = AnnexBParser.prependMissingParameterSets(
            codec = BridgeConstants.CODEC_H264,
            data = idrOnly,
            parsed = parsedIdrOnly,
            cachedVps = null,
            cachedSps = sps,
            cachedPps = pps,
        )
        assertArrayEquals(sps + pps + idrOnly, patched)

        // When SPS and PPS are already inline, return original array without copying.
        val fullKeyframe = sps + pps + idrOnly
        val parsedFull = AnnexBParser.parseAccessUnit(BridgeConstants.CODEC_H264, fullKeyframe)
        val untouched = AnnexBParser.prependMissingParameterSets(
            codec = BridgeConstants.CODEC_H264,
            data = fullKeyframe,
            parsed = parsedFull,
            cachedVps = null,
            cachedSps = sps,
            cachedPps = pps,
        )
        assertSame(fullKeyframe, untouched)
    }

    @Test
    fun handlesEmptyAndMalformedBuffersSafely() {
        assertTrue(AnnexBParser.parseNalUnits(byteArrayOf()).isEmpty())
        assertTrue(AnnexBParser.parseNalUnits(byteArrayOf(0x00, 0x00, 0x01)).isEmpty())
        assertTrue(AnnexBParser.parseNalUnits(byteArrayOf(0x00, 0x00, 0x00, 0x01)).isEmpty())
        assertTrue(AnnexBParser.parseNalUnits(byteArrayOf(0x11, 0x22, 0x33, 0x44, 0x55)).isEmpty())

        val parsedMalformed = AnnexBParser.parseAccessUnit(
            BridgeConstants.CODEC_H264,
            byteArrayOf(0xFF.toByte(), 0x00, 0x00, 0x02, 0x65),
        )
        assertFalse(parsedMalformed.isKeyframe)
        assertNull(parsedMalformed.sps)
        assertNull(parsedMalformed.pps)
    }

    @Test
    fun softwareCodecNameHeuristicsDistinguishHwFromSw() {
        assertTrue(CodecCapsProber.isSoftwareCodecName("OMX.google.h264.decoder"))
        assertTrue(CodecCapsProber.isSoftwareCodecName("c2.android.avc.decoder"))
        assertTrue(CodecCapsProber.isSoftwareCodecName("c2.android.hevc.decoder"))
        assertTrue(CodecCapsProber.isSoftwareCodecName("c2.android.av1.decoder"))
        assertTrue(CodecCapsProber.isSoftwareCodecName("OMX.SEC.avc.sw.dec"))

        assertFalse(CodecCapsProber.isSoftwareCodecName("c2.qti.avc.decoder"))
        assertFalse(CodecCapsProber.isSoftwareCodecName("c2.exynos.hevc.decoder"))
        assertFalse(CodecCapsProber.isSoftwareCodecName("OMX.qcom.video.decoder.avc"))
        assertFalse(CodecCapsProber.isSoftwareCodecName("c2.mtk.av1.decoder"))
    }
}

package dev.veloxdesk.bridge

/**
 * Callback interface invoked from Rust native background threads via JNI.
 *
 * IMPORTANT: Every method here is looked up by exact JNI descriptor in `crates/android-bridge`.
 * Do NOT rename methods or change parameter types without updating both sides together.
 */
interface VeloxSessionCallback {
    // JNI: (ILjava/lang/String;)V
    fun onStatus(statusCode: Int, detail: String)

    // JNI: (Ljava/lang/String;Ljava/lang/String;ZZ)V
    fun onAuthRequired(
        hostDeviceId: String,
        hostFingerprint: String,
        allowPerm: Boolean,
        allowOtp: Boolean,
    )

    // JNI: (Ljava/lang/String;Ljava/lang/String;IIIIIILjava/lang/String;ZIIILjava/lang/String;)V
    fun onSessionConfig(
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
    )

    // JNI: (Ljava/lang/String;)V
    fun onDisplayList(displaysJson: String)

    // JNI: (IIIIJZZ[B)V
    fun onVideoFrame(
        codec: Int,
        width: Int,
        height: Int,
        frameId: Int,
        captureTsUs: Long,
        keyframe: Boolean,
        discontinuity: Boolean,
        data: ByteArray,
    )

    // JNI: (II[S)V
    fun onAudioPcm(sampleRate: Int, channels: Int, pcm16: ShortArray)

    // JNI: (FIFFFFFFFFI)V
    fun onStats(
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
    )

    // JNI: (Ljava/lang/String;)V
    fun onClipboardText(text: String)

    // JNI: (III)V
    fun onRumble(padId: Int, lowFreq: Int, highFreq: Int)

    // JNI: (JIIII[B)V
    fun onCursorShape(
        shapeId: Long,
        width: Int,
        height: Int,
        hotspotX: Int,
        hotspotY: Int,
        rgba: ByteArray,
    )

    // JNI: (IIZJ)V
    fun onCursorPosition(x: Int, y: Int, visible: Boolean, shapeId: Long)

    // JNI: (Ljava/lang/String;Z)V
    fun onClosed(reason: String, isError: Boolean)
}

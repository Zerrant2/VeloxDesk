package dev.veloxdesk.bridge

/**
 * Callback interface invoked from Rust host engine threads via JNI (`crates/android-bridge`).
 *
 * IMPORTANT: Every method here matches the frozen v0.4.0 JNI descriptor contract in
 * `crates/android-bridge`. Do NOT rename methods or change parameter types without updating both sides.
 */
interface VeloxHostCallback {
    // JNI: (Ljava/lang/String;Ljava/lang/String;I)V
    fun onHostListening(deviceId: String, otp: String, port: Int)

    // JNI: (Ljava/lang/String;)V
    fun onHostOtpRotated(otp: String)

    // JNI: (JLjava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V
    fun onHostApprovalRequested(
        requestId: Long,
        peerDeviceId: String,
        peerName: String,
        peerFingerprint: String,
        peerPubKeyHex: String,
    )

    // JNI: (Ljava/lang/String;)V
    fun onHostTrustedPeerAdded(peerJson: String)

    // JNI: (Ljava/lang/String;Ljava/lang/String;II)V
    fun onHostSessionStarted(
        peerDeviceId: String,
        peerName: String,
        requestedBitrateKbps: Int,
        requestedFps: Int,
    )

    // JNI: (Ljava/lang/String;Ljava/lang/String;)V
    fun onHostSessionEnded(peerDeviceId: String, reason: String)

    // JNI: (I)V
    fun onHostBitrateChanged(bitrateKbps: Int)

    // JNI: ()V
    fun onHostKeyframeRequested()

    // JNI: (IIIIZIIILjava/lang/String;)V
    fun onHostInputEvent(
        kind: Int,
        x: Int,
        y: Int,
        code: Int,
        down: Boolean,
        modifiers: Int,
        dx: Int,
        dy: Int,
        text: String,
    )

    // JNI: (Ljava/lang/String;)V
    fun onHostClipboardText(text: String)

    // JNI: (Ljava/lang/String;)V
    fun onHostError(message: String)

    // JNI: ()V
    fun onHostStopped()
}

# Keep all JNI bridge classes, native methods, and callbacks invoked from Rust (`libveloxdesk_android.so`).
-keep class dev.veloxdesk.bridge.** { *; }
-keep interface dev.veloxdesk.bridge.** { *; }
-keepclasseswithmembers class * {
    native <methods>;
}
-keep class * implements dev.veloxdesk.bridge.VeloxSessionCallback {
    public void onStatus(int, java.lang.String);
    public void onAuthRequired(java.lang.String, java.lang.String, boolean, boolean);
    public void onSessionConfig(java.lang.String, java.lang.String, int, int, int, int, int, int, java.lang.String, boolean, int, int, int, java.lang.String);
    public void onDisplayList(java.lang.String);
    public void onVideoFrame(int, int, int, int, long, boolean, boolean, byte[]);
    public void onAudioPcm(int, int, short[]);
    public void onStats(float, int, float, float, float, float, float, float, float, float, int);
    public void onClipboardText(java.lang.String);
    public void onRumble(int, int, int);
    public void onCursorShape(long, int, int, int, int, byte[]);
    public void onCursorPosition(int, int, boolean, long);
    public void onClosed(java.lang.String, boolean);
}
-keep class * implements dev.veloxdesk.bridge.VeloxHostCallback {
    public void onHostListening(java.lang.String, java.lang.String, int);
    public void onHostOtpRotated(java.lang.String);
    public void onHostApprovalRequested(long, java.lang.String, java.lang.String, java.lang.String, java.lang.String);
    public void onHostTrustedPeerAdded(java.lang.String);
    public void onHostSessionStarted(java.lang.String, java.lang.String, int, int);
    public void onHostSessionEnded(java.lang.String, java.lang.String);
    public void onHostBitrateChanged(int);
    public void onHostKeyframeRequested();
    public void onHostInputEvent(int, int, int, int, boolean, int, int, int, java.lang.String);
    public void onHostClipboardText(java.lang.String);
    public void onHostError(java.lang.String);
    public void onHostStopped();
}

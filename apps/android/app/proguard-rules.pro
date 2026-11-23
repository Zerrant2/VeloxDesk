# Keep all JNI bridge classes, interfaces, fields, constructors, native methods, and callbacks
# invoked from Rust (`libveloxdesk_android.so`, `crates/android-bridge/src/jni_bridge.rs`).
-keep class dev.veloxdesk.bridge.** { *; }
-keep interface dev.veloxdesk.bridge.** { *; }
-keepclassmembers class dev.veloxdesk.bridge.** {
    <init>(...);
    <fields>;
    <methods>;
}

# Preserve VeloxNative object, INSTANCE field, and all 40 @JvmStatic native entrypoints + descriptors.
-keep class dev.veloxdesk.bridge.VeloxNative {
    public static final dev.veloxdesk.bridge.VeloxNative INSTANCE;
    public static native <methods>;
    <methods>;
}
-keepclasseswithmembers,includedescriptorclasses class * {
    native <methods>;
}
-keepclasseswithmembernames,includedescriptorclasses class * {
    native <methods>;
}

# Preserve every VeloxSessionCallback implementation and all 14 JNI callback methods looked up via GetMethodID.
-keep class * implements dev.veloxdesk.bridge.VeloxSessionCallback {
    <init>(...);
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
    public void onChatMessage(long, java.lang.String, java.lang.String, long);
    public void onChatAck(long);
    public void onClosed(java.lang.String, boolean);
}
-keepclassmembers class * implements dev.veloxdesk.bridge.VeloxSessionCallback {
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
    public void onChatMessage(long, java.lang.String, java.lang.String, long);
    public void onChatAck(long);
    public void onClosed(java.lang.String, boolean);
}

# Preserve every VeloxHostCallback implementation and all 14 JNI callback methods looked up via GetMethodID.
-keep class * implements dev.veloxdesk.bridge.VeloxHostCallback {
    <init>(...);
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
    public void onHostChatMessage(java.lang.String, long, java.lang.String, java.lang.String, long);
    public void onHostChatAck(java.lang.String, long);
    public void onHostError(java.lang.String);
    public void onHostStopped();
}
-keepclassmembers class * implements dev.veloxdesk.bridge.VeloxHostCallback {
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
    public void onHostChatMessage(java.lang.String, long, java.lang.String, java.lang.String, long);
    public void onHostChatAck(java.lang.String, long);
    public void onHostError(java.lang.String);
    public void onHostStopped();
}

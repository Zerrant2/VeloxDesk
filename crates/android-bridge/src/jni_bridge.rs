//! Safe JNI gateway implementing `dev.veloxdesk.bridge.VeloxNative` and invoking
//! `dev.veloxdesk.bridge.VeloxSessionCallback` from Rust background threads.
//!
//! Safety invariants:
//! - Every `extern "system"` entrypoint is wrapped in `std::panic::catch_unwind` (guards
//!   `debug` / `test` builds where `panic = "unwind"`). Because `[profile.release]` sets
//!   `panic = "abort"`, `catch_unwind` is a zero-cost no-op in release builds; therefore the
//!   primary safety invariant is strict zero-`.unwrap()` / zero-`.expect()` discipline across
//!   every JNI boundary and untrusted input path so no panic can ever be triggered.
//! - Sessions are tracked by opaque non-zero `u64` IDs in a global [`HandleTable`]
//!   (never raw pointers), so double-disconnect and stale handles are safe no-ops.
//! - All `JMethodID`s for `VeloxSessionCallback` are cached on the Kotlin calling thread
//!   during `nativeConnect` (with the app classloader active).
//! - Background callbacks attach via `attach_current_thread_as_daemon`, execute inside
//!   `with_local_frame(16, ...)`, and clear any pending Java exception.
//! - Passwords are wrapped in [`crate::SecretString`] immediately upon crossing JNI (zeroized
//!   on drop even if a handle is stale) and are never logged.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use jni::objects::{GlobalRef, JClass, JMethodID, JObject, JString, JValue};
use jni::signature::{Primitive, ReturnType};
use jni::sys::{jboolean, jint, jlong, jsize, jstring};
use jni::{JNIEnv, JavaVM};
use veloxdesk_protocol::control::SessionConfig;

use crate::session::{ConnectConfig, HandleTable, SessionCallbacks, load_or_create_identity};
use crate::stats::HudMetrics;
use crate::{BUILD_DISPLAY, SecretString};

fn handles() -> &'static Arc<HandleTable> {
    static TABLE: OnceLock<Arc<HandleTable>> = OnceLock::new();
    TABLE.get_or_init(|| Arc::new(HandleTable::new()))
}

/// Wrapper making a cached `JMethodID` `Send + Sync`.
///
/// SAFETY: Per the JNI specification, a `jmethodID` remains valid for as long as its defining
/// class remains loaded. [`JniSessionCallbacks`] holds a `GlobalRef` to an instance of that
/// class, which keeps both the class and its classloader alive for the entire lifetime of `self`.
#[derive(Clone, Copy)]
struct CachedMethodId(JMethodID);

unsafe impl Send for CachedMethodId {}
unsafe impl Sync for CachedMethodId {}

struct JniSessionCallbacks {
    vm: JavaVM,
    callback_obj: GlobalRef,
    m_on_status: CachedMethodId,
    m_on_auth_required: CachedMethodId,
    m_on_session_config: CachedMethodId,
    m_on_display_list: CachedMethodId,
    m_on_video_frame: CachedMethodId,
    m_on_audio_pcm: CachedMethodId,
    m_on_stats: CachedMethodId,
    m_on_clipboard_text: CachedMethodId,
    m_on_rumble: CachedMethodId,
    m_on_closed: CachedMethodId,
}

impl JniSessionCallbacks {
    fn from_env(env: &mut JNIEnv<'_>, callback: &JObject<'_>) -> jni::errors::Result<Self> {
        let vm = env.get_java_vm()?;
        let callback_obj = env.new_global_ref(callback)?;
        let cls = env.get_object_class(callback)?;

        let m_on_status = CachedMethodId(env.get_method_id(&cls, "onStatus", "(ILjava/lang/String;)V")?);
        let m_on_auth_required = CachedMethodId(env.get_method_id(
            &cls,
            "onAuthRequired",
            "(Ljava/lang/String;Ljava/lang/String;ZZ)V",
        )?);
        let m_on_session_config = CachedMethodId(env.get_method_id(
            &cls,
            "onSessionConfig",
            "(Ljava/lang/String;Ljava/lang/String;IIIIIILjava/lang/String;ZIIILjava/lang/String;)V",
        )?);
        let m_on_display_list =
            CachedMethodId(env.get_method_id(&cls, "onDisplayList", "(Ljava/lang/String;)V")?);
        let m_on_video_frame =
            CachedMethodId(env.get_method_id(&cls, "onVideoFrame", "(IIIIJZZ[B)V")?);
        let m_on_audio_pcm = CachedMethodId(env.get_method_id(&cls, "onAudioPcm", "(II[S)V")?);
        let m_on_stats = CachedMethodId(env.get_method_id(&cls, "onStats", "(FIFFFFFFFFI)V")?);
        let m_on_clipboard_text =
            CachedMethodId(env.get_method_id(&cls, "onClipboardText", "(Ljava/lang/String;)V")?);
        let m_on_rumble = CachedMethodId(env.get_method_id(&cls, "onRumble", "(III)V")?);
        let m_on_closed =
            CachedMethodId(env.get_method_id(&cls, "onClosed", "(Ljava/lang/String;Z)V")?);

        Ok(Self {
            vm,
            callback_obj,
            m_on_status,
            m_on_auth_required,
            m_on_session_config,
            m_on_display_list,
            m_on_video_frame,
            m_on_audio_pcm,
            m_on_stats,
            m_on_clipboard_text,
            m_on_rumble,
            m_on_closed,
        })
    }

    fn with_env<F>(&self, op: &str, f: F)
    where
        F: FnOnce(&mut JNIEnv<'_>) -> jni::errors::Result<()>,
    {
        let mut env = match self.vm.attach_current_thread_as_daemon() {
            Ok(env) => env,
            Err(e) => {
                tracing::warn!("JNI attach failed in {op}: {e}");
                return;
            }
        };
        let res = env.with_local_frame(16, |env| {
            let call_res = f(env);
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
                tracing::warn!("cleared Java exception thrown inside callback {op}");
            }
            call_res
        });
        if let Err(e) = res {
            tracing::debug!("JNI callback {op} error: {e}");
        }
    }
}

impl SessionCallbacks for JniSessionCallbacks {
    fn on_status(&self, status_code: i32, detail: &str) {
        self.with_env("onStatus", |env| {
            let j_detail = env.new_string(detail)?;
            let args = [JValue::Int(status_code).as_jni(), JValue::Object(&j_detail).as_jni()];
            // SAFETY: method ID and signature `(ILjava/lang/String;)V` were verified on `callback_obj`'s class.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_status.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_auth_required(&self, host_device_id: &str, host_fingerprint: &str, allow_perm: bool, allow_otp: bool) {
        self.with_env("onAuthRequired", |env| {
            let j_id = env.new_string(host_device_id)?;
            let j_fp = env.new_string(host_fingerprint)?;
            let args = [
                JValue::Object(&j_id).as_jni(),
                JValue::Object(&j_fp).as_jni(),
                JValue::Bool(jboolean::from(allow_perm)).as_jni(),
                JValue::Bool(jboolean::from(allow_otp)).as_jni(),
            ];
            // SAFETY: method ID and signature `(Ljava/lang/String;Ljava/lang/String;ZZ)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_auth_required.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_session_config(&self, cfg: &SessionConfig, displays_json: &str) {
        self.with_env("onSessionConfig", |env| {
            let j_host_name = env.new_string(&cfg.host_name)?;
            let j_host_os = env.new_string(&cfg.host_os)?;
            let (codec, width, height, fps, encoder_name, hw_encoder) = match &cfg.video {
                Some(v) => (v.codec, v.width as i32, v.height as i32, v.fps as i32, v.encoder_name.as_str(), v.hw_encoder),
                None => (0, 0, 0, 0, "", false),
            };
            let (audio_rate, audio_ch, audio_frame_us) = match &cfg.audio {
                Some(a) => (a.sample_rate as i32, a.channels as i32, a.frame_duration_us as i32),
                None => (0, 0, 0),
            };
            let j_encoder = env.new_string(encoder_name)?;
            let j_displays = env.new_string(displays_json)?;

            let args = [
                JValue::Object(&j_host_name).as_jni(),
                JValue::Object(&j_host_os).as_jni(),
                JValue::Int(cfg.mode).as_jni(),
                JValue::Int(cfg.active_display as i32).as_jni(),
                JValue::Int(codec).as_jni(),
                JValue::Int(width).as_jni(),
                JValue::Int(height).as_jni(),
                JValue::Int(fps).as_jni(),
                JValue::Object(&j_encoder).as_jni(),
                JValue::Bool(jboolean::from(hw_encoder)).as_jni(),
                JValue::Int(audio_rate).as_jni(),
                JValue::Int(audio_ch).as_jni(),
                JValue::Int(audio_frame_us).as_jni(),
                JValue::Object(&j_displays).as_jni(),
            ];
            // SAFETY: method ID and 14-arg signature match `VeloxSessionCallback.onSessionConfig`.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_session_config.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_display_list(&self, displays_json: &str) {
        self.with_env("onDisplayList", |env| {
            let j_displays = env.new_string(displays_json)?;
            let args = [JValue::Object(&j_displays).as_jni()];
            // SAFETY: method ID and signature `(Ljava/lang/String;)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_display_list.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_video_frame(
        &self,
        codec: i32,
        width: u32,
        height: u32,
        frame_id: u32,
        capture_ts_us: i64,
        keyframe: bool,
        discontinuity: bool,
        data: &[u8],
    ) {
        self.with_env("onVideoFrame", |env| {
            let j_data = env.byte_array_from_slice(data)?;
            let args = [
                JValue::Int(codec).as_jni(),
                JValue::Int(width as i32).as_jni(),
                JValue::Int(height as i32).as_jni(),
                JValue::Int(frame_id as i32).as_jni(),
                JValue::Long(capture_ts_us).as_jni(),
                JValue::Bool(jboolean::from(keyframe)).as_jni(),
                JValue::Bool(jboolean::from(discontinuity)).as_jni(),
                JValue::Object(&j_data).as_jni(),
            ];
            // SAFETY: method ID and signature `(IIIIJZZ[B)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_video_frame.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_audio_pcm(&self, sample_rate: i32, channels: i32, pcm16: &[i16]) {
        self.with_env("onAudioPcm", |env| {
            let Ok(len) = jsize::try_from(pcm16.len()) else {
                return Ok(());
            };
            let j_pcm = env.new_short_array(len)?;
            env.set_short_array_region(&j_pcm, 0, pcm16)?;
            let args = [
                JValue::Int(sample_rate).as_jni(),
                JValue::Int(channels).as_jni(),
                JValue::Object(&j_pcm).as_jni(),
            ];
            // SAFETY: method ID and signature `(II[S)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_audio_pcm.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_stats(&self, stats: &HudMetrics) {
        self.with_env("onStats", |env| {
            let args = [
                JValue::Float(stats.fps).as_jni(),
                JValue::Int(stats.bitrate_kbps).as_jni(),
                JValue::Float(stats.rtt_ms).as_jni(),
                JValue::Float(stats.loss_pct).as_jni(),
                JValue::Float(stats.fec_pct).as_jni(),
                JValue::Float(stats.capture_ms).as_jni(),
                JValue::Float(stats.encode_ms).as_jni(),
                JValue::Float(stats.net_ms).as_jni(),
                JValue::Float(stats.decode_ms).as_jni(),
                JValue::Float(stats.total_ms).as_jni(),
                JValue::Int(stats.path_kind).as_jni(),
            ];
            // SAFETY: method ID and signature `(FIFFFFFFFFI)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_stats.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_clipboard_text(&self, text: &str) {
        self.with_env("onClipboardText", |env| {
            let j_text = env.new_string(text)?;
            let args = [JValue::Object(&j_text).as_jni()];
            // SAFETY: method ID and signature `(Ljava/lang/String;)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_clipboard_text.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_rumble(&self, pad_id: i32, low_freq: i32, high_freq: i32) {
        self.with_env("onRumble", |env| {
            let args = [
                JValue::Int(pad_id).as_jni(),
                JValue::Int(low_freq).as_jni(),
                JValue::Int(high_freq).as_jni(),
            ];
            // SAFETY: method ID and signature `(III)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_rumble.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }

    fn on_closed(&self, reason: &str, is_error: bool) {
        self.with_env("onClosed", |env| {
            let j_reason = env.new_string(reason)?;
            let args = [
                JValue::Object(&j_reason).as_jni(),
                JValue::Bool(jboolean::from(is_error)).as_jni(),
            ];
            // SAFETY: method ID and signature `(Ljava/lang/String;Z)V` match.
            unsafe {
                env.call_method_unchecked(
                    &self.callback_obj,
                    self.m_on_closed.0,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )?;
            }
            Ok(())
        });
    }
}

fn jstring_to_rust(env: &mut JNIEnv<'_>, s: &JString<'_>) -> Option<String> {
    if s.is_null() {
        return None;
    }
    match env.get_string(s) {
        Ok(js) => Some(js.into()),
        Err(_) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            None
        }
    }
}

fn rust_to_jstring(env: &mut JNIEnv<'_>, s: &str) -> jstring {
    match env.new_string(s) {
        Ok(js) => js.into_raw(),
        Err(_) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            std::ptr::null_mut()
        }
    }
}

fn handle_id_from_jlong(handle: jlong) -> Option<u64> {
    u64::try_from(handle).ok().filter(|&id| id != 0)
}

// ───────────────────────── Exported JNI Entrypoints ─────────────────────────

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeInitLogging<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    let _ = catch_unwind(|| {
        #[cfg(target_os = "android")]
        {
            android_logger::init_once(
                android_logger::Config::default()
                    .with_max_level(log::LevelFilter::Info)
                    .with_tag("VeloxDeskNative"),
            );
            log::info!("VeloxDesk Android native bridge initialized ({BUILD_DISPLAY})");
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeGetBuildDisplay<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    catch_unwind(AssertUnwindSafe(|| rust_to_jstring(&mut env, BUILD_DISPLAY))).unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeInitIdentity<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    files_dir: JString<'local>,
) -> jstring {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(dir) = jstring_to_rust(&mut env, &files_dir) else {
            return rust_to_jstring(&mut env, "");
        };
        let out = match load_or_create_identity(&PathBuf::from(dir)) {
            Ok(id) => id.device_id(),
            Err(e) => {
                tracing::error!("nativeInitIdentity failed: {e:#}");
                String::new()
            }
        };
        rust_to_jstring(&mut env, &out)
    }))
    .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeGetFingerprint<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    files_dir: JString<'local>,
) -> jstring {
    catch_unwind(AssertUnwindSafe(|| {
        let Some(dir) = jstring_to_rust(&mut env, &files_dir) else {
            return rust_to_jstring(&mut env, "");
        };
        let out = match load_or_create_identity(&PathBuf::from(dir)) {
            Ok(id) => id.fingerprint(),
            Err(e) => {
                tracing::error!("nativeGetFingerprint failed: {e:#}");
                String::new()
            }
        };
        rust_to_jstring(&mut env, &out)
    }))
    .unwrap_or(std::ptr::null_mut())
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeConnect<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    files_dir: JString<'local>,
    target: JString<'local>,
    rendezvous_addr: JString<'local>,
    relay_only: jboolean,
    password: JString<'local>,
    mode: jint,
    max_bitrate_kbps: jint,
    audio_enabled: jboolean,
    clipboard_enabled: jboolean,
    viewer_width: jint,
    viewer_height: jint,
    viewer_refresh_hz: jint,
    device_name: JString<'local>,
    codec_caps_mask: jint,
    callback: JObject<'local>,
) -> jlong {
    catch_unwind(AssertUnwindSafe(|| {
        if callback.is_null() {
            return 0;
        }
        let callbacks = match JniSessionCallbacks::from_env(&mut env, &callback) {
            Ok(cb) => Arc::new(cb),
            Err(e) => {
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_clear();
                }
                tracing::error!("nativeConnect: failed to bind VeloxSessionCallback: {e}");
                return 0;
            }
        };

        let Some(files_dir_str) = jstring_to_rust(&mut env, &files_dir) else {
            callbacks.on_closed("Missing filesDir", true);
            return 0;
        };
        let Some(target_str) = jstring_to_rust(&mut env, &target) else {
            callbacks.on_closed("Missing target", true);
            return 0;
        };
        let rendezvous_str = jstring_to_rust(&mut env, &rendezvous_addr).unwrap_or_default();
        let password_opt = jstring_to_rust(&mut env, &password)
            .map(SecretString::new)
            .filter(|s| !s.is_empty());
        let device_name_str = jstring_to_rust(&mut env, &device_name).unwrap_or_default();

        let cfg = ConnectConfig {
            files_dir: PathBuf::from(files_dir_str),
            target: target_str,
            rendezvous_addr: rendezvous_str,
            relay_only: relay_only != 0,
            password: password_opt,
            mode,
            max_bitrate_kbps: max_bitrate_kbps.max(0) as u32,
            audio_enabled: audio_enabled != 0,
            clipboard_enabled: clipboard_enabled != 0,
            viewer_width: viewer_width.max(0) as u32,
            viewer_height: viewer_height.max(0) as u32,
            viewer_refresh_hz: viewer_refresh_hz.max(0) as u32,
            device_name: device_name_str,
            codec_caps_mask: codec_caps_mask as u32,
        };

        match handles().spawn_session(cfg, callbacks.clone()) {
            Ok(id) => id as jlong,
            Err(e) => {
                callbacks.on_closed(&format!("Failed to start session: {e}"), true);
                0
            }
        }
    }))
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSubmitPassword<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    password: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(pw) = jstring_to_rust(&mut env, &password).map(SecretString::new) else { return };
        let Some(id) = handle_id_from_jlong(handle) else { return };
        if let Some(h) = handles().get(id) {
            h.submit_password(pw);
        }
    }));
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeCancelAuth<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.cancel_auth();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeDisconnect<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().remove(id)
        {
            h.disconnect();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeRequestKeyframe<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.request_keyframe();
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeReportDecodeTimeUs<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    decode_us: jint,
    render_us: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.report_decode_time_us(decode_us.max(0) as u32, render_us.max(0) as u32);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSetMaxBitrate<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    bitrate_kbps: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.set_max_bitrate(bitrate_kbps.max(0) as u32);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSwitchDisplay<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    display_index: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.switch_display(display_index.max(0) as u32);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSetAudioEnabled<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    enabled: jboolean,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.set_audio_enabled(enabled != 0);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSetClipboardEnabled<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    enabled: jboolean,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.set_clipboard_enabled(enabled != 0);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendMouseMove<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    x: jint,
    y: jint,
    display: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.send_mouse_move(x, y, display.max(0) as u32);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendMouseRelative<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    dx: jint,
    dy: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.send_mouse_relative(dx, dy);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendMouseButton<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    button: jint,
    down: jboolean,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.send_mouse_button(button, down != 0);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendMouseWheel<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    dx: jint,
    dy: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.send_mouse_wheel(dx, dy);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendKey<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    hid_usage: jint,
    down: jboolean,
    modifiers: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.send_key(hid_usage.max(0) as u32, down != 0, modifiers.max(0) as u32);
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendText<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    text: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(id) = handle_id_from_jlong(handle) else { return };
        let Some(s) = jstring_to_rust(&mut env, &text) else { return };
        if let Some(h) = handles().get(id) {
            h.send_text(s);
        }
    }));
}

#[unsafe(no_mangle)]
#[allow(clippy::too_many_arguments)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendGamepad<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    pad_id: jint,
    connected: jboolean,
    buttons: jint,
    left_trigger: jint,
    right_trigger: jint,
    left_x: jint,
    left_y: jint,
    right_x: jint,
    right_y: jint,
) {
    let _ = catch_unwind(|| {
        if let Some(id) = handle_id_from_jlong(handle)
            && let Some(h) = handles().get(id)
        {
            h.send_gamepad(
                pad_id.max(0) as u32,
                connected != 0,
                buttons as u32,
                left_trigger.max(0) as u32,
                right_trigger.max(0) as u32,
                left_x,
                left_y,
                right_x,
                right_y,
            );
        }
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_veloxdesk_bridge_VeloxNative_nativeSendClipboardText<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    text: JString<'local>,
) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        let Some(id) = handle_id_from_jlong(handle) else { return };
        let Some(s) = jstring_to_rust(&mut env, &text) else { return };
        if let Some(h) = handles().get(id) {
            h.send_clipboard_text(s);
        }
    }));
}

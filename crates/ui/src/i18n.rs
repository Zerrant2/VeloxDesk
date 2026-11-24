//! EN/RU localization catalog and dynamic formatting helpers for VeloxDesk UI and client.

use std::fmt::Display;
use std::net::SocketAddr;

use veloxdesk_protocol::control::DisplayInfo;

use crate::settings::Language;

macro_rules! define_strings {
    ($($field:ident),* $(,)?) => {
        /// Static UI strings for a single [`Language`].
        #[derive(Debug, Clone, Copy)]
        pub struct Strings {
            $(pub $field: &'static str,)*
        }

        impl Strings {
            /// Every `(field_name, value)` pair in this translation table (used by completeness tests).
            pub fn all_fields(&self) -> Vec<(&'static str, &'static str)> {
                vec![$((stringify!($field), self.$field),)*]
            }
        }
    };
}

define_strings!(
    // Common / Language
    app_title,
    lang_switch_tooltip,
    refresh_btn,
    copy_btn,
    hide_btn,
    show_btn,
    connect_btn,
    cancel_btn,
    settings_btn,
    remove_tooltip,

    // Enums (settings.rs)
    mode_desktop,
    mode_gaming,
    quality_low,
    quality_balanced,
    quality_high,
    quality_ultra,
    present_mailbox,
    present_immediate,
    present_fifo,
    scale_fit,
    scale_stretch,
    scale_original,
    fps_60,
    fps_120,
    fps_144,
    fps_240,

    // ConnectScreen (connect.rs)
    exit_fullscreen_btn,
    exit_fullscreen_tooltip,
    this_desk_heading,
    status_ready,
    status_offline,
    this_desk_subtitle,
    device_id_label,
    unavailable,
    copy_id_btn,
    device_id_copied,
    tailscale_ip_label,
    tailscale_ip_copied,
    otp_label,
    otp_refresh_tooltip,
    otp_copied,
    host_listening,
    host_stopped,
    host_starting,
    stop_hosting_btn,
    start_hosting_btn,
    require_accept_label,
    require_accept_tooltip,
    remote_desk_heading,
    remote_desk_subtitle,
    remote_address_hint,
    mode_colon_label,
    recent_connections_heading,
    tailscale_devices_heading,
    tailscale_refresh_tooltip,
    tailscale_install_hint,
    tailscale_download_btn,
    tailscale_not_connected,
    tailscale_start_btn,
    tailscale_no_peers,
    err_enter_address,
    err_enter_target,

    // SessionToolbar (session.rs)
    session_toolbar_tooltip,
    collapse_toolbar_tooltip,
    pointer_locked_pill,
    display_primary_suffix,
    toolbar_audio,
    toolbar_audio_tooltip,
    toolbar_mic,
    toolbar_mic_tooltip,
    toolbar_locked,
    toolbar_lock,
    toolbar_lock_cursor_btn,
    toolbar_unlock_cursor_btn,
    toolbar_lock_tooltip,
    toolbar_files,
    toolbar_term,
    toolbar_tunnels,
    toolbar_privacy,
    toolbar_privacy_tooltip,
    toolbar_clip,
    toolbar_clip_tooltip,
    toolbar_hud,
    toolbar_fullscreen,
    toolbar_disconnect,

    // settings_window_at (session.rs)
    settings_title,
    settings_language_label,
    settings_default_mode,
    settings_editing_profile,
    settings_quality,
    settings_target_fps,
    settings_color_hdr,
    settings_hdr_10bit,
    settings_yuv444,
    settings_audio,
    settings_remote_audio,
    settings_microphone,
    settings_presentation,
    settings_scaling,
    settings_perf_hud,
    settings_hud_show_on_connect,
    settings_decoder,
    settings_prefer_hw_decode,
    settings_gaming_mouse,
    settings_relative_mouse,
    settings_clipboard,
    settings_clipboard_sync,
    settings_rendezvous_server,

    // R5 settings_window_at hover/hold tooltips (17 targets across 14 rows)
    tip_language,
    tip_default_mode,
    tip_editing_profile,
    tip_quality,
    tip_target_fps,
    tip_color_hdr,
    tip_hdr_10bit,
    tip_yuv444,
    tip_audio,
    tip_remote_audio,
    tip_microphone,
    tip_presentation,
    tip_scaling,
    tip_perf_hud,
    tip_decoder,
    tip_gaming_mouse,
    tip_clipboard,
    tip_rendezvous_server,

    // PasswordPrompt & AcceptPrompt (auth.rs)
    auth_required_title,
    enter_password_hint,
    auth_methods_both,
    auth_methods_perm,
    auth_methods_otp,
    incoming_connection_title,
    unknown_device,
    granted_permissions_heading,
    perm_view,
    perm_input,
    perm_clipboard,
    perm_file_transfer,
    perm_audio,
    perm_tunnel,
    perm_terminal,
    perm_privacy_mode,
    perm_restart,
    trust_device_checkbox,
    accept_btn,
    reject_btn,

    // FileManagerWindow (files.rs)
    files_title,
    files_up_btn,
    files_col_name,
    files_col_type,
    files_col_size,
    files_type_dir,
    files_type_file,
    files_local_path_label,
    files_local_path_hint,
    files_resume_cb,
    files_download_btn,
    files_upload_btn,
    files_new_name_hint,
    files_mkdir_btn,
    files_rename_btn,
    files_delete_btn,
    unit_b,
    unit_kib,
    unit_mib,
    unit_gib,

    // TerminalWindow (terminal.rs)
    terminal_title,
    terminal_shell_label,
    terminal_shell_hint,
    terminal_open_btn,
    terminal_close_btn,
    terminal_size_label,
    terminal_resize_btn,
    terminal_clear_btn,
    terminal_input_hint,
    terminal_send_btn,

    // TunnelsWindow (tunnels.rs)
    tunnels_title,
    tunnels_local_port_label,
    tunnels_add_btn,
    tunnels_empty,
    tunnels_col_local,
    tunnels_col_remote,
    tunnels_stop_btn,
    err_tunnel_empty_host,
    err_tunnel_zero_port,

    // Performance HUD (crates/render/src/hud.rs)
    hud_capture,
    hud_encode,
    hud_net,
    hud_rtt,
    hud_decode,
    hud_render,
    hud_total,
    hud_ms,
    hud_fps,
    hud_mbps,
    hud_loss,
    hud_fec_rec,
    hud_decode_prefix,
    hud_zero_copy,
    hud_yes,
    hud_no,
    hud_path_unknown,
    hud_path_direct,
    hud_path_p2p,
    hud_path_relay,
    hud_pointer_locked,

    // apps/client (tray, tailscale, app, viewer, embedded_host)
    tray_show_window,
    tray_copy_id,
    tray_rotate_otp,
    tray_quit,
    ts_needs_login,
    ts_stopped,
    ts_starting,
    ts_daemon_not_running,
    ts_not_running,
    ts_not_installed,
    waiting_for_video,
    authenticating,
    waiting_for_host,
    waiting_for_host_accept,
    host_peer_viewer,
    err_no_rendezvous_for_id,
    err_no_rendezvous_configured,
    err_net_feature_disabled,
    err_embedded_host_requires_net,
    err_open_control_timeout,
    err_host_closed_connection,
    err_host_closed_control,
    err_auth_pk_len,
    err_auth_pk_mismatch,
    err_auth_device_id_mismatch,
    err_kdf_panicked,
    err_handshake_timeout,
    err_reading_handshake,
    err_sending_auth_response,
    err_auth_rejected,
    host_reject_ui_unavailable,
    host_reject_by_user,
    host_reject_prompt_closed,

    // R6 Version / Build ID & R7 Auto-Updater
    settings_version_label,
    tip_version,
    settings_updates_label,
    tip_updates,
    update_check_btn,
    update_checking,
    update_up_to_date,
    update_now_btn,
    update_later_btn,
    update_retry_btn,
    update_dismiss_btn,
    update_release_notes_btn,
    update_downloading,
    update_applying,

    // v0.3.0 Host Access & Security (Permanent Password, Password Modes, Trusted Devices)
    pw_mode_onetime,
    pw_mode_permanent,
    pw_mode_both,
    settings_security_heading,
    settings_password_mode,
    tip_password_mode,
    pw_mode_permanent_fallback_hint,
    settings_permanent_password,
    tip_permanent_password,
    pw_status_set,
    pw_status_not_set,
    pw_new_placeholder,
    pw_save_btn,
    pw_clear_btn,
    tip_pw_save,
    tip_pw_clear,
    pw_too_short_err,
    pw_saving_status,
    pw_saved_notice,
    pw_cleared_notice,
    settings_require_accept,
    tip_settings_require_accept,
    settings_trusted_devices,
    tip_trusted_devices,
    trusted_devices_empty,
    trusted_remove_tooltip,
    connect_permanent_active,
    connect_plus_permanent_badge,

    // v0.4.0 Themes, OTP Auto-CAPS, Android Host Navigation
    theme_system,
    theme_dark,
    theme_light,
    settings_theme_label,
    tip_theme,
    theme_switch_tooltip,
    caps_btn,
    tip_caps,
    toolbar_android_back,
    toolbar_android_back_tooltip,
    toolbar_android_home,
    toolbar_android_home_tooltip,
    toolbar_android_recents,
    toolbar_android_recents_tooltip,

    // v0.5.0 Echo Cancellation, Recording, Chat, Remote Power, Auto-Reconnect
    perm_chat,
    perm_power,
    settings_echo_cancellation,
    settings_echo_cancellation_cb,
    tip_echo_cancellation,
    tip_echo_cancellation_gaming,
    settings_recordings_dir,
    settings_recordings_dir_hint,
    tip_recordings_dir,
    toolbar_chat,
    toolbar_chat_tooltip,
    toolbar_rec,
    toolbar_rec_start_tooltip,
    toolbar_rec_stop_tooltip,
    toolbar_rec_waiting,
    toolbar_power,
    toolbar_power_tooltip,
    toolbar_power_denied_tooltip,
    chat_title,
    host_chat_title,
    chat_empty,
    chat_input_hint,
    chat_send_btn,
    chat_disabled_notice,
    chat_delivered_tooltip,
    chat_sending_tooltip,
    host_chat_btn,
    host_chat_tooltip,
    power_reboot,
    power_reboot_safe_mode,
    power_lock_screen,
    power_sleep,
    power_confirm_title,
    power_confirm_btn,
    power_warn_reboot,
    power_warn_reboot_safe_mode,
    power_warn_lock_screen,
    power_warn_sleep,
    power_reboot_hint_reconnect,
    reconnect_title,
    reconnect_timed_out,

    // v0.6.0 Deep Profile Defaults, Audio & Voice, Recording & Session, Host Policy
    audio_latency_auto,
    audio_latency_2_5ms,
    audio_latency_5ms,
    audio_latency_10ms,
    audio_latency_20ms,
    privacy_mode_curtain,
    privacy_mode_vdd,
    settings_reset_profile_btn,
    tip_reset_profile,
    settings_audio_heading,
    settings_noise_suppression,
    tip_noise_suppression,
    settings_audio_frame_latency,
    tip_audio_frame_latency,
    settings_recording_heading,
    settings_open_folder_btn,
    tip_open_folder,
    settings_auto_reconnect,
    settings_auto_reconnect_cb,
    tip_auto_reconnect,
    settings_host_permissions,
    tip_host_permissions,
    host_perm_input,
    host_perm_clipboard,
    host_perm_file_transfer,
    host_perm_audio,
    host_perm_chat,
    host_perm_terminal_tunnel,
    host_perm_power,
    settings_host_echo_exclusion,
    settings_host_echo_exclusion_cb,
    tip_host_echo_exclusion,
    settings_host_privacy_mode,
    tip_host_privacy_mode,

    // v0.6.2 Remote Microphone / Discord Routing & Built-in VB-CABLE Installer
    settings_host_mic_routing_heading,
    tip_host_mic_routing,
    settings_host_mic_output_label,
    host_mic_output_auto,
    host_mic_output_speakers,
    host_mic_refresh_tooltip,
    host_mic_missing_warn,
    vbcable_install_btn,
    vbcable_reinstall_btn,
    tip_vbcable_install,
    vbcable_downloading,
    vbcable_extracting,
    vbcable_waiting_uac,
    vbcable_cancelled,
    vbcable_post_install_hint,
);

pub const EN: Strings = Strings {
    // Common / Language
    app_title: "VeloxDesk",
    lang_switch_tooltip: "Switch interface language (English / Русский)",
    refresh_btn: "Refresh",
    copy_btn: "Copy",
    hide_btn: "Hide",
    show_btn: "Show",
    connect_btn: "Connect",
    cancel_btn: "Cancel",
    settings_btn: "Settings",
    remove_tooltip: "Remove",

    // Enums
    mode_desktop: "Desktop",
    mode_gaming: "Gaming",
    quality_low: "Low (5 Mb/s)",
    quality_balanced: "Balanced (20 Mb/s)",
    quality_high: "High (50 Mb/s)",
    quality_ultra: "Ultra (100 Mb/s)",
    present_mailbox: "Mailbox (no tearing)",
    present_immediate: "Immediate (VSync off)",
    present_fifo: "VSync",
    scale_fit: "Fit",
    scale_stretch: "Stretch",
    scale_original: "Original (1:1)",
    fps_60: "60 FPS",
    fps_120: "120 FPS",
    fps_144: "144 FPS",
    fps_240: "240 FPS",

    // ConnectScreen
    exit_fullscreen_btn: "Exit Fullscreen (Ctrl+Shift+F)",
    exit_fullscreen_tooltip: "Restore windowed mode and titlebar controls",
    this_desk_heading: "This Desk",
    status_ready: "Ready",
    status_offline: "Offline",
    this_desk_subtitle: "Share your Device ID and one-time password to allow remote access.",
    device_id_label: "Device ID",
    unavailable: "Unavailable",
    copy_id_btn: "Copy ID",
    device_id_copied: "Device ID copied",
    tailscale_ip_label: "Tailscale IP",
    tailscale_ip_copied: "Tailscale IP copied",
    otp_label: "One-Time Password",
    otp_refresh_tooltip: "Generate a new one-time password",
    otp_copied: "OTP copied",
    host_listening: "Listening",
    host_stopped: "Stopped",
    host_starting: "Starting…",
    stop_hosting_btn: "Stop Hosting",
    start_hosting_btn: "Start Hosting",
    require_accept_label: "Require Accept Dialog",
    require_accept_tooltip: "Prompt to accept or reject incoming connections with per-session permissions",
    remote_desk_heading: "Control Remote Desk",
    remote_desk_subtitle: "Enter a 9-digit Device ID or direct host address (ip:port).",
    remote_address_hint: "123 456 789 or 192.168.1.20:21118",
    mode_colon_label: "Mode:",
    recent_connections_heading: "Recent Connections",
    tailscale_devices_heading: "Tailscale Devices",
    tailscale_refresh_tooltip: "Query tailscale status",
    tailscale_install_hint: "No white IP? Install free Tailscale for 1-click P2P",
    tailscale_download_btn: "Download Tailscale",
    tailscale_not_connected: "Tailscale is not connected",
    tailscale_start_btn: "Start / Login Tailscale",
    tailscale_no_peers: "Connected to Tailscale — no other devices found yet.",
    err_enter_address: "enter an address",
    err_enter_target: "enter a 9-digit Device ID or address",

    // SessionToolbar
    session_toolbar_tooltip: "Session toolbar",
    collapse_toolbar_tooltip: "Collapse toolbar",
    pointer_locked_pill: "Locked (Ctrl+Alt+G)",
    display_primary_suffix: ", primary",
    toolbar_audio: "Audio",
    toolbar_audio_tooltip: "Toggle remote audio playback",
    toolbar_mic: "Mic",
    toolbar_mic_tooltip: "Toggle microphone pass-through",
    toolbar_locked: "Locked",
    toolbar_lock: "Lock",
    toolbar_lock_cursor_btn: "Lock Cursor (Ctrl+Alt+G)",
    toolbar_unlock_cursor_btn: "Unlock Cursor (Ctrl+Alt+G)",
    toolbar_lock_tooltip: "Pointer Lock (Ctrl+Alt+G)",
    toolbar_files: "Files",
    toolbar_term: "Term",
    toolbar_tunnels: "Tunnels",
    toolbar_privacy: "Privacy",
    toolbar_privacy_tooltip: "Blank host monitor (Privacy Mode)",
    toolbar_clip: "Clip",
    toolbar_clip_tooltip: "Toggle clipboard synchronization",
    toolbar_hud: "HUD",
    toolbar_fullscreen: "Fullscreen",
    toolbar_disconnect: "Disconnect",

    // settings_window_at
    settings_title: "Settings",
    settings_language_label: "Language",
    settings_default_mode: "Default mode",
    settings_editing_profile: "Editing profile",
    settings_quality: "Quality",
    settings_target_fps: "Target FPS",
    settings_color_hdr: "Color / HDR",
    settings_hdr_10bit: "10-bit HDR",
    settings_yuv444: "YUV 4:4:4",
    settings_audio: "Audio",
    settings_remote_audio: "Remote audio",
    settings_microphone: "Microphone",
    settings_presentation: "Presentation",
    settings_scaling: "Scaling",
    settings_perf_hud: "Performance HUD",
    settings_hud_show_on_connect: "Show on connect (Ctrl+Shift+H)",
    settings_decoder: "Decoder",
    settings_prefer_hw_decode: "Prefer hardware decoding",
    settings_gaming_mouse: "Gaming mouse",
    settings_relative_mouse: "Pointer Lock (Ctrl+Alt+G), relative motion",
    settings_clipboard: "Clipboard",
    settings_clipboard_sync: "Sync clipboard (text, images, files)",
    settings_rendezvous_server: "Rendezvous server",

    // R5 tooltips
    tip_language: "Switch the client interface language between English and Russian (saved across launches).",
    tip_default_mode: "Choose the default session mode: Desktop for standard cursor control, or Gaming for relative mouse lock and gamepads.",
    tip_editing_profile: "Select which mode's independent streaming profile (Desktop or Gaming) you are configuring below.",
    tip_quality: "Maximum video bitrate ceiling (5–100 Mb/s). Higher values improve image sharpness at the cost of network bandwidth.",
    tip_target_fps: "Target frame rate cap (60–240 FPS) requested from the remote host (updates live during active sessions).",
    tip_color_hdr: "Color depth and chroma subsampling options for the active streaming profile.",
    tip_hdr_10bit: "Request 10-bit HDR (P010 / HEVC Main10 / AV1 10-bit) when both host and client GPU support it.",
    tip_yuv444: "Request full-chroma YUV 4:4:4 encoding for razor-sharp small text and UI edges (uses more bandwidth).",
    tip_audio: "Remote sound playback and local microphone pass-through settings for the active profile.",
    tip_remote_audio: "Play incoming remote desktop audio (low-latency Opus).",
    tip_microphone: "Stream your local microphone audio to the remote host.",
    tip_presentation: "Swapchain frame presentation mode: Mailbox (low latency, no tearing), Immediate (lowest latency, VSync off), or VSync (FIFO).",
    tip_scaling: "How remote video fits the client window: Fit (preserve aspect ratio), Stretch (fill window), or Original (1:1 pixel-exact).",
    tip_perf_hud: "Automatically show the per-stage latency and FPS Performance HUD overlay on connect (toggle anytime with Ctrl+Shift+H).",
    tip_decoder: "Prefer GPU hardware video decoding (DX11/NVDEC); uncheck to force CPU software decoding (applies on reconnect).",
    tip_gaming_mouse: "In Gaming mode, lock the pointer (Ctrl+Alt+G) and send raw relative mouse movement for 3D games.",
    tip_clipboard: "Synchronize clipboard contents (text, images, and file lists) between your local computer and the remote host.",
    tip_rendezvous_server: "Address (host[:port], default port 21116) of the Rendezvous server used for 9-digit Device ID discovery and P2P/relay.",

    // PasswordPrompt & AcceptPrompt
    auth_required_title: "Authentication Required",
    enter_password_hint: "Enter password",
    auth_methods_both: "Enter permanent or one-time password",
    auth_methods_perm: "Enter permanent password",
    auth_methods_otp: "Enter one-time password",
    incoming_connection_title: "Incoming Connection",
    unknown_device: "Unknown device",
    granted_permissions_heading: "Granted permissions",
    perm_view: "View screen",
    perm_input: "Keyboard & mouse",
    perm_clipboard: "Clipboard",
    perm_file_transfer: "File transfer",
    perm_audio: "Audio",
    perm_tunnel: "TCP tunnels",
    perm_terminal: "Terminal",
    perm_privacy_mode: "Privacy mode",
    perm_restart: "Restart host",
    trust_device_checkbox: "Trust this device (allow passwordless reconnect)",
    accept_btn: "Accept",
    reject_btn: "Reject",

    // FileManagerWindow
    files_title: "File Manager",
    files_up_btn: "Up",
    files_col_name: "Name",
    files_col_type: "Type",
    files_col_size: "Size",
    files_type_dir: "Dir",
    files_type_file: "File",
    files_local_path_label: "Local path:",
    files_local_path_hint: "C:\\path\\to\\file",
    files_resume_cb: "Resume",
    files_download_btn: "Download",
    files_upload_btn: "Upload",
    files_new_name_hint: "Folder or new name",
    files_mkdir_btn: "Mkdir",
    files_rename_btn: "Rename",
    files_delete_btn: "Delete",
    unit_b: "B",
    unit_kib: "KiB",
    unit_mib: "MiB",
    unit_gib: "GiB",

    // TerminalWindow
    terminal_title: "Remote Terminal",
    terminal_shell_label: "Shell:",
    terminal_shell_hint: "default",
    terminal_open_btn: "Open",
    terminal_close_btn: "Close",
    terminal_size_label: "Size:",
    terminal_resize_btn: "Resize",
    terminal_clear_btn: "Clear",
    terminal_input_hint: "Type command and press Enter",
    terminal_send_btn: "Send",

    // TunnelsWindow
    tunnels_title: "TCP Port Forwarding",
    tunnels_local_port_label: "Local port:",
    tunnels_add_btn: "Add",
    tunnels_empty: "No active port forwards.",
    tunnels_col_local: "Local",
    tunnels_col_remote: "Remote",
    tunnels_stop_btn: "Stop",
    err_tunnel_empty_host: "remote host must not be empty",
    err_tunnel_zero_port: "remote port must be > 0",

    // HUD
    hud_capture: "Capture",
    hud_encode: "Encode",
    hud_net: "Net",
    hud_rtt: "RTT",
    hud_decode: "Decode",
    hud_render: "Render",
    hud_total: "Total",
    hud_ms: "ms",
    hud_fps: "fps",
    hud_mbps: "Mb/s",
    hud_loss: "loss",
    hud_fec_rec: "FEC rec",
    hud_decode_prefix: "decode",
    hud_zero_copy: "zero-copy",
    hud_yes: "yes",
    hud_no: "no",
    hud_path_unknown: "Unknown",
    hud_path_direct: "Direct",
    hud_path_p2p: "P2P Direct",
    hud_path_relay: "Relay",
    hud_pointer_locked: "pointer locked (Ctrl+Alt+G)",

    // apps/client
    tray_show_window: "Show Window",
    tray_copy_id: "Copy Device ID",
    tray_rotate_otp: "Rotate OTP",
    tray_quit: "Quit",
    ts_needs_login: "Tailscale needs login",
    ts_stopped: "Tailscale is stopped",
    ts_starting: "Tailscale is starting…",
    ts_daemon_not_running: "Tailscale daemon is not running",
    ts_not_running: "Tailscale is not running",
    ts_not_installed: "Tailscale is not installed",
    waiting_for_video: "Waiting for video…",
    authenticating: "Authenticating…",
    waiting_for_host: "Waiting for host…",
    waiting_for_host_accept: "Waiting for host to accept…",
    host_peer_viewer: "viewer",
    err_no_rendezvous_for_id: "Set a Rendezvous server in Settings (or pass --rendezvous) to connect by Device ID",
    err_no_rendezvous_configured: "no rendezvous server configured",
    err_net_feature_disabled: "network support disabled at compile time (rebuild with `--features net`)",
    err_embedded_host_requires_net: "embedded host requires the `net` feature",
    err_open_control_timeout: "timed out opening control stream",
    err_host_closed_connection: "host closed connection",
    err_host_closed_control: "host closed control stream",
    err_auth_pk_len: "AuthChallenge host_public_key must be 32 bytes",
    err_auth_pk_mismatch: "host public key in AuthChallenge does not match TLS certificate",
    err_auth_device_id_mismatch: "host device_id does not match host public key",
    err_kdf_panicked: "password derivation task panicked",
    err_handshake_timeout: "handshake timed out",
    err_reading_handshake: "reading handshake message",
    err_sending_auth_response: "sending AuthResponse",
    err_auth_rejected: "authentication rejected",
    host_reject_ui_unavailable: "host UI unavailable",
    host_reject_by_user: "rejected by host user",
    host_reject_prompt_closed: "approval prompt closed",

    // R6 / R7
    settings_version_label: "Version",
    tip_version: "Installed VeloxDesk version and Git commit build identifier.",
    settings_updates_label: "Updates",
    tip_updates: "Check GitHub Releases for a newer version of VeloxDesk and update automatically.",
    update_check_btn: "Check for updates",
    update_checking: "Checking for updates…",
    update_up_to_date: "Up to date",
    update_now_btn: "Update & Restart",
    update_later_btn: "Later",
    update_retry_btn: "Retry",
    update_dismiss_btn: "Dismiss",
    update_release_notes_btn: "Release notes",
    update_downloading: "Downloading update…",
    update_applying: "Applying update & restarting…",

    // v0.3.0 Host Access & Security
    pw_mode_onetime: "One-time only",
    pw_mode_permanent: "Permanent only",
    pw_mode_both: "Both (OTP + permanent)",
    settings_security_heading: "Host Access & Security",
    settings_password_mode: "Password mode",
    tip_password_mode: "Choose which passwords are accepted for incoming connections: rotating one-time password (OTP), permanent password, or both.",
    pw_mode_permanent_fallback_hint: "No permanent password set — one-time password stays active as fallback until you save one.",
    settings_permanent_password: "Permanent password",
    tip_permanent_password: "Set a persistent password (stored locally as an Argon2id hash, never plaintext) for unattended access to this desk.",
    pw_status_set: "Set",
    pw_status_not_set: "Not set",
    pw_new_placeholder: "New password (min 6 chars)",
    pw_save_btn: "Save",
    pw_clear_btn: "Clear",
    tip_pw_save: "Hash with Argon2id and save the new permanent password (minimum 6 characters).",
    tip_pw_clear: "Remove the saved permanent password from this desk.",
    pw_too_short_err: "Password must be at least 6 characters",
    pw_saving_status: "Hashing password…",
    pw_saved_notice: "Permanent password saved",
    pw_cleared_notice: "Permanent password cleared",
    settings_require_accept: "Incoming approval",
    tip_settings_require_accept: "Require clicking Accept on incoming connections. Uncheck together with a permanent password to enable unattended access.",
    settings_trusted_devices: "Trusted devices",
    tip_trusted_devices: "Devices remembered via \"Trust this device\" can reconnect without a password or prompt. Revoke anytime below.",
    trusted_devices_empty: "No trusted devices yet",
    trusted_remove_tooltip: "Revoke trust for this device immediately",
    connect_permanent_active: "Permanent password active",
    connect_plus_permanent_badge: "+ Permanent",

    // v0.4.0 Themes, OTP Auto-CAPS, Android Host Navigation
    theme_system: "System",
    theme_dark: "Dark",
    theme_light: "Light",
    settings_theme_label: "Theme",
    tip_theme: "Choose the desktop interface color theme: follow the operating system (System), always Dark, or always Light.",
    theme_switch_tooltip: "Switch interface color theme (System / Dark / Light)",
    caps_btn: "CAPS",
    tip_caps: "Automatically convert letters to UPPERCASE and strip spaces for one-time password (OTP) entry. Click to toggle for mixed-case permanent passwords.",
    toolbar_android_back: "Back",
    toolbar_android_back_tooltip: "Send Android Back navigation key (Escape)",
    toolbar_android_home: "Home",
    toolbar_android_home_tooltip: "Send Android Home navigation key (Meta/Super)",
    toolbar_android_recents: "Recents",
    toolbar_android_recents_tooltip: "Open Android Recent Apps switcher (Alt+Tab)",

    // v0.5.0 Echo Cancellation, Recording, Chat, Remote Power, Auto-Reconnect
    perm_chat: "Session chat",
    perm_power: "Remote power (reboot/lock/sleep)",
    settings_echo_cancellation: "Echo cancellation",
    settings_echo_cancellation_cb: "Suppress remote audio echo (AEC)",
    tip_echo_cancellation: "Exclude VeloxDesk playback from host audio capture and run adaptive acoustic echo cancellation on viewer microphone.",
    tip_echo_cancellation_gaming: "Echo cancellation is always enabled in Gaming mode to prevent voice feedback loops.",
    settings_recordings_dir: "Recordings folder",
    settings_recordings_dir_hint: "Default (Videos\\VeloxDesk)",
    tip_recordings_dir: "Folder where zero re-encode .mkv session recordings are saved. Leave empty to use Videos\\VeloxDesk.",
    toolbar_chat: "Chat",
    toolbar_chat_tooltip: "Toggle in-session text chat",
    toolbar_rec: "REC",
    toolbar_rec_start_tooltip: "Start zero re-encode .mkv session recording",
    toolbar_rec_stop_tooltip: "Stop and finalize session recording",
    toolbar_rec_waiting: "waiting IDR",
    toolbar_power: "Power",
    toolbar_power_tooltip: "Remote host power actions (Reboot, Safe Mode, Lock, Sleep)",
    toolbar_power_denied_tooltip: "Remote power actions are not permitted by the host",
    chat_title: "Session Chat",
    host_chat_title: "Host Chat",
    chat_empty: "No messages yet. Say hello!",
    chat_input_hint: "Type a message and press Enter…",
    chat_send_btn: "Send",
    chat_disabled_notice: "Chat is disabled by host permissions.",
    chat_delivered_tooltip: "Delivered",
    chat_sending_tooltip: "Sending…",
    host_chat_btn: "Chat",
    host_chat_tooltip: "Open chat with connected viewers",
    power_reboot: "Reboot",
    power_reboot_safe_mode: "Reboot Safe Mode",
    power_lock_screen: "Lock Screen",
    power_sleep: "Sleep",
    power_confirm_title: "Confirm Remote Power Action",
    power_confirm_btn: "Confirm",
    power_warn_reboot: "This will immediately reboot the remote host. Unsaved work on the remote computer may be lost.",
    power_warn_reboot_safe_mode: "This will reboot the remote host into Safe Mode with Networking. Unsaved work may be lost.",
    power_warn_lock_screen: "This will lock the remote host user session screen.",
    power_warn_sleep: "This will put the remote host to sleep and disconnect the active session.",
    power_reboot_hint_reconnect: "VeloxDesk will automatically retry reconnecting while the host restarts (up to 5 minutes).",
    reconnect_title: "Reconnecting to remote host…",
    reconnect_timed_out: "Auto-reconnect timed out after 5 minutes.",

    // v0.6.0 Deep Profile Defaults, Audio & Voice, Recording & Session, Host Policy
    audio_latency_auto: "Auto (5 ms Gaming / 10 ms Desktop)",
    audio_latency_2_5ms: "2.5 ms",
    audio_latency_5ms: "5 ms",
    audio_latency_10ms: "10 ms",
    audio_latency_20ms: "20 ms",
    privacy_mode_curtain: "Curtain Overlay (no VDD)",
    privacy_mode_vdd: "VDD + Monitor Off",
    settings_reset_profile_btn: "Reset profile defaults",
    tip_reset_profile: "Restore the edited Desktop or Gaming profile back to its recommended default values.",
    settings_audio_heading: "Audio & Voice",
    settings_noise_suppression: "Noise suppression",
    tip_noise_suppression: "Suppress stationary microphone background noise (fans, keyboard) with a smooth VAD gate before echo cancellation and Opus encoding.",
    settings_audio_frame_latency: "Opus frame latency",
    tip_audio_frame_latency: "Opus audio frame duration: Auto uses 5 ms in Gaming mode and 10 ms in Desktop mode; lower values reduce latency while higher values save bandwidth.",
    settings_recording_heading: "Recording & Session",
    settings_open_folder_btn: "Open folder",
    tip_open_folder: "Create the recordings folder if needed and open it in the system file manager.",
    settings_auto_reconnect: "Auto-reconnect",
    settings_auto_reconnect_cb: "Automatically reconnect after remote reboot",
    tip_auto_reconnect: "Automatically retry connecting for up to 5 minutes after triggering a remote host reboot.",
    settings_host_permissions: "Default permissions",
    tip_host_permissions: "Default permissions granted to incoming sessions and pre-checked in the Accept dialog.",
    host_perm_input: "Input",
    host_perm_clipboard: "Clipboard",
    host_perm_file_transfer: "File Transfer",
    host_perm_audio: "Audio & Mic",
    host_perm_chat: "Chat",
    host_perm_terminal_tunnel: "Terminal & Tunnels",
    host_perm_power: "Remote Power / Reboot",
    settings_host_echo_exclusion: "Host echo exclusion",
    settings_host_echo_exclusion_cb: "Host Loopback Echo Exclusion",
    tip_host_echo_exclusion: "Exclude VeloxDesk's own audio playback from Windows WASAPI loopback capture so remote voice does not echo back.",
    settings_host_privacy_mode: "Windows Privacy Mode",
    tip_host_privacy_mode: "Choose how Privacy Mode blanks physical monitors on Windows: Curtain Overlay works without VDD; VDD + Monitor Off also powers off displays when a virtual display is active.",

    // v0.6.2 Remote Microphone / Discord Routing & Built-in VB-CABLE Installer
    settings_host_mic_routing_heading: "Remote Microphone / Discord Routing",
    tip_host_mic_routing: "Route incoming viewer microphone audio into a virtual audio cable (VB-CABLE) so Discord, Zoom, and games on this PC can use it as a microphone input.",
    settings_host_mic_output_label: "Mic output device",
    host_mic_output_auto: "Auto (Virtual Cable if installed)",
    host_mic_output_speakers: "Default Speakers",
    host_mic_refresh_tooltip: "Refresh playback audio devices",
    host_mic_missing_warn: "Virtual microphone not installed — sound will play through PC speakers (Discord will not hear it)",
    vbcable_install_btn: "Install Virtual Microphone (VB-CABLE)",
    vbcable_reinstall_btn: "Reinstall Driver (VB-CABLE)",
    tip_vbcable_install: "Download and silently install the official Microsoft WHQL-signed VB-Audio Virtual Cable driver (prompts for Windows UAC elevation).",
    vbcable_downloading: "Downloading VB-CABLE driver…",
    vbcable_extracting: "Extracting driver package…",
    vbcable_waiting_uac: "Confirm Windows UAC prompt to install driver…",
    vbcable_cancelled: "Driver installation cancelled.",
    vbcable_post_install_hint: "Select \"CABLE Output\" as your microphone in Discord; if the device has not appeared yet, restart your PC.",
};

pub const RU: Strings = Strings {
    // Common / Language
    app_title: "VeloxDesk",
    lang_switch_tooltip: "Переключить язык интерфейса (English / Русский)",
    refresh_btn: "Обновить",
    copy_btn: "Копировать",
    hide_btn: "Скрыть",
    show_btn: "Показать",
    connect_btn: "Подключиться",
    cancel_btn: "Отмена",
    settings_btn: "Настройки",
    remove_tooltip: "Удалить",

    // Enums
    mode_desktop: "Рабочий стол",
    mode_gaming: "Игровой",
    quality_low: "Низкое (5 Мбит/с)",
    quality_balanced: "Баланс (20 Мбит/с)",
    quality_high: "Высокое (50 Мбит/с)",
    quality_ultra: "Ультра (100 Мбит/с)",
    present_mailbox: "Mailbox (без разрывов)",
    present_immediate: "Immediate (без VSync)",
    present_fifo: "VSync",
    scale_fit: "Вписать",
    scale_stretch: "Растянуть",
    scale_original: "Оригинал (1:1)",
    fps_60: "60 FPS",
    fps_120: "120 FPS",
    fps_144: "144 FPS",
    fps_240: "240 FPS",

    // ConnectScreen
    exit_fullscreen_btn: "Выйти из полного экрана (Ctrl+Shift+F)",
    exit_fullscreen_tooltip: "Вернуть оконный режим и элементы управления окном",
    this_desk_heading: "Это устройство",
    status_ready: "Готово",
    status_offline: "Не в сети",
    this_desk_subtitle: "Передайте ID устройства и одноразовый пароль для удалённого доступа.",
    device_id_label: "ID устройства",
    unavailable: "Недоступен",
    copy_id_btn: "Копировать ID",
    device_id_copied: "ID устройства скопирован",
    tailscale_ip_label: "IP Tailscale",
    tailscale_ip_copied: "IP Tailscale скопирован",
    otp_label: "Одноразовый пароль",
    otp_refresh_tooltip: "Создать новый одноразовый пароль",
    otp_copied: "Пароль скопирован",
    host_listening: "Ожидание подключений",
    host_stopped: "Остановлен",
    host_starting: "Запуск…",
    stop_hosting_btn: "Отключить доступ",
    start_hosting_btn: "Включить доступ",
    require_accept_label: "Запрашивать подтверждение",
    require_accept_tooltip: "Запрашивать подтверждение входящих подключений с выбором разрешений",
    remote_desk_heading: "Удалённое управление",
    remote_desk_subtitle: "Введите 9-значный ID устройства или прямой адрес (ip:порт).",
    remote_address_hint: "123 456 789 или 192.168.1.20:21118",
    mode_colon_label: "Режим:",
    recent_connections_heading: "Недавние подключения",
    tailscale_devices_heading: "Устройства Tailscale",
    tailscale_refresh_tooltip: "Запросить статус Tailscale",
    tailscale_install_hint: "Нет белого IP? Установите бесплатный Tailscale для P2P в 1 клик",
    tailscale_download_btn: "Скачать Tailscale",
    tailscale_not_connected: "Tailscale не подключён",
    tailscale_start_btn: "Запустить / войти в Tailscale",
    tailscale_no_peers: "Подключено к Tailscale — другие устройства пока не найдены.",
    err_enter_address: "введите адрес",
    err_enter_target: "введите 9-значный ID устройства или адрес",

    // SessionToolbar
    session_toolbar_tooltip: "Панель сеанса",
    collapse_toolbar_tooltip: "Свернуть панель",
    pointer_locked_pill: "Захвачен (Ctrl+Alt+G)",
    display_primary_suffix: ", основной",
    toolbar_audio: "Звук",
    toolbar_audio_tooltip: "Вкл./выкл. воспроизведение удалённого звука",
    toolbar_mic: "Микр.",
    toolbar_mic_tooltip: "Вкл./выкл. передачу микрофона",
    toolbar_locked: "Захвачен",
    toolbar_lock: "Захват",
    toolbar_lock_cursor_btn: "Захватить курсор (Ctrl+Alt+G)",
    toolbar_unlock_cursor_btn: "Освободить курсор (Ctrl+Alt+G)",
    toolbar_lock_tooltip: "Захват курсора (Ctrl+Alt+G)",
    toolbar_files: "Файлы",
    toolbar_term: "Терм.",
    toolbar_tunnels: "Туннели",
    toolbar_privacy: "Приватн.",
    toolbar_privacy_tooltip: "Погасить экран хоста (режим приватности)",
    toolbar_clip: "Буфер",
    toolbar_clip_tooltip: "Вкл./выкл. синхронизацию буфера обмена",
    toolbar_hud: "HUD",
    toolbar_fullscreen: "Полный экран",
    toolbar_disconnect: "Отключиться",

    // settings_window_at
    settings_title: "Настройки",
    settings_language_label: "Язык",
    settings_default_mode: "Режим по умолчанию",
    settings_editing_profile: "Настройка профиля",
    settings_quality: "Качество",
    settings_target_fps: "Целевой FPS",
    settings_color_hdr: "Цвет / HDR",
    settings_hdr_10bit: "10-бит HDR",
    settings_yuv444: "YUV 4:4:4",
    settings_audio: "Аудио",
    settings_remote_audio: "Удалённый звук",
    settings_microphone: "Микрофон",
    settings_presentation: "Вывод кадра",
    settings_scaling: "Масштабирование",
    settings_perf_hud: "Статистика (HUD)",
    settings_hud_show_on_connect: "Показывать при подключении (Ctrl+Shift+H)",
    settings_decoder: "Декодер",
    settings_prefer_hw_decode: "Предпочитать аппаратное декодирование",
    settings_gaming_mouse: "Игровая мышь",
    settings_relative_mouse: "Захват курсора (Ctrl+Alt+G), относительное движение",
    settings_clipboard: "Буфер обмена",
    settings_clipboard_sync: "Синхронизация буфера (текст, изображения, файлы)",
    settings_rendezvous_server: "Сервер рандеву",

    // R5 tooltips
    tip_language: "Переключить язык интерфейса между английским и русским (сохраняется между запусками).",
    tip_default_mode: "Выберите режим сеанса по умолчанию: Рабочий стол для обычного курсора или Игровой для захвата мыши и геймпадов.",
    tip_editing_profile: "Выберите независимый профиль трансляции (Рабочий стол или Игровой), параметры которого настраиваются ниже.",
    tip_quality: "Максимальный битрейт видео (5–100 Мбит/с). Более высокое значение повышает чёткость, но требует больше пропускной способности.",
    tip_target_fps: "Целевое ограничение частоты кадров (60–240 FPS), запрашиваемое у удалённого хоста (применяется на лету в активном сеансе).",
    tip_color_hdr: "Настройки глубины цвета и цветовой субдискретизации для выбранного профиля трансляции.",
    tip_hdr_10bit: "Запрашивать 10-битный HDR (P010 / HEVC Main10 / AV1 10-bit), если его поддерживают хост и видеокарта.",
    tip_yuv444: "Запрашивать полную чёткость цвета YUV 4:4:4 для мелкого текста и элементов интерфейса (увеличивает трафик).",
    tip_audio: "Параметры воспроизведения удалённого звука и передачи микрофона для выбранного профиля.",
    tip_remote_audio: "Воспроизводить звук с удалённого рабочего стола (низкая задержка Opus).",
    tip_microphone: "Передавать звук с вашего локального микрофона на удалённый хост.",
    tip_presentation: "Режим вывода кадров: Mailbox (низкая задержка без разрывов), Immediate (мин. задержка без VSync) или VSync (FIFO).",
    tip_scaling: "Способ масштабирования видео в окне: Вписать (сохранять пропорции), Растянуть (на всё окно) или Оригинал (1:1 по пикселям).",
    tip_perf_hud: "Автоматически показывать оверлей статистики задержки и FPS при подключении (переключается в любой момент по Ctrl+Shift+H).",
    tip_decoder: "Использовать аппаратное декодирование видео на GPU (DX11/NVDEC); снимите флажок для декодирования на CPU (при переподключении).",
    tip_gaming_mouse: "В игровом режиме захватывать курсор (Ctrl+Alt+G) и передавать относительные смещения мыши для 3D-игр.",
    tip_clipboard: "Синхронизировать буфер обмена (текст, изображения и списки файлов) между локальным ПК и удалённым хостом.",
    tip_rendezvous_server: "Адрес сервера рандеву (host[:port], порт по умолчанию 21116) для поиска по 9-значному ID и P2P/relay-подключений.",

    // PasswordPrompt & AcceptPrompt
    auth_required_title: "Требуется аутентификация",
    enter_password_hint: "Введите пароль",
    auth_methods_both: "Введите постоянный или одноразовый пароль",
    auth_methods_perm: "Введите постоянный пароль",
    auth_methods_otp: "Введите одноразовый пароль",
    incoming_connection_title: "Входящее подключение",
    unknown_device: "Неизвестное устройство",
    granted_permissions_heading: "Выданные разрешения",
    perm_view: "Просмотр экрана",
    perm_input: "Клавиатура и мышь",
    perm_clipboard: "Буфер обмена",
    perm_file_transfer: "Передача файлов",
    perm_audio: "Аудио",
    perm_tunnel: "TCP-туннели",
    perm_terminal: "Терминал",
    perm_privacy_mode: "Режим приватности",
    perm_restart: "Перезапуск хоста",
    trust_device_checkbox: "Доверять этому устройству (переподключение без пароля)",
    accept_btn: "Принять",
    reject_btn: "Отклонить",

    // FileManagerWindow
    files_title: "Файловый менеджер",
    files_up_btn: "Вверх",
    files_col_name: "Имя",
    files_col_type: "Тип",
    files_col_size: "Размер",
    files_type_dir: "Папка",
    files_type_file: "Файл",
    files_local_path_label: "Локальный путь:",
    files_local_path_hint: "C:\\путь\\к\\файлу",
    files_resume_cb: "Докачка",
    files_download_btn: "Скачать",
    files_upload_btn: "Загрузить",
    files_new_name_hint: "Папка или новое имя",
    files_mkdir_btn: "Создать папку",
    files_rename_btn: "Переименовать",
    files_delete_btn: "Удалить",
    unit_b: "Б",
    unit_kib: "КиБ",
    unit_mib: "МиБ",
    unit_gib: "ГиБ",

    // TerminalWindow
    terminal_title: "Удалённый терминал",
    terminal_shell_label: "Оболочка:",
    terminal_shell_hint: "по умолчанию",
    terminal_open_btn: "Открыть",
    terminal_close_btn: "Закрыть",
    terminal_size_label: "Размер:",
    terminal_resize_btn: "Применить",
    terminal_clear_btn: "Очистить",
    terminal_input_hint: "Введите команду и нажмите Enter",
    terminal_send_btn: "Отправить",

    // TunnelsWindow
    tunnels_title: "Проброс TCP-портов",
    tunnels_local_port_label: "Локальный порт:",
    tunnels_add_btn: "Добавить",
    tunnels_empty: "Нет активных пробросов портов.",
    tunnels_col_local: "Локальный",
    tunnels_col_remote: "Удалённый",
    tunnels_stop_btn: "Остановить",
    err_tunnel_empty_host: "удалённый хост не может быть пустым",
    err_tunnel_zero_port: "удалённый порт должен быть > 0",

    // HUD
    hud_capture: "Захват",
    hud_encode: "Кодир.",
    hud_net: "Сеть",
    hud_rtt: "RTT",
    hud_decode: "Декод.",
    hud_render: "Рендер",
    hud_total: "Итого",
    hud_ms: "мс",
    hud_fps: "к/с",
    hud_mbps: "Мбит/с",
    hud_loss: "потери",
    hud_fec_rec: "FEC восст.",
    hud_decode_prefix: "декодер",
    hud_zero_copy: "zero-copy",
    hud_yes: "да",
    hud_no: "нет",
    hud_path_unknown: "Неизвестно",
    hud_path_direct: "Прямое",
    hud_path_p2p: "P2P прямое",
    hud_path_relay: "Релей",
    hud_pointer_locked: "курсор захвачен (Ctrl+Alt+G)",

    // apps/client
    tray_show_window: "Показать окно",
    tray_copy_id: "Копировать ID устройства",
    tray_rotate_otp: "Обновить пароль (OTP)",
    tray_quit: "Выход",
    ts_needs_login: "Требуется вход в Tailscale",
    ts_stopped: "Tailscale остановлен",
    ts_starting: "Запуск Tailscale…",
    ts_daemon_not_running: "Служба Tailscale не запущена",
    ts_not_running: "Tailscale не запущен",
    ts_not_installed: "Tailscale не установлен",
    waiting_for_video: "Ожидание видео…",
    authenticating: "Аутентификация…",
    waiting_for_host: "Ожидание ответа хоста…",
    waiting_for_host_accept: "Ожидание подтверждения от хоста…",
    host_peer_viewer: "клиент",
    err_no_rendezvous_for_id: "Укажите сервер рандеву в Настройках (или --rendezvous) для подключения по ID",
    err_no_rendezvous_configured: "сервер рандеву не настроен",
    err_net_feature_disabled: "сетевая поддержка отключена при компиляции (пересоберите с `--features net`)",
    err_embedded_host_requires_net: "встроенный хост требует функцию `net`",
    err_open_control_timeout: "истекло время ожидания открытия канала управления",
    err_host_closed_connection: "хост закрыл соединение",
    err_host_closed_control: "хост закрыл канал управления",
    err_auth_pk_len: "AuthChallenge host_public_key должен быть длиной 32 байта",
    err_auth_pk_mismatch: "открытый ключ хоста в AuthChallenge не совпадает с сертификатом TLS",
    err_auth_device_id_mismatch: "device_id хоста не соответствует открытому ключу хоста",
    err_kdf_panicked: "сбой задачи вычисления ключа пароля",
    err_handshake_timeout: "истекло время ожидания рукопожатия",
    err_reading_handshake: "ошибка чтения сообщения рукопожатия",
    err_sending_auth_response: "ошибка отправки AuthResponse",
    err_auth_rejected: "аутентификация отклонена",
    host_reject_ui_unavailable: "интерфейс хоста недоступен",
    host_reject_by_user: "отклонено пользователем хоста",
    host_reject_prompt_closed: "окно подтверждения закрыто",

    // R6 / R7
    settings_version_label: "Версия",
    tip_version: "Установленная версия VeloxDesk и идентификатор сборки (коммит Git).",
    settings_updates_label: "Обновления",
    tip_updates: "Проверить наличие новой версии VeloxDesk на GitHub Releases и установить автоматически.",
    update_check_btn: "Проверить обновления",
    update_checking: "Проверка обновлений…",
    update_up_to_date: "Установлена последняя версия",
    update_now_btn: "Обновить и перезапустить",
    update_later_btn: "Позже",
    update_retry_btn: "Повторить",
    update_dismiss_btn: "Скрыть",
    update_release_notes_btn: "Список изменений",
    update_downloading: "Загрузка обновления…",
    update_applying: "Установка обновления и перезапуск…",

    // v0.3.0 Host Access & Security
    pw_mode_onetime: "Только одноразовый",
    pw_mode_permanent: "Только постоянный",
    pw_mode_both: "Оба (OTP + постоянный)",
    settings_security_heading: "Доступ к этому ПК и безопасность",
    settings_password_mode: "Режим пароля",
    tip_password_mode: "Выберите, какие пароли принимаются при входящих подключениях: одноразовый (OTP), постоянный или оба.",
    pw_mode_permanent_fallback_hint: "Постоянный пароль не задан — одноразовый пароль остаётся активным, пока вы не сохраните постоянный.",
    settings_permanent_password: "Постоянный пароль",
    tip_permanent_password: "Задайте постоянный пароль (хранится локально как хэш Argon2id, никогда в открытом виде) для доступа без присутствия.",
    pw_status_set: "Задан",
    pw_status_not_set: "Не задан",
    pw_new_placeholder: "Новый пароль (мин. 6 символов)",
    pw_save_btn: "Сохранить",
    pw_clear_btn: "Удалить",
    tip_pw_save: "Вычислить хэш Argon2id и сохранить новый постоянный пароль (минимум 6 символов).",
    tip_pw_clear: "Удалить сохранённый постоянный пароль с этого устройства.",
    pw_too_short_err: "Пароль должен содержать минимум 6 символов",
    pw_saving_status: "Сохранение пароля…",
    pw_saved_notice: "Постоянный пароль сохранён",
    pw_cleared_notice: "Постоянный пароль удалён",
    settings_require_accept: "Подтверждение входа",
    tip_settings_require_accept: "Запрашивать нажатие «Принять» для входящих подключений. Снимите флажок вместе с постоянным паролем для доступа без присутствия.",
    settings_trusted_devices: "Доверенные устройства",
    tip_trusted_devices: "Устройства, добавленные через «Доверять этому устройству», могут подключаться без пароля и подтверждения. Доступ можно отозвать ниже.",
    trusted_devices_empty: "Доверенных устройств пока нет",
    trusted_remove_tooltip: "Немедленно отозвать доверие для этого устройства",
    connect_permanent_active: "Активен постоянный пароль",
    connect_plus_permanent_badge: "+ Постоянный",

    // v0.4.0 Themes, OTP Auto-CAPS, Android Host Navigation
    theme_system: "Системная",
    theme_dark: "Тёмная",
    theme_light: "Светлая",
    settings_theme_label: "Тема",
    tip_theme: "Выберите цветовую тему интерфейса: как в операционной системе (Системная), всегда Тёмная или всегда Светлая.",
    theme_switch_tooltip: "Переключить цветовую тему интерфейса (Системная / Тёмная / Светлая)",
    caps_btn: "CAPS",
    tip_caps: "Автоматически переводить буквы в ВЕРХНИЙ РЕГИСТР и удалять пробелы для ввода одноразового пароля (OTP). Нажмите, чтобы переключить для обычного пароля.",
    toolbar_android_back: "Назад",
    toolbar_android_back_tooltip: "Отправить кнопку «Назад» для Android (Escape)",
    toolbar_android_home: "Домой",
    toolbar_android_home_tooltip: "Отправить кнопку «Домой» для Android (Meta/Super)",
    toolbar_android_recents: "Недавние",
    toolbar_android_recents_tooltip: "Открыть список недавних приложений Android (Alt+Tab)",

    // v0.5.0 Echo Cancellation, Recording, Chat, Remote Power, Auto-Reconnect
    perm_chat: "Чат в сеансе",
    perm_power: "Питание хоста (перезагрузка/блокировка/сон)",
    settings_echo_cancellation: "Подавление эха",
    settings_echo_cancellation_cb: "Подавлять эхо удалённого звука (AEC)",
    tip_echo_cancellation: "Исключает звук VeloxDesk из захвата на хосте и включает адаптивное акустическое эхоподавление для микрофона.",
    tip_echo_cancellation_gaming: "В игровом режиме подавление эха всегда включено для предотвращения акустической петли.",
    settings_recordings_dir: "Папка записей",
    settings_recordings_dir_hint: "По умолчанию (Videos\\VeloxDesk)",
    tip_recordings_dir: "Папка для сохранения записей сеанса в формате .mkv без перекодирования. Оставьте пустой для Videos\\VeloxDesk.",
    toolbar_chat: "Чат",
    toolbar_chat_tooltip: "Открыть или скрыть текстовый чат сеанса",
    toolbar_rec: "REC",
    toolbar_rec_start_tooltip: "Начать запись сеанса в .mkv без перекодирования",
    toolbar_rec_stop_tooltip: "Остановить и сохранить запись сеанса",
    toolbar_rec_waiting: "ожидание IDR",
    toolbar_power: "Питание",
    toolbar_power_tooltip: "Управление питанием удалённого хоста (Перезагрузка, Безопасный режим, Блокировка, Сон)",
    toolbar_power_denied_tooltip: "Управление питанием запрещено настройками разрешений хоста",
    chat_title: "Чат сеанса",
    host_chat_title: "Чат хоста",
    chat_empty: "Сообщений пока нет. Напишите что-нибудь!",
    chat_input_hint: "Введите сообщение и нажмите Enter…",
    chat_send_btn: "Отправить",
    chat_disabled_notice: "Чат отключён разрешениями хоста.",
    chat_delivered_tooltip: "Доставлено",
    chat_sending_tooltip: "Отправка…",
    host_chat_btn: "Чат",
    host_chat_tooltip: "Открыть чат с подключёнными пользователями",
    power_reboot: "Перезагрузить",
    power_reboot_safe_mode: "Перезагрузка в безопасный режим",
    power_lock_screen: "Заблокировать экран",
    power_sleep: "Сон",
    power_confirm_title: "Подтвердите действие с питанием",
    power_confirm_btn: "Подтвердить",
    power_warn_reboot: "Удалённый хост будет немедленно перезагружен. Несохранённые данные на удалённом компьютере могут быть потеряны.",
    power_warn_reboot_safe_mode: "Удалённый хост будет перезагружен в безопасный режим с загрузкой сетевых драйверов. Несохранённые данные могут быть потеряны.",
    power_warn_lock_screen: "Экран удалённого хоста будет заблокирован.",
    power_warn_sleep: "Удалённый хост перейдёт в спящий режим, текущий сеанс будет завершён.",
    power_reboot_hint_reconnect: "VeloxDesk будет автоматически пытаться переподключиться, пока хост перезагружается (до 5 минут).",
    reconnect_title: "Переподключение к удалённому хосту…",
    reconnect_timed_out: "Автопереподключение не удалось за 5 минут.",

    // v0.6.0 Deep Profile Defaults, Audio & Voice, Recording & Session, Host Policy
    audio_latency_auto: "Авто (5 мс Игровой / 10 мс Рабочий стол)",
    audio_latency_2_5ms: "2.5 мс",
    audio_latency_5ms: "5 мс",
    audio_latency_10ms: "10 мс",
    audio_latency_20ms: "20 мс",
    privacy_mode_curtain: "Чёрный экран поверх окон (без VDD)",
    privacy_mode_vdd: "VDD + отключение монитора",
    settings_reset_profile_btn: "Сбросить профиль по умолчанию",
    tip_reset_profile: "Сбросить настройки выбранного профиля (Рабочий стол или Игровой) к рекомендуемым значениям по умолчанию.",
    settings_audio_heading: "Аудио и голос",
    settings_noise_suppression: "Шумоподавление",
    tip_noise_suppression: "Подавлять фоновый шум микрофона (вентиляторы, клавиатура) с плавным VAD-фильтром до эхоподавления и кодирования Opus.",
    settings_audio_frame_latency: "Кадр аудио Opus",
    tip_audio_frame_latency: "Длительность кадра Opus: «Авто» использует 5 мс в игровом режиме и 10 мс на рабочем столе; меньше — ниже задержка, больше — ниже трафик.",
    settings_recording_heading: "Запись и сеанс",
    settings_open_folder_btn: "Открыть папку",
    tip_open_folder: "Создать папку записей при необходимости и открыть её в системном файловом менеджере.",
    settings_auto_reconnect: "Автопереподключение",
    settings_auto_reconnect_cb: "Автоматически переподключаться после перезагрузки хоста",
    tip_auto_reconnect: "Автоматически повторять попытки подключения до 5 минут после перезагрузки удалённого хоста.",
    settings_host_permissions: "Разрешения по умолчанию",
    tip_host_permissions: "Разрешения по умолчанию для входящих подключений и окна подтверждения.",
    host_perm_input: "Ввод",
    host_perm_clipboard: "Буфер обмена",
    host_perm_file_transfer: "Передача файлов",
    host_perm_audio: "Аудио и микрофон",
    host_perm_chat: "Чат",
    host_perm_terminal_tunnel: "Терминал и туннели",
    host_perm_power: "Питание / Перезагрузка",
    settings_host_echo_exclusion: "Исключение эха хоста",
    settings_host_echo_exclusion_cb: "Исключать звук VeloxDesk из захвата на хосте",
    tip_host_echo_exclusion: "Исключать собственное воспроизведение VeloxDesk из системного захвата WASAPI на Windows, чтобы голос не возвращался эхом.",
    settings_host_privacy_mode: "Режим приватности (Windows)",
    tip_host_privacy_mode: "Способ скрытия физических мониторов на Windows: «Чёрный экран» работает без VDD; «VDD + отключение монитора» также выключает дисплей при активном VDD.",

    // v0.6.2 Remote Microphone / Discord Routing & Built-in VB-CABLE Installer
    settings_host_mic_routing_heading: "Удалённый микрофон / Маршрутизация Discord",
    tip_host_mic_routing: "Направляйте входящий микрофон клиента в виртуальный аудиокабель (VB-CABLE), чтобы Discord, Zoom и игры на этом ПК слышали его как микрофон.",
    settings_host_mic_output_label: "Вывод микрофона",
    host_mic_output_auto: "Авто (виртуальный кабель, если установлен)",
    host_mic_output_speakers: "Динамики по умолчанию",
    host_mic_refresh_tooltip: "Обновить список аудиоустройств вывода",
    host_mic_missing_warn: "Виртуальный микрофон не установлен — звук пойдёт в динамики ПК (Discord его не услышит)",
    vbcable_install_btn: "Установить виртуальный микрофон (VB-CABLE)",
    vbcable_reinstall_btn: "Переустановить драйвер",
    tip_vbcable_install: "Скачать и тихо установить официальный WHQL-драйвер VB-Audio Virtual Cable с подписью Microsoft (появится запрос Windows UAC).",
    vbcable_downloading: "Загрузка драйвера VB-CABLE…",
    vbcable_extracting: "Распаковка пакета драйвера…",
    vbcable_waiting_uac: "Подтвердите запрос Windows UAC для установки драйвера…",
    vbcable_cancelled: "Установка драйвера отменена.",
    vbcable_post_install_hint: "Выберите «CABLE Output» как микрофон в Discord; если устройство ещё не появилось — перезагрузите ПК",
};

/// Return the static localization table for `lang`.
pub const fn tr(lang: Language) -> &'static Strings {
    match lang {
        Language::En => &EN,
        Language::Ru => &RU,
    }
}

impl Strings {
    fn is_ru(&self) -> bool {
        self.this_desk_heading == RU.this_desk_heading
    }

    pub fn version_with_rendezvous(&self, version: &str, rendezvous: &str) -> String {
        if self.is_ru() {
            format!("v{version}  ·  Рандеву: {rendezvous}")
        } else {
            format!("v{version}  ·  Rendezvous: {rendezvous}")
        }
    }

    pub fn active_sessions_pill(&self, count: usize) -> String {
        if self.is_ru() {
            format!("Активно: {count}")
        } else {
            format!("{count} active")
        }
    }

    pub fn host_status_line(&self, status: &str) -> String {
        if self.is_ru() {
            format!("Статус: {status}")
        } else {
            format!("Status: {status}")
        }
    }

    pub fn err_cannot_resolve(&self, host: &str, err: &dyn Display) -> String {
        if self.is_ru() {
            format!("не удалось разрешить {host}: {err}")
        } else {
            format!("cannot resolve {host}: {err}")
        }
    }

    pub fn err_no_address(&self, host: &str) -> String {
        if self.is_ru() {
            format!("не найден адрес для {host}")
        } else {
            format!("no address for {host}")
        }
    }

    pub fn display_numbered(&self, one_based: u32) -> String {
        if self.is_ru() {
            format!("Экран {one_based}")
        } else {
            format!("Display {one_based}")
        }
    }

    pub fn display_label(&self, d: &DisplayInfo) -> String {
        let name = if d.name.is_empty() {
            self.display_numbered(d.index + 1)
        } else {
            d.name.clone()
        };
        let suffix = if d.primary { self.display_primary_suffix } else { "" };
        format!("{name} ({}x{}{suffix})", d.width, d.height)
    }

    pub fn toolbar_pads(&self, count: u8) -> String {
        if self.is_ru() {
            format!("Геймп.×{count}")
        } else {
            format!("Pad×{count}")
        }
    }

    pub fn host_id_line(&self, formatted_id: &str) -> String {
        if self.is_ru() {
            format!("ID хоста: {formatted_id}")
        } else {
            format!("Host ID: {formatted_id}")
        }
    }

    pub fn device_id_line(&self, formatted_id: &str) -> String {
        if self.is_ru() {
            format!("ID устройства: {formatted_id}")
        } else {
            format!("Device ID: {formatted_id}")
        }
    }

    pub fn key_fingerprint_line(&self, fp: &str) -> String {
        if self.is_ru() {
            format!("Отпечаток ключа: {fp}")
        } else {
            format!("Key fingerprint: {fp}")
        }
    }

    pub fn ed25519_fingerprint_line(&self, fp: &str) -> String {
        if self.is_ru() {
            format!("Отпечаток Ed25519: {fp}")
        } else {
            format!("Ed25519 fingerprint: {fp}")
        }
    }

    pub fn format_size(&self, bytes: u64) -> String {
        const KIB: f64 = 1024.0;
        const MIB: f64 = KIB * 1024.0;
        const GIB: f64 = MIB * 1024.0;
        let b = bytes as f64;
        if b >= GIB {
            format!("{:.2} {}", b / GIB, self.unit_gib)
        } else if b >= MIB {
            format!("{:.2} {}", b / MIB, self.unit_mib)
        } else if b >= KIB {
            format!("{:.1} {}", b / KIB, self.unit_kib)
        } else {
            format!("{bytes} {}", self.unit_b)
        }
    }

    pub fn terminal_exit_code(&self, code: i32) -> String {
        if self.is_ru() {
            format!("Процесс завершился с кодом {code}")
        } else {
            format!("Process exited with code {code}")
        }
    }

    pub fn err_tunnel_expected_full(&self, got: &str) -> String {
        if self.is_ru() {
            format!("ожидалось local_port:remote_host:remote_port, получено {got:?}")
        } else {
            format!("expected local_port:remote_host:remote_port, got {got:?}")
        }
    }

    pub fn err_tunnel_invalid_local_port(&self, s: &str) -> String {
        if self.is_ru() {
            format!("некорректный локальный порт {s:?}")
        } else {
            format!("invalid local port {s:?}")
        }
    }

    pub fn err_tunnel_expected_remote(&self, got: &str) -> String {
        if self.is_ru() {
            format!("ожидалось remote_host:remote_port, получено {got:?}")
        } else {
            format!("expected remote_host:remote_port, got {got:?}")
        }
    }

    pub fn err_tunnel_invalid_remote_port(&self, s: &str) -> String {
        if self.is_ru() {
            format!("некорректный удалённый порт {s:?}")
        } else {
            format!("invalid remote port {s:?}")
        }
    }

    // apps/client formatters

    pub fn fmt_invalid_rendezvous(&self, err: &dyn Display) -> String {
        if self.is_ru() {
            format!("Некорректный сервер рандеву: {err}")
        } else {
            format!("Invalid rendezvous server: {err}")
        }
    }

    pub fn fmt_connecting_to(&self, target: &str) -> String {
        if self.is_ru() {
            format!("Подключение к {target}…")
        } else {
            format!("Connecting to {target}…")
        }
    }

    pub fn fmt_window_title_host(&self, host: &str) -> String {
        format!("VeloxDesk — {host}")
    }

    pub fn fmt_tray_tooltip_id(&self, formatted_id: &str) -> String {
        format!("VeloxDesk — ID {formatted_id}")
    }

    pub fn fmt_test_pattern_hint(&self, w: u32, h: u32) -> String {
        if self.is_ru() {
            format!("Тест-паттерн {w}x{h} · Ctrl+Shift+H HUD · Esc выход")
        } else {
            format!("Test pattern {w}x{h} · Ctrl+Shift+H HUD · Esc quit")
        }
    }

    pub fn fmt_hud_audio_buf(&self, ms: f64) -> String {
        if self.is_ru() {
            format!("буфер звука {ms:.1} мс")
        } else {
            format!("audio buf {ms:.1} ms")
        }
    }

    pub fn fmt_hud_gamepads(&self, count: u8) -> String {
        if self.is_ru() {
            format!("геймпады {count}")
        } else {
            format!("gamepads {count}")
        }
    }

    pub fn fmt_host_listening_port(&self, port: u16) -> String {
        if self.is_ru() {
            format!("Слушает :{port}")
        } else {
            format!("Listening on :{port}")
        }
    }

    pub fn fmt_host_listening_rdv(&self, port: u16, rdv: SocketAddr) -> String {
        if self.is_ru() {
            format!("Слушает :{port} · Рандеву {rdv}")
        } else {
            format!("Listening on :{port} · Rendezvous {rdv}")
        }
    }

    pub fn fmt_host_connected(&self, peer: &str) -> String {
        if self.is_ru() {
            format!("Подключён: {peer}")
        } else {
            format!("Connected: {peer}")
        }
    }

    pub fn fmt_ts_parse_err(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось разобрать статус Tailscale: {err}")
        } else {
            format!("failed to parse tailscale status: {err}")
        }
    }

    pub fn fmt_ts_state(&self, state: &str) -> String {
        if self.is_ru() {
            format!("Состояние Tailscale: {state}")
        } else {
            format!("Tailscale state: {state}")
        }
    }

    pub fn fmt_ts_cannot_run(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось запустить tailscale: {err}")
        } else {
            format!("cannot run tailscale: {err}")
        }
    }

    pub fn fmt_ts_failed_launch(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось запустить Tailscale: {err}")
        } else {
            format!("failed to launch tailscale: {err}")
        }
    }

    pub fn fmt_err_async_runtime(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось запустить async runtime: {err}")
        } else {
            format!("cannot start async runtime: {err}")
        }
    }

    pub fn fmt_err_identity(&self, err: &str) -> String {
        if self.is_ru() {
            format!("ошибка ключа устройства: {err}")
        } else {
            format!("identity error: {err}")
        }
    }

    pub fn fmt_err_connect_failed(&self, err: &str) -> String {
        if self.is_ru() {
            format!("ошибка подключения: {err}")
        } else {
            format!("connect failed: {err}")
        }
    }

    pub fn fmt_err_connect_timeout(&self, target: &str) -> String {
        if self.is_ru() {
            format!("истекло время подключения к {target}")
        } else {
            format!("timed out connecting to {target}")
        }
    }

    pub fn fmt_err_p2p_connect_failed(&self, err: &str) -> String {
        if self.is_ru() {
            format!("ошибка P2P-подключения: {err}")
        } else {
            format!("P2P connect failed: {err}")
        }
    }

    pub fn fmt_err_open_control_stream(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось открыть канал управления: {err}")
        } else {
            format!("open control stream failed: {err}")
        }
    }

    pub fn fmt_err_send_hello(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось отправить Hello: {err}")
        } else {
            format!("send Hello failed: {err}")
        }
    }

    pub fn fmt_err_transport(&self, err: &str) -> String {
        if self.is_ru() {
            format!("ошибка транспорта: {err}")
        } else {
            format!("transport error: {err}")
        }
    }

    pub fn fmt_host_closed_code(&self, code: u32) -> String {
        if self.is_ru() {
            format!("хост завершил сеанс (код {code})")
        } else {
            format!("host closed session (code {code})")
        }
    }

    pub fn fmt_err_control_stream(&self, err: &str) -> String {
        if self.is_ru() {
            format!("ошибка канала управления: {err}")
        } else {
            format!("control stream error: {err}")
        }
    }

    pub fn fmt_err_kdf(&self, err: &str) -> String {
        if self.is_ru() {
            format!("ошибка вычисления ключа пароля: {err}")
        } else {
            format!("password key derivation failed: {err}")
        }
    }

    pub fn fmt_err_no_auth_methods(&self, methods: &[i32]) -> String {
        if self.is_ru() {
            format!("хост не предложил поддерживаемых способов аутентификации ({methods:?})")
        } else {
            format!("host offered no supported authentication methods ({methods:?})")
        }
    }

    pub fn fmt_host_rejected_code(&self, code: u32) -> String {
        if self.is_ru() {
            format!("хост отклонил сеанс (код {code})")
        } else {
            format!("host rejected session (code {code})")
        }
    }

    pub fn fmt_err_tunnel(&self, local_port: u16, remote_host: &str, remote_port: u16, err: &str) -> String {
        if self.is_ru() {
            format!("туннель {local_port} -> {remote_host}:{remote_port}: {err}")
        } else {
            format!("tunnel {local_port} -> {remote_host}:{remote_port}: {err}")
        }
    }

    pub fn fmt_file_created_dir(&self, path: &str) -> String {
        if self.is_ru() {
            format!("Создана папка {path}")
        } else {
            format!("Created directory {path}")
        }
    }

    pub fn fmt_file_deleted(&self, path: &str) -> String {
        if self.is_ru() {
            format!("Удалено: {path}")
        } else {
            format!("Deleted {path}")
        }
    }

    pub fn fmt_file_renamed(&self, to: &str) -> String {
        if self.is_ru() {
            format!("Переименовано в {to}")
        } else {
            format!("Renamed to {to}")
        }
    }

    pub fn fmt_file_downloading(&self, path: &str) -> String {
        if self.is_ru() {
            format!("Скачивание {path}")
        } else {
            format!("Downloading {path}")
        }
    }

    pub fn fmt_file_downloaded(&self, path: &str, total: u64, resumed_from: u64) -> String {
        if self.is_ru() {
            format!("Скачано {path} ({total} байт, докачка с {resumed_from})")
        } else {
            format!("Downloaded {path} ({total} bytes, resumed from {resumed_from})")
        }
    }

    pub fn fmt_file_uploading(&self, path: &str) -> String {
        if self.is_ru() {
            format!("Загрузка {path}")
        } else {
            format!("Uploading {path}")
        }
    }

    pub fn fmt_file_uploaded(&self, path: &str, total: u64, resumed_from: u64) -> String {
        if self.is_ru() {
            format!("Загружено {path} ({total} байт, докачка с {resumed_from})")
        } else {
            format!("Uploaded {path} ({total} bytes, resumed from {resumed_from})")
        }
    }

    pub fn fmt_err_host_runtime(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось запустить среду выполнения хоста: {err}")
        } else {
            format!("cannot start host runtime: {err}")
        }
    }

    pub fn fmt_err_host_start(&self, err: &str) -> String {
        if self.is_ru() {
            format!("не удалось запустить локальный хост: {err}")
        } else {
            format!("failed to start local host: {err}")
        }
    }

    pub fn build_with_rendezvous(&self, build_display: &str, rendezvous: &str) -> String {
        if self.is_ru() {
            format!("{build_display}  ·  Рандеву: {rendezvous}")
        } else {
            format!("{build_display}  ·  Rendezvous: {rendezvous}")
        }
    }

    pub fn fmt_update_available(&self, latest: &str, current: &str) -> String {
        if self.is_ru() {
            format!("Доступно обновление: v{latest} (текущая v{current})")
        } else {
            format!("Update available: v{latest} (current v{current})")
        }
    }

    pub fn fmt_update_available_short(&self, latest: &str) -> String {
        if self.is_ru() {
            format!("Доступна v{latest}")
        } else {
            format!("v{latest} available")
        }
    }

    pub fn fmt_update_error(&self, err: &str) -> String {
        if self.is_ru() {
            format!("Ошибка обновления: {err}")
        } else {
            format!("Update error: {err}")
        }
    }

    pub fn fmt_update_downloading_bytes(&self, downloaded: u64, total: Option<u64>) -> String {
        let dl = self.format_size(downloaded);
        match total {
            Some(tot) if tot > 0 => {
                let pct = ((downloaded as f64 / tot as f64) * 100.0).clamp(0.0, 100.0);
                let tot_s = self.format_size(tot);
                if self.is_ru() {
                    format!("Загрузка обновления: {dl} / {tot_s} ({pct:.0}%)")
                } else {
                    format!("Downloading update: {dl} / {tot_s} ({pct:.0}%)")
                }
            }
            _ => {
                if self.is_ru() {
                    format!("Загрузка обновления: {dl}")
                } else {
                    format!("Downloading update: {dl}")
                }
            }
        }
    }

    pub fn fmt_chat_with_unread(&self, base: &str, unread: usize) -> String {
        if unread == 0 {
            base.to_string()
        } else {
            format!("{base} ({unread})")
        }
    }

    pub fn fmt_power_confirm_target(&self, host: &str, action_label: &str) -> String {
        let h = if host.trim().is_empty() {
            if self.is_ru() { "удалённый хост" } else { "remote host" }
        } else {
            host.trim()
        };
        if self.is_ru() {
            format!("Действие «{action_label}» для {h}")
        } else {
            format!("{action_label} on {h}")
        }
    }

    pub fn fmt_reconnect_target(&self, target: &str) -> String {
        if self.is_ru() {
            format!("Хост: {target}")
        } else {
            format!("Target: {target}")
        }
    }

    pub fn fmt_reconnect_progress(&self, attempt: u32, next_retry_secs: u64, elapsed_secs: u64, max_secs: u64) -> String {
        let el_m = elapsed_secs / 60;
        let el_s = elapsed_secs % 60;
        let max_m = max_secs / 60;
        let max_s = max_secs % 60;
        if self.is_ru() {
            if next_retry_secs > 0 {
                format!("Попытка #{attempt} · повтор через {next_retry_secs}с · {el_m}:{el_s:02} / {max_m}:{max_s:02}")
            } else {
                format!("Попытка #{attempt} · подключение… · {el_m}:{el_s:02} / {max_m}:{max_s:02}")
            }
        } else if next_retry_secs > 0 {
            format!("Attempt #{attempt} · retry in {next_retry_secs}s · {el_m}:{el_s:02} / {max_m}:{max_s:02}")
        } else {
            format!("Attempt #{attempt} · connecting… · {el_m}:{el_s:02} / {max_m}:{max_s:02}")
        }
    }

    pub fn fmt_power_failed(&self, err: &str) -> String {
        if self.is_ru() {
            format!("Ошибка управления питанием: {err}")
        } else {
            format!("Remote power action failed: {err}")
        }
    }

    pub fn fmt_host_mic_active(&self, name: &str) -> String {
        if self.is_ru() {
            format!("Виртуальный микрофон активен: {name} — выберите «CABLE Output» в Discord")
        } else {
            format!("Virtual microphone active: {name} — select \"CABLE Output\" in Discord")
        }
    }

    pub fn fmt_vbcable_installed(&self, name: Option<&str>) -> String {
        match name.filter(|s| !s.trim().is_empty()) {
            Some(dev) => {
                if self.is_ru() {
                    format!("Драйвер установлен ({dev}). {}", self.vbcable_post_install_hint)
                } else {
                    format!("Driver installed ({dev}). {}", self.vbcable_post_install_hint)
                }
            }
            None => self.vbcable_post_install_hint.to_string(),
        }
    }

    pub fn fmt_vbcable_error(&self, err: &str) -> String {
        if self.is_ru() {
            format!("Ошибка установки VB-CABLE: {err}")
        } else {
            format!("VB-CABLE install error: {err}")
        }
    }
}

#[cfg(test)]
mod tests {
    use veloxdesk_protocol::control::{ChatMessage, DisplayInfo, FileEntry, Permissions, SystemPowerAction};

    use super::*;
    use crate::auth::{AcceptPrompt, IncomingSessionInfo, PasswordPrompt};
    use crate::chat::ChatWindow;
    use crate::connect::{ConnectScreen, TailscaleInfo, TailscalePeer, parse_host_port_i18n, parse_target_i18n};
    use crate::files::FileManagerWindow;
    use crate::power::{PowerConfirmDialog, ReconnectBannerState, ReconnectOverlay};
    use crate::session::{SessionInfo, SessionToolbar, settings_window_at};
    use crate::settings::{FpsCap, Mode, Present, Quality, Scale, Settings};
    use crate::terminal::TerminalWindow;
    use crate::tunnels::{ActiveTunnel, TunnelsWindow, parse_tunnel_spec_i18n};

    const FORBIDDEN_MISSING_GLYPHS: [char; 8] = [
        '\u{25CF}', '\u{2715}', '\u{2699}', '\u{25BE}', '\u{25B4}', '\u{2B06}', '\u{1F4C1}', '\u{2192}',
    ];

    fn verify_text_shapes_have_glyphs(ctx: &egui::Context, shape: &egui::epaint::Shape) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    verify_text_shapes_have_glyphs(ctx, s);
                }
            }
            egui::epaint::Shape::Text(ts) => {
                for section in &ts.galley.job.sections {
                    let range = section.byte_range.start.0..section.byte_range.end.0;
                    let slice = &ts.galley.job.text[range];
                    let printable: String = slice.chars().filter(|c| !c.is_control()).collect();
                    let ok = ctx.fonts_mut(|f| f.has_glyphs(&section.format.font_id, &printable));
                    assert!(ok, "missing glyph in rendered text section: {slice:?}");
                }
            }
            _ => {}
        }
    }

    fn collect_text(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_text(s, out);
                }
            }
            egui::epaint::Shape::Text(ts) => {
                out.push(ts.galley.text().to_string());
            }
            _ => {}
        }
    }

    #[test]
    fn i18n_catalog_completeness_and_no_forbidden_glyphs() {
        let ctx = egui::Context::default();
        // Run one minimal frame so default fonts are loaded into FontDefinitions.
        let mut init_out = ctx.run_ui(egui::RawInput::default(), |_| {});
        init_out.textures_delta.clear();

        for lang in Language::ALL {
            let t = tr(lang);
            let fields = t.all_fields();
            assert!(!fields.is_empty());
            for (name, val) in &fields {
                assert!(!val.trim().is_empty(), "empty i18n field {name} for {lang:?}");
                for bad in FORBIDDEN_MISSING_GLYPHS {
                    assert!(
                        !val.contains(bad),
                        "forbidden missing-glyph char U+{:04X} in {lang:?}.{name}: {val:?}",
                        bad as u32
                    );
                }
                assert!(
                    ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), val)),
                    "missing proportional glyph in {lang:?}.{name}: {val:?}"
                );
                assert!(
                    ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::monospace(12.5), val)),
                    "missing monospace glyph in {lang:?}.{name}: {val:?}"
                );
            }
        }

        // Verify dynamic helpers on RU have no forbidden glyphs and full font coverage.
        let ru = tr(Language::Ru);
        let d0 = DisplayInfo { index: 0, name: "".into(), width: 1920, height: 1080, primary: true, ..Default::default() };
        let samples = vec![
            ru.version_with_rendezvous("0.1.0", "rdv.example.com:21116"),
            ru.active_sessions_pill(2),
            ru.host_status_line(ru.host_listening),
            ru.display_label(&d0),
            ru.toolbar_pads(1),
            ru.host_id_line("123 456 789"),
            ru.device_id_line("987 654 321"),
            ru.key_fingerprint_line("abcd1234"),
            ru.ed25519_fingerprint_line("ef567890"),
            ru.format_size(512),
            ru.format_size(2048),
            ru.format_size(3 * 1024 * 1024),
            ru.format_size(2 * 1024 * 1024 * 1024),
            ru.terminal_exit_code(0),
            parse_target_i18n("", Language::Ru).unwrap_err(),
            parse_host_port_i18n("", 21118, Language::Ru).unwrap_err(),
            parse_tunnel_spec_i18n("bad", Language::Ru).unwrap_err(),
        ];
        for s in &samples {
            assert!(!s.is_empty());
            assert!(ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::proportional(14.0), s)));
            assert!(ctx.fonts_mut(|f| f.has_glyphs(&egui::FontId::monospace(12.5), s)));
        }

        for lang in Language::ALL {
            for m in [Mode::Desktop, Mode::Gaming] {
                assert!(!m.label_for(lang).is_empty());
            }
            for q in Quality::ALL {
                assert!(!q.label_for(lang).is_empty());
            }
            for p in Present::ALL {
                assert!(!p.label_for(lang).is_empty());
            }
            for s in Scale::ALL {
                assert!(!s.label_for(lang).is_empty());
            }
            for f in FpsCap::ALL {
                assert!(!f.label_for(lang).is_empty());
            }
            for th in crate::settings::ThemePreference::ALL {
                assert!(!th.label_for(lang).is_empty());
            }
            for lat in crate::settings::AudioFrameLatency::ALL {
                assert!(!lat.label_for(lang).is_empty());
            }
            for pm in crate::settings::HostPrivacyMode::ALL {
                assert!(!pm.label_for(lang).is_empty());
            }
        }
    }

    #[test]
    fn all_ui_screens_render_ru_without_missing_glyphs() {
        let ctx = egui::Context::default();
        let make_input = || egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            ..Default::default()
        };

        let mut settings = Settings { language: Language::Ru, ..Default::default() };
        settings.recent.push("10.0.0.5:21118".into());
        settings.rendezvous_server = "127.0.0.1:21116".into();

        let mut connect = ConnectScreen::new(&settings);
        connect.device_id = "123456789".into();
        connect.host_running = true;
        connect.active_sessions = 1;
        connect.host_otp = "AB12CD34".into();
        connect.tailscale = TailscaleInfo {
            installed: true,
            running: true,
            self_ip: Some("100.64.1.10".into()),
            self_name: Some("my-pc".into()),
            peers: vec![TailscalePeer {
                name: "gaming-rig".into(),
                ip: "100.64.1.20".into(),
                os: "windows".into(),
                online: true,
            }],
            error: None,
        };

        let info = SessionInfo {
            host_name: "host".into(),
            host_os: "android".into(),
            displays: vec![
                DisplayInfo { index: 0, name: "A".into(), width: 1920, height: 1080, primary: true, ..Default::default() },
                DisplayInfo { index: 1, name: "B".into(), width: 2560, height: 1440, ..Default::default() },
            ],
            active_display: 0,
            hud_visible: true,
            pointer_locked: true,
            active_gamepads: 2,
            privacy_mode: false,
            files_open: true,
            terminal_open: true,
            tunnels_open: true,
            chat_open: true,
            chat_unread: 3,
            allow_chat: true,
            allow_power: true,
            recording_active: true,
            recording_waiting_keyframe: false,
            recording_elapsed_ms: 95_000,
            recording_bytes: 12_400_000,
        };

        let mut toolbar_collapsed = SessionToolbar { expanded: false, ..Default::default() };
        let mut toolbar_expanded = SessionToolbar { expanded: true, ..Default::default() };
        let mut chat = ChatWindow::new("ru-chat", false);
        chat.set_open(true);
        chat.push_incoming(ChatMessage {
            id: 1,
            sender_name: "Хост".into(),
            text: "Привет!".into(),
            ts_ms: 45_000_000,
        });
        chat.push_outgoing(2, "Я".into(), "Ответ".into(), 45_060_000);
        chat.mark_acked(2);
        let power_dlg = PowerConfirmDialog::new(SystemPowerAction::RebootSafeMode, "Хост".into());
        let reconnect_state = ReconnectBannerState {
            active: true,
            target: "10.0.0.5:21118".into(),
            attempt: 2,
            elapsed_secs: 15,
            max_secs: 300,
            next_retry_secs: 4,
            last_error: None,
        };
        let mut settings_open = true;
        let mut files = FileManagerWindow {
            open: true,
            entries: vec![
                FileEntry { name: "subdir".into(), is_dir: true, size: 0, modified_unix: 0 },
                FileEntry { name: "readme.md".into(), is_dir: false, size: 4096, modified_unix: 0 },
            ],
            ..Default::default()
        };
        let mut tunnels = TunnelsWindow {
            open: true,
            active: vec![ActiveTunnel {
                id: 1,
                local_addr: "127.0.0.1:8080".into(),
                remote_host: "127.0.0.1".into(),
                remote_port: 80,
            }],
            ..Default::default()
        };
        let mut pw = PasswordPrompt::new(
            "123456789".into(),
            "0123456789abcdef0123456789abcdef".into(),
            tr(Language::Ru).auth_methods_both.into(),
        );
        let mut accept = AcceptPrompt::new(
            IncomingSessionInfo {
                peer_device_id: "987654321".into(),
                peer_name: "Gaming-PC".into(),
                peer_fingerprint: "abcdef0123456789abcdef0123456789".into(),
                peer_public_key: [7u8; 32],
            },
            Permissions { view: true, input: true, ..Default::default() },
        );
        let mut term = TerminalWindow { open: true, connected: true, ..Default::default() };
        term.append_output(b"VELOXDESK_RU_OK\r\n");

        let mut texts1 = Vec::new();
        for _ in 0..2 {
            let mut out1 = ctx.run_ui(make_input(), |ui| {
                let _ = connect.show(ui, &mut settings, true);
                let _ = toolbar_collapsed.show(ui, &info, &mut settings);
            });
            out1.textures_delta.clear();
            texts1.clear();
            for clipped in &out1.shapes {
                verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
                collect_text(&clipped.shape, &mut texts1);
            }
        }
        assert!(texts1.iter().any(|s| s.contains("Это устройство")));

        let mut texts2 = Vec::new();
        for _ in 0..2 {
            let mut out2 = ctx.run_ui(make_input(), |ui| {
                let _ = toolbar_expanded.show(ui, &info, &mut settings);
                let _ = settings_window_at(ui.ctx(), &mut settings_open, &mut settings, None);
                let _ = files.show_for(ui.ctx(), settings.language);
                let _ = tunnels.show_for(ui.ctx(), settings.language);
                let _ = pw.show_for(ui.ctx(), settings.language);
                let _ = accept.show_for(ui.ctx(), settings.language);
                let _ = term.show_for(ui.ctx(), settings.language);
                let _ = chat.show_for(ui.ctx(), settings.language);
                let _ = power_dlg.show_for(ui.ctx(), settings.language);
                let _ = ReconnectOverlay::show_for(ui.ctx(), &reconnect_state, settings.language);
            });
            out2.textures_delta.clear();
            texts2.clear();
            for clipped in &out2.shapes {
                verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
                collect_text(&clipped.shape, &mut texts2);
            }
        }
        for expected in [
            "Настройки",
            "Требуется аутентификация",
            "Входящее подключение",
            "Файловый менеджер",
            "Удалённый терминал",
            "Проброс TCP-портов",
        ] {
            assert!(
                texts2.iter().any(|s| s.contains(expected)),
                "expected RU heading {expected:?} in rendered output"
            );
        }
    }

    #[test]
    fn settings_all_v060_sections_render_dark_light_en_ru_without_missing_glyphs() {
        use crate::settings::ThemePreference;

        let make_input = |events: Vec<egui::Event>| egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 720.0))),
            events,
            ..Default::default()
        };

        for theme in [ThemePreference::Dark, ThemePreference::Light] {
            for lang in Language::ALL {
                let t = tr(lang);
                let ctx = egui::Context::default();
                ctx.set_theme(theme.to_egui());
                let mut settings = Settings { language: lang, theme, ..Default::default() };
                let mut open = true;

                // Top view (unscrolled): profile reset, Audio & Voice, Recording & Session.
                let mut top_texts = Vec::new();
                for _ in 0..2 {
                    let mut out = ctx.run_ui(make_input(Vec::new()), |ui| {
                        let _ = settings_window_at(ui.ctx(), &mut open, &mut settings, None);
                    });
                    out.textures_delta.clear();
                    top_texts.clear();
                    for clipped in &out.shapes {
                        verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
                        collect_text(&clipped.shape, &mut top_texts);
                    }
                }
                for needle in [
                    t.settings_reset_profile_btn,
                    t.settings_audio_heading,
                    t.settings_noise_suppression,
                    t.settings_audio_frame_latency,
                    t.settings_recording_heading,
                    t.settings_open_folder_btn,
                    t.settings_auto_reconnect,
                ] {
                    assert!(
                        top_texts.iter().any(|s| s.contains(needle)),
                        "missing top settings text {needle:?} for ({theme:?}, {lang:?})"
                    );
                }

                // Bottom view (scrolled): Host Access & Security with permissions, echo exclusion, privacy mode.
                let mut bottom_texts = Vec::new();
                for _ in 0..3 {
                    let mut out = ctx.run_ui(
                        make_input(vec![
                            egui::Event::PointerMoved(egui::pos2(300.0, 300.0)),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point,
                                delta: egui::vec2(0.0, -600.0),
                                modifiers: egui::Modifiers::NONE,
                                phase: egui::TouchPhase::Move,
                                source: egui::MouseWheelSource::Trackpad,
                            },
                        ]),
                        |ui| {
                            let _ = settings_window_at(ui.ctx(), &mut open, &mut settings, None);
                        },
                    );
                    out.textures_delta.clear();
                    bottom_texts.clear();
                    for clipped in &out.shapes {
                        verify_text_shapes_have_glyphs(&ctx, &clipped.shape);
                        collect_text(&clipped.shape, &mut bottom_texts);
                    }
                }
                for needle in [
                    t.settings_security_heading,
                    t.settings_host_permissions,
                    t.host_perm_input,
                    t.host_perm_power,
                    t.settings_host_echo_exclusion_cb,
                    t.settings_host_privacy_mode,
                    t.privacy_mode_curtain,
                ] {
                    assert!(
                        bottom_texts.iter().any(|s| s.contains(needle)),
                        "missing host security text {needle:?} for ({theme:?}, {lang:?})"
                    );
                }
            }
        }
    }
}

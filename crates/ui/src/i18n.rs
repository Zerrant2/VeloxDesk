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
    tip_target_fps: "Target frame rate cap (60–240 FPS) requested from the remote host (applies on reconnect).",
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
    tip_target_fps: "Целевое ограничение частоты кадров (60–240 FPS), запрашиваемое у удалённого хоста (применяется при переподключении).",
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
}

#[cfg(test)]
mod tests {
    use veloxdesk_protocol::control::{DisplayInfo, FileEntry, Permissions};

    use super::*;
    use crate::auth::{AcceptPrompt, IncomingSessionInfo, PasswordPrompt};
    use crate::connect::{ConnectScreen, TailscaleInfo, TailscalePeer, parse_host_port_i18n, parse_target_i18n};
    use crate::files::FileManagerWindow;
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
        };

        let mut toolbar_collapsed = SessionToolbar { expanded: false };
        let mut toolbar_expanded = SessionToolbar { expanded: true };
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
}

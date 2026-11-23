//! Privacy mode controller (physical screen blanking / curtain overlay + local input lock)
//! with hard RAII [`PrivacyGuard`].

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use veloxdesk_core_traits::{CoreError, Result};

/// Physical monitor blanking strategy used by [`PrivacyController`] on Windows (v0.6.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrivacyModeKind {
    /// Black fullscreen topmost curtain overlay per physical monitor with `WDA_EXCLUDEFROMCAPTURE`
    /// (works on Windows 10 2004+ / 11 without VDD and without admin privileges).
    #[default]
    CurtainOverlay,
    /// Curtain overlay plus `SC_MONITORPOWER` blanking when a Virtual Display (VDD) is active.
    VddMonitorOff,
}

#[inline]
fn privacy_mode_to_u8(mode: PrivacyModeKind) -> u8 {
    match mode {
        PrivacyModeKind::CurtainOverlay => 0,
        PrivacyModeKind::VddMonitorOff => 1,
    }
}

#[inline]
fn privacy_mode_from_u8(v: u8) -> PrivacyModeKind {
    match v {
        1 => PrivacyModeKind::VddMonitorOff,
        _ => PrivacyModeKind::CurtainOverlay,
    }
}

/// Controls host privacy mode (hiding physical monitors and optionally blocking local input).
pub trait PrivacyController: Send + Sync {
    fn enable(&self) -> Result<()>;
    fn disable(&self) -> Result<()>;
    fn is_enabled(&self) -> bool;
    /// Configure the active privacy strategy (v0.6.0). Default is a no-op for mock/simple controllers.
    fn set_mode(&self, _mode: PrivacyModeKind) {}
    /// Current privacy strategy (v0.6.0).
    fn mode(&self) -> PrivacyModeKind {
        PrivacyModeKind::CurtainOverlay
    }
}

/// RAII guard for an active privacy-mode session.
///
/// Guarantees [`PrivacyController::disable`] is invoked on [`Drop`] so a session disconnect,
/// cancellation, or panic unwind never leaves the host screen blanked or local input locked.
pub struct PrivacyGuard {
    controller: Arc<dyn PrivacyController>,
    active: bool,
}

impl PrivacyGuard {
    /// Enable privacy mode and return a guard that restores normal state on drop.
    pub fn activate(controller: Arc<dyn PrivacyController>) -> Result<Self> {
        controller.enable()?;
        Ok(Self { controller, active: true })
    }

    pub fn is_active(&self) -> bool {
        self.active && self.controller.is_enabled()
    }

    /// Explicitly disable privacy mode and consume the guard.
    pub fn disable(mut self) -> Result<()> {
        if self.active {
            self.active = false;
            self.controller.disable()?;
        }
        Ok(())
    }
}

impl Drop for PrivacyGuard {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            if let Err(e) = self.controller.disable() {
                tracing::error!("PrivacyGuard::drop failed to restore display/input: {e}");
            }
        }
    }
}

/// Deterministic in-memory [`PrivacyController`] for unit and integration tests.
#[derive(Default)]
pub struct SimulatedPrivacyController {
    enabled: AtomicBool,
    enables: AtomicU64,
    disables: AtomicU64,
    fail_enable: AtomicBool,
    mode: AtomicU8,
}

impl SimulatedPrivacyController {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enable_count(&self) -> u64 {
        self.enables.load(Ordering::Acquire)
    }

    pub fn disable_count(&self) -> u64 {
        self.disables.load(Ordering::Acquire)
    }

    pub fn set_fail_enable(&self, fail: bool) {
        self.fail_enable.store(fail, Ordering::Release);
    }
}

impl PrivacyController for SimulatedPrivacyController {
    fn enable(&self) -> Result<()> {
        if self.fail_enable.load(Ordering::Acquire) {
            return Err(CoreError::AccessDenied("simulated privacy mode denied".into()));
        }
        self.enables.fetch_add(1, Ordering::AcqRel);
        self.enabled.store(true, Ordering::Release);
        Ok(())
    }

    fn disable(&self) -> Result<()> {
        self.disables.fetch_add(1, Ordering::AcqRel);
        self.enabled.store(false, Ordering::Release);
        Ok(())
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }

    fn set_mode(&self, mode: PrivacyModeKind) {
        self.mode.store(privacy_mode_to_u8(mode), Ordering::Release);
    }

    fn mode(&self) -> PrivacyModeKind {
        privacy_mode_from_u8(self.mode.load(Ordering::Acquire))
    }
}

#[cfg(windows)]
pub mod win {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use veloxdesk_core_traits::{CoreError, Result};
    use windows::Win32::Foundation::{
        COLORREF, HANDLE, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM,
    };
    use windows::Win32::Graphics::Gdi::{
        BLACK_BRUSH, BeginPaint, DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DrawTextW,
        EndPaint, EnumDisplayMonitors, FillRect, GetMonitorInfoW, GetStockObject, HBRUSH, HDC,
        HMONITOR, MONITORINFO, PAINTSTRUCT, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
        UpdateWindow,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::StationsAndDesktops::{
        CloseDesktop, DESKTOP_ACCESS_FLAGS, GetUserObjectInformationW, OpenInputDesktop, UOI_NAME,
    };
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::BlockInput;
    use windows::Win32::UI::WindowsAndMessaging::{
        CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW,
        GetClientRect, GetMessageW, GetSystemMetrics, HWND_BROADCAST, HWND_TOPMOST, IsWindow,
        KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostMessageW, PostQuitMessage,
        PostThreadMessageW, RegisterClassExW, SC_MONITORPOWER, SM_CXVIRTUALSCREEN,
        SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SMTO_ABORTIFHUNG,
        SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW,
        SendMessageTimeoutW, SetTimer, SetWindowDisplayAffinity, SetWindowPos, ShowWindow,
        TranslateMessage, UnregisterClassW, WDA_EXCLUDEFROMCAPTURE, WM_CLOSE, WM_ENDSESSION,
        WM_ERASEBKGND, WM_PAINT, WM_QUIT, WM_SYSCOMMAND, WM_TIMER, WNDCLASSEXW, WS_EX_NOACTIVATE,
        WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
    };
    use windows::core::{BOOL, HSTRING, PCWSTR};

    use super::{PrivacyController, PrivacyModeKind, privacy_mode_from_u8, privacy_mode_to_u8};

    const MONITOR_OFF: isize = 2;
    const MONITOR_ON: isize = -1;
    const DESKTOP_WATCH_TIMER_ID: usize = 1;
    const DESKTOP_WATCH_INTERVAL_MS: u32 = 250;
    const DESKTOP_READOBJECTS: u32 = 0x0001;
    const CURTAIN_LABEL: &str = "VeloxDesk Privacy Mode Active";
    const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

    struct DesktopWatchState {
        initial_desktop: Option<String>,
        hwnds: Vec<HWND>,
    }

    thread_local! {
        static CURTAIN_WATCH: RefCell<Option<DesktopWatchState>> = const { RefCell::new(None) };
    }

    struct CurtainStartup {
        thread_id: u32,
        hwnds: Vec<isize>,
    }

    struct CurtainRuntime {
        thread_id: u32,
        hwnds: Vec<isize>,
        thread: Option<JoinHandle<()>>,
    }

    /// Two-tier Windows privacy mode controller:
    ///
    /// 1. **Curtain Overlay** (Windows 10 2004+ / 11, works without VDD and without admin):
    ///    spawns a dedicated Win32 message-loop thread that covers every physical monitor with a
    ///    black topmost overlay window protected by `SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)`,
    ///    so local onlookers see only the black curtain while DXGI Desktop Duplication / WGC
    ///    continue capturing the real desktop underneath.
    /// 2. **Optional hardware layer**:
    ///    calls `BlockInput(true)` (gracefully degrading when non-elevated) and only invokes
    ///    `SC_MONITORPOWER` when a virtual display (VDD) is active (`with_vdd_active` /
    ///    `set_vdd_active`, default `false`).
    pub struct WindowsPrivacyController {
        enabled: Arc<AtomicBool>,
        input_blocked: Arc<AtomicBool>,
        mode: AtomicU8,
        monitor_powered_off: AtomicBool,
        runtime: Mutex<Option<CurtainRuntime>>,
    }

    impl Default for WindowsPrivacyController {
        fn default() -> Self {
            Self::new()
        }
    }

    impl WindowsPrivacyController {
        pub fn new() -> Self {
            Self {
                enabled: Arc::new(AtomicBool::new(false)),
                input_blocked: Arc::new(AtomicBool::new(false)),
                mode: AtomicU8::new(privacy_mode_to_u8(PrivacyModeKind::CurtainOverlay)),
                monitor_powered_off: AtomicBool::new(false),
                runtime: Mutex::new(None),
            }
        }

        /// Builder method selecting the initial [`PrivacyModeKind`] strategy.
        pub fn with_mode(self, mode: PrivacyModeKind) -> Self {
            self.set_mode(mode);
            self
        }

        /// Builder method indicating whether a virtual display (VDD) is currently active.
        ///
        /// When `true`, selects [`PrivacyModeKind::VddMonitorOff`] so `enable()` additionally
        /// sends `SC_MONITORPOWER` to power down physical monitors; when `false` (default),
        /// selects [`PrivacyModeKind::CurtainOverlay`] (capture-excluded curtain overlay + optional
        /// `BlockInput`, no `SC_MONITORPOWER`).
        pub fn with_vdd_active(self, active: bool) -> Self {
            self.set_vdd_active(active);
            self
        }

        /// Update whether a virtual display (VDD) is active (`true` -> `VddMonitorOff`,
        /// `false` -> `CurtainOverlay`).
        pub fn set_vdd_active(&self, active: bool) {
            let mode = if active {
                PrivacyModeKind::VddMonitorOff
            } else {
                PrivacyModeKind::CurtainOverlay
            };
            self.set_mode(mode);
        }

        /// Returns whether VDD-assisted `SC_MONITORPOWER` blanking is enabled.
        pub fn is_vdd_active(&self) -> bool {
            self.mode() == PrivacyModeKind::VddMonitorOff
        }

        /// Number of active curtain overlay windows currently managed by this controller.
        pub fn active_window_count(&self) -> usize {
            if !self.enabled.load(Ordering::Acquire) {
                return 0;
            }
            let guard = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            guard.as_ref().map_or(0, |rt| rt.hwnds.len())
        }

        fn set_monitor_power(state: isize) {
            let mut result = 0usize;
            // SAFETY: non-blocking broadcast with 250 ms timeout so hung windows cannot stall us.
            let _ = unsafe {
                SendMessageTimeoutW(
                    HWND_BROADCAST,
                    WM_SYSCOMMAND,
                    WPARAM(SC_MONITORPOWER as usize),
                    LPARAM(state),
                    SMTO_ABORTIFHUNG,
                    250,
                    Some(&mut result),
                )
            };
        }

        fn query_active_input_desktop() -> Option<String> {
            // SAFETY: OpenInputDesktop/GetUserObjectInformationW/CloseDesktop on stack buffer.
            unsafe {
                let hdesk = OpenInputDesktop(
                    windows::Win32::System::StationsAndDesktops::DESKTOP_CONTROL_FLAGS(0),
                    false,
                    DESKTOP_ACCESS_FLAGS(DESKTOP_READOBJECTS),
                )
                .ok()?;
                let mut buf = [0u16; 128];
                let mut needed = 0u32;
                let ok = GetUserObjectInformationW(
                    HANDLE(hdesk.0),
                    UOI_NAME,
                    Some(buf.as_mut_ptr().cast::<c_void>()),
                    (buf.len() * 2) as u32,
                    Some(&raw mut needed),
                );
                let _ = CloseDesktop(hdesk);
                if ok.is_err() {
                    return None;
                }
                let words = ((needed as usize) / 2).min(buf.len());
                let len = buf[..words].iter().position(|&c| c == 0).unwrap_or(words);
                Some(String::from_utf16_lossy(&buf[..len]))
            }
        }

        fn enumerate_monitor_rects() -> Vec<RECT> {
            unsafe extern "system" fn enum_proc(
                hmon: HMONITOR,
                _hdc: HDC,
                _clip: *mut RECT,
                lparam: LPARAM,
            ) -> BOOL {
                let rects = unsafe { &mut *(lparam.0 as *mut Vec<RECT>) };
                let mut mi = MONITORINFO {
                    cbSize: std::mem::size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
                    rects.push(mi.rcMonitor);
                }
                BOOL(1)
            }

            let mut rects: Vec<RECT> = Vec::new();
            // SAFETY: `enum_proc` mutates `rects` synchronously during `EnumDisplayMonitors`.
            let _ = unsafe {
                EnumDisplayMonitors(
                    None,
                    None,
                    Some(enum_proc),
                    LPARAM((&raw mut rects) as isize),
                )
            };
            if rects.is_empty() {
                // SAFETY: `GetSystemMetrics` is always safe to call.
                let (x, y, w, h) = unsafe {
                    (
                        GetSystemMetrics(SM_XVIRTUALSCREEN),
                        GetSystemMetrics(SM_YVIRTUALSCREEN),
                        GetSystemMetrics(SM_CXVIRTUALSCREEN),
                        GetSystemMetrics(SM_CYVIRTUALSCREEN),
                    )
                };
                if w > 0 && h > 0 {
                    rects.push(RECT { left: x, top: y, right: x + w, bottom: y + h });
                }
            }
            rects
        }

        unsafe extern "system" fn wnd_proc(
            hwnd: HWND,
            msg: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            match msg {
                WM_ERASEBKGND => LRESULT(1),
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    // SAFETY: balanced BeginPaint / EndPaint for this window.
                    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
                    if !hdc.is_invalid() {
                        let mut rc = RECT::default();
                        let _ = unsafe { GetClientRect(hwnd, &mut rc) };
                        let black_brush = HBRUSH(unsafe { GetStockObject(BLACK_BRUSH) }.0);
                        unsafe {
                            let _ = FillRect(hdc, &rc, black_brush);
                            let _ = SetBkMode(hdc, TRANSPARENT);
                            SetTextColor(hdc, COLORREF(0x0066_6666));
                            let font = GetStockObject(DEFAULT_GUI_FONT);
                            let prev_font = SelectObject(hdc, font);
                            let mut label: Vec<u16> = CURTAIN_LABEL.encode_utf16().collect();
                            let _ = DrawTextW(
                                hdc,
                                &mut label,
                                &mut rc,
                                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                            );
                            let _ = SelectObject(hdc, prev_font);
                        }
                    }
                    let _ = unsafe { EndPaint(hwnd, &ps) };
                    LRESULT(0)
                }
                WM_TIMER if wparam.0 == DESKTOP_WATCH_TIMER_ID => {
                    let should_teardown = CURTAIN_WATCH.with(|cell| {
                        let borrow = cell.borrow();
                        let Some(state) = borrow.as_ref() else {
                            return false;
                        };
                        if let Some(initial) = &state.initial_desktop {
                            match Self::query_active_input_desktop() {
                                Some(curr) if curr.eq_ignore_ascii_case(initial) => {
                                    // Keep curtains pinned on top while active on the same desktop.
                                    for &h in &state.hwnds {
                                        let _ = unsafe {
                                            SetWindowPos(
                                                h,
                                                Some(HWND_TOPMOST),
                                                0,
                                                0,
                                                0,
                                                0,
                                                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                                            )
                                        };
                                    }
                                    false
                                }
                                _ => true,
                            }
                        } else {
                            false
                        }
                    });
                    if should_teardown {
                        tracing::info!(
                            "desktop switch detected (Ctrl+Alt+Del / secure desktop); tearing down privacy curtain"
                        );
                        unsafe { PostQuitMessage(0) };
                    }
                    LRESULT(0)
                }
                WM_CLOSE => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                    unsafe { PostQuitMessage(0) };
                    LRESULT(0)
                }
                WM_ENDSESSION => {
                    unsafe { PostQuitMessage(0) };
                    LRESULT(0)
                }
                _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
            }
        }

        fn run_curtain_thread(
            startup_tx: mpsc::SyncSender<Result<CurtainStartup>>,
            enabled: Arc<AtomicBool>,
            input_blocked: Arc<AtomicBool>,
        ) {
            // SAFETY: Win32 thread-local message queue and window creation on this dedicated thread.
            unsafe {
                let tid = GetCurrentThreadId();
                // Ensure the thread message queue exists before signaling readiness so
                // PostThreadMessageW(tid, WM_QUIT, ...) cannot race with queue creation.
                let mut init_msg = MSG::default();
                let _ = PeekMessageW(&mut init_msg, None, 0, 0, PM_NOREMOVE);

                let hmodule = match GetModuleHandleW(PCWSTR::null()) {
                    Ok(m) => m,
                    Err(e) => {
                        let _ = startup_tx.send(Err(CoreError::platform(format!(
                            "GetModuleHandleW failed: {e}"
                        ))));
                        return;
                    }
                };
                let hinstance = HINSTANCE(hmodule.0);
                let class_name = HSTRING::from(format!("VeloxDeskPrivacyCurtain_{tid}"));
                let black_brush = HBRUSH(GetStockObject(BLACK_BRUSH).0);
                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(Self::wnd_proc),
                    hInstance: hinstance,
                    hbrBackground: black_brush,
                    lpszClassName: PCWSTR(class_name.as_ptr()),
                    ..Default::default()
                };
                if RegisterClassExW(&wc) == 0 {
                    let _ = startup_tx.send(Err(CoreError::platform(
                        "RegisterClassExW failed for privacy curtain",
                    )));
                    return;
                }

                let rects = Self::enumerate_monitor_rects();
                if rects.is_empty() {
                    let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));
                    let _ = startup_tx.send(Err(CoreError::platform(
                        "no physical display monitors found for privacy curtain",
                    )));
                    return;
                }

                let window_title = HSTRING::from("VeloxDesk Privacy Curtain");
                let ex_style =
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
                let mut hwnds: Vec<HWND> = Vec::with_capacity(rects.len());

                let mut fail_reason: Option<String> = None;
                for rc in &rects {
                    let width = (rc.right - rc.left).max(1);
                    let height = (rc.bottom - rc.top).max(1);
                    let hwnd = match CreateWindowExW(
                        ex_style,
                        PCWSTR(class_name.as_ptr()),
                        PCWSTR(window_title.as_ptr()),
                        WS_POPUP,
                        rc.left,
                        rc.top,
                        width,
                        height,
                        None,
                        None,
                        Some(hinstance),
                        None,
                    ) {
                        Ok(h) => h,
                        Err(e) => {
                            fail_reason = Some(format!("CreateWindowExW failed: {e}"));
                            break;
                        }
                    };
                    hwnds.push(hwnd);

                    if let Err(e) = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE) {
                        fail_reason = Some(format!(
                            "SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE) failed (requires Windows 10 2004+ / 11): {e}"
                        ));
                        break;
                    }

                    let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                    let _ = SetWindowPos(
                        hwnd,
                        Some(HWND_TOPMOST),
                        rc.left,
                        rc.top,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_SHOWWINDOW,
                    );
                    let _ = UpdateWindow(hwnd);
                }

                if let Some(reason) = fail_reason {
                    for h in hwnds {
                        if IsWindow(Some(h)).as_bool() {
                            let _ = DestroyWindow(h);
                        }
                    }
                    let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));
                    let _ = startup_tx.send(Err(CoreError::platform(reason)));
                    return;
                }

                let initial_desktop = Self::query_active_input_desktop();
                let timer_hwnd = hwnds[0];
                let timer_installed =
                    SetTimer(Some(timer_hwnd), DESKTOP_WATCH_TIMER_ID, DESKTOP_WATCH_INTERVAL_MS, None) != 0;
                CURTAIN_WATCH.with(|cell| {
                    *cell.borrow_mut() = Some(DesktopWatchState {
                        initial_desktop,
                        hwnds: hwnds.clone(),
                    });
                });

                let raw_hwnds: Vec<isize> = hwnds.iter().map(|h| h.0 as isize).collect();
                if startup_tx.send(Ok(CurtainStartup { thread_id: tid, hwnds: raw_hwnds })).is_err() {
                    // Caller vanished before receiving startup confirmation; tear down immediately.
                    if timer_installed {
                        let _ = KillTimer(Some(timer_hwnd), DESKTOP_WATCH_TIMER_ID);
                    }
                    CURTAIN_WATCH.with(|cell| {
                        *cell.borrow_mut() = None;
                    });
                    for h in hwnds {
                        if IsWindow(Some(h)).as_bool() {
                            let _ = DestroyWindow(h);
                        }
                    }
                    let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));
                    return;
                }

                let mut msg = MSG::default();
                loop {
                    let ret = GetMessageW(&mut msg, None, 0, 0).0;
                    if ret <= 0 {
                        break;
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }

                if timer_installed && IsWindow(Some(timer_hwnd)).as_bool() {
                    let _ = KillTimer(Some(timer_hwnd), DESKTOP_WATCH_TIMER_ID);
                }
                CURTAIN_WATCH.with(|cell| {
                    *cell.borrow_mut() = None;
                });
                for h in hwnds {
                    if IsWindow(Some(h)).as_bool() {
                        let _ = DestroyWindow(h);
                    }
                }
                let _ = UnregisterClassW(PCWSTR(class_name.as_ptr()), Some(hinstance));

                if input_blocked.swap(false, Ordering::AcqRel) {
                    let _ = BlockInput(false);
                }
                enabled.store(false, Ordering::Release);
            }
        }
    }

    impl PrivacyController for WindowsPrivacyController {
        fn enable(&self) -> Result<()> {
            let mut guard = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            if self.enabled.load(Ordering::Acquire) && guard.is_some() {
                return Ok(());
            }
            // Clean up any stale runtime (e.g., curtain exited after a desktop switch).
            if let Some(mut stale) = guard.take()
                && let Some(t) = stale.thread.take()
            {
                let _ = t.join();
            }

            let (startup_tx, startup_rx) = mpsc::sync_channel::<Result<CurtainStartup>>(1);
            let enabled_bg = self.enabled.clone();
            let input_blocked_bg = self.input_blocked.clone();
            let thread = std::thread::Builder::new()
                .name("vdk-privacy-curtain".into())
                .spawn(move || Self::run_curtain_thread(startup_tx, enabled_bg, input_blocked_bg))
                .map_err(CoreError::Io)?;

            let startup = match startup_rx.recv_timeout(STARTUP_TIMEOUT) {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    let _ = thread.join();
                    self.enabled.store(false, Ordering::Release);
                    return Err(e);
                }
                Err(_) => {
                    self.enabled.store(false, Ordering::Release);
                    return Err(CoreError::Timeout);
                }
            };

            // Optional layer 2a: BlockInput (requires elevation; degrade gracefully if denied).
            // SAFETY: Win32 BlockInput toggles local mouse/keyboard input lock.
            match unsafe { BlockInput(true) } {
                Ok(()) => self.input_blocked.store(true, Ordering::Release),
                Err(e) => {
                    tracing::warn!(
                        "BlockInput(true) unavailable (likely non-elevated): {e}; privacy curtain remains active"
                    );
                    self.input_blocked.store(false, Ordering::Release);
                }
            }

            // Optional layer 2b: SC_MONITORPOWER ONLY when PrivacyModeKind::VddMonitorOff is selected.
            if self.mode() == PrivacyModeKind::VddMonitorOff {
                Self::set_monitor_power(MONITOR_OFF);
                self.monitor_powered_off.store(true, Ordering::Release);
            } else {
                self.monitor_powered_off.store(false, Ordering::Release);
            }

            tracing::info!(
                mode = ?self.mode(),
                curtains = startup.hwnds.len(),
                input_blocked = self.input_blocked.load(Ordering::Acquire),
                vdd_power_off = self.monitor_powered_off.load(Ordering::Acquire),
                "Windows privacy mode enabled"
            );

            self.enabled.store(true, Ordering::Release);
            *guard = Some(CurtainRuntime {
                thread_id: startup.thread_id,
                hwnds: startup.hwnds,
                thread: Some(thread),
            });
            Ok(())
        }

        fn disable(&self) -> Result<()> {
            let runtime = {
                let mut guard = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                guard.take()
            };

            if self.input_blocked.swap(false, Ordering::AcqRel) {
                // SAFETY: restore local input blocked by enable().
                let _ = unsafe { BlockInput(false) };
            }
            if self.monitor_powered_off.swap(false, Ordering::AcqRel) {
                Self::set_monitor_power(MONITOR_ON);
            }
            self.enabled.store(false, Ordering::Release);

            if let Some(mut rt) = runtime {
                // SAFETY: post WM_CLOSE to each curtain window and WM_QUIT to the curtain loop thread.
                unsafe {
                    for &raw_hwnd in &rt.hwnds {
                        let hwnd = HWND(raw_hwnd as *mut c_void);
                        if IsWindow(Some(hwnd)).as_bool() {
                            let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
                        }
                    }
                    let _ = PostThreadMessageW(rt.thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
                if let Some(t) = rt.thread.take() {
                    let _ = t.join();
                }
                tracing::info!("Windows privacy mode disabled");
            }
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            self.enabled.load(Ordering::Acquire)
        }

        fn set_mode(&self, mode: PrivacyModeKind) {
            self.mode.store(privacy_mode_to_u8(mode), Ordering::Release);
            if self.is_enabled() {
                match mode {
                    PrivacyModeKind::CurtainOverlay => {
                        if self.monitor_powered_off.swap(false, Ordering::AcqRel) {
                            Self::set_monitor_power(MONITOR_ON);
                        }
                    }
                    PrivacyModeKind::VddMonitorOff => {
                        if !self.monitor_powered_off.swap(true, Ordering::AcqRel) {
                            Self::set_monitor_power(MONITOR_OFF);
                        }
                    }
                }
            }
        }

        fn mode(&self) -> PrivacyModeKind {
            privacy_mode_from_u8(self.mode.load(Ordering::Acquire))
        }
    }

    impl Drop for WindowsPrivacyController {
        fn drop(&mut self) {
            let has_runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner()).is_some();
            if self.is_enabled() || has_runtime {
                let _ = self.disable();
            }
        }
    }
}

#[cfg(target_os = "linux")]
pub mod linux {
    use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

    use veloxdesk_core_traits::{CoreError, Result};

    use super::{PrivacyController, PrivacyModeKind, privacy_mode_from_u8, privacy_mode_to_u8};

    /// Linux DPMS privacy controller interface.
    #[derive(Default)]
    pub struct LinuxPrivacyController {
        enabled: AtomicBool,
        mode: AtomicU8,
    }

    impl LinuxPrivacyController {
        pub fn new() -> Self {
            Self::default()
        }
    }

    impl PrivacyController for LinuxPrivacyController {
        fn enable(&self) -> Result<()> {
            Err(CoreError::unsupported("Linux DPMS privacy mode requires an active X11/Wayland compositor hook"))
        }

        fn disable(&self) -> Result<()> {
            self.enabled.store(false, Ordering::Release);
            Ok(())
        }

        fn is_enabled(&self) -> bool {
            self.enabled.load(Ordering::Acquire)
        }

        fn set_mode(&self, mode: PrivacyModeKind) {
            self.mode.store(privacy_mode_to_u8(mode), Ordering::Release);
        }

        fn mode(&self) -> PrivacyModeKind {
            privacy_mode_from_u8(self.mode.load(Ordering::Acquire))
        }
    }
}

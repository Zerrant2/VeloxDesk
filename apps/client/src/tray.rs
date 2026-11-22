//! Lightweight system tray icon (Win32 `Shell_NotifyIconW` on Windows, clean no-op elsewhere).

/// User action chosen from the system tray icon or its context menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayAction {
    ShowWindow,
    CopyDeviceId,
    RotateOtp,
    Quit,
}

#[cfg(windows)]
pub use win::TrayHandle;

#[cfg(not(windows))]
pub struct TrayHandle;

#[cfg(not(windows))]
impl TrayHandle {
    pub fn spawn(
        _tooltip: String,
        _proxy: veloxdesk_render::winit::event_loop::EventLoopProxy<crate::app::UserEvent>,
    ) -> Option<Self> {
        Some(Self)
    }
}

#[cfg(windows)]
mod win {
    use std::ptr::{null, null_mut};
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};

    use veloxdesk_render::winit::event_loop::EventLoopProxy;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::Shell::{
        NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, GWLP_USERDATA,
        GetCursorPos, GetMessageW, GetWindowLongPtrW, IDI_APPLICATION, LoadIconW, MF_SEPARATOR, MF_STRING, MSG,
        PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TPM_NONOTIFY,
        TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, UnregisterClassW, WM_APP, WM_CLOSE,
        WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW,
    };

    use super::TrayAction;
    use crate::app::UserEvent;

    const WM_TRAY_CALLBACK: u32 = WM_APP + 1;
    const TRAY_UID: u32 = 1;
    const ID_SHOW: usize = 1001;
    const ID_COPY_ID: usize = 1002;
    const ID_ROTATE_OTP: usize = 1003;
    const ID_QUIT: usize = 1004;

    struct TrayState {
        proxy: EventLoopProxy<UserEvent>,
    }

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn copy_tip(dst: &mut [u16; 128], tip: &str) {
        for (slot, ch) in dst.iter_mut().take(127).zip(tip.encode_utf16()) {
            *slot = ch;
        }
    }

    fn show_context_menu(hwnd: HWND, proxy: &EventLoopProxy<UserEvent>) {
        // SAFETY: standard Win32 popup menu lifecycle on our thread-owned hidden tray HWND.
        unsafe {
            let hmenu = CreatePopupMenu();
            if hmenu.is_null() {
                return;
            }
            let lbl_show = to_wide("Show Window");
            let lbl_copy = to_wide("Copy Device ID");
            let lbl_otp = to_wide("Rotate OTP");
            let lbl_quit = to_wide("Quit");
            AppendMenuW(hmenu, MF_STRING, ID_SHOW, lbl_show.as_ptr());
            AppendMenuW(hmenu, MF_STRING, ID_COPY_ID, lbl_copy.as_ptr());
            AppendMenuW(hmenu, MF_STRING, ID_ROTATE_OTP, lbl_otp.as_ptr());
            AppendMenuW(hmenu, MF_SEPARATOR, 0, null());
            AppendMenuW(hmenu, MF_STRING, ID_QUIT, lbl_quit.as_ptr());

            let mut pt = POINT { x: 0, y: 0 };
            let _ = GetCursorPos(&raw mut pt);
            let _ = SetForegroundWindow(hwnd);
            let cmd = TrackPopupMenu(
                hmenu,
                TPM_RIGHTBUTTON | TPM_NONOTIFY | TPM_RETURNCMD,
                pt.x,
                pt.y,
                0,
                hwnd,
                null(),
            );
            let _ = DestroyMenu(hmenu);
            let _ = PostMessageW(hwnd, WM_NULL, 0, 0);

            let action = match cmd as usize {
                ID_SHOW => Some(TrayAction::ShowWindow),
                ID_COPY_ID => Some(TrayAction::CopyDeviceId),
                ID_ROTATE_OTP => Some(TrayAction::RotateOtp),
                ID_QUIT => Some(TrayAction::Quit),
                _ => None,
            };
            if let Some(a) = action {
                let _ = proxy.send_event(UserEvent::Tray(a));
            }
        }
    }

    unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        // SAFETY: `GWLP_USERDATA` holds a heap-allocated `TrayState` pointer installed after window creation
        // and reclaimed on `WM_DESTROY`.
        unsafe {
            match msg {
                WM_TRAY_CALLBACK => {
                    let event = (lparam & 0xffff) as u32;
                    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const TrayState;
                    if !ptr.is_null() {
                        let state = &*ptr;
                        match event {
                            WM_LBUTTONDBLCLK | WM_LBUTTONUP => {
                                let _ = state.proxy.send_event(UserEvent::Tray(TrayAction::ShowWindow));
                            }
                            WM_RBUTTONUP | WM_CONTEXTMENU => {
                                show_context_menu(hwnd, &state.proxy);
                            }
                            _ => {}
                        }
                    }
                    0
                }
                WM_DESTROY => {
                    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
                    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
                    nid.hWnd = hwnd;
                    nid.uID = TRAY_UID;
                    let _ = Shell_NotifyIconW(NIM_DELETE, &raw const nid);

                    let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *mut TrayState;
                    if !ptr.is_null() {
                        drop(Box::from_raw(ptr));
                    }
                    PostQuitMessage(0);
                    0
                }
                _ => DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        }
    }

    /// Background Win32 system tray icon handle. Removes the icon cleanly on [`Drop`].
    pub struct TrayHandle {
        hwnd: isize,
        thread: Option<JoinHandle<()>>,
    }

    impl TrayHandle {
        pub fn spawn(tooltip: String, proxy: EventLoopProxy<UserEvent>) -> Option<Self> {
            let (tx, rx) = mpsc::channel::<Option<isize>>();
            let thread = thread::Builder::new()
                .name("veloxdesk-tray".into())
                .spawn(move || run_tray_thread(tooltip, proxy, tx))
                .ok()?;
            match rx.recv() {
                Ok(Some(hwnd)) => Some(Self { hwnd, thread: Some(thread) }),
                _ => {
                    let _ = thread.join();
                    None
                }
            }
        }
    }

    impl Drop for TrayHandle {
        fn drop(&mut self) {
            if self.hwnd != 0 {
                // SAFETY: `self.hwnd` is our hidden tray window; `WM_CLOSE` triggers `DefWindowProcW` ->
                // `DestroyWindow` -> `WM_DESTROY` -> `Shell_NotifyIconW(NIM_DELETE)` + `PostQuitMessage(0)`.
                unsafe {
                    let _ = PostMessageW(self.hwnd as HWND, WM_CLOSE, 0, 0);
                }
            }
            if let Some(t) = self.thread.take() {
                let _ = t.join();
            }
        }
    }

    fn run_tray_thread(tooltip: String, proxy: EventLoopProxy<UserEvent>, ready: mpsc::Sender<Option<isize>>) {
        // SAFETY: thread-local hidden Win32 window + `Shell_NotifyIconW(NIM_ADD)` + message pump.
        unsafe {
            let hinst = GetModuleHandleW(null());
            let class_name = to_wide("VeloxDeskTrayClass");
            let mut wc: WNDCLASSW = std::mem::zeroed();
            wc.lpfnWndProc = Some(wnd_proc);
            wc.hInstance = hinst;
            wc.lpszClassName = class_name.as_ptr();
            let atom = RegisterClassW(&raw const wc);
            if atom == 0 {
                tracing::warn!("tray RegisterClassW failed");
                let _ = ready.send(None);
                return;
            }

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                class_name.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                null_mut(),
                null_mut(),
                hinst,
                null(),
            );
            if hwnd.is_null() {
                let _ = UnregisterClassW(class_name.as_ptr(), hinst);
                let _ = ready.send(None);
                return;
            }

            let state_ptr = Box::into_raw(Box::new(TrayState { proxy }));
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);

            let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
            nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            nid.hWnd = hwnd;
            nid.uID = TRAY_UID;
            nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            nid.uCallbackMessage = WM_TRAY_CALLBACK;
            nid.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);
            copy_tip(&mut nid.szTip, &tooltip);

            if Shell_NotifyIconW(NIM_ADD, &raw const nid) == 0 {
                tracing::warn!("tray Shell_NotifyIconW(NIM_ADD) failed");
                let _ = PostMessageW(hwnd, WM_CLOSE, 0, 0);
                let _ = ready.send(None);
            } else {
                let _ = ready.send(Some(hwnd as isize));
            }

            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&raw mut msg, null_mut(), 0, 0) > 0 {
                let _ = TranslateMessage(&raw const msg);
                DispatchMessageW(&raw const msg);
            }

            let _ = UnregisterClassW(class_name.as_ptr(), hinst);
        }
    }
}

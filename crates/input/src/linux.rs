//! Linux backend: two `/dev/uinput` virtual devices — a keyboard + relative mouse and an
//! absolute pointer (tablet-like, `INPUT_PROP_DIRECT`) spanning the virtual desktop with a
//! 0..=65535 range. Works on X11 and Wayland compositors (needs write access to
//! `/dev/uinput`, e.g. the `input` group or a udev rule).
//!
//! Text input: uinput can only press keys, so `TextInput` is typed through a US-layout
//! reverse map for printable ASCII; other characters are skipped (a libei / XTest path
//! for arbitrary Unicode is planned with the M4 service).

use std::ffi::CString;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use veloxdesk_core_traits::input::{
    DisplayGeometry, InputCaps, InputEvent, InputFeedback, InputInjector, MouseButton,
};
use veloxdesk_core_traits::{CoreError, Result};

use crate::core::{Action, InjectorCore, VirtualBounds};
use crate::gamepad::{GamepadDriver, GamepadRouter};
use crate::gamepad_linux::UinputGamepadDriver;
use crate::keymap::{hid_to_evdev, EVDEV_MAX_KEY};

// linux/input-event-codes.h
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_REL: u16 = 0x02;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0;
const REL_X: u16 = 0x00;
const REL_Y: u16 = 0x01;
const REL_HWHEEL: u16 = 0x06;
const REL_WHEEL: u16 = 0x08;
const REL_WHEEL_HI_RES: u16 = 0x0B;
const REL_HWHEEL_HI_RES: u16 = 0x0C;
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const BTN_TOUCH: u16 = 0x14A;
const INPUT_PROP_DIRECT: u16 = 0x01;
const BUS_VIRTUAL: u16 = 0x06;

// linux/uinput.h ioctls (generic _IOC encoding: x86, arm, riscv).
const fn ioc(dir: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((b'U' as u64) << 8) | nr
}
const IOC_NONE: u64 = 0;
const IOC_WRITE: u64 = 1;
const UI_DEV_CREATE: u64 = ioc(IOC_NONE, 1, 0);
const UI_DEV_DESTROY: u64 = ioc(IOC_NONE, 2, 0);
const UI_DEV_SETUP: u64 = ioc(IOC_WRITE, 3, size_of::<UinputSetup>() as u64);
const UI_ABS_SETUP: u64 = ioc(IOC_WRITE, 4, size_of::<UinputAbsSetup>() as u64);
const UI_SET_EVBIT: u64 = ioc(IOC_WRITE, 100, 4);
const UI_SET_KEYBIT: u64 = ioc(IOC_WRITE, 101, 4);
const UI_SET_RELBIT: u64 = ioc(IOC_WRITE, 102, 4);
const UI_SET_ABSBIT: u64 = ioc(IOC_WRITE, 103, 4);
const UI_SET_PROPBIT: u64 = ioc(IOC_WRITE, 110, 4);

#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}

#[repr(C)]
struct UinputSetup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

#[repr(C)]
struct InputAbsinfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
struct UinputAbsSetup {
    code: u16,
    absinfo: InputAbsinfo,
}

fn last_err(what: &str) -> CoreError {
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::PermissionDenied {
        CoreError::AccessDenied(format!("{what}: {e} (need write access to /dev/uinput)").into())
    } else {
        CoreError::platform(format!("{what}: {e}"))
    }
}

/// One uinput device.
struct UinputDevice {
    fd: OwnedFd,
    pending: Vec<libc::input_event>,
}

impl UinputDevice {
    fn open() -> Result<Self> {
        let path = CString::new("/dev/uinput").expect("static path");
        // SAFETY: valid NUL-terminated path.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(last_err("open /dev/uinput"));
        }
        // SAFETY: fd is a freshly opened, owned descriptor.
        Ok(Self { fd: unsafe { OwnedFd::from_raw_fd(fd) }, pending: Vec::with_capacity(32) })
    }

    fn ioctl_int(&self, req: u64, val: u16) -> Result<()> {
        // SAFETY: uinput SET_*BIT ioctls take an int by value.
        let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), req as _, val as libc::c_int) };
        if r < 0 { Err(last_err("uinput ioctl")) } else { Ok(()) }
    }

    fn ioctl_ptr<T>(&self, req: u64, arg: &T) -> Result<()> {
        // SAFETY: `arg` matches the struct size encoded in `req`.
        let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), req as _, arg as *const T) };
        if r < 0 { Err(last_err("uinput ioctl")) } else { Ok(()) }
    }

    fn create(&self, name: &str, product: u16) -> Result<()> {
        let mut setup = UinputSetup {
            id: InputId { bustype: BUS_VIRTUAL, vendor: 0x5644, product, version: 1 },
            name: [0; 80],
            ff_effects_max: 0,
        };
        let n = name.len().min(79);
        setup.name[..n].copy_from_slice(&name.as_bytes()[..n]);
        self.ioctl_ptr(UI_DEV_SETUP, &setup)?;
        // SAFETY: UI_DEV_CREATE takes no argument.
        let r = unsafe { libc::ioctl(self.fd.as_raw_fd(), UI_DEV_CREATE as _) };
        if r < 0 { Err(last_err("UI_DEV_CREATE")) } else { Ok(()) }
    }

    fn push(&mut self, kind: u16, code: u16, value: i32) {
        // SAFETY: input_event is plain data; zero is a valid timestamp (kernel fills it).
        let mut ev: libc::input_event = unsafe { zeroed() };
        ev.type_ = kind;
        ev.code = code;
        ev.value = value;
        self.pending.push(ev);
    }

    fn sync(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        self.push(EV_SYN, SYN_REPORT, 0);
        let bytes = self.pending.len() * size_of::<libc::input_event>();
        // SAFETY: pending is a contiguous slice of input_event of `bytes` length.
        let w = unsafe { libc::write(self.fd.as_raw_fd(), self.pending.as_ptr().cast(), bytes) };
        self.pending.clear();
        if w < 0 { Err(last_err("write uinput")) } else { Ok(()) }
    }
}

impl Drop for UinputDevice {
    fn drop(&mut self) {
        // SAFETY: destroying a device we created; errors are irrelevant on drop.
        unsafe {
            libc::ioctl(self.fd.as_raw_fd(), UI_DEV_DESTROY as _);
        }
    }
}

pub struct UinputInjector {
    core: InjectorCore,
    kbd: UinputDevice,
    abs: UinputDevice,
    gamepad_router: GamepadRouter,
    gamepad: UinputGamepadDriver,
}

impl UinputInjector {
    pub fn new() -> Result<Self> {
        let kbd = UinputDevice::open()?;
        kbd.ioctl_int(UI_SET_EVBIT, EV_KEY)?;
        kbd.ioctl_int(UI_SET_EVBIT, EV_REL)?;
        for code in 1..=EVDEV_MAX_KEY {
            kbd.ioctl_int(UI_SET_KEYBIT, code)?;
        }
        for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
            kbd.ioctl_int(UI_SET_KEYBIT, b)?;
        }
        for r in [REL_X, REL_Y, REL_WHEEL, REL_HWHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES] {
            kbd.ioctl_int(UI_SET_RELBIT, r)?;
        }
        kbd.create("VeloxDesk Virtual Keyboard/Mouse", 1)?;

        let abs = UinputDevice::open()?;
        abs.ioctl_int(UI_SET_EVBIT, EV_KEY)?;
        abs.ioctl_int(UI_SET_EVBIT, EV_ABS)?;
        abs.ioctl_int(UI_SET_PROPBIT, INPUT_PROP_DIRECT)?;
        for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_TOUCH] {
            abs.ioctl_int(UI_SET_KEYBIT, b)?;
        }
        for code in [ABS_X, ABS_Y] {
            abs.ioctl_int(UI_SET_ABSBIT, code)?;
            let s = UinputAbsSetup {
                code,
                absinfo: InputAbsinfo { value: 0, minimum: 0, maximum: 65535, fuzz: 0, flat: 0, resolution: 0 },
            };
            abs.ioctl_ptr(UI_ABS_SETUP, &s)?;
        }
        abs.create("VeloxDesk Virtual Pointer", 2)?;
        Ok(Self {
            core: InjectorCore::new(),
            kbd,
            abs,
            gamepad_router: GamepadRouter::new(),
            gamepad: UinputGamepadDriver::new(),
        })
    }

    fn bounds(&self) -> VirtualBounds {
        self.core.bounds().unwrap_or(VirtualBounds { x: 0, y: 0, width: 65536, height: 65536 })
    }

    fn run(&mut self, actions: &[Action]) -> Result<()> {
        for a in actions {
            match a {
                Action::MoveAbs { x, y } => {
                    let (nx, ny) = self.bounds().normalize(*x, *y);
                    self.abs.push(EV_ABS, ABS_X, nx);
                    self.abs.push(EV_ABS, ABS_Y, ny);
                }
                Action::MoveRel { dx, dy } => {
                    if *dx != 0 {
                        self.kbd.push(EV_REL, REL_X, *dx);
                    }
                    if *dy != 0 {
                        self.kbd.push(EV_REL, REL_Y, *dy);
                    }
                }
                Action::Button { button, down } => {
                    self.kbd.push(EV_KEY, evdev_button(*button), *down as i32);
                }
                Action::Wheel { dx, dy } => {
                    // Hi-res axes use the same 1/120 notch units; legacy axes get whole notches.
                    if *dy != 0 {
                        self.kbd.push(EV_REL, REL_WHEEL_HI_RES, *dy);
                        if dy / 120 != 0 {
                            self.kbd.push(EV_REL, REL_WHEEL, dy / 120);
                        }
                    }
                    if *dx != 0 {
                        self.kbd.push(EV_REL, REL_HWHEEL_HI_RES, *dx);
                        if dx / 120 != 0 {
                            self.kbd.push(EV_REL, REL_HWHEEL, dx / 120);
                        }
                    }
                }
                Action::Key { usage, down } => match hid_to_evdev(*usage) {
                    Some(code) => self.kbd.push(EV_KEY, code, *down as i32),
                    None => tracing::debug!(usage, "unmapped HID usage"),
                },
                Action::Text(text) => {
                    for ch in text.chars() {
                        let Some((usage, shift)) = ascii_to_hid(ch) else {
                            tracing::debug!(?ch, "uinput cannot type non-ASCII text");
                            continue;
                        };
                        let Some(code) = hid_to_evdev(usage) else { continue };
                        const LSHIFT: u16 = 42;
                        if shift {
                            self.kbd.push(EV_KEY, LSHIFT, 1);
                        }
                        self.kbd.push(EV_KEY, code, 1);
                        self.kbd.sync()?;
                        self.kbd.push(EV_KEY, code, 0);
                        if shift {
                            self.kbd.push(EV_KEY, LSHIFT, 0);
                        }
                        self.kbd.sync()?;
                    }
                }
                Action::Gamepad(state) => {
                    let g_actions = self.gamepad_router.apply(state);
                    self.gamepad.apply_actions(&g_actions)?;
                }
            }
        }
        self.abs.sync()?;
        self.kbd.sync()
    }
}

fn evdev_button(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => BTN_LEFT,
        MouseButton::Right => BTN_RIGHT,
        MouseButton::Middle => BTN_MIDDLE,
        MouseButton::Back => BTN_SIDE,
        MouseButton::Forward => BTN_EXTRA,
    }
}

/// US-layout printable ASCII → (HID usage, needs shift).
pub(crate) fn ascii_to_hid(ch: char) -> Option<(u32, bool)> {
    crate::text::ascii_to_hid(ch)
}

impl InputInjector for UinputInjector {
    fn caps(&self) -> InputCaps {
        InputCaps {
            backend: "uinput",
            keyboard: true,
            mouse_absolute: true,
            mouse_relative: true,
            gamepads: true,
            gamepad_backend: self.gamepad.backend_name(),
            max_gamepads: self.gamepad.max_gamepads(),
            touch: false,
        }
    }

    fn set_displays(&mut self, displays: &[DisplayGeometry]) {
        self.core.set_displays(displays);
    }

    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        let actions = self.core.translate(event);
        self.run(&actions)
    }

    fn release_all(&mut self) -> Result<()> {
        let gp_disc = self.gamepad_router.disconnect_all();
        let gp_res = self.gamepad.apply_actions(&gp_disc);
        let actions = self.core.release_actions();
        let kb_res = self.run(&actions);
        kb_res.and(gp_res)
    }

    fn poll_feedback(&mut self) -> Vec<InputFeedback> {
        self.gamepad.poll_feedback()
    }
}

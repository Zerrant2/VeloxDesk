//! Linux virtual Xbox 360 gamepads via `/dev/uinput` (up to 4 simultaneous pads) with
//! `EV_FF` (`FF_RUMBLE`) force-feedback translation into [`Rumble`].

use std::collections::HashMap;
use std::ffi::CString;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use veloxdesk_core_traits::input::Rumble;
use veloxdesk_core_traits::{CoreError, Result};

use crate::gamepad::{GamepadDriver, MAX_GAMEPADS, XInputReport, xinput_btn};

// linux/input-event-codes.h
const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_ABS: u16 = 0x03;
const EV_FF: u16 = 0x15;
const EV_UINPUT: u16 = 0x0101;
const SYN_REPORT: u16 = 0;

const UI_FF_UPLOAD: u16 = 1;
const UI_FF_ERASE: u16 = 2;

const FF_RUMBLE: u16 = 0x50;

const BTN_SOUTH: u16 = 0x130; // A
const BTN_EAST: u16 = 0x131; // B
const BTN_NORTH: u16 = 0x133; // X
const BTN_WEST: u16 = 0x134; // Y
const BTN_TL: u16 = 0x136; // LB
const BTN_TR: u16 = 0x137; // RB
const BTN_SELECT: u16 = 0x13A; // Back
const BTN_START: u16 = 0x13B; // Start
const BTN_MODE: u16 = 0x13C; // Guide
const BTN_THUMBL: u16 = 0x13D; // LS
const BTN_THUMBR: u16 = 0x13E; // RS

const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02; // LT
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05; // RT
const ABS_HAT0X: u16 = 0x10;
const ABS_HAT0Y: u16 = 0x11;

const BUS_USB: u16 = 0x03;
const XBOX360_VENDOR: u16 = 0x045E;
const XBOX360_PRODUCT: u16 = 0x028E;

const fn ioc(dir: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | ((b'U' as u64) << 8) | nr
}
const IOC_NONE: u64 = 0;
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;
const IOC_RW: u64 = IOC_READ | IOC_WRITE;

const UI_DEV_CREATE: u64 = ioc(IOC_NONE, 1, 0);
const UI_DEV_DESTROY: u64 = ioc(IOC_NONE, 2, 0);
const UI_DEV_SETUP: u64 = ioc(IOC_WRITE, 3, size_of::<UinputSetup>() as u64);
const UI_ABS_SETUP: u64 = ioc(IOC_WRITE, 4, size_of::<UinputAbsSetup>() as u64);
const UI_SET_EVBIT: u64 = ioc(IOC_WRITE, 100, 4);
const UI_SET_KEYBIT: u64 = ioc(IOC_WRITE, 101, 4);
const UI_SET_ABSBIT: u64 = ioc(IOC_WRITE, 103, 4);
const UI_SET_FFBIT: u64 = ioc(IOC_WRITE, 107, 4);
const UI_BEGIN_FF_UPLOAD: u64 = ioc(IOC_RW, 200, size_of::<UinputFfUpload>() as u64);
const UI_END_FF_UPLOAD: u64 = ioc(IOC_WRITE, 201, size_of::<UinputFfUpload>() as u64);
const UI_BEGIN_FF_ERASE: u64 = ioc(IOC_RW, 202, size_of::<UinputFfErase>() as u64);
const UI_END_FF_ERASE: u64 = ioc(IOC_WRITE, 203, size_of::<UinputFfErase>() as u64);

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

#[repr(C)]
#[derive(Clone, Copy)]
struct FfTrigger {
    button: u16,
    interval: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FfReplay {
    length: u16,
    delay: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FfRumbleEffect {
    strong_magnitude: u16,
    weak_magnitude: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
union FfEffectUnion {
    rumble: FfRumbleEffect,
    _raw: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct FfEffect {
    type_: u16,
    id: i16,
    direction: u16,
    trigger: FfTrigger,
    replay: FfReplay,
    u: FfEffectUnion,
}

#[repr(C)]
struct UinputFfUpload {
    request_id: u32,
    retval: i32,
    effect: FfEffect,
    old: FfEffect,
}

#[repr(C)]
struct UinputFfErase {
    request_id: u32,
    retval: i32,
    effect_id: u32,
}

fn last_err(what: &str) -> CoreError {
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::PermissionDenied {
        CoreError::AccessDenied(format!("{what}: {e} (need /dev/uinput access)").into())
    } else {
        CoreError::platform(format!("{what}: {e}"))
    }
}

struct UinputPad {
    fd: OwnedFd,
    effects: HashMap<i16, (u16, u16)>,
    pending: Vec<libc::input_event>,
}

impl UinputPad {
    fn open(pad_id: u32) -> Result<Self> {
        let path = CString::new("/dev/uinput").expect("static path");
        // SAFETY: valid NUL-terminated path; O_RDWR allows reading EV_FF / EV_UINPUT rumble events.
        let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(last_err("open /dev/uinput for gamepad"));
        }
        let pad = Self {
            // SAFETY: freshly opened owned descriptor.
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
            effects: HashMap::new(),
            pending: Vec::with_capacity(24),
        };
        pad.ioctl_int(UI_SET_EVBIT, EV_KEY)?;
        pad.ioctl_int(UI_SET_EVBIT, EV_ABS)?;
        pad.ioctl_int(UI_SET_EVBIT, EV_FF)?;
        pad.ioctl_int(UI_SET_FFBIT, FF_RUMBLE)?;
        for btn in [
            BTN_SOUTH, BTN_EAST, BTN_NORTH, BTN_WEST, BTN_TL, BTN_TR, BTN_SELECT, BTN_START, BTN_MODE, BTN_THUMBL,
            BTN_THUMBR,
        ] {
            pad.ioctl_int(UI_SET_KEYBIT, btn)?;
        }
        for &(code, min, max, fuzz, flat) in &[
            (ABS_X, -32768, 32767, 16, 128),
            (ABS_Y, -32768, 32767, 16, 128),
            (ABS_RX, -32768, 32767, 16, 128),
            (ABS_RY, -32768, 32767, 16, 128),
            (ABS_Z, 0, 255, 0, 0),
            (ABS_RZ, 0, 255, 0, 0),
            (ABS_HAT0X, -1, 1, 0, 0),
            (ABS_HAT0Y, -1, 1, 0, 0),
        ] {
            pad.ioctl_int(UI_SET_ABSBIT, code)?;
            let setup = UinputAbsSetup {
                code,
                absinfo: InputAbsinfo { value: 0, minimum: min, maximum: max, fuzz, flat, resolution: 0 },
            };
            pad.ioctl_ptr(UI_ABS_SETUP, &setup)?;
        }
        let mut dev = UinputSetup {
            id: InputId { bustype: BUS_USB, vendor: XBOX360_VENDOR, product: XBOX360_PRODUCT, version: 0x0110 },
            name: [0; 80],
            ff_effects_max: 16,
        };
        let label = format!("VeloxDesk Virtual Xbox 360 Pad {}", pad_id + 1);
        let n = label.len().min(79);
        dev.name[..n].copy_from_slice(&label.as_bytes()[..n]);
        pad.ioctl_ptr(UI_DEV_SETUP, &dev)?;
        // SAFETY: UI_DEV_CREATE takes no arguments.
        if unsafe { libc::ioctl(pad.fd.as_raw_fd(), UI_DEV_CREATE as _) } < 0 {
            return Err(last_err("UI_DEV_CREATE gamepad"));
        }
        Ok(pad)
    }

    fn ioctl_int(&self, req: u64, val: u16) -> Result<()> {
        // SAFETY: uinput SET_*BIT ioctls take an int.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), req as _, val as libc::c_int) } < 0 {
            Err(last_err("uinput gamepad ioctl"))
        } else {
            Ok(())
        }
    }

    fn ioctl_ptr<T>(&self, req: u64, arg: &T) -> Result<()> {
        // SAFETY: struct layout matches kernel ABI for `req`.
        if unsafe { libc::ioctl(self.fd.as_raw_fd(), req as _, arg as *const T) } < 0 {
            Err(last_err("uinput gamepad ioctl"))
        } else {
            Ok(())
        }
    }

    fn push(&mut self, kind: u16, code: u16, value: i32) {
        // SAFETY: input_event is plain C data.
        let mut ev: libc::input_event = unsafe { zeroed() };
        ev.type_ = kind;
        ev.code = code;
        ev.value = value;
        self.pending.push(ev);
    }

    fn submit(&mut self, r: XInputReport) -> Result<()> {
        let btn = |mask: u16| i32::from((r.buttons & mask) != 0);
        self.push(EV_KEY, BTN_SOUTH, btn(xinput_btn::A));
        self.push(EV_KEY, BTN_EAST, btn(xinput_btn::B));
        self.push(EV_KEY, BTN_NORTH, btn(xinput_btn::X));
        self.push(EV_KEY, BTN_WEST, btn(xinput_btn::Y));
        self.push(EV_KEY, BTN_TL, btn(xinput_btn::LEFT_SHOULDER));
        self.push(EV_KEY, BTN_TR, btn(xinput_btn::RIGHT_SHOULDER));
        self.push(EV_KEY, BTN_SELECT, btn(xinput_btn::BACK));
        self.push(EV_KEY, BTN_START, btn(xinput_btn::START));
        self.push(EV_KEY, BTN_MODE, btn(xinput_btn::GUIDE));
        self.push(EV_KEY, BTN_THUMBL, btn(xinput_btn::LEFT_THUMB));
        self.push(EV_KEY, BTN_THUMBR, btn(xinput_btn::RIGHT_THUMB));

        let hat_x = btn(xinput_btn::DPAD_RIGHT) - btn(xinput_btn::DPAD_LEFT);
        let hat_y = btn(xinput_btn::DPAD_DOWN) - btn(xinput_btn::DPAD_UP);
        self.push(EV_ABS, ABS_HAT0X, hat_x);
        self.push(EV_ABS, ABS_HAT0Y, hat_y);
        self.push(EV_ABS, ABS_Z, r.left_trigger as i32);
        self.push(EV_ABS, ABS_RZ, r.right_trigger as i32);
        self.push(EV_ABS, ABS_X, r.left_x as i32);
        // Linux evdev Y-axis is inverted relative to XInput (+down vs +up).
        self.push(EV_ABS, ABS_Y, -(r.left_y as i32).clamp(-32767, 32767));
        self.push(EV_ABS, ABS_RX, r.right_x as i32);
        self.push(EV_ABS, ABS_RY, -(r.right_y as i32).clamp(-32767, 32767));
        self.push(EV_SYN, SYN_REPORT, 0);

        let bytes = self.pending.len() * size_of::<libc::input_event>();
        // SAFETY: contiguous buffer of `input_event`.
        let w = unsafe { libc::write(self.fd.as_raw_fd(), self.pending.as_ptr().cast(), bytes) };
        self.pending.clear();
        if w < 0 { Err(last_err("write uinput gamepad")) } else { Ok(()) }
    }

    fn poll_rumble(&mut self, pad_id: u32) -> Vec<Rumble> {
        let mut out = Vec::new();
        loop {
            // SAFETY: non-blocking read of one input_event.
            let mut ev: libc::input_event = unsafe { zeroed() };
            let n = unsafe { libc::read(self.fd.as_raw_fd(), (&raw mut ev).cast(), size_of::<libc::input_event>()) };
            if n != size_of::<libc::input_event>() as isize {
                break;
            }
            match ev.type_ {
                EV_UINPUT if ev.code == UI_FF_UPLOAD => {
                    // SAFETY: kernel uinput FF upload handshake.
                    unsafe {
                        let mut up: UinputFfUpload = zeroed();
                        up.request_id = ev.value as u32;
                        if libc::ioctl(self.fd.as_raw_fd(), UI_BEGIN_FF_UPLOAD as _, &raw mut up) >= 0 {
                            if up.effect.type_ == FF_RUMBLE {
                                let r = up.effect.u.rumble;
                                self.effects.insert(up.effect.id, (r.strong_magnitude, r.weak_magnitude));
                            }
                            up.retval = 0;
                            let _ = libc::ioctl(self.fd.as_raw_fd(), UI_END_FF_UPLOAD as _, &raw const up);
                        }
                    }
                }
                EV_UINPUT if ev.code == UI_FF_ERASE => {
                    // SAFETY: kernel uinput FF erase handshake.
                    unsafe {
                        let mut er: UinputFfErase = zeroed();
                        er.request_id = ev.value as u32;
                        if libc::ioctl(self.fd.as_raw_fd(), UI_BEGIN_FF_ERASE as _, &raw mut er) >= 0 {
                            self.effects.remove(&(er.effect_id as i16));
                            er.retval = 0;
                            let _ = libc::ioctl(self.fd.as_raw_fd(), UI_END_FF_ERASE as _, &raw const er);
                        }
                    }
                }
                EV_FF => {
                    let (low, high) = if ev.value > 0 {
                        self.effects.get(&(ev.code as i16)).copied().unwrap_or((0, 0))
                    } else {
                        (0, 0)
                    };
                    out.push(Rumble { pad_id, low_freq: low as u32, high_freq: high as u32 });
                }
                _ => {}
            }
        }
        out
    }
}

impl Drop for UinputPad {
    fn drop(&mut self) {
        // SAFETY: destroy our virtual uinput device.
        unsafe {
            libc::ioctl(self.fd.as_raw_fd(), UI_DEV_DESTROY as _);
        }
    }
}

pub struct UinputGamepadDriver {
    pads: [Option<UinputPad>; MAX_GAMEPADS],
}

impl UinputGamepadDriver {
    pub fn new() -> Self {
        Self { pads: [None, None, None, None] }
    }
}

impl Default for UinputGamepadDriver {
    fn default() -> Self {
        Self::new()
    }
}

impl GamepadDriver for UinputGamepadDriver {
    fn backend_name(&self) -> Option<&'static str> {
        Some("uinput")
    }

    fn max_gamepads(&self) -> u32 {
        MAX_GAMEPADS as u32
    }

    fn connect(&mut self, pad_id: u32) -> Result<()> {
        let idx = pad_id as usize;
        if idx >= MAX_GAMEPADS || self.pads[idx].is_some() {
            return Ok(());
        }
        self.pads[idx] = Some(UinputPad::open(pad_id)?);
        Ok(())
    }

    fn update(&mut self, pad_id: u32, report: XInputReport) -> Result<()> {
        let idx = pad_id as usize;
        if let Some(pad) = self.pads.get_mut(idx).and_then(|s| s.as_mut()) {
            pad.submit(report)?;
        }
        Ok(())
    }

    fn disconnect(&mut self, pad_id: u32) -> Result<()> {
        let idx = pad_id as usize;
        if let Some(slot) = self.pads.get_mut(idx) {
            *slot = None;
        }
        Ok(())
    }

    fn poll_rumble(&mut self) -> Vec<Rumble> {
        let mut out = Vec::new();
        for (i, slot) in self.pads.iter_mut().enumerate() {
            if let Some(pad) = slot.as_mut() {
                out.extend(pad.poll_rumble(i as u32));
            }
        }
        out
    }
}

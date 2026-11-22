//! Linux X11 screen capture backend (`caps().backend == "x11-shm"`).
//!
//! Enumerates outputs via RandR (`get_monitors`, falling back to the root screen) and
//! captures BGRA8 frames over the X11 connection.

use std::time::{Duration, Instant};

use bytes::Bytes;
use veloxdesk_core_traits::video::{
    CaptureCaps, CaptureConfig, CapturedFrame, ColorRange, ColorSpace, Display, DisplayId, Frame, FrameCapture,
    FrameData, PixelFormat, Plane,
};
use veloxdesk_core_traits::{CoreError, Result, Timestamp};
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as RandrExt;
use x11rb::protocol::xproto::{ConnectionExt as XprotoExt, ImageFormat};
use x11rb::rust_connection::RustConnection;

fn open_conn() -> Result<(RustConnection, usize)> {
    x11rb::connect(None).map_err(|e| CoreError::unsupported(format!("X11 connect failed: {e}")))
}

pub(crate) fn list_displays() -> Result<Vec<Display>> {
    let (conn, screen_num) = open_conn()?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;
    if let Ok(cookie) = conn.randr_get_monitors(root, true)
        && let Ok(reply) = cookie.reply()
        && !reply.monitors.is_empty()
    {
        let mut out = Vec::with_capacity(reply.monitors.len());
        for (i, m) in reply.monitors.iter().enumerate() {
            let name = conn
                .get_atom_name(m.name)
                .ok()
                .and_then(|c| c.reply().ok())
                .and_then(|r| String::from_utf8(r.name).ok())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| format!("X11-{i}"));
            out.push(Display {
                id: DisplayId(name.clone()),
                index: i as u32,
                name,
                x: m.x as i32,
                y: m.y as i32,
                width: m.width as u32,
                height: m.height as u32,
                refresh_hz: 60,
                scale_factor: 1.0,
                primary: m.primary,
                hdr: false,
            });
        }
        return Ok(out);
    }

    Ok(vec![Display {
        id: DisplayId("X11-0".into()),
        index: 0,
        name: "X11 Root".into(),
        x: 0,
        y: 0,
        width: screen.width_in_pixels as u32,
        height: screen.height_in_pixels as u32,
        refresh_hz: 60,
        scale_factor: 1.0,
        primary: true,
        hdr: false,
    }])
}

struct X11Session {
    x: i16,
    y: i16,
    width: u16,
    height: u16,
    min_interval: Duration,
}

/// X11 screen capture backend.
pub struct X11Capture {
    conn: RustConnection,
    root: u32,
    session: Option<X11Session>,
    last_delivery: Option<Instant>,
}

impl X11Capture {
    pub fn new(_prefer_gpu: bool) -> Result<Self> {
        let (conn, screen_num) = open_conn()?;
        let root = conn.setup().roots[screen_num].root;
        Ok(Self { conn, root, session: None, last_delivery: None })
    }
}

impl FrameCapture for X11Capture {
    fn caps(&self) -> CaptureCaps {
        CaptureCaps {
            backend: "x11-shm",
            gpu_frames: false,
            dirty_rects: false,
            separate_cursor: false,
            hdr: false,
            formats: vec![PixelFormat::Bgra8],
        }
    }

    fn displays(&self) -> Result<Vec<Display>> {
        list_displays()
    }

    fn start(&mut self, cfg: &CaptureConfig) -> Result<()> {
        let displays = list_displays()?;
        let d = if cfg.display.0.is_empty() {
            displays.iter().find(|d| d.primary).or(displays.first())
        } else {
            displays.iter().find(|d| d.id == cfg.display)
        }
        .ok_or_else(|| CoreError::invalid(format!("X11 display {:?} not found", cfg.display.0)))?;
        let min_interval = if cfg.max_fps == 0 {
            Duration::ZERO
        } else {
            Duration::from_micros(1_000_000 / cfg.max_fps as u64)
        };
        self.session = Some(X11Session {
            x: d.x as i16,
            y: d.y as i16,
            width: d.width as u16,
            height: d.height as u16,
            min_interval,
        });
        self.last_delivery = None;
        Ok(())
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CapturedFrame>> {
        let Some(sess) = &self.session else {
            return Err(CoreError::reinit("X11 capture not started"));
        };
        if let Some(last) = self.last_delivery {
            let next = last + sess.min_interval;
            let now = Instant::now();
            if next > now {
                let wait = next - now;
                if wait > timeout {
                    std::thread::sleep(timeout);
                    return Ok(None);
                }
                std::thread::sleep(wait);
            }
        }
        let t0 = Instant::now();
        let reply = self
            .conn
            .get_image(ImageFormat::Z_PIXMAP, self.root, sess.x, sess.y, sess.width, sess.height, !0)
            .map_err(|e| CoreError::platform(format!("X11 get_image send: {e}")))?
            .reply()
            .map_err(|e| CoreError::platform(format!("X11 get_image reply: {e}")))?;
        let capture_us = t0.elapsed().as_micros() as u32;
        self.last_delivery = Some(Instant::now());
        let w = sess.width as u32;
        let h = sess.height as u32;
        let stride = (w * 4) as usize;
        if reply.data.len() < stride * h as usize {
            return Err(CoreError::unsupported("X11 root depth is not 32-bit BGRA"));
        }
        Ok(Some(CapturedFrame {
            frame: Frame {
                width: w,
                height: h,
                format: PixelFormat::Bgra8,
                color_space: ColorSpace::Srgb,
                color_range: ColorRange::Full,
                data: FrameData::Cpu(vec![Plane { data: Bytes::from(reply.data), stride }]),
                capture_ts: Timestamp::now(),
            },
            dirty_rects: Vec::new(),
            cursor: None,
            capture_us,
            convert_us: 0,
        }))
    }

    fn stop(&mut self) {
        self.session = None;
        self.last_delivery = None;
    }
}

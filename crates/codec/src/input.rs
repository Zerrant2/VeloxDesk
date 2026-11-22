//! Turning arbitrary input [`Frame`]s into CPU planes for software encoders.

use std::time::Instant;

use veloxdesk_core_traits::video::{Frame, FrameData, PixelFormat};
use veloxdesk_core_traits::{CoreError, Result};

use crate::convert::{self, YuvMatrix};

/// Reusable I420 buffers fed from BGRA / NV12 / I420 CPU frames (or GPU frames via readback).
pub struct CpuYuvInput {
    w: u32,
    h: u32,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    matrix: YuvMatrix,
    #[cfg(windows)]
    readback: Option<crate::win::d3d::Readback>,
}

impl CpuYuvInput {
    pub fn new(w: u32, h: u32, matrix: YuvMatrix) -> Self {
        let (cw, ch) = convert::chroma_dims(w, h);
        Self {
            w,
            h,
            y: vec![0; (w * h) as usize],
            u: vec![128; cw * ch],
            v: vec![128; cw * ch],
            matrix,
            #[cfg(windows)]
            readback: None,
        }
    }

    pub fn dims(&self) -> (u32, u32) {
        (self.w, self.h)
    }

    pub fn strides(&self) -> (usize, usize, usize) {
        let cw = self.w.div_ceil(2) as usize;
        (self.w as usize, cw, cw)
    }

    pub fn planes(&self) -> (&[u8], &[u8], &[u8]) {
        (&self.y, &self.u, &self.v)
    }

    /// Convert `frame` into the internal I420 buffers. Returns conversion time (µs).
    /// Frames larger than the configured size are cropped; smaller ones leave the rest black.
    pub fn load(&mut self, frame: &Frame) -> Result<u32> {
        let t0 = Instant::now();
        match &frame.data {
            FrameData::Cpu(planes) => {
                let p: Vec<(&[u8], usize)> = planes.iter().map(|p| (&p.data[..], p.stride)).collect();
                self.load_cpu(frame.format, frame.width, frame.height, &p)?
            }
            FrameData::Gpu(g) => {
                #[cfg(windows)]
                {
                    let mut rb = self.readback.take().unwrap_or_default();
                    let res = rb
                        .read(g)
                        .and_then(|(fmt, planes)| self.load_cpu(fmt, frame.width, frame.height, &planes));
                    self.readback = Some(rb);
                    res?;
                }
                #[cfg(not(windows))]
                {
                    let _ = g;
                    return Err(CoreError::unsupported("GPU frames need a hardware encoder on this OS"));
                }
            }
        }
        Ok(t0.elapsed().as_micros() as u32)
    }

    fn load_cpu(
        &mut self,
        fmt: PixelFormat,
        fw: u32,
        fh: u32,
        planes: &[(&[u8], usize)],
    ) -> Result<()> {
        let w = self.w.min(fw);
        let h = self.h.min(fh);
        let (ys, us, vs) = self.strides();
        let (cw, ch) = convert::chroma_dims(w, h);
        let need = |n: usize| {
            if planes.len() < n {
                Err(CoreError::invalid(format!("{fmt:?} frame needs {n} planes")))
            } else {
                Ok(())
            }
        };
        match fmt {
            PixelFormat::Bgra8 => {
                need(1)?;
                convert::bgra_to_i420(
                    planes[0].0,
                    planes[0].1,
                    w,
                    h,
                    &mut self.y,
                    ys,
                    &mut self.u,
                    us,
                    &mut self.v,
                    vs,
                    &self.matrix,
                );
            }
            PixelFormat::Nv12 => {
                need(2)?;
                convert::copy_plane(planes[0].0, planes[0].1, &mut self.y, ys, w as usize, h as usize);
                convert::nv12_chroma_to_i420(
                    planes[1].0,
                    planes[1].1,
                    cw,
                    ch,
                    &mut self.u,
                    us,
                    &mut self.v,
                    vs,
                );
            }
            PixelFormat::I420 => {
                need(3)?;
                convert::copy_plane(planes[0].0, planes[0].1, &mut self.y, ys, w as usize, h as usize);
                convert::copy_plane(planes[1].0, planes[1].1, &mut self.u, us, cw, ch);
                convert::copy_plane(planes[2].0, planes[2].1, &mut self.v, vs, cw, ch);
            }
            other => return Err(CoreError::unsupported(format!("SW encoder input format {other:?}"))),
        }
        Ok(())
    }
}

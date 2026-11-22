//! OS clipboard backend (`arboard` text/HTML/PNG + Windows `CF_HDROP` file list).

use std::borrow::Cow;
use std::io::Cursor;

use arboard::{Clipboard, ImageData};
use bytes::Bytes;
use veloxdesk_core_traits::{CoreError, Result};

use crate::{ClipboardBackend, ClipboardItem};

fn encode_rgba_png(width: usize, height: usize, rgba: &[u8]) -> Result<Bytes> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width as u32, height as u32);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| CoreError::platform(format!("png header: {e}")))?;
        writer.write_image_data(rgba).map_err(|e| CoreError::platform(format!("png encode: {e}")))?;
    }
    Ok(Bytes::from(out))
}

fn decode_png_rgba(png_bytes: &[u8]) -> Result<(usize, usize, Vec<u8>)> {
    let mut decoder = png::Decoder::new(Cursor::new(png_bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::ALPHA);
    let mut reader = decoder.read_info().map_err(|e| CoreError::invalid(format!("png decode info: {e}")))?;
    let buf_size = reader.output_buffer_size().ok_or_else(|| CoreError::invalid("png output buffer overflow"))?;
    let mut buf = vec![0u8; buf_size];
    let info = reader.next_frame(&mut buf).map_err(|e| CoreError::invalid(format!("png decode frame: {e}")))?;
    let raw = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => raw.to_vec(),
        png::ColorType::Rgb => {
            let mut v = Vec::with_capacity((info.width * info.height * 4) as usize);
            for p in raw.chunks_exact(3) {
                v.extend_from_slice(&[p[0], p[1], p[2], 255]);
            }
            v
        }
        png::ColorType::GrayscaleAlpha => {
            let mut v = Vec::with_capacity((info.width * info.height * 4) as usize);
            for p in raw.chunks_exact(2) {
                v.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
            v
        }
        png::ColorType::Grayscale => {
            let mut v = Vec::with_capacity((info.width * info.height * 4) as usize);
            for &g in raw {
                v.extend_from_slice(&[g, g, g, 255]);
            }
            v
        }
        other => return Err(CoreError::unsupported(format!("unsupported png color type {other:?}"))),
    };
    Ok((info.width as usize, info.height as usize, rgba))
}

/// Native OS clipboard backend using `arboard` (text, HTML, PNG images) and Windows `CF_HDROP`.
pub struct SystemClipboard {
    inner: Clipboard,
    #[cfg(windows)]
    last_seq: Option<u32>,
    #[cfg(windows)]
    cached: Option<ClipboardItem>,
}

// SAFETY: `SystemClipboard` is owned by a single polling task/thread (ClipboardBackend: Send).
unsafe impl Send for SystemClipboard {}

impl SystemClipboard {
    pub fn new() -> Result<Self> {
        let inner = Clipboard::new().map_err(|e| CoreError::unsupported(format!("OS clipboard unavailable: {e}")))?;
        Ok(Self {
            inner,
            #[cfg(windows)]
            last_seq: None,
            #[cfg(windows)]
            cached: None,
        })
    }
}

impl ClipboardBackend for SystemClipboard {
    fn read(&mut self) -> Result<Option<ClipboardItem>> {
        #[cfg(windows)]
        {
            let seq = crate::win_files::sequence_number();
            if self.last_seq == Some(seq) {
                return Ok(self.cached.clone());
            }
            self.last_seq = Some(seq);
            if let Ok(Some(files)) = crate::win_files::read_file_list() {
                let item = ClipboardItem::FileList(files);
                self.cached = Some(item.clone());
                return Ok(Some(item));
            }
        }

        let item = if let Ok(text) = self.inner.get_text() {
            if text.is_empty() { None } else { Some(ClipboardItem::Text(text)) }
        } else if let Ok(img) = self.inner.get_image() {
            let png = encode_rgba_png(img.width, img.height, &img.bytes)?;
            Some(ClipboardItem::ImagePng(png))
        } else {
            None
        };

        #[cfg(windows)]
        {
            self.cached = item.clone();
        }
        Ok(item)
    }

    fn write(&mut self, item: &ClipboardItem) -> Result<()> {
        match item {
            ClipboardItem::Text(s) => {
                self.inner.set_text(s.clone()).map_err(|e| CoreError::platform(format!("clipboard set_text: {e}")))?;
            }
            ClipboardItem::Html(html) => {
                self.inner
                    .set_html(html.clone(), None)
                    .or_else(|_| self.inner.set_text(html.clone()))
                    .map_err(|e| CoreError::platform(format!("clipboard set_html: {e}")))?;
            }
            ClipboardItem::ImagePng(png_bytes) => {
                let (width, height, rgba) = decode_png_rgba(png_bytes)?;
                let img = ImageData { width, height, bytes: Cow::Owned(rgba) };
                self.inner.set_image(img).map_err(|e| CoreError::platform(format!("clipboard set_image: {e}")))?;
            }
            ClipboardItem::FileList(paths) => {
                #[cfg(windows)]
                {
                    crate::win_files::write_file_list(paths)?;
                }
                #[cfg(not(windows))]
                {
                    self.inner
                        .set_text(paths.join("\n"))
                        .map_err(|e| CoreError::platform(format!("clipboard set file list text: {e}")))?;
                }
            }
        }
        #[cfg(windows)]
        {
            self.last_seq = Some(crate::win_files::sequence_number());
            self.cached = Some(item.clone());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_rgba_roundtrip() {
        let rgba = vec![255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 128];
        let png = encode_rgba_png(2, 2, &rgba).expect("encode png");
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
        let (w, h, out) = decode_png_rgba(&png).expect("decode png");
        assert_eq!((w, h), (2, 2));
        assert_eq!(out, rgba);
    }
}

//! Micro-benchmarks for CPU color conversions in `veloxdesk-codec`.

use std::hint::black_box;

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use veloxdesk_codec::convert::{YuvMatrix, bgra_to_i420, bgra_to_nv12, chroma_dims, i420_to_rgba, nv12_to_rgba};
use veloxdesk_core_traits::video::{ColorRange, ColorSpace};

fn make_bgra(w: u32, h: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            buf[i] = (x & 0xFF) as u8;
            buf[i + 1] = (y & 0xFF) as u8;
            buf[i + 2] = ((x ^ y) & 0xFF) as u8;
            buf[i + 3] = 255;
        }
    }
    buf
}

fn color_convert(c: &mut Criterion) {
    let matrix = YuvMatrix::new(ColorSpace::Bt709, ColorRange::Limited);

    for &(label, w, h) in &[("1080p", 1920u32, 1080u32), ("1440p", 2560u32, 1440u32)] {
        let mut g = c.benchmark_group(format!("convert/{label}"));
        let pixels = (w as u64) * (h as u64);
        g.throughput(Throughput::Bytes(pixels * 4));

        let bgra = make_bgra(w, h);
        let bgra_stride = (w * 4) as usize;
        let y_stride = w as usize;
        let (cw, ch) = chroma_dims(w, h);

        let mut y = vec![0u8; y_stride * h as usize];
        let mut uv = vec![0u8; cw * 2 * ch];
        let mut u = vec![0u8; cw * ch];
        let mut v = vec![0u8; cw * ch];
        let mut rgba = vec![0u8; bgra_stride * h as usize];

        g.bench_function("bgra_to_nv12", |b| {
            b.iter(|| {
                bgra_to_nv12(
                    black_box(&bgra),
                    bgra_stride,
                    w,
                    h,
                    black_box(&mut y),
                    y_stride,
                    black_box(&mut uv),
                    cw * 2,
                    &matrix,
                );
            })
        });

        g.bench_function("bgra_to_i420", |b| {
            b.iter(|| {
                bgra_to_i420(
                    black_box(&bgra),
                    bgra_stride,
                    w,
                    h,
                    black_box(&mut y),
                    y_stride,
                    black_box(&mut u),
                    cw,
                    black_box(&mut v),
                    cw,
                    &matrix,
                );
            })
        });

        // Fill YUV planes with valid data before benchmarking reverse direction.
        bgra_to_nv12(&bgra, bgra_stride, w, h, &mut y, y_stride, &mut uv, cw * 2, &matrix);
        bgra_to_i420(&bgra, bgra_stride, w, h, &mut y, y_stride, &mut u, cw, &mut v, cw, &matrix);

        g.bench_function("nv12_to_rgba", |b| {
            b.iter(|| {
                nv12_to_rgba(
                    black_box(&y),
                    y_stride,
                    black_box(&uv),
                    cw * 2,
                    w,
                    h,
                    black_box(&mut rgba),
                    bgra_stride,
                    &matrix,
                );
            })
        });

        g.bench_function("i420_to_rgba", |b| {
            b.iter(|| {
                i420_to_rgba(
                    black_box(&y),
                    y_stride,
                    black_box(&u),
                    cw,
                    black_box(&v),
                    cw,
                    w,
                    h,
                    black_box(&mut rgba),
                    bgra_stride,
                    &matrix,
                );
            })
        });

        g.finish();
    }
}

criterion_group!(benches, color_convert);
criterion_main!(benches);

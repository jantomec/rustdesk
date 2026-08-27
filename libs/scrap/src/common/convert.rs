#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(improper_ctypes)]
#![allow(dead_code)]

include!(concat!(env!("OUT_DIR"), "/yuv_ffi.rs"));

#[cfg(not(target_os = "ios"))]
use crate::PixelBuffer;
use crate::{generate_call_macro, EncodeYuvFormat, TraitPixelBuffer};
use hbb_common::{bail, log, ResultType};

generate_call_macro!(call_yuv, false);

#[cfg(not(target_os = "ios"))]
pub fn convert_to_yuv(
    captured: &PixelBuffer,
    dst_fmt: EncodeYuvFormat,
    dst: &mut Vec<u8>,
    mid_data: &mut Vec<u8>,
) -> ResultType<()> {
    let src = captured.data();
    let src_stride = captured.stride();
    let src_pixfmt = captured.pixfmt();
    let src_width = captured.width();
    let src_height = captured.height();
    if src_width > dst_fmt.w || src_height > dst_fmt.h {
        bail!(
            "src rect > dst rect: ({src_width}, {src_height}) > ({},{})",
            dst_fmt.w,
            dst_fmt.h
        );
    }
    if src_pixfmt == crate::Pixfmt::BGRA
        || src_pixfmt == crate::Pixfmt::RGBA
        || src_pixfmt == crate::Pixfmt::RGB565LE
    {
        // stride is calculated, not real, so we need to check it
        if src_stride[0] < src_width * src_pixfmt.bytes_per_pixel() {
            bail!(
                "src_stride too small: {} < {}",
                src_stride[0],
                src_width * src_pixfmt.bytes_per_pixel()
            );
        }
        if src.len() < src_stride[0] * src_height {
            bail!(
                "wrong src len, {} < {} * {}",
                src.len(),
                src_stride[0],
                src_height
            );
        }
    }
    let align = |x: usize| (x + 63) / 64 * 64;
    let unsupported = format!(
        "unsupported pixfmt conversion: {src_pixfmt:?} -> {:?}",
        dst_fmt.pixfmt
    );

    match (src_pixfmt, dst_fmt.pixfmt) {
        (crate::Pixfmt::BGRA, crate::Pixfmt::I420)
        | (crate::Pixfmt::RGBA, crate::Pixfmt::I420)
        | (crate::Pixfmt::RGB565LE, crate::Pixfmt::I420) => {
            let dst_stride_y = dst_fmt.stride[0];
            let dst_stride_uv = dst_fmt.stride[1];
            dst.resize(dst_fmt.h * dst_stride_y * 2, 0); // waste some memory to ensure memory safety
            let dst_y = dst.as_mut_ptr();
            let dst_u = dst[dst_fmt.u..].as_mut_ptr();
            let dst_v = dst[dst_fmt.v..].as_mut_ptr();
            let f = match src_pixfmt {
                crate::Pixfmt::BGRA => ARGBToI420,
                crate::Pixfmt::RGBA => ABGRToI420,
                crate::Pixfmt::RGB565LE => RGB565ToI420,
                _ => bail!(unsupported),
            };
            let ret = unsafe {
                par_rgb_to_i420(
                    f,
                    src.as_ptr(),
                    src_stride[0] as _,
                    dst_y,
                    dst_stride_y as _,
                    dst_u,
                    dst_stride_uv as _,
                    dst_v,
                    dst_stride_uv as _,
                    src_width as _,
                    src_height as _,
                )
            };
            if ret != 0 {
                return Err(
                    crate::Error::FailedCall(format!("errcode={ret} par_rgb_to_i420")).into(),
                );
            }
        }
        (crate::Pixfmt::BGRA, crate::Pixfmt::NV12)
        | (crate::Pixfmt::RGBA, crate::Pixfmt::NV12)
        | (crate::Pixfmt::RGB565LE, crate::Pixfmt::NV12) => {
            let dst_stride_y = dst_fmt.stride[0];
            let dst_stride_uv = dst_fmt.stride[1];
            dst.resize(
                align(dst_fmt.h) * (align(dst_stride_y) + align(dst_stride_uv / 2)),
                0,
            );
            let dst_y = dst.as_mut_ptr();
            let dst_uv = dst[dst_fmt.u..].as_mut_ptr();
            let (input, input_stride) = match src_pixfmt {
                crate::Pixfmt::BGRA => (src.as_ptr(), src_stride[0]),
                crate::Pixfmt::RGBA => (src.as_ptr(), src_stride[0]),
                crate::Pixfmt::RGB565LE => {
                    let mid_stride = src_width * 4;
                    mid_data.resize(mid_stride * src_height, 0);
                    call_yuv!(RGB565ToARGB(
                        src.as_ptr(),
                        src_stride[0] as _,
                        mid_data.as_mut_ptr(),
                        mid_stride as _,
                        src_width as _,
                        src_height as _,
                    ));
                    (mid_data.as_ptr(), mid_stride)
                }
                _ => bail!(unsupported),
            };
            let f = match src_pixfmt {
                crate::Pixfmt::BGRA => ARGBToNV12,
                crate::Pixfmt::RGBA => ABGRToNV12,
                crate::Pixfmt::RGB565LE => ARGBToNV12,
                _ => bail!(unsupported),
            };
            let ret = unsafe {
                par_rgb_to_nv12(
                    f,
                    input,
                    input_stride as _,
                    dst_y,
                    dst_stride_y as _,
                    dst_uv,
                    dst_stride_uv as _,
                    src_width as _,
                    src_height as _,
                )
            };
            if ret != 0 {
                return Err(
                    crate::Error::FailedCall(format!("errcode={ret} par_rgb_to_nv12")).into(),
                );
            }
        }
        (crate::Pixfmt::BGRA, crate::Pixfmt::I444)
        | (crate::Pixfmt::RGBA, crate::Pixfmt::I444)
        | (crate::Pixfmt::RGB565LE, crate::Pixfmt::I444) => {
            let dst_stride_y = dst_fmt.stride[0];
            let dst_stride_u = dst_fmt.stride[1];
            let dst_stride_v = dst_fmt.stride[2];
            dst.resize(
                align(dst_fmt.h)
                    * (align(dst_stride_y) + align(dst_stride_u) + align(dst_stride_v)),
                0,
            );
            let dst_y = dst.as_mut_ptr();
            let dst_u = dst[dst_fmt.u..].as_mut_ptr();
            let dst_v = dst[dst_fmt.v..].as_mut_ptr();
            let (input, input_stride) = match src_pixfmt {
                crate::Pixfmt::BGRA => (src.as_ptr(), src_stride[0]),
                crate::Pixfmt::RGBA => {
                    mid_data.resize(src.len(), 0);
                    call_yuv!(ABGRToARGB(
                        src.as_ptr(),
                        src_stride[0] as _,
                        mid_data.as_mut_ptr(),
                        src_stride[0] as _,
                        src_width as _,
                        src_height as _,
                    ));
                    (mid_data.as_ptr(), src_stride[0])
                }
                crate::Pixfmt::RGB565LE => {
                    let mid_stride = src_width * 4;
                    mid_data.resize(mid_stride * src_height, 0);
                    call_yuv!(RGB565ToARGB(
                        src.as_ptr(),
                        src_stride[0] as _,
                        mid_data.as_mut_ptr(),
                        mid_stride as _,
                        src_width as _,
                        src_height as _,
                    ));
                    (mid_data.as_ptr(), mid_stride)
                }
                _ => bail!(unsupported),
            };

            call_yuv!(ARGBToI444(
                input,
                input_stride as _,
                dst_y,
                dst_stride_y as _,
                dst_u,
                dst_stride_u as _,
                dst_v,
                dst_stride_v as _,
                src_width as _,
                src_height as _,
            ));
        }
        _ => {
            bail!(unsupported);
        }
    }
    Ok(())
}

// ---- parallel row-sliced conversions ---------------------------------------
//
// libyuv's 4:2:0 conversions are row-independent (chroma is derived from
// 2-row pairs), so slicing a frame at even row offsets and converting the
// slices concurrently is bit-identical to a single call. At hidpi sizes (a 5K
// frame is 14.7 Mpx) these conversions are the largest CPU stage of both the
// capture->encode and decode->render paths.

type RgbToNv12Fn =
    unsafe extern "C" fn(*const u8, i32, *mut u8, i32, *mut u8, i32, i32, i32) -> i32;
type RgbToI420Fn = unsafe extern "C" fn(
    *const u8,
    i32,
    *mut u8,
    i32,
    *mut u8,
    i32,
    *mut u8,
    i32,
    i32,
    i32,
) -> i32;
type Nv12ToRgbFn =
    unsafe extern "C" fn(*const u8, i32, *const u8, i32, *mut u8, i32, i32, i32) -> i32;
type I420ToRgbFn = unsafe extern "C" fn(
    *const u8,
    i32,
    *const u8,
    i32,
    *const u8,
    i32,
    *mut u8,
    i32,
    i32,
    i32,
) -> i32;

const PAR_MIN_PIXELS: usize = 2 * 1024 * 1024;

/// `(y0, rows)` per slice; slice starts are even so 2x2-subsampled chroma
/// offsets stay exact. A single full-height slice means "convert inline".
fn par_rows(width: i32, height: i32) -> Vec<(usize, usize)> {
    let (w, h) = (width.max(0) as usize, height.max(0) as usize);
    static THREADS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let threads = *THREADS.get_or_init(|| {
        std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(1)
            .min(8)
    });
    if threads < 2 || w.saturating_mul(h) < PAR_MIN_PIXELS {
        return vec![(0, h)];
    }
    let n = threads.min(h / 64).max(1);
    let rows = (((h + n - 1) / n) + 1) & !1;
    let mut slices = Vec::with_capacity(n);
    let mut y0 = 0;
    while y0 < h {
        let r = rows.min(h - y0);
        slices.push((y0, r));
        y0 += r;
    }
    slices
}

fn par_run<F: Fn(usize, usize) -> i32 + Copy + Send>(slices: Vec<(usize, usize)>, f: F) -> i32 {
    use std::sync::atomic::{AtomicI32, Ordering};
    let ret = AtomicI32::new(0);
    std::thread::scope(|s| {
        for (y0, rows) in slices.iter().skip(1) {
            let (ret, y0, rows) = (&ret, *y0, *rows);
            s.spawn(move || {
                let r = f(y0, rows);
                if r != 0 {
                    ret.store(r, Ordering::SeqCst);
                }
            });
        }
        let (y0, rows) = slices[0];
        let r = f(y0, rows);
        if r != 0 {
            ret.store(r, Ordering::SeqCst);
        }
    });
    ret.load(std::sync::atomic::Ordering::SeqCst)
}

pub unsafe fn par_rgb_to_nv12(
    f: RgbToNv12Fn,
    src: *const u8,
    src_stride: i32,
    dst_y: *mut u8,
    dst_stride_y: i32,
    dst_uv: *mut u8,
    dst_stride_uv: i32,
    width: i32,
    height: i32,
) -> i32 {
    let slices = par_rows(width, height);
    if slices.len() <= 1 {
        return f(
            src,
            src_stride,
            dst_y,
            dst_stride_y,
            dst_uv,
            dst_stride_uv,
            width,
            height,
        );
    }
    let (src, dst_y, dst_uv) = (src as usize, dst_y as usize, dst_uv as usize);
    par_run(slices, move |y0, rows| unsafe {
        f(
            (src + y0 * src_stride as usize) as *const u8,
            src_stride,
            (dst_y + y0 * dst_stride_y as usize) as *mut u8,
            dst_stride_y,
            (dst_uv + y0 / 2 * dst_stride_uv as usize) as *mut u8,
            dst_stride_uv,
            width,
            rows as i32,
        )
    })
}

pub unsafe fn par_rgb_to_i420(
    f: RgbToI420Fn,
    src: *const u8,
    src_stride: i32,
    dst_y: *mut u8,
    dst_stride_y: i32,
    dst_u: *mut u8,
    dst_stride_u: i32,
    dst_v: *mut u8,
    dst_stride_v: i32,
    width: i32,
    height: i32,
) -> i32 {
    let slices = par_rows(width, height);
    if slices.len() <= 1 {
        return f(
            src,
            src_stride,
            dst_y,
            dst_stride_y,
            dst_u,
            dst_stride_u,
            dst_v,
            dst_stride_v,
            width,
            height,
        );
    }
    let (src, dst_y, dst_u, dst_v) = (src as usize, dst_y as usize, dst_u as usize, dst_v as usize);
    par_run(slices, move |y0, rows| unsafe {
        f(
            (src + y0 * src_stride as usize) as *const u8,
            src_stride,
            (dst_y + y0 * dst_stride_y as usize) as *mut u8,
            dst_stride_y,
            (dst_u + y0 / 2 * dst_stride_u as usize) as *mut u8,
            dst_stride_u,
            (dst_v + y0 / 2 * dst_stride_v as usize) as *mut u8,
            dst_stride_v,
            width,
            rows as i32,
        )
    })
}

pub unsafe fn par_nv12_to_rgb(
    f: Nv12ToRgbFn,
    src_y: *const u8,
    src_stride_y: i32,
    src_uv: *const u8,
    src_stride_uv: i32,
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
) -> i32 {
    let slices = par_rows(width, height);
    if slices.len() <= 1 {
        return f(
            src_y,
            src_stride_y,
            src_uv,
            src_stride_uv,
            dst,
            dst_stride,
            width,
            height,
        );
    }
    let (src_y, src_uv, dst) = (src_y as usize, src_uv as usize, dst as usize);
    par_run(slices, move |y0, rows| unsafe {
        f(
            (src_y + y0 * src_stride_y as usize) as *const u8,
            src_stride_y,
            (src_uv + y0 / 2 * src_stride_uv as usize) as *const u8,
            src_stride_uv,
            (dst + y0 * dst_stride as usize) as *mut u8,
            dst_stride,
            width,
            rows as i32,
        )
    })
}

pub unsafe fn par_i420_to_rgb(
    f: I420ToRgbFn,
    src_y: *const u8,
    src_stride_y: i32,
    src_u: *const u8,
    src_stride_u: i32,
    src_v: *const u8,
    src_stride_v: i32,
    dst: *mut u8,
    dst_stride: i32,
    width: i32,
    height: i32,
) -> i32 {
    let slices = par_rows(width, height);
    if slices.len() <= 1 {
        return f(
            src_y,
            src_stride_y,
            src_u,
            src_stride_u,
            src_v,
            src_stride_v,
            dst,
            dst_stride,
            width,
            height,
        );
    }
    let (src_y, src_u, src_v, dst) = (
        src_y as usize,
        src_u as usize,
        src_v as usize,
        dst as usize,
    );
    par_run(slices, move |y0, rows| unsafe {
        f(
            (src_y + y0 * src_stride_y as usize) as *const u8,
            src_stride_y,
            (src_u + y0 / 2 * src_stride_u as usize) as *const u8,
            src_stride_u,
            (src_v + y0 / 2 * src_stride_v as usize) as *const u8,
            src_stride_v,
            (dst + y0 * dst_stride as usize) as *mut u8,
            dst_stride,
            width,
            rows as i32,
        )
    })
}

#[cfg(test)]
mod par_convert_tests {
    use super::*;

    // Sized to exceed PAR_MIN_PIXELS so the parallel path actually runs; odd
    // height exercises the trailing slice.
    const W: usize = 2560;
    const H: usize = 831;

    fn rgba(seed: u8) -> Vec<u8> {
        (0..W * H * 4)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    #[ignore = "timing probe, run explicitly with --ignored --nocapture"]
    fn bench_5k() {
        const BW: usize = 5120;
        const BH: usize = 2880;
        let yuv: Vec<u8> = (0..BW * BH * 2).map(|i| (i % 251) as u8).collect();
        let (y, uv) = (yuv.as_ptr(), yuv[BW * BH..].as_ptr());
        let mut dst = vec![0u8; BW * BH * 4];
        let mut rgb_src = vec![0u8; BW * BH * 4];
        rgb_src.copy_from_slice(&dst);
        for (name, par) in [("single", false), ("parallel", true)] {
            let t = std::time::Instant::now();
            const N: u32 = 30;
            for _ in 0..N {
                unsafe {
                    if par {
                        par_nv12_to_rgb(
                            NV12ToARGB,
                            y,
                            BW as _,
                            uv,
                            BW as _,
                            dst.as_mut_ptr(),
                            (BW * 4) as _,
                            BW as _,
                            BH as _,
                        );
                    } else {
                        NV12ToARGB(
                            y,
                            BW as _,
                            uv,
                            BW as _,
                            dst.as_mut_ptr(),
                            (BW * 4) as _,
                            BW as _,
                            BH as _,
                        );
                    }
                }
            }
            eprintln!(
                "nv12->argb 5K {name}: {:.2} ms/frame",
                t.elapsed().as_secs_f64() * 1000.0 / N as f64
            );
            let t = std::time::Instant::now();
            for _ in 0..N {
                unsafe {
                    if par {
                        par_rgb_to_nv12(
                            ARGBToNV12,
                            rgb_src.as_ptr(),
                            (BW * 4) as _,
                            dst.as_mut_ptr(),
                            BW as _,
                            dst[BW * BH..].as_mut_ptr(),
                            BW as _,
                            BW as _,
                            BH as _,
                        );
                    } else {
                        ARGBToNV12(
                            rgb_src.as_ptr(),
                            (BW * 4) as _,
                            dst.as_mut_ptr(),
                            BW as _,
                            dst[BW * BH..].as_mut_ptr(),
                            BW as _,
                            BW as _,
                            BH as _,
                        );
                    }
                }
            }
            eprintln!(
                "argb->nv12 5K {name}: {:.2} ms/frame",
                t.elapsed().as_secs_f64() * 1000.0 / N as f64
            );
        }
    }

    #[test]
    fn par_rgb_to_nv12_matches_single() {
        let src = rgba(7);
        let (sy, suv) = (W, W);
        let mut one = vec![0u8; sy * H + suv * (H + 1) / 2];
        let mut par = one.clone();
        unsafe {
            let r = ARGBToNV12(
                src.as_ptr(),
                (W * 4) as _,
                one.as_mut_ptr(),
                sy as _,
                one[sy * H..].as_mut_ptr(),
                suv as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
            let r = par_rgb_to_nv12(
                ARGBToNV12,
                src.as_ptr(),
                (W * 4) as _,
                par.as_mut_ptr(),
                sy as _,
                par[sy * H..].as_mut_ptr(),
                suv as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
        }
        assert!(one == par);
    }

    #[test]
    fn par_rgb_to_i420_matches_single() {
        let src = rgba(3);
        let (sy, sc) = (W, W / 2);
        let plane_c = sc * (H + 1) / 2;
        let mut one = vec![0u8; sy * H + 2 * plane_c];
        let mut par = one.clone();
        unsafe {
            let r = ARGBToI420(
                src.as_ptr(),
                (W * 4) as _,
                one.as_mut_ptr(),
                sy as _,
                one[sy * H..].as_mut_ptr(),
                sc as _,
                one[sy * H + plane_c..].as_mut_ptr(),
                sc as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
            let r = par_rgb_to_i420(
                ARGBToI420,
                src.as_ptr(),
                (W * 4) as _,
                par.as_mut_ptr(),
                sy as _,
                par[sy * H..].as_mut_ptr(),
                sc as _,
                par[sy * H + plane_c..].as_mut_ptr(),
                sc as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
        }
        assert!(one == par);
    }

    #[test]
    fn par_nv12_to_rgb_matches_single() {
        let sy = W;
        let suv = W;
        let yuv = rgba(11);
        let (y, uv) = (yuv.as_ptr(), yuv[sy * H..].as_ptr());
        let mut one = vec![0u8; W * H * 4];
        let mut par = one.clone();
        unsafe {
            let r = NV12ToARGB(
                y,
                sy as _,
                uv,
                suv as _,
                one.as_mut_ptr(),
                (W * 4) as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
            let r = par_nv12_to_rgb(
                NV12ToARGB,
                y,
                sy as _,
                uv,
                suv as _,
                par.as_mut_ptr(),
                (W * 4) as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
        }
        assert!(one == par);
    }

    #[test]
    fn par_i420_to_rgb_matches_single() {
        let sy = W;
        let sc = W / 2;
        let plane_c = sc * (H + 1) / 2;
        let yuv = rgba(23);
        let y = yuv.as_ptr();
        let u = yuv[sy * H..].as_ptr();
        let v = yuv[sy * H + plane_c..].as_ptr();
        let mut one = vec![0u8; W * H * 4];
        let mut par = one.clone();
        unsafe {
            let r = I420ToARGB(
                y,
                sy as _,
                u,
                sc as _,
                v,
                sc as _,
                one.as_mut_ptr(),
                (W * 4) as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
            let r = par_i420_to_rgb(
                I420ToARGB,
                y,
                sy as _,
                u,
                sc as _,
                v,
                sc as _,
                par.as_mut_ptr(),
                (W * 4) as _,
                W as _,
                H as _,
            );
            assert_eq!(r, 0);
        }
        assert!(one == par);
    }
}

#[cfg(not(target_os = "ios"))]
pub fn convert(captured: &PixelBuffer, pixfmt: crate::Pixfmt, dst: &mut Vec<u8>) -> ResultType<()> {
    if captured.pixfmt() == pixfmt {
        dst.extend_from_slice(captured.data());
        return Ok(());
    }

    let src = captured.data();
    let src_stride = captured.stride();
    let src_pixfmt = captured.pixfmt();
    let src_width = captured.width();
    let src_height = captured.height();

    let unsupported = format!(
        "unsupported pixfmt conversion: {src_pixfmt:?} -> {:?}",
        pixfmt
    );

    match (src_pixfmt, pixfmt) {
        (crate::Pixfmt::BGRA, crate::Pixfmt::RGBA) | (crate::Pixfmt::RGBA, crate::Pixfmt::BGRA) => {
            dst.resize(src.len(), 0);
            call_yuv!(ABGRToARGB(
                src.as_ptr(),
                src_stride[0] as _,
                dst.as_mut_ptr(),
                src_stride[0] as _,
                src_width as _,
                src_height as _,
            ));
        }
        _ => {
            bail!(unsupported);
        }
    }
    Ok(())
}

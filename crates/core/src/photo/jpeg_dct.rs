//! Lossless-outside-the-faces JPEG redaction in the DCT domain.
//!
//! Decoding a JPEG to pixels and encoding it again degrades the *whole*
//! image. Instead we read the quantized DCT coefficients with libjpeg
//! (`jpeg_read_coefficients`), recompute only the 8×8 blocks that overlap a
//! redaction region from the redacted pixels, and write the coefficients back
//! (`jpeg_write_coefficients`). Every other block is copied bit-for-bit, so
//! the image outside the faces is **exactly** the original.
//!
//! Supported: 8-bit baseline/progressive, YCbCr (any chroma subsampling) and
//! grayscale. Other color spaces (CMYK, YCCK, RGB-coded JPEGs) are reported as
//! [`Error::Unsupported`] so the caller can fall back to a full re-encode.

use crate::{Error, Result};
use mozjpeg_sys::*;
use std::f32::consts::PI;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Decoded pixels the recomputed blocks are taken from.
#[derive(Debug)]
pub struct Pixels<'a> {
    pub width: usize,
    pub height: usize,
    /// 1 (gray) or 3 (RGB) interleaved 8-bit channels.
    pub channels: usize,
    pub data: &'a [u8],
}

impl Pixels<'_> {
    /// Sample of component `ci` (0 = Y, 1 = Cb, 2 = Cr) at an image pixel,
    /// with edge replication (the same padding libjpeg uses).
    #[inline]
    fn component(&self, ci: usize, x: usize, y: usize) -> f32 {
        let x = x.min(self.width - 1);
        let y = y.min(self.height - 1);
        let i = (y * self.width + x) * self.channels;
        if self.channels == 1 {
            return self.data[i] as f32;
        }
        let (r, g, b) = (self.data[i] as f32, self.data[i + 1] as f32, self.data[i + 2] as f32);
        // JFIF (full range BT.601) RGB -> YCbCr, as in libjpeg's jccolor.c.
        match ci {
            0 => 0.299 * r + 0.587 * g + 0.114 * b,
            1 => -0.168_736 * r - 0.331_264 * g + 0.5 * b + 128.0,
            _ => 0.5 * r - 0.418_688 * g - 0.081_312 * b + 128.0,
        }
    }
}

/// Statistics about a DCT-domain edit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DctStats {
    pub blocks_total: usize,
    pub blocks_changed: usize,
}

/// Rewrites the blocks of `jpeg` for which `covered(x, y)` holds for at least
/// one pixel of their footprint, using `pixels` as the new content.
///
/// The returned JPEG contains no `APPn`/`COM` segments; the caller grafts the
/// (filtered) original metadata back.
pub fn redact(
    jpeg: &[u8],
    progressive: bool,
    pixels: &Pixels<'_>,
    covered: &(dyn Fn(usize, usize) -> bool + Sync),
    bounds: (usize, usize, usize, usize),
) -> Result<(Vec<u8>, DctStats)> {
    match catch_unwind(AssertUnwindSafe(|| unsafe { redact_unchecked(jpeg, progressive, pixels, covered, bounds) })) {
        Ok(r) => r,
        Err(panic) => {
            let msg = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "unknown libjpeg failure".into());
            Err(Error::Codec(msg))
        }
    }
}

/// libjpeg reports fatal errors through this callback; we turn them into a
/// Rust panic that unwinds back to [`redact`] (enabled by mozjpeg-sys'
/// `unwinding` feature).
unsafe extern "C-unwind" fn error_exit(cinfo: &mut jpeg_common_struct) {
    let mut buf = [0u8; 80];
    if let Some(fmt) = (*cinfo.err).format_message {
        // The binding declares the buffer as `&[u8; 80]` but libjpeg writes to it.
        let fmt: unsafe extern "C-unwind" fn(&mut jpeg_common_struct, *mut u8) = std::mem::transmute(fmt);
        fmt(cinfo, buf.as_mut_ptr());
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    std::panic::panic_any(format!("libjpeg: {}", String::from_utf8_lossy(&buf[..end])));
}

unsafe extern "C-unwind" fn silent(_: &mut jpeg_common_struct, _: std::os::raw::c_int) {}

/// Owns the libjpeg state and releases it even when unwinding.
struct Codec {
    err: jpeg_error_mgr,
    d: jpeg_decompress_struct,
    c: jpeg_compress_struct,
    d_init: bool,
    c_init: bool,
    out: *mut u8,
    out_len: std::os::raw::c_ulong,
}

impl Drop for Codec {
    fn drop(&mut self) {
        unsafe {
            if self.c_init {
                jpeg_destroy_compress(&mut self.c);
            }
            if self.d_init {
                jpeg_destroy_decompress(&mut self.d);
            }
            if !self.out.is_null() {
                libc::free(self.out.cast());
            }
        }
    }
}

unsafe fn redact_unchecked(
    jpeg: &[u8],
    progressive: bool,
    pixels: &Pixels<'_>,
    covered: &(dyn Fn(usize, usize) -> bool + Sync),
    bounds: (usize, usize, usize, usize),
) -> Result<(Vec<u8>, DctStats)> {
    // Boxed so that the self-referential pointers (cinfo.err) stay valid.
    let mut k: Box<Codec> = Box::new(std::mem::zeroed());
    jpeg_std_error(&mut k.err);
    k.err.error_exit = Some(error_exit);
    k.err.emit_message = Some(silent);
    let err: *mut jpeg_error_mgr = &mut k.err;

    k.d.common.err = err;
    jpeg_create_decompress(&mut k.d);
    k.d_init = true;
    jpeg_mem_src(&mut k.d, jpeg.as_ptr(), jpeg.len() as _);
    jpeg_read_header(&mut k.d, 1);

    let ncomp = k.d.num_components as usize;
    let supported = matches!(
        (k.d.jpeg_color_space, ncomp, pixels.channels),
        (J_COLOR_SPACE::JCS_YCbCr, 3, 3) | (J_COLOR_SPACE::JCS_GRAYSCALE, 1, 1)
    );
    if !supported {
        return Err(Error::Unsupported(format!("JPEG color space {:?} with {ncomp} components", k.d.jpeg_color_space)));
    }
    if k.d.image_width as usize != pixels.width || k.d.image_height as usize != pixels.height {
        return Err(Error::Codec("decoded size does not match the JPEG header".into()));
    }

    let coefs = jpeg_read_coefficients(&mut k.d);
    if coefs.is_null() {
        return Err(Error::Codec("cannot read DCT coefficients".into()));
    }

    let basis = dct_basis();
    let (hmax, vmax) = (k.d.max_h_samp_factor as usize, k.d.max_v_samp_factor as usize);
    let mut stats = DctStats::default();
    let access = (*k.d.common.mem).access_virt_barray.expect("libjpeg memory manager");

    for ci in 0..ncomp {
        let comp = &*k.d.comp_info.add(ci);
        let qt = &*comp.quant_table;
        // Image pixels covered by one component sample.
        let fx = hmax / comp.h_samp_factor as usize;
        let fy = vmax / comp.v_samp_factor as usize;
        let (bw, bh) = (comp.width_in_blocks as usize, comp.height_in_blocks as usize);
        stats.blocks_total += bw * bh;

        // Only visit blocks intersecting the redaction bounds.
        let (x0, y0, x1, y1) = bounds;
        let bx0 = x0 / (8 * fx);
        let by0 = y0 / (8 * fy);
        let bx1 = (x1.div_ceil(8 * fx)).min(bw);
        let by1 = (y1.div_ceil(8 * fy)).min(bh);

        for by in by0..by1 {
            let rows = access(&mut k.d.common, *coefs.add(ci), by as JDIMENSION, 1, 1);
            let row: JBLOCKROW = *rows;
            for bx in bx0..bx1 {
                let (px0, py0) = (bx * 8 * fx, by * 8 * fy);
                let (px1, py1) = ((px0 + 8 * fx).min(pixels.width), (py0 + 8 * fy).min(pixels.height));
                let touched = (py0..py1).any(|y| (px0..px1).any(|x| covered(x, y)));
                if !touched {
                    continue;
                }
                // Gather the 8x8 component samples (averaging over the
                // subsampling footprint), level shifted.
                let mut samples = [0f32; 64];
                for sy in 0..8 {
                    for sx in 0..8 {
                        let mut acc = 0.0;
                        for dy in 0..fy {
                            for dx in 0..fx {
                                acc += pixels.component(ci, px0 + sx * fx + dx, py0 + sy * fy + dy);
                            }
                        }
                        samples[sy * 8 + sx] = acc / (fx * fy) as f32 - 128.0;
                    }
                }
                let block: &mut JBLOCK = &mut *row.add(bx);
                fdct_quantize(&samples, &basis, &qt.quantval, block);
                stats.blocks_changed += 1;
            }
        }
    }

    k.c.common.err = err;
    jpeg_create_compress(&mut k.c);
    k.c_init = true;
    // Plain libjpeg-turbo behaviour; mozjpeg's extra passes don't apply to
    // coefficient transcoding anyway.
    jpeg_c_set_int_param(&mut k.c, J_INT_PARAM::JINT_COMPRESS_PROFILE, JINT_COMPRESS_PROFILE_VALUE::JCP_FASTEST as _);
    let (out_ptr, len_ptr): (*mut *mut u8, *mut std::os::raw::c_ulong) = (&mut k.out, &mut k.out_len);
    jpeg_mem_dest(&mut k.c, out_ptr, len_ptr);
    jpeg_copy_critical_parameters(&k.d, &mut k.c);
    k.c.optimize_coding = 1; // optimal Huffman tables: lossless and smaller
    k.c.write_JFIF_header = 0;
    k.c.write_Adobe_marker = 0;
    if progressive {
        jpeg_simple_progression(&mut k.c);
    }
    jpeg_write_coefficients(&mut k.c, coefs);
    jpeg_finish_compress(&mut k.c);
    jpeg_finish_decompress(&mut k.d);

    let out = std::slice::from_raw_parts(k.out, k.out_len as usize).to_vec();
    Ok((out, stats))
}

/// `basis[u][x] = C(u)/2 · cos((2x+1)uπ/16)`, the 1-D DCT-II basis with the
/// JPEG normalisation.
fn dct_basis() -> [[f32; 8]; 8] {
    std::array::from_fn(|u| {
        let cu = if u == 0 { std::f32::consts::FRAC_1_SQRT_2 } else { 1.0 };
        std::array::from_fn(|x| cu / 2.0 * ((2 * x + 1) as f32 * u as f32 * PI / 16.0).cos())
    })
}

/// Forward 2-D DCT of a level-shifted block followed by quantization.
/// Coefficients are stored in natural (row-major) order, like libjpeg.
fn fdct_quantize(samples: &[f32; 64], basis: &[[f32; 8]; 8], qt: &[u16; 64], out: &mut [i16; 64]) {
    // Rows first: tmp[y][u] = Σx basis[u][x] · s[y][x]
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            tmp[y * 8 + u] = (0..8).map(|x| basis[u][x] * samples[y * 8 + x]).sum();
        }
    }
    // Then columns: F[v][u] = Σy basis[v][y] · tmp[y][u]
    for v in 0..8 {
        for u in 0..8 {
            let f: f32 = (0..8).map(|y| basis[v][y] * tmp[y * 8 + u]).sum();
            let q = qt[v * 8 + u].max(1) as f32;
            out[v * 8 + u] = (f / q).round().clamp(-32768.0, 32767.0) as i16;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_of_flat_block() {
        let basis = dct_basis();
        let samples = [-128.0f32; 64]; // black
        let mut out = [0i16; 64];
        fdct_quantize(&samples, &basis, &[1; 64], &mut out);
        assert_eq!(out[0], -1024, "DC must be 8 × mean");
        assert!(out[1..].iter().all(|&c| c == 0));
    }

    #[test]
    fn dct_is_orthonormal_up_to_jpeg_scaling() {
        // A single cosine pattern must map to a single coefficient.
        let basis = dct_basis();
        let samples: [f32; 64] = std::array::from_fn(|i| 50.0 * basis[1][i % 8] * 2.0);
        let mut out = [0i16; 64];
        fdct_quantize(&samples, &basis, &[1; 64], &mut out);
        let big: Vec<usize> = (0..64).filter(|&i| out[i].abs() > 1).collect();
        assert_eq!(big, vec![1]);
    }
}

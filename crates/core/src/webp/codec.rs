//! The libwebp FFI: the only `unsafe` code of the WebP module.
//!
//! libwebp reports every failure through a return value (a null pointer, a
//! zero size or a zero status), so nothing unwinds through C and no Rust
//! callback is handed to it. Every size handed to libwebp is checked first,
//! and every buffer libwebp allocates is freed by [`LibWebpBuffer`] on every
//! path, the error paths included.

use std::os::raw::c_int;
use std::{ptr, slice};

use libwebp_sys::{WebPDecodeRGBAInto, WebPEncodeRGBA, WebPFree, WebPGetInfo};

use crate::lossy::{Quality, reserve_pixels};
use crate::plan::check_pixels;

/// A still image as 8-bit RGBA, four bytes per pixel, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rgba {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pixels: Vec<u8>,
}

/// What the lossy path needs from a WebP codec. [`LibWebp`] is the real one;
/// the tests pass doubles.
pub(super) trait Codec {
    /// Decode a still WebP to RGBA.
    fn decode_rgba(&self, bytes: &[u8]) -> Result<Rgba, String>;
    /// Encode RGBA as a lossy WebP at `quality`.
    fn encode_lossy(&self, rgba: &Rgba, quality: Quality) -> Result<Vec<u8>, String>;
}

/// libwebp's decoder and its simple lossy encoder.
pub(super) struct LibWebp;

impl Codec for LibWebp {
    /// Decode into a buffer of our own, sized from the header and checked
    /// against [`crate::MAX_PIXELS`] before it is allocated; libwebp
    /// allocates no output buffer of its own on this path.
    fn decode_rgba(&self, bytes: &[u8]) -> Result<Rgba, String> {
        let unreadable = |why: &str| format!("could not read the WebP data: {why}");
        let (width, height) =
            info(bytes).ok_or_else(|| unreadable("libwebp could not read its header"))?;
        check_pixels(width, height)?;
        // At most 100 megapixels of 4 bytes (`check_pixels`): 400 MB, which
        // no multiplication here can overflow.
        let stride = width as usize * 4;
        let mut pixels = reserve_pixels(stride * height as usize)?;
        let output_stride = c_int_of(stride).map_err(|why| unreadable(&why))?;
        // SAFETY: libwebp reads at most `bytes.len()` bytes from `bytes`, and
        // writes at most `pixels.len()` bytes into `pixels`, `output_stride`
        // bytes per row; it returns null instead when the image does not fit
        // that buffer or cannot be decoded. Both slices outlive the call.
        let written = unsafe {
            WebPDecodeRGBAInto(
                bytes.as_ptr(),
                bytes.len(),
                pixels.as_mut_ptr(),
                pixels.len(),
                output_stride,
            )
        };
        if written.is_null() {
            return Err(unreadable("libwebp could not decode the image"));
        }
        Ok(Rgba {
            width,
            height,
            pixels,
        })
    }

    /// libwebp's simple `WebPEncodeRGBA`: its default lossy settings at
    /// `quality`, with its default alpha quality of 100. An image with any
    /// alpha below 255 gets an `ALPH` chunk (`VP8X`, `ALPH`, `VP8 `); an
    /// opaque one is a simple `VP8 ` file.
    fn encode_lossy(&self, rgba: &Rgba, quality: Quality) -> Result<Vec<u8>, String> {
        let failed = |why: &str| format!("could not encode the lossy WebP: {why}");
        let stride = (rgba.width as usize).checked_mul(4);
        let len = stride.and_then(|stride| stride.checked_mul(rgba.height as usize));
        let (Some(stride), Some(len)) = (stride, len) else {
            return Err(failed("the pixels do not match the image size"));
        };
        if len != rgba.pixels.len() {
            return Err(failed("the pixels do not match the image size"));
        }
        let width = c_int_of(rgba.width as usize).map_err(|why| failed(&why))?;
        let height = c_int_of(rgba.height as usize).map_err(|why| failed(&why))?;
        let stride = c_int_of(stride).map_err(|why| failed(&why))?;
        let mut output: *mut u8 = ptr::null_mut();
        // SAFETY: `rgba.pixels` holds exactly `height` rows of `stride`
        // bytes (checked above), which libwebp only reads. `output` is a
        // valid place for libwebp to store the pointer to its buffer.
        let size = unsafe {
            WebPEncodeRGBA(
                rgba.pixels.as_ptr(),
                width,
                height,
                stride,
                f32::from(quality.get()),
                &raw mut output,
            )
        };
        // Owned from here, so it is freed on every path below.
        let buffer = LibWebpBuffer(output);
        if size == 0 || buffer.0.is_null() {
            return Err(failed("libwebp failed"));
        }
        // SAFETY: on success `output` points at the `size` bytes libwebp
        // wrote and allocated; `buffer` frees them only after this copy.
        Ok(unsafe { slice::from_raw_parts(buffer.0, size) }.to_vec())
    }
}

/// A buffer libwebp allocated, freed with `WebPFree` when dropped, whatever
/// path the code takes. Null is allowed: `WebPFree(NULL)` does nothing.
struct LibWebpBuffer(*mut u8);

impl Drop for LibWebpBuffer {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or a buffer libwebp allocated and
        // handed over; this guard is its only owner, so it is freed once.
        unsafe { WebPFree(self.0.cast()) }
    }
}

/// The width and height libwebp reads from a WebP's headers (the canvas for
/// an extended file, which libwebp checks against a still image's own size),
/// or `None` when it cannot read them. Nothing is decoded.
pub(super) fn info(bytes: &[u8]) -> Option<(u32, u32)> {
    let (mut width, mut height): (c_int, c_int) = (0, 0);
    // SAFETY: libwebp reads at most `bytes.len()` bytes from `bytes` and
    // writes the two ints, which outlive the call.
    let ok = unsafe { WebPGetInfo(bytes.as_ptr(), bytes.len(), &raw mut width, &raw mut height) };
    if ok == 0 {
        return None;
    }
    Some((u32::try_from(width).ok()?, u32::try_from(height).ok()?))
}

/// `value` as a C `int`, for a size handed to libwebp.
fn c_int_of(value: usize) -> Result<c_int, String> {
    c_int::try_from(value).map_err(|_| "the image is too large for libwebp".to_owned())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::webp::fixtures as fx;
    use crate::webp::riff::tests::{riff, vp8l};

    const W: u32 = 64;
    const H: u32 = 48;

    fn q(value: u8) -> Quality {
        Quality::new(value).unwrap()
    }

    // --- decoding --------------------------------------------------------------

    #[test]
    fn the_real_decoder_gives_the_same_pixels_as_image() {
        let pixels = fx::photo_rgba(W, H, true);
        let file = fx::lossless(&pixels, W, H);
        let decoded = LibWebp.decode_rgba(&file).unwrap();
        assert_eq!((decoded.width, decoded.height), (W, H));
        // A lossless file: the source pixels exactly, as `image` reads them.
        assert_eq!(decoded.pixels, pixels);
        assert_eq!(decoded.pixels, fx::decode_rgba(&file).2);
        // A lossy file with alpha decodes to its size, with the transparent
        // corner still transparent.
        let lossy = fx::lossy(&pixels, W, H, 80.0);
        let decoded = LibWebp.decode_rgba(&lossy).unwrap();
        assert_eq!((decoded.width, decoded.height), (W, H));
        assert_eq!(decoded.pixels.len(), (W * H * 4) as usize);
        assert_eq!(decoded.pixels[3], 0);
    }

    #[test]
    fn the_real_decoder_refuses_bytes_it_cannot_read() {
        for garbage in [&b"not a WebP"[..], b""] {
            assert_eq!(
                LibWebp.decode_rgba(garbage).unwrap_err(),
                "could not read the WebP data: libwebp could not read its header"
            );
        }
        // A VP8L header libwebp accepts, over a bitstream of zeros.
        let broken = riff(&[vp8l(8, 8)]);
        assert_eq!(info(&broken), Some((8, 8)), "fixture: the header reads");
        assert_eq!(
            LibWebp.decode_rgba(&broken).unwrap_err(),
            "could not read the WebP data: libwebp could not decode the image"
        );
    }

    #[test]
    fn the_real_decoder_refuses_a_header_over_max_pixels() {
        // 16384x16384 (268 MP) in a few bytes; refused before any buffer.
        let bomb = riff(&[vp8l(16_384, 16_384)]);
        assert_eq!(info(&bomb), Some((16_384, 16_384)), "fixture");
        let started = Instant::now();
        assert_eq!(
            LibWebp.decode_rgba(&bomb).unwrap_err(),
            "image too large: 16384×16384 exceeds 100 megapixels"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    // --- encoding --------------------------------------------------------------

    #[test]
    fn the_real_encoder_writes_a_lossy_webp_of_the_same_size() {
        let rgba = Rgba {
            width: W,
            height: H,
            pixels: fx::photo_rgba(W, H, false),
        };
        let bytes = LibWebp.encode_lossy(&rgba, q(75)).unwrap();
        assert_eq!(fx::chunk_names(&bytes), ["VP8 "]);
        assert_eq!(info(&bytes), Some((W, H)));
        let (w, h, _) = fx::decode_rgba(&bytes);
        assert_eq!((w, h), (W, H));
        // With alpha: libwebp writes an ALPH chunk next to the image.
        let alpha = Rgba {
            pixels: fx::photo_rgba(W, H, true),
            ..rgba
        };
        let bytes = LibWebp.encode_lossy(&alpha, q(75)).unwrap();
        assert_eq!(fx::chunk_names(&bytes), ["VP8X", "ALPH", "VP8 "]);
    }

    #[test]
    fn the_real_encoder_refuses_pixels_that_do_not_match_the_size() {
        let mut pixels = fx::photo_rgba(W, H, false);
        pixels.pop();
        let short = Rgba {
            width: W,
            height: H,
            pixels,
        };
        assert_eq!(
            LibWebp.encode_lossy(&short, q(75)).unwrap_err(),
            "could not encode the lossy WebP: the pixels do not match the image size"
        );
        // No pixels for a width no C int holds: the size check, not libwebp.
        let wide = Rgba {
            width: u32::MAX,
            height: 0,
            pixels: Vec::new(),
        };
        assert_eq!(
            LibWebp.encode_lossy(&wide, q(75)).unwrap_err(),
            "could not encode the lossy WebP: the image is too large for libwebp"
        );
    }

    #[test]
    fn the_real_encoder_turns_a_libwebp_failure_into_an_error() {
        // Wider than libwebp's 16383 pixels: refused by `WebPEncodeRGBA`.
        let rgba = Rgba {
            width: 20_000,
            height: 1,
            pixels: vec![0; 80_000],
        };
        assert_eq!(
            LibWebp.encode_lossy(&rgba, q(75)).unwrap_err(),
            "could not encode the lossy WebP: libwebp failed"
        );
    }

    // --- helpers -----------------------------------------------------------------

    #[test]
    fn c_int_of_accepts_values_up_to_i32_max() {
        assert_eq!(c_int_of(0), Ok(0));
        assert_eq!(c_int_of(16_383 * 4), Ok(65_532));
        assert_eq!(c_int_of(i32::MAX as usize), Ok(c_int::MAX));
    }

    #[test]
    fn c_int_of_refuses_larger_values() {
        for value in [i32::MAX as usize + 1, u32::MAX as usize, usize::MAX] {
            assert_eq!(
                c_int_of(value).unwrap_err(),
                "the image is too large for libwebp",
                "{value}"
            );
        }
    }

    #[test]
    fn info_reads_the_size_of_a_valid_webp() {
        let file = fx::lossless(&fx::photo_rgba(W, H, false), W, H);
        assert_eq!(info(&file), Some((W, H)));
        let extended = fx::with_chunks(&file, &[(*b"ICCP", &[1; 8])]);
        assert_eq!(info(&extended), Some((W, H)));
    }

    #[test]
    fn info_is_none_for_bytes_libwebp_cannot_read() {
        let file = fx::lossless(&fx::photo_rgba(W, H, false), W, H);
        for bytes in [&b"not a WebP"[..], b"", &file[..10]] {
            assert_eq!(info(bytes), None, "{bytes:?}");
        }
    }
}

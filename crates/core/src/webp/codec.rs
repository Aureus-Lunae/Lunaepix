//! The libwebp FFI: the only `unsafe` code of the WebP module.
//!
//! libwebp reports every failure through a return value (a null pointer, a
//! zero size or a zero status), so nothing unwinds through C and no Rust
//! callback is handed to it. Every size handed to libwebp is checked first,
//! and everything libwebp allocates is released by a guard on every path,
//! the error paths included: [`LibWebpBuffer`] and [`LibWebpData`] for
//! encoded bytes, [`AnimDecoder`] and [`AnimEncoder`] for the animation
//! decoder and encoder, [`Picture`] for a frame handed to the encoder.

use std::ffi::CStr;
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use std::os::raw::c_int;
use std::{ptr, slice};

use libwebp_sys::{
    WEBP_CSP_MODE, WEBP_DEMUX_ABI_VERSION, WEBP_ENCODER_ABI_VERSION, WEBP_MUX_ABI_VERSION,
    WebPAnimDecoder, WebPAnimDecoderDelete, WebPAnimDecoderGetInfo, WebPAnimDecoderGetNext,
    WebPAnimDecoderHasMoreFrames, WebPAnimDecoderNewInternal, WebPAnimDecoderOptions,
    WebPAnimDecoderOptionsInitInternal, WebPAnimEncoder, WebPAnimEncoderAdd,
    WebPAnimEncoderAssemble, WebPAnimEncoderDelete, WebPAnimEncoderGetError,
    WebPAnimEncoderNewInternal, WebPAnimEncoderOptions, WebPAnimEncoderOptionsInitInternal,
    WebPAnimInfo, WebPConfig, WebPConfigInitInternal, WebPData, WebPDecodeRGBAInto, WebPEncodeRGBA,
    WebPFree, WebPGetInfo, WebPMuxAnimParams, WebPPicture, WebPPictureFree, WebPPictureImportRGBA,
    WebPPictureInitInternal, WebPPreset, WebPValidateConfig,
};

use super::riff;
use crate::lossy::{Quality, reserve_pixels};
use crate::plan::check_pixels;

/// A still image as 8-bit RGBA, four bytes per pixel, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rgba {
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) pixels: Vec<u8>,
}

/// The animation-wide settings of an animated WebP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AnimationInfo {
    /// The canvas: every frame is a full canvas of this size.
    pub(super) width: u32,
    pub(super) height: u32,
    /// How often it plays; 0 is forever. From the source's `ANIM`.
    pub(super) loop_count: u32,
    /// The background colour, as libwebp reads it from the source's `ANIM`.
    pub(super) background: u32,
    /// The total duration: the end of the last frame.
    pub(super) duration_ms: u32,
}

/// One frame of an animation, as a full canvas of 8-bit RGBA, and when it
/// starts showing: the end of the frame before it, 0 for the first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Frame {
    pub(super) rgba: Vec<u8>,
    pub(super) timestamp_ms: u32,
}

/// The frames of an animation, decoded one at a time as they are taken, so
/// only one canvas is held at once.
pub(super) type Frames<'a> = Box<dyn Iterator<Item = Result<Frame, String>> + 'a>;

/// The longest total duration libwebp's animation code can count, in
/// milliseconds: its timestamps are C ints.
pub(super) const MAX_TIMESTAMP_MS: u64 = i32::MAX as u64;

/// What the lossy path needs from a WebP codec. [`LibWebp`] is the real one;
/// the tests pass doubles.
pub(super) trait Codec {
    /// Decode a still WebP to RGBA.
    fn decode_rgba(&self, bytes: &[u8]) -> Result<Rgba, String>;
    /// Encode RGBA as a lossy WebP at `quality`.
    fn encode_lossy(&self, rgba: &Rgba, quality: Quality) -> Result<Vec<u8>, String>;
    /// Open an animated WebP: its settings, and its frames, each decoded as
    /// a full canvas when it is taken.
    fn decode_animation<'a>(&self, bytes: &'a [u8]) -> Result<(AnimationInfo, Frames<'a>), String>;
    /// Encode `frames`, taken one at a time, as a lossy animated WebP at
    /// `quality`, with the settings in `info`.
    fn encode_animation(
        &self,
        info: &AnimationInfo,
        frames: &mut dyn Iterator<Item = Result<Frame, String>>,
        quality: Quality,
    ) -> Result<Vec<u8>, String>;
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

    /// libwebp's `WebPAnimDecoder` in non-premultiplied RGBA, without
    /// threads. The total duration comes from the frame headers
    /// ([`riff::animation_header`]); it is checked against
    /// [`MAX_TIMESTAMP_MS`] before libwebp sees the file, because libwebp
    /// adds the frame durations up in a C int and an overflow there is
    /// undefined behaviour. The frames are decoded as they are taken
    /// ([`LibWebpFrames`]).
    fn decode_animation<'a>(&self, bytes: &'a [u8]) -> Result<(AnimationInfo, Frames<'a>), String> {
        let unreadable = |why: &str| format!("could not read the WebP data: {why}");
        let header = riff::animation_header(&riff::parse(bytes)?)?;
        let duration_ms = u32::try_from(header.duration_ms)
            .ok()
            .filter(|&total| u64::from(total) <= MAX_TIMESTAMP_MS)
            .ok_or_else(|| {
                unreadable(&format!(
                    "the animation is longer than libwebp can count ({} ms)",
                    header.duration_ms
                ))
            })?;

        let mut options = WebPAnimDecoderOptions {
            color_mode: WEBP_CSP_MODE::MODE_RGBA,
            use_threads: 0,
            padding: [0; 7],
        };
        // SAFETY: `options` is a valid struct for libwebp to write its
        // defaults into; it returns 0 without writing on an ABI mismatch.
        let ok = unsafe { WebPAnimDecoderOptionsInitInternal(&raw mut options, DEMUX_ABI) };
        if ok == 0 {
            return Err(unreadable(
                "libwebp's animation decoder is of another version",
            ));
        }
        options.color_mode = WEBP_CSP_MODE::MODE_RGBA;
        options.use_threads = 0;
        let data = WebPData {
            bytes: bytes.as_ptr(),
            size: bytes.len(),
        };
        // SAFETY: libwebp reads `data` and `options` during the call only.
        // The decoder keeps pointing into `bytes` (its demuxer does not copy
        // them), which the guard's lifetime `'a` keeps alive for as long as
        // the decoder exists. Null on any failure.
        let decoder = AnimDecoder(
            unsafe { WebPAnimDecoderNewInternal(&raw const data, &raw const options, DEMUX_ABI) },
            PhantomData,
        );
        if decoder.0.is_null() {
            return Err(unreadable("libwebp could not decode the animation"));
        }
        let mut raw = WebPAnimInfo::default();
        // SAFETY: `decoder` is a live decoder and `raw` a valid struct to
        // write into.
        if unsafe { WebPAnimDecoderGetInfo(decoder.0, &raw mut raw) } == 0 {
            return Err(unreadable("libwebp could not read the animation"));
        }
        // libwebp allocated its canvas as exactly this many bytes.
        let canvas_len = (raw.canvas_width as usize)
            .checked_mul(4)
            .and_then(|stride| stride.checked_mul(raw.canvas_height as usize))
            .ok_or_else(|| unreadable("the canvas is too large"))?;
        let info = AnimationInfo {
            width: raw.canvas_width,
            height: raw.canvas_height,
            loop_count: raw.loop_count,
            background: raw.bgcolor,
            duration_ms,
        };
        let frames = LibWebpFrames {
            decoder,
            canvas_len,
            end_ms: 0,
            number: 0,
            done: false,
        };
        Ok((info, Box::new(frames)))
    }

    /// libwebp's `WebPAnimEncoder`: the loop count and background colour of
    /// `info`, `allow_mixed = 0` and libwebp's other defaults, and every
    /// frame encoded lossy with the default preset at `quality` (alpha
    /// quality 100, as for a still image). Each frame is added at its
    /// timestamp as it is taken and freed again, and the animation ends at
    /// `info.duration_ms`.
    fn encode_animation(
        &self,
        info: &AnimationInfo,
        frames: &mut dyn Iterator<Item = Result<Frame, String>>,
        quality: Quality,
    ) -> Result<Vec<u8>, String> {
        let failed = |why: &str| format!("could not encode the lossy animation: {why}");
        let too_long = || failed("the animation is too long for libwebp");
        let width = c_int_of(info.width as usize).map_err(|why| failed(&why))?;
        let height = c_int_of(info.height as usize).map_err(|why| failed(&why))?;
        let stride = (info.width as usize).checked_mul(4);
        let canvas = stride.and_then(|stride| stride.checked_mul(info.height as usize));
        let (Some(stride), Some(canvas_len)) = (stride, canvas) else {
            return Err(failed("the image is too large for libwebp"));
        };
        let stride = c_int_of(stride).map_err(|why| failed(&why))?;
        let loop_count = c_int_of(info.loop_count as usize)
            .map_err(|_| failed("the loop count is too large for libwebp"))?;
        let end = c_int_of(info.duration_ms as usize).map_err(|_| too_long())?;

        let config = lossy_config(quality).map_err(|why| failed(&why))?;
        let mut options = WebPAnimEncoderOptions {
            anim_params: WebPMuxAnimParams {
                bgcolor: 0,
                loop_count: 0,
            },
            minimize_size: 0,
            kmin: 0,
            kmax: 0,
            allow_mixed: 0,
            verbose: 0,
            padding: [0; 4],
        };
        // SAFETY: `options` is a valid struct for libwebp to write its
        // defaults into; it returns 0 without writing on an ABI mismatch.
        if unsafe { WebPAnimEncoderOptionsInitInternal(&raw mut options, MUX_ABI) } == 0 {
            return Err(failed("libwebp's animation encoder is of another version"));
        }
        options.anim_params.bgcolor = info.background;
        options.anim_params.loop_count = loop_count;
        options.allow_mixed = 0;
        // SAFETY: libwebp reads `options` during the call only, and checks
        // the canvas size itself; null on any failure.
        let encoder = AnimEncoder(unsafe {
            WebPAnimEncoderNewInternal(width, height, &raw const options, MUX_ABI)
        });
        if encoder.0.is_null() {
            return Err(failed("libwebp could not start the encoder"));
        }

        for frame in frames {
            let frame = frame?;
            if frame.rgba.len() != canvas_len {
                return Err(failed("a frame does not match the canvas"));
            }
            let timestamp = c_int_of(frame.timestamp_ms as usize).map_err(|_| too_long())?;
            let mut picture = Picture::new(width, height).map_err(|why| failed(&why))?;
            // SAFETY: `picture` is initialised with the canvas size and ARGB
            // storage; libwebp allocates that storage itself (freed by the
            // guard) and reads `height` rows of `stride` bytes from
            // `frame.rgba`, which holds exactly that many (checked above).
            let imported =
                unsafe { WebPPictureImportRGBA(&raw mut picture.0, frame.rgba.as_ptr(), stride) };
            if imported == 0 {
                return Err(failed("libwebp could not take the frame"));
            }
            drop(frame);
            // SAFETY: `encoder` is live; libwebp reads `picture` and
            // `config` during the call only (it copies what it keeps).
            let added = unsafe {
                WebPAnimEncoderAdd(encoder.0, &raw mut picture.0, timestamp, &raw const config)
            };
            if added == 0 {
                return Err(failed(&encoder.error()));
            }
        }
        // SAFETY: `encoder` is live; a null frame marks the end of the last
        // frame at `end`, and libwebp reads no config for it.
        if unsafe { WebPAnimEncoderAdd(encoder.0, ptr::null_mut(), end, ptr::null()) } == 0 {
            return Err(failed(&encoder.error()));
        }
        let mut data = LibWebpData(WebPData {
            bytes: ptr::null(),
            size: 0,
        });
        // SAFETY: `encoder` is live and `data` a valid, empty struct for
        // libwebp to store its buffer in; `data` owns that buffer from here
        // and frees it on every path.
        if unsafe { WebPAnimEncoderAssemble(encoder.0, &raw mut data.0) } == 0 {
            return Err(failed(&encoder.error()));
        }
        if data.0.bytes.is_null() || data.0.size == 0 {
            return Err(failed("libwebp wrote nothing"));
        }
        // SAFETY: on success `data` points at the `size` bytes libwebp
        // allocated and wrote; `data` frees them only after this copy.
        Ok(unsafe { slice::from_raw_parts(data.0.bytes, data.0.size) }.to_vec())
    }
}

/// The ABI versions of the headers libwebp-sys was generated from, which
/// libwebp checks on every `…Internal` call.
const DEMUX_ABI: c_int = WEBP_DEMUX_ABI_VERSION as c_int;
const MUX_ABI: c_int = WEBP_MUX_ABI_VERSION as c_int;
const ENCODER_ABI: c_int = WEBP_ENCODER_ABI_VERSION as c_int;

/// libwebp's default lossy settings at `quality`, as `WebPEncodeRGBA` uses
/// them for a still image, checked by libwebp.
fn lossy_config(quality: Quality) -> Result<WebPConfig, String> {
    let mut config = MaybeUninit::<WebPConfig>::zeroed();
    // SAFETY: libwebp writes its defaults into `config` (all zeros is a
    // valid `WebPConfig`, and libwebp leaves one field as it is); it returns
    // 0 on an ABI mismatch.
    let ok = unsafe {
        WebPConfigInitInternal(
            config.as_mut_ptr(),
            WebPPreset::WEBP_PRESET_DEFAULT,
            f32::from(quality.get()),
            ENCODER_ABI,
        )
    };
    if ok == 0 {
        return Err("libwebp's encoder is of another version".to_owned());
    }
    // SAFETY: zeroed, then initialised by libwebp: every field is valid.
    let mut config = unsafe { config.assume_init() };
    config.lossless = 0;
    // SAFETY: libwebp only reads `config`.
    if unsafe { WebPValidateConfig(&raw const config) } == 0 {
        return Err("libwebp refused the settings".to_owned());
    }
    Ok(config)
}

/// The frames of a [`LibWebp`] animation, each decoded when it is taken.
/// After an error the iterator ends.
struct LibWebpFrames<'a> {
    decoder: AnimDecoder<'a>,
    /// The size of libwebp's canvas buffer: width × 4 × height.
    canvas_len: usize,
    /// Where the frame before ended; the next one starts there.
    end_ms: u32,
    /// How many frames were taken, for the message.
    number: usize,
    done: bool,
}

impl Iterator for LibWebpFrames<'_> {
    type Item = Result<Frame, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        // SAFETY: the decoder is live; libwebp only reads it.
        if unsafe { WebPAnimDecoderHasMoreFrames(self.decoder.0) } == 0 {
            self.done = true;
            return None;
        }
        self.number += 1;
        let frame = self.decode_next();
        self.done = frame.is_err();
        Some(frame)
    }
}

impl LibWebpFrames<'_> {
    fn decode_next(&mut self) -> Result<Frame, String> {
        let failed = || {
            format!(
                "could not read the WebP data: libwebp could not decode frame {}",
                self.number
            )
        };
        let mut buffer: *mut u8 = ptr::null_mut();
        let mut timestamp: c_int = 0;
        // SAFETY: the decoder is live and both out-pointers are valid places
        // to write to.
        let ok =
            unsafe { WebPAnimDecoderGetNext(self.decoder.0, &raw mut buffer, &raw mut timestamp) };
        if ok == 0 || buffer.is_null() {
            return Err(failed());
        }
        // The end of this frame: never before the end of the one before
        // (durations are unsigned, and the total fits a C int).
        let end = u32::try_from(timestamp)
            .ok()
            .filter(|&end| end >= self.end_ms)
            .ok_or_else(failed)?;
        let mut rgba = reserve_pixels(self.canvas_len)?;
        // SAFETY: on success `buffer` is the decoder's own canvas of
        // `canvas_len` bytes (`anim_decode.c` allocates it as canvas width
        // × 4 × canvas height), which it owns and keeps unchanged until the
        // next `GetNext` or `Delete`; it is copied before either.
        rgba.copy_from_slice(unsafe { slice::from_raw_parts(buffer, self.canvas_len) });
        let frame = Frame {
            rgba,
            timestamp_ms: self.end_ms,
        };
        self.end_ms = end;
        Ok(frame)
    }
}

/// A `WebPAnimDecoder`, deleted when dropped, whatever path the code takes.
/// It reads from bytes that must outlive it, hence the lifetime.
struct AnimDecoder<'a>(*mut WebPAnimDecoder, PhantomData<&'a [u8]>);

impl Drop for AnimDecoder<'_> {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or a decoder libwebp created; this
        // guard is its only owner, so it is deleted once. Null is allowed.
        unsafe { WebPAnimDecoderDelete(self.0) }
    }
}

/// A `WebPAnimEncoder`, deleted when dropped, whatever path the code takes.
struct AnimEncoder(*mut WebPAnimEncoder);

impl AnimEncoder {
    /// libwebp's message for the last failed call, as `libwebp: …`.
    fn error(&self) -> String {
        // SAFETY: the encoder is live; libwebp returns a pointer to its own
        // NUL-terminated message buffer inside the encoder, which lives as
        // long as the encoder, and the text is copied out at once.
        let message = unsafe { WebPAnimEncoderGetError(self.0) };
        if message.is_null() {
            return "libwebp failed".to_owned();
        }
        // SAFETY: see above: a NUL-terminated string owned by the encoder.
        let text = unsafe { CStr::from_ptr(message) }.to_string_lossy();
        format!("libwebp: {text}")
    }
}

impl Drop for AnimEncoder {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or an encoder libwebp created; this
        // guard is its only owner, so it is deleted once. Null is allowed.
        unsafe { WebPAnimEncoderDelete(self.0) }
    }
}

/// A `WebPPicture` holding one frame, its pixel storage freed with
/// `WebPPictureFree` when dropped.
struct Picture(WebPPicture);

impl Picture {
    /// An empty ARGB picture of the canvas size; libwebp allocates its
    /// storage when a frame is imported.
    fn new(width: c_int, height: c_int) -> Result<Picture, String> {
        let mut picture = MaybeUninit::<WebPPicture>::zeroed();
        // SAFETY: libwebp sets every field of `picture` (all zeros is a
        // valid `WebPPicture` before that); it returns 0 on an ABI mismatch.
        if unsafe { WebPPictureInitInternal(picture.as_mut_ptr(), ENCODER_ABI) } == 0 {
            return Err("libwebp's encoder is of another version".to_owned());
        }
        // SAFETY: zeroed, then initialised by libwebp.
        let mut picture = Picture(unsafe { picture.assume_init() });
        picture.0.use_argb = 1;
        picture.0.width = width;
        picture.0.height = height;
        Ok(picture)
    }
}

impl Drop for Picture {
    fn drop(&mut self) {
        // SAFETY: an initialised picture whose storage, if any, libwebp
        // allocated; `WebPPictureFree` frees it and leaves the struct empty.
        unsafe { WebPPictureFree(&raw mut self.0) }
    }
}

/// An encoded animation libwebp allocated, freed with `WebPFree` when
/// dropped. Null is allowed.
struct LibWebpData(WebPData);

impl Drop for LibWebpData {
    fn drop(&mut self) {
        // SAFETY: the pointer is null or a buffer libwebp allocated and
        // handed over; this guard is its only owner, so it is freed once.
        unsafe { WebPFree(self.0.bytes.cast_mut().cast()) }
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
    use crate::webp::riff::tests::{anmf_header, chunk, riff, vp8l, vp8x};

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

    // --- animations: fixtures ---------------------------------------------------------

    /// Opaque RGBA of the photo with its colours shifted by `k`, so every `k`
    /// gives a different frame.
    fn frame_rgba(k: u8) -> Vec<u8> {
        let mut pixels = fx::photo_rgba(W, H, false);
        for pixel in pixels.chunks_mut(4) {
            for channel in &mut pixel[..3] {
                *channel = channel.wrapping_add(k.wrapping_mul(50));
            }
        }
        pixels
    }

    const LOOPS: u16 = 3;
    const BACKGROUND: u32 = 0x8040_2010;

    /// An animation of these frames (lossless, from `image`), with these
    /// durations, looping 3 times on a background colour.
    fn animation_of(frames: &[Vec<u8>], durations: &[u32]) -> Vec<u8> {
        let files: Vec<Vec<u8>> = frames.iter().map(|f| fx::lossless(f, W, H)).collect();
        fx::animated_with(&files, W, H, durations, LOOPS, BACKGROUND)
    }

    fn settings(duration_ms: u32) -> AnimationInfo {
        AnimationInfo {
            width: W,
            height: H,
            loop_count: u32::from(LOOPS),
            background: BACKGROUND,
            duration_ms,
        }
    }

    fn frames_at(rgba: &[Vec<u8>], timestamps: &[u32]) -> Vec<Result<Frame, String>> {
        rgba.iter()
            .zip(timestamps)
            .map(|(rgba, &timestamp_ms)| {
                Ok(Frame {
                    rgba: rgba.clone(),
                    timestamp_ms,
                })
            })
            .collect()
    }

    fn encode(
        info: AnimationInfo,
        frames: Vec<Result<Frame, String>>,
        quality: u8,
    ) -> Result<Vec<u8>, String> {
        LibWebp.encode_animation(&info, &mut frames.into_iter(), q(quality))
    }

    /// `frames` 1x1 frame headers of `duration_ms` each, on a 1x1 canvas,
    /// with no image data: the duration check is all that can pass.
    fn long_animation(durations: &[u32]) -> Vec<u8> {
        let mut chunks = vec![vp8x(0x02, 1, 1), chunk(b"ANIM", &fx::anim_payload(0, 0))];
        chunks.extend(durations.iter().map(|&d| anmf_header(1, 1, d)));
        riff(&chunks)
    }

    /// 128 frames of the longest 24-bit duration and one of `last` ms:
    /// `i32::MAX` in total for `last` = 127.
    fn durations_ending_in(last: u32) -> Vec<u32> {
        let mut durations = vec![0xFF_FFFF; 128];
        durations.push(last);
        durations
    }

    // --- animations: decoding -----------------------------------------------------------

    #[test]
    fn the_real_animation_decoder_gives_every_frame_as_a_full_canvas_with_its_timestamp() {
        let rgba = [frame_rgba(0), frame_rgba(1), frame_rgba(2)];
        let file = animation_of(&rgba, &[100, 250, 70]);

        let (info, frames) = LibWebp.decode_animation(&file).unwrap();
        let frames: Vec<Result<Frame, String>> = frames.collect();

        assert_eq!(info, settings(420));
        let timestamps: Vec<u32> = frames
            .iter()
            .map(|frame| frame.as_ref().unwrap().timestamp_ms)
            .collect();
        assert_eq!(timestamps, [0, 100, 350]);
        // Lossless frames: the source pixels exactly. (Not compared with
        // `image`'s animation decoder, whose compositing is one level off in
        // places.)
        for (at, frame) in frames.iter().enumerate() {
            assert_eq!(frame.as_ref().unwrap().rgba, rgba[at], "frame {at}");
        }
    }

    #[test]
    fn the_real_animation_decoder_composes_a_small_frame_onto_the_full_canvas() {
        // The second frame is 16x16 at 8,4; the canvas around it keeps the
        // first frame (no disposal, and the small frame is opaque).
        let small = fx::photo_rgba(16, 16, false);
        let files = [
            fx::lossless(&frame_rgba(0), W, H),
            fx::lossless(&small, 16, 16),
        ];
        let full_size = fx::animated_with(&files, W, H, &[100, 100], 0, 0);
        // The fixture writes the canvas size into every frame header; the
        // second frame's is its own 16x16.
        let mut chunks = fx::chunks_of(&full_size);
        let anmf = &mut chunks
            .iter_mut()
            .filter(|(n, _)| n == b"ANMF")
            .nth(1)
            .unwrap()
            .1;
        anmf[6..9].copy_from_slice(&15u32.to_le_bytes()[..3]);
        anmf[9..12].copy_from_slice(&15u32.to_le_bytes()[..3]);
        let file = fx::with_frame_at(&fx::from_chunks(&chunks), 1, 8, 4);

        let (_, frames) = LibWebp.decode_animation(&file).unwrap();
        let frames: Vec<Frame> = frames.map(Result::unwrap).collect();

        // The canvas expected: the first frame with the small one pasted at
        // 8,4, row by row.
        let mut expected = frame_rgba(0);
        for row in 0..16 {
            let at = (((4 + row) * W + 8) * 4) as usize;
            let from = (row * 16 * 4) as usize;
            expected[at..at + 64].copy_from_slice(&small[from..from + 64]);
        }
        assert_eq!(frames[0].rgba, frame_rgba(0));
        assert_eq!(frames[1].rgba.len(), (W * H * 4) as usize);
        assert_eq!(frames[1].rgba, expected);
    }

    #[test]
    fn the_real_animation_decoder_refuses_bytes_it_cannot_read() {
        let err = LibWebp.decode_animation(b"not a WebP").err().unwrap();
        assert_eq!(err, "not a WebP file (the content is not WebP data)");
        // A frame of the full canvas width, moved 2 pixels right: it runs
        // off the canvas, which libwebp's demuxer refuses.
        let file = animation_of(&[frame_rgba(0), frame_rgba(1)], &[100, 100]);
        let off = fx::with_frame_at(&file, 1, 2, 0);
        let err = LibWebp.decode_animation(&off).err().unwrap();
        assert_eq!(
            err,
            "could not read the WebP data: libwebp could not decode the animation"
        );
        // The same file in place opens.
        assert!(LibWebp.decode_animation(&file).is_ok());
    }

    #[test]
    fn the_real_animation_decoder_fails_on_a_frame_it_cannot_decode_and_then_stops() {
        // The second frame's image is a VP8L header over a bitstream of zeros.
        let file = animation_of(&[frame_rgba(0), frame_rgba(1), frame_rgba(2)], &[100; 3]);
        let mut chunks = fx::chunks_of(&file);
        let anmf = &mut chunks
            .iter_mut()
            .filter(|(n, _)| n == b"ANMF")
            .nth(1)
            .unwrap()
            .1;
        anmf.truncate(16);
        anmf.extend_from_slice(&vp8l(W, H));
        let broken = fx::from_chunks(&chunks);

        let (_, frames) = LibWebp.decode_animation(&broken).unwrap();
        let frames: Vec<Result<Frame, String>> = frames.collect();

        assert_eq!(frames.len(), 2, "the third frame is never decoded");
        assert_eq!(frames[0].as_ref().unwrap().rgba, frame_rgba(0));
        assert_eq!(
            frames[1].as_ref().unwrap_err(),
            "could not read the WebP data: libwebp could not decode frame 2"
        );
    }

    #[test]
    fn the_real_animation_decoder_refuses_a_duration_over_i32_max_before_libwebp() {
        let file = long_animation(&durations_ending_in(128));
        let started = Instant::now();
        let err = LibWebp.decode_animation(&file).err().unwrap();
        assert_eq!(
            err,
            "could not read the WebP data: the animation is longer than libwebp can count \
             (2147483648 ms)"
        );
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn the_real_animation_decoder_passes_a_duration_of_i32_max_on_to_libwebp() {
        // Exactly i32::MAX in total: past the check, so libwebp gets to
        // refuse the frames, which hold no image.
        let file = long_animation(&durations_ending_in(127));
        let err = LibWebp.decode_animation(&file).err().unwrap();
        assert_eq!(
            err,
            "could not read the WebP data: libwebp could not decode the animation"
        );
    }

    // --- animations: encoding -----------------------------------------------------------

    #[test]
    fn the_real_animation_encoder_writes_the_frames_settings_and_durations() {
        let rgba = [frame_rgba(0), frame_rgba(1), frame_rgba(2)];
        let out = encode(settings(420), frames_at(&rgba, &[0, 100, 350]), 75).unwrap();

        assert_eq!(&fx::chunk_names(&out)[..2], ["VP8X", "ANIM"]);
        assert_eq!(
            fx::chunk_payload(&out, b"ANIM"),
            Some(fx::anim_payload(LOOPS, BACKGROUND))
        );
        assert_eq!(
            out[fx::VP8X_FLAGS_AT] & fx::FLAG_ANIMATION,
            fx::FLAG_ANIMATION
        );
        assert_eq!(info(&out), Some((W, H)));
        assert_eq!(fx::anmf_durations(&out), [100, 250, 70]);
        for chunks in fx::anmf_image_chunks(&out) {
            assert!(chunks.contains(&"VP8 ".to_owned()), "{chunks:?}");
        }
        // An independent decoder sees three frames of the canvas size.
        let frames = fx::decode_frames(&out);
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|(rgba, _)| rgba.len() == (W * H * 4) as usize)
        );
    }

    #[test]
    fn the_real_animation_encoder_ends_the_last_frame_at_the_given_total_duration() {
        let rgba = [frame_rgba(0), frame_rgba(1), frame_rgba(2)];
        let out = encode(settings(500), frames_at(&rgba, &[0, 100, 350]), 75).unwrap();
        assert_eq!(fx::anmf_durations(&out), [100, 250, 150]);
    }

    #[test]
    fn the_real_animation_encoder_writes_every_frame_lossy_even_a_flat_one() {
        let flat: Vec<u8> = [0x80, 0x40, 0x20, 0xFF].repeat((W * H) as usize);
        let rgba = [frame_rgba(0), flat];
        let out = encode(settings(200), frames_at(&rgba, &[0, 100]), 75).unwrap();
        let frames = fx::anmf_image_chunks(&out);
        assert_eq!(frames.len(), 2);
        for chunks in frames {
            assert!(chunks.contains(&"VP8 ".to_owned()), "{chunks:?}");
            assert!(!chunks.contains(&"VP8L".to_owned()), "{chunks:?}");
        }
    }

    #[test]
    fn the_real_animation_encoder_gives_a_smaller_file_at_a_lower_quality() {
        let rgba = [frame_rgba(0), frame_rgba(1)];
        let low = encode(settings(200), frames_at(&rgba, &[0, 100]), 30).unwrap();
        let high = encode(settings(200), frames_at(&rgba, &[0, 100]), 90).unwrap();
        assert!(low.len() < high.len(), "{} vs {}", low.len(), high.len());
    }

    #[test]
    fn the_real_animation_encoder_gives_the_same_bytes_at_the_same_quality() {
        // The control for the test above: only the quality changes the size.
        let rgba = [frame_rgba(0), frame_rgba(1)];
        let first = encode(settings(200), frames_at(&rgba, &[0, 100]), 30).unwrap();
        let second = encode(settings(200), frames_at(&rgba, &[0, 100]), 30).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn the_real_animation_encoder_refuses_a_frame_that_does_not_match_the_canvas() {
        let mut short = frame_rgba(1);
        short.pop();
        let err = encode(
            settings(200),
            frames_at(&[frame_rgba(0), short], &[0, 100]),
            75,
        );
        assert_eq!(
            err.unwrap_err(),
            "could not encode the lossy animation: a frame does not match the canvas"
        );
    }

    #[test]
    fn the_real_animation_encoder_passes_on_a_frame_error() {
        let mut frames = frames_at(&[frame_rgba(0)], &[0]);
        frames.push(Err("frame broke".to_owned()));
        frames.extend(frames_at(&[frame_rgba(1)], &[200]));
        assert_eq!(
            encode(settings(300), frames, 75).unwrap_err(),
            "frame broke"
        );
    }

    #[test]
    fn the_real_animation_encoder_turns_libwebp_failures_into_errors() {
        let rgba = [frame_rgba(0), frame_rgba(1)];
        let out_of_order = "could not encode the lossy animation: libwebp: \
                            ERROR adding frame: timestamps must be non-decreasing.";
        // A frame that starts before the one before it.
        let err = encode(settings(200), frames_at(&rgba, &[100, 0]), 75).unwrap_err();
        assert_eq!(err, out_of_order);
        // A total duration that ends before the last frame starts.
        let err = encode(settings(50), frames_at(&rgba, &[0, 100]), 75).unwrap_err();
        assert_eq!(err, out_of_order);
        // No frames at all.
        let err = encode(settings(0), Vec::new(), 75).unwrap_err();
        assert_eq!(
            err,
            "could not encode the lossy animation: libwebp: ERROR: No frames to assemble."
        );
        // An empty canvas.
        let empty = AnimationInfo {
            width: 0,
            ..settings(100)
        };
        let err = encode(empty, Vec::new(), 75).unwrap_err();
        assert_eq!(
            err,
            "could not encode the lossy animation: libwebp could not start the encoder"
        );
    }

    #[test]
    fn the_real_animation_encoder_refuses_numbers_no_c_int_holds() {
        let wide = AnimationInfo {
            width: u32::MAX,
            ..settings(100)
        };
        assert_eq!(
            encode(wide, Vec::new(), 75).unwrap_err(),
            "could not encode the lossy animation: the image is too large for libwebp"
        );
        // A width a C int holds, whose row of 4 bytes per pixel it does not.
        let wide_row = AnimationInfo {
            width: 1 << 29,
            height: 1,
            ..settings(100)
        };
        assert_eq!(
            encode(wide_row, Vec::new(), 75).unwrap_err(),
            "could not encode the lossy animation: the image is too large for libwebp"
        );
        let loops = AnimationInfo {
            loop_count: u32::MAX,
            ..settings(100)
        };
        assert_eq!(
            encode(loops, Vec::new(), 75).unwrap_err(),
            "could not encode the lossy animation: the loop count is too large for libwebp"
        );
        let too_long =
            "could not encode the lossy animation: the animation is too long for libwebp";
        let late = frames_at(&[frame_rgba(0)], &[u32::MAX]);
        assert_eq!(encode(settings(100), late, 75).unwrap_err(), too_long);
        let frames = frames_at(&[frame_rgba(0)], &[0]);
        assert_eq!(
            encode(settings(u32::MAX), frames, 75).unwrap_err(),
            too_long
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

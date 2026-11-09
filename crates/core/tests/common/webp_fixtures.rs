//! WebP fixtures for the tests, made with encoders other than the code under
//! test: lossless files from the `image` crate (pure Rust), lossy files from
//! libwebp's simple `WebPEncodeRGBA`, and containers put together here, byte
//! by byte. The chunk walk below is deliberately its own small implementation,
//! so a test does not rely on the parser it checks.
//!
//! The file has no `use super`, so the unit tests in `src/webp` include it by
//! path as well.
#![allow(dead_code)]

use std::io::Cursor;

use image::AnimationDecoder;

/// Where the `VP8X` flags byte is in a file that starts with `VP8X`: after the
/// 12-byte RIFF header and the 8-byte chunk header.
pub const VP8X_FLAGS_AT: usize = 20;

/// `VP8X` flag bits.
pub const FLAG_ICC: u8 = 0x20;
pub const FLAG_ALPHA: u8 = 0x10;
pub const FLAG_EXIF: u8 = 0x08;
pub const FLAG_XMP: u8 = 0x04;
pub const FLAG_ANIMATION: u8 = 0x02;

/// A chunk as the fixtures hold it: FourCC and payload, without padding.
pub type RawChunk = ([u8; 4], Vec<u8>);

// --- pixels -------------------------------------------------------------------

/// RGBA pixels with gradients and deterministic noise, so an encoder has
/// something to do. With `alpha`, the alpha channel varies too (and one
/// corner is fully transparent with a colour under it).
pub fn photo_rgba(w: u32, h: u32, alpha: bool) -> Vec<u8> {
    let mut seed = 0x2545_F491_u32 ^ w.wrapping_mul(31) ^ h;
    let mut noise = move || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        (seed % 24) as u8
    };
    let mut pixels = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            pixels.push(((x * 255) / w) as u8 / 2 + noise());
            pixels.push(((y * 255) / h) as u8 / 2 + noise());
            pixels.push((((x + y) * 255) / (w + h)) as u8 / 2 + noise());
            let a = if !alpha {
                255
            } else if x < w / 4 && y < h / 4 {
                0
            } else {
                255 - ((x * 100) / w) as u8
            };
            pixels.push(a);
        }
    }
    pixels
}

// --- encoders -------------------------------------------------------------------

/// A simple lossless WebP (`RIFF`, `VP8L`) from the `image` crate's encoder.
pub fn lossless(rgba: &[u8], w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut out)
        .encode(rgba, w, h, image::ExtendedColorType::Rgba8)
        .unwrap();
    out
}

/// A lossy WebP from libwebp. With any alpha below 255 in `rgba` the file is
/// `VP8X`, `ALPH`, `VP8 `; without, a simple `VP8 ` file.
pub fn lossy(rgba: &[u8], w: u32, h: u32, quality: f32) -> Vec<u8> {
    assert_eq!(rgba.len(), (w * h * 4) as usize);
    let mut output: *mut u8 = std::ptr::null_mut();
    // SAFETY: `rgba` holds w*h*4 bytes (asserted) and the stride is one row;
    // libwebp allocates `output` and returns its size, and `WebPFree` frees
    // it once, after the copy.
    unsafe {
        let size = libwebp_sys::WebPEncodeRGBA(
            rgba.as_ptr(),
            w as i32,
            h as i32,
            (w * 4) as i32,
            quality,
            &mut output,
        );
        assert!(size > 0 && !output.is_null(), "libwebp could not encode");
        let bytes = std::slice::from_raw_parts(output, size).to_vec();
        libwebp_sys::WebPFree(output.cast());
        bytes
    }
}

// --- containers -------------------------------------------------------------------

/// The chunks of a file, in order, ignoring the padding bytes and whatever
/// follows the RIFF size.
pub fn chunks_of(file: &[u8]) -> Vec<RawChunk> {
    assert_eq!(&file[..4], b"RIFF");
    assert_eq!(&file[8..12], b"WEBP");
    let end = 8 + u32::from_le_bytes(file[4..8].try_into().unwrap()) as usize;
    let mut chunks = Vec::new();
    let mut at = 12;
    while at + 8 <= end {
        let fourcc: [u8; 4] = file[at..at + 4].try_into().unwrap();
        let size = u32::from_le_bytes(file[at + 4..at + 8].try_into().unwrap()) as usize;
        chunks.push((fourcc, file[at + 8..at + 8 + size].to_vec()));
        at += 8 + size + (size & 1);
    }
    chunks
}

/// A file of exactly these chunks: every odd-sized one followed by its
/// padding byte, and a RIFF size that is right.
pub fn from_chunks(chunks: &[RawChunk]) -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    for (fourcc, data) in chunks {
        body.extend_from_slice(fourcc);
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(data);
        if data.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// The payload of the first chunk called `fourcc`.
pub fn chunk_payload(file: &[u8], fourcc: &[u8; 4]) -> Option<Vec<u8>> {
    chunks_of(file)
        .into_iter()
        .find(|(name, _)| name == fourcc)
        .map(|(_, data)| data)
}

/// The FourCCs of a file's chunks, as text, in order.
pub fn chunk_names(file: &[u8]) -> Vec<String> {
    chunks_of(file)
        .iter()
        .map(|(name, _)| String::from_utf8_lossy(name).into_owned())
        .collect()
}

/// A `VP8X` payload: flags, 3 reserved zero bytes, and the canvas size.
pub fn vp8x_payload(flags: u8, w: u32, h: u32) -> Vec<u8> {
    let mut data = vec![flags, 0, 0, 0];
    data.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
    data.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
    data
}

/// The size, and whether it has alpha, of the still image in `chunks`.
fn still_image_header(chunks: &[RawChunk]) -> (u32, u32, bool) {
    for (fourcc, data) in chunks {
        match fourcc {
            b"VP8L" => {
                let bits = u32::from_le_bytes(data[1..5].try_into().unwrap());
                return (
                    (bits & 0x3FFF) + 1,
                    ((bits >> 14) & 0x3FFF) + 1,
                    bits >> 28 & 1 == 1,
                );
            }
            b"VP8 " => {
                let w = u16::from_le_bytes([data[6], data[7]]) & 0x3FFF;
                let h = u16::from_le_bytes([data[8], data[9]]) & 0x3FFF;
                let has_alph = chunks.iter().any(|(name, _)| name == b"ALPH");
                return (u32::from(w), u32::from(h), has_alph);
            }
            _ => {}
        }
    }
    panic!("no image chunk");
}

/// `file` with these chunks added, and the matching `VP8X` flags set (`ICCP`,
/// `EXIF`, `XMP `; any other FourCC adds a chunk and no flag). A simple file
/// becomes an extended one, with a canvas as large as its image. `ICCP` goes
/// right after the `VP8X`, every other chunk at the end.
pub fn with_chunks(file: &[u8], extra: &[([u8; 4], &[u8])]) -> Vec<u8> {
    let mut chunks = chunks_of(file);
    if &chunks[0].0 != b"VP8X" {
        let (w, h, alpha) = still_image_header(&chunks);
        let flags = if alpha { FLAG_ALPHA } else { 0 };
        chunks.insert(0, (*b"VP8X", vp8x_payload(flags, w, h)));
    }
    for (fourcc, data) in extra {
        chunks[0].1[0] |= match fourcc {
            b"ICCP" => FLAG_ICC,
            b"EXIF" => FLAG_EXIF,
            b"XMP " => FLAG_XMP,
            _ => 0,
        };
        let at = if fourcc == b"ICCP" { 1 } else { chunks.len() };
        chunks.insert(at, (*fourcc, data.to_vec()));
    }
    from_chunks(&chunks)
}

/// An animated WebP: `VP8X`, `ANIM` and one `ANMF` per frame, each frame the
/// image chunks of one of `frames` (whole simple files of `w`x`h` pixels),
/// shown for 100 ms.
pub fn animated(frames: &[Vec<u8>], w: u32, h: u32) -> Vec<u8> {
    let mut chunks: Vec<RawChunk> = vec![
        (*b"VP8X", vp8x_payload(FLAG_ANIMATION | FLAG_ALPHA, w, h)),
        // Background colour, then a loop count of 0 (forever).
        (*b"ANIM", vec![0; 6]),
    ];
    for frame in frames {
        // The frame header: X and Y (halved), width-1, height-1, the
        // duration, and a flags byte, 3 bytes each but the last.
        let mut data = vec![0u8; 6];
        data.extend_from_slice(&(w - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&(h - 1).to_le_bytes()[..3]);
        data.extend_from_slice(&100u32.to_le_bytes()[..3]);
        data.push(0);
        let inner: Vec<RawChunk> = chunks_of(frame)
            .into_iter()
            .filter(|(name, _)| matches!(name, b"ALPH" | b"VP8 " | b"VP8L"))
            .collect();
        // The sub-chunks, in the same layout as in a file.
        data.extend_from_slice(&from_chunks(&inner)[12..]);
        chunks.push((*b"ANMF", data));
    }
    from_chunks(&chunks)
}

// --- decoding ----------------------------------------------------------------------

/// A still image decoded with the `image` crate: its size and RGBA bytes.
pub fn decode_rgba(file: &[u8]) -> (u32, u32, Vec<u8>) {
    let image = image::load_from_memory_with_format(file, image::ImageFormat::WebP)
        .expect("the file must decode")
        .to_rgba8();
    (image.width(), image.height(), image.into_raw())
}

/// Every frame of an animated file decoded with the `image` crate: the RGBA
/// bytes and the delay of each.
pub fn decode_frames(file: &[u8]) -> Vec<(Vec<u8>, (u32, u32))> {
    image::codecs::webp::WebPDecoder::new(Cursor::new(file))
        .expect("the file must open")
        .into_frames()
        .collect_frames()
        .expect("every frame must decode")
        .into_iter()
        .map(|frame| {
            (
                frame.buffer().as_raw().clone(),
                frame.delay().numer_denom_ms(),
            )
        })
        .collect()
}

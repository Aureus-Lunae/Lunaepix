//! Lossy PNG: the source is decoded to 8-bit RGBA, reduced to a palette of
//! at most 256 colours with alpha by quantizr (with dithering), written as an
//! indexed PNG and then passed through oxipng with today's lossless settings
//! ([`run_oxipng`]).
//!
//! The source's ancillary chunks before IDAT (iCCP, sRGB, gAMA, cHRM, cICP,
//! pHYs, text, ...) are put back into the indexed PNG unchanged, so oxipng's
//! `StripChunks::Safe` keeps or drops each of them exactly as it does on the
//! lossless path. Chunks whose data describe the source's pixel format
//! (tRNS, bKGD, sBIT, hIST) or its frames (acTL, fcTL, fdAT) are not put
//! back: the indexed output has its own format.
//!
//! One exception to the indexed output: a greyscale source with an iCCP
//! chunk. Its profile is a GRAY profile, which the PNG format allows only on
//! a greyscale image (and oxipng never turns an image with an iCCP back into
//! greyscale). So it is written as 8-bit greyscale (with alpha when the
//! quantised palette has any) from the quantised grey levels, and keeps its
//! profile.
//!
//! Before decoding, oxipng's own parser reads the source
//! ([`check_with_oxipng`]): whatever the lossless path refuses (a bad CRC,
//! image data longer or shorter than the IHDR declares, corrupt data) is
//! refused here too, with the same reason, before anything is quantised.
//!
//! An animated PNG (with an `acTL` chunk) gets the lossless treatment with
//! [`Note::KeptLosslessAnimatedPng`].
//!
//! Pure Rust: no `unsafe` here.

use std::io::Cursor;

use super::{
    MAX_DECOMPRESSED_BYTES, SIGNATURE, check_chunks, chunk_headers, oxipng_options, oxipng_reason,
    run_oxipng,
};
use crate::lossy::{
    Lossy, Note, Quality, Shape, check_shape, finish, kept_lossless, reserve_pixels,
};

/// quantizr's dithering level: full Floyd-Steinberg-style error diffusion.
const DITHERING: f32 = 1.0;

/// The largest palette a PNG can hold.
const MAX_COLOURS: usize = 256;

/// A source decoded to 8-bit RGBA, four bytes per pixel, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rgba {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// A palette of RGBA entries and one palette index per pixel, row by row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Quantised {
    palette: Vec<[u8; 4]>,
    indices: Vec<u8>,
}

/// Compress PNG bytes lossily at `quality`.
///
/// The result is never larger than the source (see [`finish`]); an animated
/// PNG gets the lossless treatment with [`Note::KeptLosslessAnimatedPng`].
/// The error is a message for the user, as for the lossless path.
pub(crate) fn optimize_lossy(bytes: &[u8], quality: Quality) -> Result<Lossy, String> {
    optimize_lossy_with(bytes, quality, quantise)
}

/// [`optimize_lossy`] with the quantiser as an argument, so tests can pass a
/// double for it.
fn optimize_lossy_with(
    bytes: &[u8],
    quality: Quality,
    quantise: impl FnOnce(&Rgba, Quality) -> Result<Quantised, String>,
) -> Result<Lossy, String> {
    if !bytes.starts_with(&SIGNATURE) {
        return Err("not a PNG file (the content is not PNG data)".to_owned());
    }
    // The chunk order and the pixel count from IHDR, before decoding.
    if check_chunks(bytes)?.has_actl {
        return Ok(kept_lossless(
            super::optimize(bytes)?,
            Note::KeptLosslessAnimatedPng,
        ));
    }
    check_with_oxipng(bytes)?;
    let image = decode(bytes)?;
    let (width, height) = (image.width, image.height);
    let quantised = quantise(&image, quality)?;
    drop(image);
    check_quantised(&quantised, width, height)?;
    let carried = carried_chunks(bytes);
    let written = if is_grey(bytes) && carried.iter().any(|chunk| &chunk[4..8] == b"iCCP") {
        write_grey(width, height, &quantised, &carried)?
    } else {
        write_indexed(width, height, &quantised, &carried)?
    };
    drop(quantised);
    let result = run_oxipng(&written)?;
    check_output(&result, width, height)?;
    Ok(finish(bytes, result))
}

/// The size of the palette at `quality`: `3 + round(253 * (quality / 100)²)`.
///
/// So 3 at 0, 66 at 50, 145 at 75 (the default) and 256 at 100. The curve is
/// quadratic because a palette's visible loss grows much faster at the low
/// end (from 16 to 8 colours) than at the high end (from 256 to 200), so most
/// of the range is spent where it shows; with dithering, 145 colours keep a
/// photo or gradient close to the source. It never decreases as the quality
/// rises. It starts at 3, not 2, because an image with fully transparent
/// pixels keeps one entry for them ([`quantise`]) and quantizr needs at least
/// 2 for the rest.
fn colors_for(quality: Quality) -> i32 {
    let q = i32::from(quality.get());
    3 + (253 * q * q + 5_000) / 10_000
}

/// The real quantiser: quantizr with [`colors_for`] colours and dithering
/// level [`DITHERING`].
///
/// Fully transparent pixels stay fully transparent at every quality. When
/// the image has any, quantizr gets one colour fewer, and those pixels are
/// then pointed at a fully transparent entry: quantizr's own, or one added
/// when its palette has none (too few colours to keep one, or dithering
/// spread the neighbours' alpha into them). So the palette never exceeds
/// [`colors_for`].
fn quantise(image: &Rgba, quality: Quality) -> Result<Quantised, String> {
    let failed = |error: quantizr::Error| format!("could not reduce the colours: {error}");
    let (width, height) = (image.width as usize, image.height as usize);
    let has_transparent = image
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .any(|pixel| pixel[3] == 0);
    let colours = colors_for(quality) - i32::from(has_transparent);
    let source = quantizr::Image::new(&image.pixels, width, height).map_err(failed)?;
    let mut options = quantizr::Options::default();
    options.set_max_colors(colours).map_err(failed)?;
    let mut result = quantizr::QuantizeResult::quantize(&source, &options);
    result.set_dithering_level(DITHERING).map_err(failed)?;
    let mut indices = reserve_pixels(width * height)?;
    result.remap_image(&source, &mut indices).map_err(failed)?;
    let palette = result.get_palette();
    let mut palette: Vec<[u8; 4]> = palette
        .entries
        .iter()
        .take(palette.count as usize)
        .map(|colour| [colour.r, colour.g, colour.b, colour.a])
        .collect();
    if has_transparent {
        let clear = match palette.iter().position(|entry| entry[3] == 0) {
            Some(at) => at,
            None => {
                palette.push([0, 0, 0, 0]);
                palette.len() - 1
            }
        };
        // quantizr returned at most 255 colours here, so even an added
        // entry has an index of at most 255.
        let clear = clear as u8;
        for (index, pixel) in indices.iter_mut().zip(image.pixels.as_chunks::<4>().0) {
            if pixel[3] == 0 {
                *index = clear;
            }
        }
    }
    Ok(Quantised { palette, indices })
}

/// Whether a quantiser's result fits the image: 1 to 256 colours, one index
/// per pixel, and every index within the palette.
fn check_quantised(quantised: &Quantised, width: u32, height: u32) -> Result<(), String> {
    let colours = quantised.palette.len();
    if !(1..=MAX_COLOURS).contains(&colours) {
        return Err(format!(
            "the quantiser returned a palette of {colours} colours"
        ));
    }
    let pixels = width as usize * height as usize;
    if quantised.indices.len() != pixels {
        return Err(format!(
            "the quantiser returned {} indices for {pixels} pixels",
            quantised.indices.len()
        ));
    }
    if let Some(index) = quantised
        .indices
        .iter()
        .find(|&&index| usize::from(index) >= colours)
    {
        return Err(format!(
            "the quantiser returned index {index} for a palette of {colours} colours"
        ));
    }
    Ok(())
}

/// The gate before decoding: oxipng's own parser reads the source, and
/// whatever it refuses is refused here with its reason, word for word what
/// the lossless path ([`super::optimize`]) gives. That covers every chunk's
/// CRC, image data that inflates past or falls short of what the IHDR
/// declares, and corrupt data. The `png` decoder is more lenient: it would
/// skip an ancillary chunk with a bad CRC (which would then be carried over)
/// and discards image data past the last row.
///
/// The parsed value holds the whole inflated image, so it is dropped before
/// the decoder runs and the two never share the memory.
fn check_with_oxipng(bytes: &[u8]) -> Result<(), String> {
    // `oxipng::internal_tests` is doc-hidden and not part of oxipng's public
    // API. oxipng is pinned at `=10.2.1` in Cargo.toml for this reason: an
    // upgrade must re-check that `PngData::from_slice` still exists and
    // still refuses what `optimize_from_memory` refuses (the tests that
    // compare the lossy reason with `png::optimize`'s show it).
    let parsed = oxipng::internal_tests::PngData::from_slice(bytes, &oxipng_options())
        .map_err(oxipng_reason)?;
    drop(parsed);
    Ok(())
}

/// Decode the source to 8-bit RGBA: palette and `tRNS` expanded, greyscale
/// expanded to RGB, 16-bit samples reduced to 8.
///
/// It runs after [`check_with_oxipng`], so a decoder error here is a file
/// the lossless path accepts but the `png` decoder refuses (for example a
/// second tRNS chunk, where oxipng keeps the last). Its reason says so: "could
/// not decode the PNG for lossy: …".
fn decode(bytes: &[u8]) -> Result<Rgba, String> {
    let unreadable =
        |error: ::png::DecodingError| format!("could not decode the PNG for lossy: {error}");
    // The pixel count is already checked from IHDR. The limit covers the
    // decoder's own buffers, of which a row is the largest: at most
    // 8 bytes per pixel of the widest image within MAX_PIXELS.
    let limits = ::png::Limits {
        bytes: MAX_DECOMPRESSED_BYTES,
    };
    let mut decoder = ::png::Decoder::new_with_limits(Cursor::new(bytes), limits);
    decoder.set_transformations(
        ::png::Transformations::EXPAND
            | ::png::Transformations::STRIP_16
            | ::png::Transformations::ALPHA,
    );
    // Text and the colour profile are carried over as raw chunks, so the
    // decoder need not inflate them.
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(unreadable)?;
    let mut buffer = reserve_pixels(reader.output_buffer_size())?;
    let frame = reader.next_frame(&mut buffer).map_err(unreadable)?;
    buffer.truncate(frame.buffer_size());
    Ok(Rgba {
        width: frame.width,
        height: frame.height,
        pixels: to_rgba(frame.color_type, frame.bit_depth, buffer)?,
    })
}

/// Decoded 8-bit samples as RGBA. With the transformations [`decode`] asks
/// for, the decoder hands out RGBA or greyscale with alpha; greyscale and
/// RGB without alpha are expanded here too, as opaque. Anything else (a
/// palette, or not 8 bits) is refused.
fn to_rgba(
    colour: ::png::ColorType,
    depth: ::png::BitDepth,
    data: Vec<u8>,
) -> Result<Vec<u8>, String> {
    use ::png::ColorType;
    if depth != ::png::BitDepth::Eight {
        return Err(format!(
            "could not read the PNG data: unexpected {depth:?}-bit {colour:?} pixels"
        ));
    }
    let expand = |samples: usize, pixel: fn(&[u8]) -> [u8; 4]| -> Result<Vec<u8>, String> {
        let mut rgba = Vec::new();
        rgba.try_reserve_exact(data.len() / samples * 4)
            .map_err(|_| "not enough memory for the pixels".to_owned())?;
        for sample in data.chunks_exact(samples) {
            rgba.extend_from_slice(&pixel(sample));
        }
        Ok(rgba)
    };
    match colour {
        ColorType::Rgba => Ok(data),
        ColorType::GrayscaleAlpha => expand(2, |s| [s[0], s[0], s[0], s[1]]),
        ColorType::Grayscale => expand(1, |s| [s[0], s[0], s[0], 255]),
        ColorType::Rgb => expand(3, |s| [s[0], s[1], s[2], 255]),
        ColorType::Indexed => {
            Err("could not read the PNG data: unexpected palette pixels after expansion".to_owned())
        }
    }
}

/// Whether a chunk type is carried over from the source: an ancillary chunk
/// (lower-case first letter) that does not describe the source's pixel
/// format or frames.
fn is_carried(kind: &[u8; 4]) -> bool {
    kind[0].is_ascii_lowercase()
        && !matches!(
            kind,
            b"tRNS" | b"bKGD" | b"sBIT" | b"hIST" | b"acTL" | b"fcTL" | b"fdAT"
        )
}

/// The source's whole chunks (length, type, data, CRC) before its first
/// IDAT that [`is_carried`] keeps, in source order.
fn carried_chunks(bytes: &[u8]) -> Vec<&[u8]> {
    chunk_headers(bytes)
        .take_while(|chunk| &chunk.kind != b"IDAT")
        .filter(|chunk| is_carried(&chunk.kind))
        .filter_map(|chunk| chunk.whole(bytes))
        .collect()
}

/// The smallest PNG bit depth whose indices reach every palette entry.
fn bit_depth_for(colours: usize) -> ::png::BitDepth {
    match colours {
        0..=2 => ::png::BitDepth::One,
        3..=4 => ::png::BitDepth::Two,
        5..=16 => ::png::BitDepth::Four,
        _ => ::png::BitDepth::Eight,
    }
}

/// Write an indexed PNG: IHDR, the `carried` chunks, PLTE, tRNS when any
/// entry has alpha below 255, the image data at the smallest bit depth
/// ([`bit_depth_for`]), IEND. `quantised` must fit the image
/// ([`check_quantised`]).
fn write_indexed(
    width: u32,
    height: u32,
    quantised: &Quantised,
    carried: &[&[u8]],
) -> Result<Vec<u8>, String> {
    let depth = bit_depth_for(quantised.palette.len());
    let packed = pack(&quantised.indices, width as usize, depth as usize)?;
    let rgb: Vec<u8> = quantised
        .palette
        .iter()
        .flat_map(|c| [c[0], c[1], c[2]])
        .collect();
    let alpha: Vec<u8> = quantised.palette.iter().map(|c| c[3]).collect();
    encode_png(width, height, &packed, carried, |encoder| {
        encoder.set_color(::png::ColorType::Indexed);
        encoder.set_depth(depth);
        encoder.set_palette(rgb);
        if alpha.iter().any(|&a| a < 255) {
            encoder.set_trns(alpha);
        }
    })
}

/// Whether the source's IHDR (its first chunk, as [`check_chunks`] made
/// sure) declares greyscale, with or without alpha.
fn is_grey(bytes: &[u8]) -> bool {
    // Signature 8, length 4, type 4, width 4, height 4, bit depth 1.
    matches!(bytes.get(25), Some(0 | 4))
}

/// Write an 8-bit greyscale PNG from the quantised grey levels (with alpha,
/// colour type 4, when any palette entry has alpha below 255; otherwise
/// colour type 0): IHDR, the `carried` chunks, the image data, IEND. Each
/// entry's grey level is the mean of its channels: quantizr averages grey
/// colours into grey entries, so the three are equal for a grey source.
/// `quantised` must fit the image ([`check_quantised`]).
fn write_grey(
    width: u32,
    height: u32,
    quantised: &Quantised,
    carried: &[&[u8]],
) -> Result<Vec<u8>, String> {
    let alpha = quantised.palette.iter().any(|entry| entry[3] < 255);
    let samples = if alpha { 2 } else { 1 };
    let levels: Vec<[u8; 2]> = quantised
        .palette
        .iter()
        .map(|&[r, g, b, a]| {
            let sum = u16::from(r) + u16::from(g) + u16::from(b);
            // At most 255 after rounding, so the cast keeps the value.
            [((sum + 1) / 3) as u8, a]
        })
        .collect();
    let mut data = Vec::new();
    data.try_reserve_exact(quantised.indices.len() * samples)
        .map_err(|_| "not enough memory for the pixels".to_owned())?;
    for &index in &quantised.indices {
        data.extend_from_slice(&levels[usize::from(index)][..samples]);
    }
    let colour = if alpha {
        ::png::ColorType::GrayscaleAlpha
    } else {
        ::png::ColorType::Grayscale
    };
    encode_png(width, height, &data, carried, |encoder| {
        encoder.set_color(colour);
        encoder.set_depth(::png::BitDepth::Eight);
    })
}

/// Encode `data` with the header that `setup` sets, then put the `carried`
/// chunks right after IHDR.
fn encode_png(
    width: u32,
    height: u32,
    data: &[u8],
    carried: &[&[u8]],
    setup: impl FnOnce(&mut ::png::Encoder<&mut Vec<u8>>),
) -> Result<Vec<u8>, String> {
    let failed = |error: ::png::EncodingError| format!("could not write the lossy PNG: {error}");
    let mut out = Vec::new();
    let mut encoder = ::png::Encoder::new(&mut out, width, height);
    setup(&mut encoder);
    // oxipng compresses the data again; this copy only has to be quick.
    encoder.set_compression(::png::Compression::Fast);
    let mut writer = encoder.write_header().map_err(failed)?;
    writer.write_image_data(data).map_err(failed)?;
    writer.finish().map_err(failed)?;
    // The encoder writes IHDR (always 13 bytes of data, so 25 in all) right
    // after the signature. The carried chunks go after it, before PLTE (if
    // any), where every one of them may stand.
    let after_ihdr = SIGNATURE.len() + 25;
    let tail = out.split_off(after_ihdr);
    for chunk in carried {
        out.extend_from_slice(chunk);
    }
    out.extend_from_slice(&tail);
    Ok(out)
}

/// The indices packed `depth` bits per pixel, high bits first, each row
/// starting on a whole byte.
fn pack(indices: &[u8], width: usize, depth: usize) -> Result<Vec<u8>, String> {
    if depth == 8 || width == 0 {
        return Ok(indices.to_vec());
    }
    let per_byte = 8 / depth;
    let row_bytes = width.div_ceil(per_byte);
    let mut packed = reserve_pixels(indices.len().div_ceil(width) * row_bytes)?;
    for (row, out) in indices.chunks(width).zip(packed.chunks_mut(row_bytes)) {
        for (x, &index) in row.iter().enumerate() {
            let shift = 8 - depth * (x % per_byte + 1);
            out[x / per_byte] |= index << shift;
        }
    }
    Ok(packed)
}

/// The shape check on the lossy result: its IHDR (the first chunk) must
/// declare the source's size.
fn check_output(bytes: &[u8], width: u32, height: u32) -> Result<(), String> {
    let found = chunk_headers(bytes)
        .next()
        .filter(|chunk| bytes.starts_with(&SIGNATURE) && &chunk.kind == b"IHDR")
        .and_then(|chunk| bytes.get(chunk.at + 8..chunk.at + 16))
        .ok_or("the lossy result is not a readable PNG (no IHDR)")?;
    let found = Shape {
        width: u32::from_be_bytes([found[0], found[1], found[2], found[3]]),
        height: u32::from_be_bytes([found[4], found[5], found[6], found[7]]),
        duration_ms: None,
    };
    let expected = Shape {
        width,
        height,
        duration_ms: None,
    };
    check_shape(expected, found)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::panic;
    use std::time::{Duration, Instant};

    use image::ImageDecoder;

    use super::*;
    use crate::lossy::Note;
    use crate::png::tests::{TOKEN_IDAT, chunk, file, ihdr};

    fn q(value: u8) -> Quality {
        Quality::new(value).unwrap()
    }

    /// A double for the quantiser that must not be reached.
    fn never(_: &Rgba, _: Quality) -> Result<Quantised, String> {
        panic!("the quantiser must not be called");
    }

    // --- fixtures ------------------------------------------------------------

    /// A PNG from the `png` encoder: fast compression and no filtering, so
    /// there is room to shrink. `before_idat` chunks (whole chunks, with CRC)
    /// go in after the header chunks, before the image data.
    fn encode(
        w: u32,
        h: u32,
        colour: ::png::ColorType,
        depth: ::png::BitDepth,
        data: &[u8],
        setup: impl FnOnce(&mut ::png::Encoder<&mut Vec<u8>>),
        before_idat: &[Vec<u8>],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = ::png::Encoder::new(&mut out, w, h);
            encoder.set_color(colour);
            encoder.set_depth(depth);
            encoder.set_compression(::png::Compression::Fast);
            encoder.set_filter(::png::FilterType::NoFilter);
            setup(&mut encoder);
            let mut writer = encoder.write_header().unwrap();
            for whole in before_idat {
                // Length, type, data, CRC: hand the type and the data over.
                let kind: [u8; 4] = whole[4..8].try_into().unwrap();
                let data = &whole[8..whole.len() - 4];
                writer
                    .write_chunk(::png::chunk::ChunkType(kind), data)
                    .unwrap();
            }
            writer.write_image_data(data).unwrap();
            writer.finish().unwrap();
        }
        out
    }

    fn rgba_png(w: u32, h: u32, pixels: &[u8], before_idat: &[Vec<u8>]) -> Vec<u8> {
        encode(
            w,
            h,
            ::png::ColorType::Rgba,
            ::png::BitDepth::Eight,
            pixels,
            |_| {},
            before_idat,
        )
    }

    const GW: u32 = 128;
    const GH: u32 = 96;

    /// An opaque RGBA gradient with far more than 256 colours.
    fn gradient_pixels(w: u32, h: u32) -> Vec<u8> {
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[
                    (x * 255 / (w - 1)) as u8,
                    (y * 255 / (h - 1)) as u8,
                    ((x + y) * 255 / (w + h - 2)) as u8,
                    255,
                ]);
            }
        }
        pixels
    }

    fn gradient_png() -> Vec<u8> {
        rgba_png(GW, GH, &gradient_pixels(GW, GH), &[])
    }

    fn decode_rgba(bytes: &[u8]) -> image::RgbaImage {
        image::load_from_memory_with_format(bytes, image::ImageFormat::Png)
            .expect("decodable PNG")
            .to_rgba8()
    }

    /// The chunk types of a whole PNG, in order.
    fn chunk_types(png: &[u8]) -> Vec<[u8; 4]> {
        let mut types = Vec::new();
        let mut at = 8;
        while at + 8 <= png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            types.push(png[at + 4..at + 8].try_into().unwrap());
            at += 12 + len;
        }
        types
    }

    /// The data of the first chunk of type `kind`.
    fn chunk_data(png: &[u8], kind: &[u8; 4]) -> Option<Vec<u8>> {
        let mut at = 8;
        while at + 8 <= png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            if &png[at + 4..at + 8] == kind {
                return Some(png[at + 8..at + 8 + len].to_vec());
            }
            at += 12 + len;
        }
        None
    }

    /// IHDR's bit depth and colour type.
    fn depth_and_colour(png: &[u8]) -> (u8, u8) {
        let ihdr = chunk_data(png, b"IHDR").unwrap();
        (ihdr[8], ihdr[9])
    }

    /// The colour type of an indexed PNG.
    const INDEXED: u8 = 3;

    /// A zlib stream of one stored block holding `data` (at most 65535
    /// bytes), with its Adler-32.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let len = data.len() as u16;
        let mut out = vec![0x78, 0x01, 0x01];
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(data);
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in data {
            a = (a + u32::from(byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        out.extend_from_slice(&((b << 16) | a).to_be_bytes());
        out
    }

    /// A made-up RGB colour profile: not sRGB, so oxipng keeps it as iCCP.
    fn profile() -> Vec<u8> {
        profile_in(b"RGB ")
    }

    /// A made-up colour profile whose header names the colour space `space`
    /// (bytes 16 to 20 of an ICC header).
    fn profile_in(space: &[u8; 4]) -> Vec<u8> {
        let mut profile = vec![0u8; 400];
        profile[0..4].copy_from_slice(&400u32.to_be_bytes());
        profile[12..16].copy_from_slice(b"mntr");
        profile[16..20].copy_from_slice(space);
        profile[36..40].copy_from_slice(b"acsp");
        for (i, byte) in profile.iter_mut().enumerate().skip(128) {
            *byte = (i * 7 % 251) as u8;
        }
        profile
    }

    fn iccp_chunk() -> Vec<u8> {
        iccp_chunk_of(&profile())
    }

    fn iccp_chunk_of(profile: &[u8]) -> Vec<u8> {
        let mut data = b"Lunaepix test\0\0".to_vec();
        data.extend(zlib_stored(profile));
        chunk(b"iCCP", &data)
    }

    /// The colour type of a greyscale PNG, and of one with alpha.
    const GREY: u8 = 0;
    const GREY_ALPHA: u8 = 4;

    /// A 96x64 greyscale gradient (with alpha: the left quarter fully
    /// transparent, the next quarter alpha 128) carrying a GRAY profile.
    fn grey_icc_png(alpha: bool) -> Vec<u8> {
        let (w, h) = (96u32, 64u32);
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                data.push(((x * 255 / (w - 1) + y) % 256) as u8);
                if alpha {
                    data.push(match x {
                        0..=23 => 0,
                        24..=47 => 128,
                        _ => 255,
                    });
                }
            }
        }
        let colour = if alpha {
            ::png::ColorType::GrayscaleAlpha
        } else {
            ::png::ColorType::Grayscale
        };
        encode(
            w,
            h,
            colour,
            ::png::BitDepth::Eight,
            &data,
            |_| {},
            &[iccp_chunk_of(&profile_in(b"GRAY"))],
        )
    }

    /// `png` with the CRC of its first `kind` chunk made wrong.
    fn with_bad_crc(png: &[u8], kind: &[u8; 4]) -> Vec<u8> {
        let mut bad = png.to_vec();
        let mut at = 8;
        while at + 8 <= bad.len() {
            let len = u32::from_be_bytes(bad[at..at + 4].try_into().unwrap()) as usize;
            if &bad[at + 4..at + 8] == kind {
                bad[at + 8 + len + 3] ^= 0x01;
                return bad;
            }
            at += 12 + len;
        }
        panic!("no {kind:?} chunk");
    }

    fn icc_of(png: &[u8]) -> Option<Vec<u8>> {
        let mut decoder = image::codecs::png::PngDecoder::new(Cursor::new(png)).unwrap();
        decoder.icc_profile().unwrap()
    }

    /// 16 colours in a 4-bit indexed PNG, already optimised by oxipng.
    fn indexed_16_colour_png() -> Vec<u8> {
        let (w, h) = (64u32, 64u32);
        let mut palette = Vec::new();
        for i in 0..16u8 {
            palette.extend_from_slice(&[i * 16, 255 - i * 16, (u32::from(i) * 37 % 255) as u8]);
        }
        let mut packed = Vec::new();
        for y in 0..h {
            for x in (0..w).step_by(2) {
                let a = ((x / 4 + y / 4) % 16) as u8;
                let b = (((x + 1) / 4 + y / 4) % 16) as u8;
                packed.push((a << 4) | b);
            }
        }
        let raw = encode(
            w,
            h,
            ::png::ColorType::Indexed,
            ::png::BitDepth::Four,
            &packed,
            |encoder| encoder.set_palette(palette),
            &[],
        );
        crate::png::optimize(&raw).unwrap()
    }

    const AW: u32 = 64;
    const AH: u32 = 48;

    /// Left third fully transparent (with varied colour underneath), middle
    /// third alpha 128 over a gradient, right third an opaque gradient.
    fn alpha_pixels() -> Vec<u8> {
        let mut pixels = Vec::new();
        for y in 0..AH {
            for x in 0..AW {
                let r = (x * 4) as u8;
                let g = (y * 5) as u8;
                let a = match x {
                    0..=20 => 0,
                    21..=41 => 128,
                    _ => 255,
                };
                pixels.extend_from_slice(&[r, g, 200 - (x as u8), a]);
            }
        }
        pixels
    }

    fn alpha_png() -> Vec<u8> {
        rgba_png(AW, AH, &alpha_pixels(), &[])
    }

    /// Every pixel where the source alpha is 0 and the output's is not,
    /// and every source alpha-128 pixel whose output alpha is off by more
    /// than 8.
    fn alpha_problems(source: &image::RgbaImage, output: &image::RgbaImage) -> Vec<String> {
        let mut problems = Vec::new();
        for (x, y, pixel) in source.enumerate_pixels() {
            let found = output.get_pixel(x, y)[3];
            match pixel[3] {
                0 if found != 0 => problems.push(format!("({x},{y}) alpha 0 became {found}")),
                128 if found.abs_diff(128) > 8 => {
                    problems.push(format!("({x},{y}) alpha 128 became {found}"))
                }
                _ => {}
            }
        }
        problems
    }

    /// A palette PNG whose tRNS makes entries 0 and 3 fully transparent and
    /// entry 2 half transparent; 8 bits per index, so there is room to shrink.
    fn trns_png() -> Vec<u8> {
        let (w, h) = (64u32, 64u32);
        let palette = vec![
            255, 0, 0, // 0: transparent
            0, 255, 0, // 1
            0, 0, 255, // 2: alpha 128
            10, 10, 10, // 3: transparent
            200, 200, 0, // 4
            0, 200, 200, // 5
        ];
        let indices: Vec<u8> = (0..h)
            .flat_map(|y| (0..w).map(move |x| ((x / 8 + y / 8) % 6) as u8))
            .collect();
        encode(
            w,
            h,
            ::png::ColorType::Indexed,
            ::png::BitDepth::Eight,
            &indices,
            |encoder| {
                encoder.set_palette(palette);
                encoder.set_trns(vec![0, 255, 128, 0]);
            },
            &[],
        )
    }

    /// An APNG of two 16x16 RGBA frames.
    fn apng() -> Vec<u8> {
        let (w, h) = (16u32, 16u32);
        let mut out = Vec::new();
        {
            let mut encoder = ::png::Encoder::new(&mut out, w, h);
            encoder.set_color(::png::ColorType::Rgba);
            encoder.set_depth(::png::BitDepth::Eight);
            encoder.set_compression(::png::Compression::Fast);
            encoder.set_animated(2, 0).unwrap();
            encoder.set_frame_delay(1, 10).unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&gradient_pixels(w, h)).unwrap();
            writer.write_image_data(&[90; 16 * 16 * 4]).unwrap();
            writer.finish().unwrap();
        }
        out
    }

    /// A signature, an IHDR for a `w`x`h` greyscale image and IEND: no
    /// image data at all.
    fn header_only(w: u32, h: u32) -> Vec<u8> {
        file(&[ihdr(w, h, 8, 0)])
    }

    // --- the result ----------------------------------------------------------

    #[test]
    fn a_gradient_rgba_png_gets_smaller_as_indexed_with_the_same_size() {
        let source = gradient_png();
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert!(
            lossy.bytes.len() < source.len(),
            "{} >= {}",
            lossy.bytes.len(),
            source.len()
        );
        assert_eq!(depth_and_colour(&lossy.bytes).1, INDEXED);
        let decoded = decode_rgba(&lossy.bytes);
        assert_eq!(decoded.dimensions(), (GW, GH));
    }

    #[test]
    fn an_indexed_16_colour_png_keeps_the_original_with_the_note() {
        let source = indexed_16_colour_png();
        assert_eq!(depth_and_colour(&source).1, INDEXED);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, Some(Note::OriginalKept));
        assert_eq!(lossy.bytes, source);
    }

    #[test]
    fn the_fixture_really_has_alpha_0_and_128() {
        let decoded = decode_rgba(&alpha_png());
        let count = |alpha: u8| decoded.pixels().filter(|p| p[3] == alpha).count();
        assert!(count(0) > 500, "{}", count(0));
        assert!(count(128) > 500, "{}", count(128));
        assert!(count(255) > 500, "{}", count(255));
        // Transparent pixels carry colour, so a decoder that dropped alpha
        // would show them.
        assert!(decoded.pixels().any(|p| p[3] == 0 && p[0] > 0));
    }

    #[test]
    fn alpha_0_stays_0_and_alpha_128_stays_within_8() {
        let source = alpha_png();
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        let problems = alpha_problems(&decode_rgba(&source), &decode_rgba(&lossy.bytes));
        assert_eq!(problems, Vec::<String>::new());
        // At the lowest quality, three colours cannot keep alpha 128, but
        // fully transparent pixels still stay fully transparent.
        let lossy = optimize_lossy(&source, q(0)).unwrap();
        let problems = alpha_problems(&decode_rgba(&source), &decode_rgba(&lossy.bytes));
        let lost: Vec<&String> = problems
            .iter()
            .filter(|p| p.contains("alpha 0 became"))
            .collect();
        assert_eq!(lost, Vec::<&String>::new());
    }

    #[test]
    fn the_alpha_comparison_detects_an_opaque_result() {
        let source = decode_rgba(&alpha_png());
        let mut opaque = source.clone();
        for pixel in opaque.pixels_mut() {
            pixel[3] = 255;
        }
        let problems = alpha_problems(&source, &opaque);
        assert!(problems.iter().any(|p| p.contains("alpha 0 became 255")));
        assert!(problems.iter().any(|p| p.contains("alpha 128 became 255")));
        let mut off_by_9 = source.clone();
        for pixel in off_by_9.pixels_mut().filter(|p| p[3] == 128) {
            pixel[3] = 137;
        }
        assert!(!alpha_problems(&source, &off_by_9).is_empty());
    }

    #[test]
    fn the_trns_fixture_really_has_transparent_palette_entries() {
        let source = trns_png();
        assert_eq!(depth_and_colour(&source), (8, INDEXED));
        let trns = chunk_data(&source, b"tRNS").expect("a tRNS chunk");
        assert_eq!(trns, vec![0, 255, 128, 0]);
        let decoded = decode_rgba(&source);
        let transparent = decoded.pixels().filter(|p| p[3] == 0).count();
        let half = decoded.pixels().filter(|p| p[3] == 128).count();
        assert!(transparent > 1000, "{transparent}");
        assert!(half > 500, "{half}");
    }

    #[test]
    fn an_indexed_png_with_trns_keeps_its_transparent_pixels() {
        let source = trns_png();
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        // Really encoded lossy, not the original kept.
        assert_eq!(lossy.note, None);
        assert!(lossy.bytes.len() < source.len());
        let before = decode_rgba(&source);
        let after = decode_rgba(&lossy.bytes);
        for (x, y, pixel) in before.enumerate_pixels() {
            let found = after.get_pixel(x, y);
            assert_eq!(found[3], pixel[3], "alpha at ({x},{y})");
            if pixel[3] != 0 {
                assert_eq!(found, pixel, "colour at ({x},{y})");
            }
        }
    }

    // --- quality -------------------------------------------------------------

    fn gradient_rgba() -> Rgba {
        Rgba {
            width: GW,
            height: GH,
            pixels: gradient_pixels(GW, GH),
        }
    }

    fn distinct_colours(pixels: &[u8]) -> usize {
        let mut colours: Vec<&[u8]> = pixels.chunks(4).collect();
        colours.sort_unstable();
        colours.dedup();
        colours.len()
    }

    #[test]
    fn quality_100_gives_256_colours_for_a_many_colour_image() {
        let image = gradient_rgba();
        assert!(distinct_colours(&image.pixels) > 1000);
        let quantised = quantise(&image, q(100)).unwrap();
        assert_eq!(quantised.palette.len(), 256);
        assert_eq!(quantised.indices.len(), (GW * GH) as usize);
    }

    #[test]
    fn quality_0_gives_no_more_colours_than_quality_50() {
        let image = gradient_rgba();
        let at_0 = quantise(&image, q(0)).unwrap().palette.len();
        let at_50 = quantise(&image, q(50)).unwrap().palette.len();
        assert!((2..=at_50).contains(&at_0), "{at_0} vs {at_50}");
        assert!(at_50 < 256, "{at_50}");
    }

    fn alpha_rgba() -> Rgba {
        Rgba {
            width: AW,
            height: AH,
            pixels: alpha_pixels(),
        }
    }

    #[test]
    fn quantise_keeps_fully_transparent_pixels_on_a_transparent_entry_at_every_quality() {
        let image = alpha_rgba();
        for value in [0, 1, 10, 50, 75, 100] {
            let quantised = quantise(&image, q(value)).unwrap();
            assert!(
                quantised.palette.len() as i32 <= colors_for(q(value)),
                "{value}"
            );
            for (index, pixel) in quantised.indices.iter().zip(image.pixels.chunks(4)) {
                if pixel[3] == 0 {
                    assert_eq!(quantised.palette[*index as usize][3], 0, "quality {value}");
                }
            }
        }
    }

    #[test]
    fn quantise_reserves_no_transparent_entry_for_an_opaque_image() {
        let image = gradient_rgba();
        for value in [0, 50, 100] {
            let quantised = quantise(&image, q(value)).unwrap();
            assert_eq!(
                quantised.palette.len() as i32,
                colors_for(q(value)),
                "{value}"
            );
            assert!(
                quantised.palette.iter().all(|entry| entry[3] == 255),
                "{value}"
            );
        }
    }

    #[test]
    fn colors_for_is_monotonic_over_0_to_100() {
        for value in 0..100u8 {
            assert!(colors_for(q(value)) <= colors_for(q(value + 1)), "{value}");
        }
        assert_eq!(colors_for(q(100)), 256);
        // Room for two colours plus a transparent entry at the lowest quality.
        assert_eq!(colors_for(q(0)), 3);
        assert!(colors_for(q(0)) < colors_for(q(50)));
        assert!(colors_for(q(50)) < colors_for(q(100)));
    }

    #[test]
    fn colors_for_never_exceeds_256_or_drops_below_2() {
        for value in 0..=100u8 {
            let colours = colors_for(q(value));
            assert!((2..=256).contains(&colours), "{value}: {colours}");
        }
    }

    // --- chunks --------------------------------------------------------------

    #[test]
    fn an_icc_profile_is_kept() {
        let source = rgba_png(GW, GH, &gradient_pixels(GW, GH), &[iccp_chunk()]);
        assert_eq!(icc_of(&source), Some(profile()));
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(icc_of(&lossy.bytes), Some(profile()));
    }

    #[test]
    fn a_source_without_iccp_gains_none() {
        let source = gradient_png();
        assert_eq!(icc_of(&source), None);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(icc_of(&lossy.bytes), None);
        assert!(!chunk_types(&lossy.bytes).contains(b"iCCP"));
    }

    #[test]
    fn the_grey_fixtures_really_are_grey_with_a_gray_profile() {
        for (alpha, colour) in [(false, GREY), (true, GREY_ALPHA)] {
            let source = grey_icc_png(alpha);
            assert_eq!(depth_and_colour(&source), (8, colour));
            let profile = icc_of(&source).expect("an iCCP chunk");
            assert_eq!(&profile[16..20], b"GRAY");
        }
        // And the RGB fixture's profile is not GRAY.
        assert_eq!(&profile()[16..20], b"RGB ");
    }

    #[test]
    fn a_greyscale_png_with_a_gray_profile_stays_grey_and_keeps_it() {
        for (alpha, colour) in [(false, GREY), (true, GREY_ALPHA)] {
            let source = grey_icc_png(alpha);
            let lossy = optimize_lossy(&source, q(75)).unwrap();
            assert_eq!(lossy.note, None, "alpha {alpha}");
            assert_eq!(depth_and_colour(&lossy.bytes).1, colour, "alpha {alpha}");
            // The profile is kept, and its chunk is the one today's lossless
            // path writes for the same file.
            assert_eq!(icc_of(&lossy.bytes), Some(profile_in(b"GRAY")));
            let lossless = crate::png::optimize(&source).unwrap();
            assert_eq!(
                chunk_data(&lossy.bytes, b"iCCP"),
                chunk_data(&lossless, b"iCCP")
            );
            let before = decode_rgba(&source);
            let after = decode_rgba(&lossy.bytes);
            assert_eq!(after.dimensions(), before.dimensions());
            for (x, y, pixel) in before.enumerate_pixels() {
                let found = after.get_pixel(x, y);
                // Still grey.
                assert!(found[0] == found[1] && found[1] == found[2], "({x},{y})");
                if pixel[3] == 0 {
                    assert_eq!(found[3], 0, "alpha at ({x},{y})");
                }
            }
            if alpha {
                assert!(after.pixels().any(|p| p[3] == 128));
            }
        }
    }

    #[test]
    fn an_rgb_png_with_an_rgb_profile_is_still_indexed() {
        let source = rgba_png(GW, GH, &gradient_pixels(GW, GH), &[iccp_chunk()]);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(depth_and_colour(&lossy.bytes).1, INDEXED);
        assert_eq!(icc_of(&lossy.bytes), Some(profile()));
    }

    #[test]
    fn a_carried_chunk_with_a_bad_crc_fails_like_lossless_without_decoding() {
        let good = rgba_png(
            GW,
            GH,
            &gradient_pixels(GW, GH),
            &[
                chunk(b"pHYs", &[0, 0, 11, 19, 0, 0, 11, 19, 1]),
                chunk(b"tEXt", b"Comment\0lunaepix"),
            ],
        );
        for kind in [b"tEXt", b"pHYs", b"IDAT"] {
            let bad = with_bad_crc(&good, kind);
            let lossless = crate::png::optimize(&bad).unwrap_err();
            assert!(lossless.contains("CRC"), "{lossless}");
            // `never` panics if the quantiser is reached.
            let start = Instant::now();
            let lossy = optimize_lossy_with(&bad, q(75), never).unwrap_err();
            assert_eq!(lossy, lossless, "{kind:?}");
            assert!(start.elapsed() < Duration::from_secs(5));
        }
    }

    #[test]
    fn the_same_file_with_correct_crcs_is_encoded_lossy() {
        let good = rgba_png(
            GW,
            GH,
            &gradient_pixels(GW, GH),
            &[
                chunk(b"pHYs", &[0, 0, 11, 19, 0, 0, 11, 19, 1]),
                chunk(b"tEXt", b"Comment\0lunaepix"),
            ],
        );
        let lossy = optimize_lossy(&good, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(depth_and_colour(&lossy.bytes).1, INDEXED);
        // tEXt is handled as today: stripped, while pHYs is kept.
        let types = chunk_types(&lossy.bytes);
        assert!(!types.contains(b"tEXt"), "{types:?}");
        assert!(types.contains(b"pHYs"), "{types:?}");
    }

    fn with_metadata() -> Vec<u8> {
        rgba_png(
            GW,
            GH,
            &gradient_pixels(GW, GH),
            &[
                chunk(b"gAMA", &45_455u32.to_be_bytes()),
                chunk(b"sRGB", &[0]),
                chunk(b"pHYs", &[0, 0, 11, 19, 0, 0, 11, 19, 1]),
                chunk(b"tEXt", b"Comment\0lunaepix"),
            ],
        )
    }

    #[test]
    fn display_chunks_before_idat_are_kept_on_the_lossy_path() {
        let source = with_metadata();
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(chunk_data(&lossy.bytes, b"sRGB"), Some(vec![0]));
        assert_eq!(
            chunk_data(&lossy.bytes, b"pHYs"),
            Some(vec![0, 0, 11, 19, 0, 0, 11, 19, 1])
        );
    }

    #[test]
    fn chunks_the_lossless_path_strips_are_stripped_on_the_lossy_path_too() {
        let source = with_metadata();
        let types = chunk_types(&source);
        assert!(
            types.contains(b"tEXt") && types.contains(b"gAMA"),
            "{types:?}"
        );
        let lossless = chunk_types(&crate::png::optimize(&source).unwrap());
        let lossy = chunk_types(&optimize_lossy(&source, q(75)).unwrap().bytes);
        for kind in [b"tEXt", b"gAMA"] {
            assert!(!lossless.contains(kind), "{lossless:?}");
            assert!(!lossy.contains(kind), "{lossy:?}");
        }
    }

    #[test]
    fn carried_chunks_are_the_ancillary_ones_before_idat() {
        let iccp = iccp_chunk();
        let phys = chunk(b"pHYs", &[0, 0, 11, 19, 0, 0, 11, 19, 1]);
        let text = chunk(b"tEXt", b"Comment\0x");
        let after = chunk(b"zTXt", b"After\0\0x");
        let png = file(&[
            ihdr(4, 4, 8, 6),
            iccp.clone(),
            phys.clone(),
            text.clone(),
            chunk(b"IDAT", &TOKEN_IDAT),
            after,
        ]);
        let carried: Vec<Vec<u8>> = carried_chunks(&png)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        assert_eq!(carried, vec![iccp, phys, text]);
    }

    #[test]
    fn carried_chunks_leave_out_trns_bkgd_sbit_hist_and_frame_chunks() {
        let keep = chunk(b"pHYs", &[0, 0, 11, 19, 0, 0, 11, 19, 1]);
        let png = file(&[
            ihdr(4, 4, 8, 3),
            chunk(b"sBIT", &[8, 8, 8]),
            chunk(b"PLTE", &[0, 0, 0, 255, 255, 255]),
            chunk(b"tRNS", &[0]),
            chunk(b"bKGD", &[1]),
            chunk(b"hIST", &[1, 0, 1, 0]),
            chunk(b"fcTL", &[0; 26]),
            keep.clone(),
            chunk(b"IDAT", &TOKEN_IDAT),
            chunk(b"fdAT", &[0; 8]),
        ]);
        let carried: Vec<Vec<u8>> = carried_chunks(&png)
            .into_iter()
            .map(<[u8]>::to_vec)
            .collect();
        assert_eq!(carried, vec![keep]);
    }

    // --- animation -----------------------------------------------------------

    #[test]
    fn an_apng_is_kept_lossless_with_the_note() {
        let source = apng();
        assert!(chunk_types(&source).contains(b"acTL"));
        let lossy = optimize_lossy_with(&source, q(75), never).unwrap();
        assert_eq!(lossy.note, Some(Note::KeptLosslessAnimatedPng));
        assert_eq!(lossy.bytes, crate::png::optimize(&source).unwrap());
        // Still animated.
        assert!(chunk_types(&lossy.bytes).contains(b"acTL"));
    }

    #[test]
    fn a_damaged_apng_fails_with_the_lossless_reason() {
        let source = apng();
        let cut = &source[..source.len() - 40];
        let err = optimize_lossy_with(cut, q(75), never).unwrap_err();
        assert!(err.contains("could not read the PNG data"), "{err}");
    }

    #[test]
    fn a_still_png_is_encoded_lossy() {
        let source = gradient_png();
        assert!(!chunk_types(&source).contains(b"acTL"));
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(depth_and_colour(&lossy.bytes).1, INDEXED);
    }

    // --- decoding ------------------------------------------------------------

    #[test]
    fn a_16_bit_png_is_reduced_and_gets_smaller() {
        let mut data = Vec::new();
        for pixel in gradient_pixels(GW, GH).chunks(4) {
            for &sample in pixel {
                data.extend_from_slice(&[sample, sample ^ 0x5A]);
            }
        }
        let source = encode(
            GW,
            GH,
            ::png::ColorType::Rgba,
            ::png::BitDepth::Sixteen,
            &data,
            |_| {},
            &[],
        );
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert!(lossy.bytes.len() < source.len());
        assert_eq!(depth_and_colour(&lossy.bytes).1, INDEXED);
        assert_eq!(decode_rgba(&lossy.bytes).dimensions(), (GW, GH));
    }

    #[test]
    fn the_quantiser_gets_rgba_pixels_for_every_colour_type_and_the_quality() {
        use ::png::{BitDepth, ColorType};
        type Setup = fn(&mut ::png::Encoder<&mut Vec<u8>>);
        type Case = (ColorType, BitDepth, Vec<u8>, Setup, Vec<u8>);
        fn none(_: &mut ::png::Encoder<&mut Vec<u8>>) {}
        fn grey_trns(encoder: &mut ::png::Encoder<&mut Vec<u8>>) {
            encoder.set_trns(vec![0, 50]);
        }
        fn palette(encoder: &mut ::png::Encoder<&mut Vec<u8>>) {
            encoder.set_palette(vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
            encoder.set_trns(vec![0, 77]);
        }
        // 3x1 images: (colour type, depth, data, setup, expected RGBA).
        let cases: Vec<Case> = vec![
            (
                ColorType::Grayscale,
                BitDepth::Eight,
                vec![0, 50, 255],
                none as Setup,
                vec![0, 0, 0, 255, 50, 50, 50, 255, 255, 255, 255, 255],
            ),
            (
                ColorType::Grayscale,
                BitDepth::Eight,
                vec![0, 50, 255],
                grey_trns as Setup,
                vec![0, 0, 0, 255, 50, 50, 50, 0, 255, 255, 255, 255],
            ),
            (
                ColorType::Grayscale,
                BitDepth::Sixteen,
                vec![0x12, 0, 0x34, 0, 0xFF, 0],
                none as Setup,
                vec![
                    0x12, 0x12, 0x12, 255, 0x34, 0x34, 0x34, 255, 255, 255, 255, 255,
                ],
            ),
            (
                ColorType::GrayscaleAlpha,
                BitDepth::Eight,
                vec![10, 0, 20, 128, 30, 255],
                none as Setup,
                vec![10, 10, 10, 0, 20, 20, 20, 128, 30, 30, 30, 255],
            ),
            (
                ColorType::Rgb,
                BitDepth::Eight,
                vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
                none as Setup,
                vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255],
            ),
            (
                ColorType::Rgba,
                BitDepth::Sixteen,
                vec![
                    0x11, 0, 0x22, 0, 0x33, 0, 0x44, 0, 0x55, 0, 0x66, 0, 0x77, 0, 0x88, 0, 0x99,
                    0, 0xAA, 0, 0xBB, 0, 0xCC, 0,
                ],
                none as Setup,
                vec![
                    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC,
                ],
            ),
            (
                ColorType::Indexed,
                BitDepth::Eight,
                vec![0, 1, 2],
                palette as Setup,
                vec![1, 2, 3, 0, 4, 5, 6, 77, 7, 8, 9, 255],
            ),
        ];
        for (colour, depth, data, setup, expected) in cases {
            let source = encode(3, 1, colour, depth, &data, setup, &[]);
            let seen = RefCell::new(None);
            let err = optimize_lossy_with(&source, q(42), |image, quality| {
                *seen.borrow_mut() = Some((image.clone(), quality.get()));
                Err("recorded".to_owned())
            })
            .unwrap_err();
            assert_eq!(err, "recorded");
            let expected = Rgba {
                width: 3,
                height: 1,
                pixels: expected,
            };
            assert_eq!(
                seen.into_inner(),
                Some((expected, 42)),
                "{colour:?} {depth:?}"
            );
        }
    }

    #[test]
    fn to_rgba_refuses_a_format_it_does_not_expand() {
        use ::png::{BitDepth, ColorType};
        for (colour, depth) in [
            (ColorType::Indexed, BitDepth::Eight),
            (ColorType::Rgba, BitDepth::Sixteen),
            (ColorType::Grayscale, BitDepth::Four),
        ] {
            let err = to_rgba(colour, depth, vec![0; 16]).unwrap_err();
            assert!(err.contains("could not read the PNG data"), "{err}");
        }
        // The formats it does expand.
        assert_eq!(
            to_rgba(ColorType::Rgba, BitDepth::Eight, vec![1, 2, 3, 4]),
            Ok(vec![1, 2, 3, 4])
        );
        assert_eq!(
            to_rgba(ColorType::GrayscaleAlpha, BitDepth::Eight, vec![9, 8]),
            Ok(vec![9, 9, 9, 8])
        );
    }

    // --- the quantiser double -----------------------------------------------

    #[test]
    fn a_quantiser_double_returning_an_error_fails_with_its_reason() {
        let err = optimize_lossy_with(&gradient_png(), q(75), |_, _| {
            Err("quantiser broke".to_owned())
        })
        .unwrap_err();
        assert_eq!(err, "quantiser broke");
    }

    #[test]
    fn a_quantiser_double_returning_a_wrong_size_buffer_fails() {
        let pixels = (GW * GH) as usize;
        for len in [0, pixels - 1, pixels + 1] {
            let err = optimize_lossy_with(&gradient_png(), q(75), |_, _| {
                Ok(Quantised {
                    palette: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
                    indices: vec![0; len],
                })
            })
            .unwrap_err();
            assert_eq!(
                err,
                format!("the quantiser returned {len} indices for {pixels} pixels"),
            );
        }
    }

    #[test]
    fn a_quantiser_double_with_an_index_past_the_palette_fails() {
        let mut indices = vec![0; (GW * GH) as usize];
        indices[100] = 2;
        let err = optimize_lossy_with(&gradient_png(), q(75), |_, _| {
            Ok(Quantised {
                palette: vec![[0, 0, 0, 255], [255, 255, 255, 255]],
                indices,
            })
        })
        .unwrap_err();
        assert_eq!(
            err,
            "the quantiser returned index 2 for a palette of 2 colours"
        );
    }

    #[test]
    fn a_quantiser_double_with_no_or_too_many_colours_fails() {
        for count in [0usize, 257] {
            let err = optimize_lossy_with(&gradient_png(), q(75), |_, _| {
                Ok(Quantised {
                    palette: vec![[1, 2, 3, 255]; count],
                    indices: vec![0; (GW * GH) as usize],
                })
            })
            .unwrap_err();
            assert_eq!(
                err,
                format!("the quantiser returned a palette of {count} colours")
            );
        }
    }

    #[test]
    fn a_valid_double_result_is_written() {
        // Two colours in vertical stripes: the output holds exactly them.
        let palette = vec![[200, 10, 10, 255], [10, 10, 200, 128]];
        let indices: Vec<u8> = (0..GH)
            .flat_map(|_| (0..GW).map(|x| (x / 4 % 2) as u8))
            .collect();
        let expected = indices.clone();
        let lossy = optimize_lossy_with(&gradient_png(), q(75), |_, _| {
            Ok(Quantised {
                palette: palette.clone(),
                indices,
            })
        })
        .unwrap();
        assert_eq!(lossy.note, None);
        let decoded = decode_rgba(&lossy.bytes);
        assert_eq!(decoded.dimensions(), (GW, GH));
        for (pixel, index) in decoded.pixels().zip(expected) {
            assert_eq!(pixel.0, palette[index as usize]);
        }
    }

    // --- writing -------------------------------------------------------------

    #[test]
    fn write_indexed_uses_the_smallest_bit_depth_and_packs_rows() {
        // An odd width, so rows end part-way through a byte.
        let (w, h) = (7u32, 3u32);
        for (colours, depth) in [
            (2usize, 1u8),
            (3, 2),
            (4, 2),
            (5, 4),
            (16, 4),
            (17, 8),
            (256, 8),
        ] {
            let palette: Vec<[u8; 4]> = (0..colours)
                .map(|i| [i as u8, 255 - i as u8, (i * 3) as u8, 255])
                .collect();
            let indices: Vec<u8> = (0..w * h)
                .map(|i| (i as usize * 5 % colours) as u8)
                .collect();
            let quantised = Quantised {
                palette: palette.clone(),
                indices: indices.clone(),
            };
            let png = write_indexed(w, h, &quantised, &[]).unwrap();
            assert_eq!(
                depth_and_colour(&png),
                (depth, INDEXED),
                "{colours} colours"
            );
            let decoded = decode_rgba(&png);
            for (pixel, index) in decoded.pixels().zip(&indices) {
                assert_eq!(pixel.0, palette[*index as usize], "{colours} colours");
            }
        }
    }

    #[test]
    fn write_indexed_writes_trns_when_an_entry_has_alpha() {
        let quantised = Quantised {
            palette: vec![[1, 1, 1, 255], [2, 2, 2, 0], [3, 3, 3, 255]],
            indices: vec![0, 1, 2, 1],
        };
        let png = write_indexed(2, 2, &quantised, &[]).unwrap();
        assert_eq!(chunk_data(&png, b"tRNS"), Some(vec![255, 0, 255]));
    }

    #[test]
    fn write_indexed_writes_no_trns_for_an_opaque_palette() {
        let quantised = Quantised {
            palette: vec![[1, 1, 1, 255], [2, 2, 2, 255]],
            indices: vec![0, 1, 1, 0],
        };
        let png = write_indexed(2, 2, &quantised, &[]).unwrap();
        assert!(!chunk_types(&png).contains(b"tRNS"));
        assert_eq!(chunk_data(&png, b"PLTE"), Some(vec![1, 1, 1, 2, 2, 2]));
    }

    #[test]
    fn write_indexed_puts_the_carried_chunks_before_plte() {
        let iccp = iccp_chunk();
        let quantised = Quantised {
            palette: vec![[1, 1, 1, 255], [2, 2, 2, 255]],
            indices: vec![0, 1, 1, 0],
        };
        let png = write_indexed(2, 2, &quantised, &[&iccp]).unwrap();
        let types = chunk_types(&png);
        let at = |kind: &[u8; 4]| types.iter().position(|t| t == kind).unwrap();
        assert!(
            at(b"IHDR") < at(b"iCCP") && at(b"iCCP") < at(b"PLTE"),
            "{types:?}"
        );
        assert_eq!(icc_of(&png), Some(profile()));
    }

    #[test]
    fn write_indexed_fails_for_a_zero_width() {
        let quantised = Quantised {
            palette: vec![[1, 1, 1, 255], [2, 2, 2, 255]],
            indices: vec![],
        };
        let err = write_indexed(0, 2, &quantised, &[]).unwrap_err();
        assert!(err.starts_with("could not write the lossy PNG"), "{err}");
    }

    #[test]
    fn check_output_accepts_the_same_size() {
        assert_eq!(check_output(&gradient_png(), GW, GH), Ok(()));
    }

    #[test]
    fn check_output_rejects_another_size_or_unreadable_bytes() {
        let err = check_output(&gradient_png(), GW, GH + 1).unwrap_err();
        assert_eq!(
            err,
            format!(
                "the lossy result has a different size ({GW}×{GH}, expected {GW}×{})",
                GH + 1
            )
        );
        for garbage in [&b"not a PNG"[..], b"", &gradient_png()[..12]] {
            let err = check_output(garbage, GW, GH).unwrap_err();
            assert!(
                err.starts_with("the lossy result is not a readable PNG"),
                "{err}"
            );
        }
    }

    // --- hostile input -------------------------------------------------------

    #[test]
    fn bytes_without_a_png_signature_are_refused() {
        for bytes in [&b"\xFF\xD8\xFF\xE0"[..], b"", &crate::png::SIGNATURE[..7]] {
            let err = optimize_lossy_with(bytes, q(75), never).unwrap_err();
            assert!(err.contains("not a PNG"), "{err}");
        }
    }

    #[test]
    fn a_header_over_max_pixels_is_refused_before_decoding() {
        // 20000x20000: 400 MP in a file of a few dozen bytes.
        let start = Instant::now();
        let err = optimize_lossy_with(&header_only(20_000, 20_000), q(75), never).unwrap_err();
        assert_eq!(err, "image too large: 20000×20000 exceeds 100 megapixels");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_header_within_max_pixels_is_passed_on_to_the_decoder() {
        // The same header at 64x64 is refused for its missing image data.
        let err = optimize_lossy_with(&header_only(64, 64), q(75), never).unwrap_err();
        assert!(!err.contains("image too large"), "{err}");
        assert!(err.contains("could not read the PNG data"), "{err}");
    }

    #[test]
    fn a_truncated_png_fails_with_a_reason_without_panic() {
        let full = gradient_png();
        let mut damaged = full.clone();
        // Flip bytes inside the image data, keeping the length.
        let middle = damaged.len() / 2;
        for byte in &mut damaged[middle..middle + 16] {
            *byte ^= 0xFF;
        }
        for bytes in [
            full[..full.len() * 2 / 3].to_vec(),
            full[..60].to_vec(),
            damaged,
        ] {
            let start = Instant::now();
            let result = panic::catch_unwind(|| optimize_lossy(&bytes, q(75)));
            let err = result.expect("no panic").unwrap_err();
            assert!(err.contains("could not read the PNG data"), "{err}");
            assert!(start.elapsed() < Duration::from_secs(5));
        }
    }

    // --- image data past the IHDR's size -------------------------------------

    /// An IHDR chunk like [`ihdr`], with Adam7 interlacing.
    fn interlaced_ihdr(w: u32, h: u32, depth: u8, colour: u8) -> Vec<u8> {
        let mut data = w.to_be_bytes().to_vec();
        data.extend_from_slice(&h.to_be_bytes());
        data.extend_from_slice(&[depth, colour, 0, 0, 1]);
        chunk(b"IHDR", &data)
    }

    /// `stream` cut into `parts` IDAT chunks of about the same length.
    fn idat_chunks(stream: &[u8], parts: usize) -> Vec<Vec<u8>> {
        stream
            .chunks(stream.len().div_ceil(parts))
            .map(|part| chunk(b"IDAT", part))
            .collect()
    }

    /// The image data of a 512x512 all-black greyscale image (256 KiB raw,
    /// a few hundred bytes compressed) under a 16x16 IHDR, which holds
    /// 272 bytes: in one IDAT chunk and split over three.
    fn pngs_with_more_image_data_than_their_ihdr() -> Vec<Vec<u8>> {
        let big = encode(
            512,
            512,
            ::png::ColorType::Grayscale,
            ::png::BitDepth::Eight,
            &vec![0; 512 * 512],
            |_| {},
            &[],
        );
        let stream = chunk_data(&big, b"IDAT").unwrap();
        let mut split = vec![ihdr(16, 16, 8, 0)];
        split.extend(idat_chunks(&stream, 3));
        vec![
            file(&[ihdr(16, 16, 8, 0), chunk(b"IDAT", &stream)]),
            file(&split),
        ]
    }

    #[test]
    fn a_png_with_more_image_data_than_its_ihdr_is_refused_before_quantising() {
        for png in pngs_with_more_image_data_than_their_ihdr() {
            let err = optimize_lossy_with(&png, q(75), never).unwrap_err();
            // The lossless path's reason, word for word.
            assert_eq!(err, crate::png::optimize(&png).unwrap_err());
            assert_eq!(
                err,
                "could not read the PNG data: Inflated data would exceed the maximum size (272 bytes)"
            );
        }
    }

    /// Run the lossy path with a quantiser double that records the image it
    /// gets and then fails, so nothing is written.
    fn quantised_image(png: &[u8]) -> Result<Option<Rgba>, String> {
        let seen = RefCell::new(None);
        match optimize_lossy_with(png, q(75), |image, _| {
            *seen.borrow_mut() = Some(image.clone());
            Err("recorded".to_owned())
        }) {
            Err(reason) if reason == "recorded" => Ok(seen.into_inner()),
            Err(reason) => Err(reason),
            Ok(_) => panic!("the double fails, so the run cannot succeed"),
        }
    }

    #[test]
    fn a_png_whose_image_data_fits_its_ihdr_is_quantised_also_when_split_over_several_idat_chunks()
    {
        let whole = gradient_png();
        let stream = chunk_data(&whole, b"IDAT").unwrap();
        let mut split = vec![chunk(b"IHDR", &chunk_data(&whole, b"IHDR").unwrap())];
        split.extend(idat_chunks(&stream, 4));
        let split = file(&split);
        assert_eq!(
            chunk_types(&split)
                .iter()
                .filter(|kind| *kind == b"IDAT")
                .count(),
            4
        );
        let expected = Rgba {
            width: GW,
            height: GH,
            pixels: gradient_pixels(GW, GH),
        };
        for png in [whole, split] {
            assert_eq!(quantised_image(&png), Ok(Some(expected.clone())));
        }
    }

    #[test]
    fn image_data_of_exactly_the_ihdr_size_is_quantised_and_one_byte_more_is_refused() {
        let palette = chunk(b"PLTE", &[0, 0, 0]);
        // (what, header chunks, the IHDR's raw size: rows of bytes, each
        // with its filter byte)
        let cases: Vec<(&str, Vec<Vec<u8>>, usize)> = vec![
            ("16x16 grey 8-bit", vec![ihdr(16, 16, 8, 0)], 16 * (1 + 16)),
            ("10x3 grey 1-bit", vec![ihdr(10, 3, 1, 0)], 3 * (1 + 2)),
            ("3x2 RGB 8-bit", vec![ihdr(3, 2, 8, 2)], 2 * (1 + 9)),
            (
                "3x3 palette 4-bit",
                vec![ihdr(3, 3, 4, 3), palette],
                3 * (1 + 2),
            ),
            ("2x2 grey-alpha 8-bit", vec![ihdr(2, 2, 8, 4)], 2 * (1 + 4)),
            ("5x2 RGBA 16-bit", vec![ihdr(5, 2, 16, 6)], 2 * (1 + 40)),
            // Adam7: seven passes of 2x2, 2x2, 4x2, 4x4, 8x4, 8x8 and 16x8
            // pixels, 30 rows in all.
            (
                "16x16 grey 8-bit interlaced",
                vec![interlaced_ihdr(16, 16, 8, 0)],
                256 + 30,
            ),
        ];
        for (what, header, size) in cases {
            let with = |length: usize| {
                let mut chunks = header.clone();
                chunks.push(chunk(b"IDAT", &zlib_stored(&vec![0; length])));
                file(&chunks)
            };
            let image = quantised_image(&with(size)).unwrap_or_else(|err| panic!("{what}: {err}"));
            assert!(image.is_some(), "{what}");

            let over = with(size + 1);
            let err = optimize_lossy_with(&over, q(75), never).unwrap_err();
            assert_eq!(err, crate::png::optimize(&over).unwrap_err(), "{what}");
            assert!(
                err.ends_with(&format!(
                    "Inflated data would exceed the maximum size ({size} bytes)"
                )),
                "{what}: {err}"
            );
        }
    }

    #[test]
    fn a_corrupt_or_short_stream_is_refused_like_lossless() {
        // 16x16 grey holds 272 bytes. A stream one byte short, and one whose
        // zlib header is broken (with a valid chunk CRC).
        let mut broken = zlib_stored(&[0; 272]);
        broken[0] = 0x00;
        for stream in [zlib_stored(&[0; 271]), broken] {
            let png = file(&[ihdr(16, 16, 8, 0), chunk(b"IDAT", &stream)]);
            let err = optimize_lossy_with(&png, q(75), never).unwrap_err();
            assert_eq!(err, crate::png::optimize(&png).unwrap_err());
        }
    }

    // --- oxipng's parser as the gate -----------------------------------------

    /// A zlib stream of stored blocks of zeros, of the given lengths (each at
    /// most 65535), the last one final, with its Adler-32.
    fn zlib_stored_blocks(lengths: &[usize]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        for (i, &length) in lengths.iter().enumerate() {
            let len = u16::try_from(length).unwrap();
            out.push(u8::from(i + 1 == lengths.len()));
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&(!len).to_le_bytes());
            out.resize(out.len() + length, 0);
        }
        // Adler-32 of zeros: a stays 1, b adds 1 per byte.
        let total: usize = lengths.iter().sum();
        let b = (total % 65_521) as u32;
        out.extend_from_slice(&((b << 16) | 1).to_be_bytes());
        out
    }

    /// The reviewer's stream for a 256x256 greyscale IHDR (65,792 raw
    /// bytes): a stored block of 65,535 bytes and one of 1 byte, so 64 KiB
    /// out, then about 1 MiB more. Returned with the offset where the 1-byte
    /// block ends, which is where the first IDAT chunk ends.
    fn window_aligned_stream() -> (Vec<u8>, usize) {
        let mut lengths = vec![65_535, 1];
        lengths.extend([65_535; 16]);
        let boundary = 2 + (5 + 65_535) + (5 + 1);
        (zlib_stored_blocks(&lengths), boundary)
    }

    fn grey_256_split_at(stream: &[u8], at: usize) -> Vec<u8> {
        file(&[
            ihdr(256, 256, 8, 0),
            chunk(b"IDAT", &stream[..at]),
            chunk(b"IDAT", &stream[at..]),
        ])
    }

    #[test]
    fn the_window_aligned_idat_split_is_refused_like_lossless() {
        let (stream, boundary) = window_aligned_stream();
        let png = grey_256_split_at(&stream, boundary);
        // `never` panics if the quantiser is reached.
        let err = optimize_lossy_with(&png, q(75), never).unwrap_err();
        assert_eq!(err, crate::png::optimize(&png).unwrap_err());
    }

    #[test]
    fn the_same_stream_split_off_the_window_boundary_is_refused_like_lossless_too() {
        let (stream, boundary) = window_aligned_stream();
        for at in [boundary - 100, boundary + 100] {
            let png = grey_256_split_at(&stream, at);
            let err = optimize_lossy_with(&png, q(75), never).unwrap_err();
            assert_eq!(
                err,
                crate::png::optimize(&png).unwrap_err(),
                "split at {at}"
            );
        }
    }

    /// All IDAT payloads of a whole PNG, joined.
    fn idat_stream(png: &[u8]) -> Vec<u8> {
        let mut stream = Vec::new();
        let mut at = 8;
        while at + 8 <= png.len() {
            let len = u32::from_be_bytes(png[at..at + 4].try_into().unwrap()) as usize;
            if &png[at + 4..at + 8] == b"IDAT" {
                stream.extend_from_slice(&png[at + 8..at + 8 + len]);
            }
            at += 12 + len;
        }
        stream
    }

    /// A 256x256 RGBA gradient (262,400 raw bytes, four 64 KiB windows and
    /// more) with its image data in five IDAT chunks, under an IHDR of
    /// `height` rows.
    fn big_gradient_split(height: u32) -> Vec<u8> {
        let source = rgba_png(256, 256, &gradient_pixels(256, 256), &[]);
        let mut chunks = vec![ihdr(256, height, 8, 6)];
        chunks.extend(idat_chunks(&idat_stream(&source), 5));
        file(&chunks)
    }

    #[test]
    fn a_valid_png_over_64_kib_of_image_data_split_over_several_idats_is_quantised() {
        let png = big_gradient_split(256);
        assert_eq!(
            chunk_types(&png)
                .iter()
                .filter(|kind| *kind == b"IDAT")
                .count(),
            5
        );
        let expected = Rgba {
            width: 256,
            height: 256,
            pixels: gradient_pixels(256, 256),
        };
        assert_eq!(quantised_image(&png), Ok(Some(expected)));
    }

    #[test]
    fn the_same_image_data_under_an_ihdr_one_row_shorter_is_refused_like_lossless() {
        let png = big_gradient_split(255);
        let err = optimize_lossy_with(&png, q(75), never).unwrap_err();
        assert_eq!(err, crate::png::optimize(&png).unwrap_err());
    }

    // --- what oxipng accepts and the decoder refuses -------------------------

    /// A 16x16 all-black greyscale PNG with the given tRNS chunks (each a
    /// grey sample of 0 or 1) before its image data.
    fn grey_with_trns(samples: &[u8]) -> Vec<u8> {
        let mut chunks = vec![ihdr(16, 16, 8, 0)];
        chunks.extend(samples.iter().map(|&sample| chunk(b"tRNS", &[0, sample])));
        chunks.push(chunk(b"IDAT", &zlib_stored(&[0; 272])));
        file(&chunks)
    }

    #[test]
    fn a_png_oxipng_accepts_but_the_decoder_refuses_fails_with_the_lossy_decode_reason() {
        // Two tRNS chunks, both with valid CRCs: oxipng keeps the last one,
        // the `png` decoder refuses the second.
        let png = grey_with_trns(&[0, 1]);
        assert!(crate::png::optimize(&png).is_ok(), "lossless accepts it");
        // `never` panics if the quantiser is reached.
        let err = optimize_lossy_with(&png, q(75), never).unwrap_err();
        assert!(
            err.starts_with("could not decode the PNG for lossy: "),
            "{err}"
        );
    }

    #[test]
    fn the_same_png_with_one_trns_chunk_is_quantised() {
        let image = quantised_image(&grey_with_trns(&[0])).expect("quantised");
        let image = image.expect("the quantiser was called");
        assert_eq!((image.width, image.height), (16, 16));
        // Black is the tRNS grey sample 0: every pixel is fully transparent.
        assert!(image.pixels.chunks(4).all(|pixel| pixel == [0, 0, 0, 0]));
    }
}

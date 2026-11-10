//! Lossy still WebP: the source, lossless or lossy, is decoded to RGBA and
//! encoded again by libwebp as a lossy WebP at the run's quality, with
//! libwebp's default alpha quality of 100 ([`codec`]).
//!
//! The source's metadata is put back under today's lossless rules
//! ([`riff::with_metadata`], which shares its choice of chunks with
//! [`riff::WebpFile::stripped`]): every `ICCP` chunk byte for byte with the
//! source's ICC flag, the first `EXIF` chunk only when it turns the picture,
//! never `XMP `.
//!
//! An animated source goes to [`optimize_animated_lossy`].
//!
//! Safe Rust: the libwebp calls are in [`codec`].

use super::codec::{self, Codec, LibWebp};
use super::riff::{self, Kind};
use crate::lossy::{Lossy, Note, Quality, Shape, check_shape, finish, kept_lossless};

/// Compress WebP bytes lossily at `quality`.
///
/// The result is never larger than the source (see [`finish`]). The error
/// is a message for the user, as for the lossless path.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "wired into the run by a later task")
)]
pub(crate) fn optimize_lossy(bytes: &[u8], quality: Quality) -> Result<Lossy, String> {
    optimize_lossy_with(bytes, quality, &LibWebp)
}

/// [`optimize_lossy`] with the codec as an argument, so tests can pass a
/// double for it.
///
/// The container is walked and the declared size checked against
/// [`crate::MAX_PIXELS`] ([`riff::parse`]) before anything is decoded.
fn optimize_lossy_with(
    bytes: &[u8],
    quality: Quality,
    codec: &impl Codec,
) -> Result<Lossy, String> {
    let file = riff::parse(bytes)?;
    if file.kind == Kind::Animated {
        return optimize_animated_lossy(bytes, quality);
    }
    let image = codec.decode_rgba(bytes)?;
    let expected = Shape {
        width: image.width,
        height: image.height,
        duration_ms: None,
    };
    let encoded = codec.encode_lossy(&image, quality)?;
    drop(image);
    let result = riff::with_metadata(&encoded, &file)?;
    drop(encoded);
    let (width, height) = codec::info(&result)
        .ok_or_else(|| "the lossy result is not a WebP libwebp can read".to_owned())?;
    let found = Shape {
        width,
        height,
        duration_ms: None,
    };
    check_shape(expected, found)?;
    Ok(finish(bytes, result))
}

/// An animated WebP in a lossy run.
///
/// A STUB until Task 6 of the lossy plan replaces it with libwebp's
/// animation decoder and encoder (and extends [`Codec`] for them): for now
/// every animation gets the lossless treatment with
/// [`Note::KeptLosslessAnimationTooLarge`]. Never released in this state.
fn optimize_animated_lossy(bytes: &[u8], _quality: Quality) -> Result<Lossy, String> {
    Ok(kept_lossless(
        super::optimize(bytes)?,
        Note::KeptLosslessAnimationTooLarge,
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::panic;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::webp::codec::Rgba;
    use crate::webp::fixtures as fx;
    use crate::webp::riff::tests::{
        chunk, raw_chunk, riff, riff_declaring, vp8_payload, vp8l, vp8l_payload, vp8x,
    };

    const W: u32 = 256;
    const H: u32 = 192;
    const QUICK: Duration = Duration::from_secs(5);
    /// A distinctive, odd-sized ICC profile.
    const ICC: &[u8] = &[0x5A; 301];

    fn q(value: u8) -> Quality {
        Quality::new(value).unwrap()
    }

    // --- fixtures ---------------------------------------------------------------

    fn photo(alpha: bool) -> Vec<u8> {
        fx::photo_rgba(W, H, alpha)
    }

    /// A lossless still WebP of the photo, from the `image` crate.
    fn lossless(alpha: bool) -> Vec<u8> {
        fx::lossless(&photo(alpha), W, H)
    }

    fn rotated_exif() -> Vec<u8> {
        fx::exif_with_orientation(6, true, true)
    }

    fn flags(file: &[u8]) -> u8 {
        file[fx::VP8X_FLAGS_AT]
    }

    /// The ICC, EXIF and XMP flags of a file; 0 for a file without `VP8X`.
    fn metadata_flags(file: &[u8]) -> u8 {
        if fx::chunk_names(file)[0] == "VP8X" {
            flags(file) & (fx::FLAG_ICC | fx::FLAG_EXIF | fx::FLAG_XMP)
        } else {
            0
        }
    }

    /// The payloads of every chunk called `fourcc`, in order.
    fn payloads(file: &[u8], fourcc: &[u8; 4]) -> Vec<Vec<u8>> {
        fx::chunks_of(file)
            .into_iter()
            .filter(|(name, _)| name == fourcc)
            .map(|(_, data)| data)
            .collect()
    }

    /// `file` with its `VP8X` flags set to `flags`.
    fn with_flags(file: &[u8], flags: u8) -> Vec<u8> {
        let mut out = file.to_vec();
        out[fx::VP8X_FLAGS_AT] = flags;
        out
    }

    /// The source as hostile_input.rs builds it: a real 32x32 lossless WebP.
    fn normal_webp() -> Vec<u8> {
        let pixels: Vec<u8> = (0..32 * 32 * 4).map(|i| (i * 7 % 251) as u8).collect();
        fx::lossless(&pixels, 32, 32)
    }

    // --- doubles -------------------------------------------------------------------

    type Decode = Box<dyn Fn(&[u8]) -> Result<Rgba, String>>;
    type Encode = Box<dyn Fn(&Rgba, Quality) -> Result<Vec<u8>, String>>;

    /// A codec double: `decode` and `encode` answer, and every call is
    /// recorded (the encoder's as width, height, pixel bytes and quality).
    struct Double {
        decode: Decode,
        encode: Encode,
        decoded: RefCell<usize>,
        encoded: RefCell<Vec<(u32, u32, usize, u8)>>,
    }

    impl Codec for Double {
        fn decode_rgba(&self, bytes: &[u8]) -> Result<Rgba, String> {
            *self.decoded.borrow_mut() += 1;
            (self.decode)(bytes)
        }

        fn encode_lossy(&self, rgba: &Rgba, quality: Quality) -> Result<Vec<u8>, String> {
            self.encoded.borrow_mut().push((
                rgba.width,
                rgba.height,
                rgba.pixels.len(),
                quality.get(),
            ));
            (self.encode)(rgba, quality)
        }
    }

    fn double(
        decode: impl Fn(&[u8]) -> Result<Rgba, String> + 'static,
        encode: impl Fn(&Rgba, Quality) -> Result<Vec<u8>, String> + 'static,
    ) -> Double {
        Double {
            decode: Box::new(decode),
            encode: Box::new(encode),
            decoded: RefCell::new(0),
            encoded: RefCell::new(Vec::new()),
        }
    }

    /// The real decoder, with an encoder that returns `result`.
    fn encoding_to(result: Result<Vec<u8>, String>) -> Double {
        double(
            |bytes| LibWebp.decode_rgba(bytes),
            move |_, _| result.clone(),
        )
    }

    /// libwebp, with every call recorded.
    fn recording() -> Double {
        double(
            |bytes| LibWebp.decode_rgba(bytes),
            |rgba, quality| LibWebp.encode_lossy(rgba, quality),
        )
    }

    fn calls(codec: &Double) -> (usize, usize) {
        (*codec.decoded.borrow(), codec.encoded.borrow().len())
    }

    // --- the real codec: still images ------------------------------------------------

    #[test]
    fn a_lossless_webp_becomes_a_smaller_vp8_with_the_same_size() {
        let source = lossless(false);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert!(lossy.bytes.len() < source.len());
        assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8 "]);
        let (w, h, _) = fx::decode_rgba(&lossy.bytes);
        assert_eq!((w, h), (W, H));
    }

    #[test]
    fn a_small_lossy_webp_at_100_keeps_the_original_with_the_note() {
        let source = fx::lossy(&photo(false), W, H, 20.0);
        let lossy = optimize_lossy(&source, q(100)).unwrap();
        assert_eq!(lossy.note, Some(Note::OriginalKept));
        assert_eq!(lossy.bytes, source);
    }

    #[test]
    fn an_alpha_webp_keeps_an_alpha_channel() {
        let source = lossless(true);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8X", "ALPH", "VP8 "]);
        assert_eq!(flags(&lossy.bytes) & fx::FLAG_ALPHA, fx::FLAG_ALPHA);
        let (w, h, pixels) = fx::decode_rgba(&lossy.bytes);
        assert_eq!((w, h), (W, H));
        // libwebp's alpha quality is 100: the alpha channel is exact.
        let alpha = |rgba: &[u8]| rgba.iter().skip(3).step_by(4).copied().collect::<Vec<_>>();
        assert_eq!(alpha(&pixels), alpha(&photo(true)));
        assert_eq!(pixels[3], 0, "the transparent corner");
    }

    #[test]
    fn an_opaque_webp_gains_no_alpha_channel() {
        let lossy = optimize_lossy(&lossless(false), q(75)).unwrap();
        assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8 "]);
        let (_, _, pixels) = fx::decode_rgba(&lossy.bytes);
        assert!(pixels.iter().skip(3).step_by(4).all(|&a| a == 255));
    }

    // --- metadata ------------------------------------------------------------------

    #[test]
    fn iccp_kept_byte_for_byte() {
        let source = fx::with_chunks(&lossless(false), &[(*b"ICCP", ICC)]);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8X", "ICCP", "VP8 "]);
        assert_eq!(payloads(&lossy.bytes, b"ICCP"), [ICC]);
        assert_eq!(flags(&lossy.bytes) & fx::FLAG_ICC, fx::FLAG_ICC);
    }

    #[test]
    fn a_source_without_iccp_gains_none() {
        for (alpha, names) in [
            (false, &["VP8 "][..]),
            (true, &["VP8X", "ALPH", "VP8 "][..]),
        ] {
            let lossy = optimize_lossy(&lossless(alpha), q(75)).unwrap();
            assert_eq!(fx::chunk_names(&lossy.bytes), names);
            assert_eq!(metadata_flags(&lossy.bytes), 0);
        }
    }

    #[test]
    fn exif_orientation_6_kept() {
        let source = fx::with_chunks(&lossless(false), &[(*b"EXIF", &rotated_exif())]);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8X", "VP8 ", "EXIF"]);
        assert_eq!(payloads(&lossy.bytes, b"EXIF"), [rotated_exif()]);
        assert_eq!(flags(&lossy.bytes), fx::FLAG_EXIF);
        // The pixels are not turned: the orientation stays metadata.
        let (w, h, _) = fx::decode_rgba(&lossy.bytes);
        assert_eq!((w, h), (W, H));
    }

    #[test]
    fn exif_orientation_1_dropped() {
        let exif = fx::exif_with_orientation(1, true, true);
        let source = fx::with_chunks(&lossless(false), &[(*b"EXIF", &exif)]);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8 "]);
    }

    #[test]
    fn xmp_dropped() {
        let source = fx::with_chunks(
            &lossless(false),
            &[
                (*b"ICCP", ICC),
                (*b"XMP ", b"<x:xmpmeta xmlns:x='adobe:ns:meta/'/>"),
            ],
        );
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8X", "ICCP", "VP8 "]);
        assert_eq!(flags(&lossy.bytes) & fx::FLAG_XMP, 0);
        // The same file's profile is still written.
        assert_eq!(payloads(&lossy.bytes, b"ICCP"), [ICC]);
    }

    #[test]
    fn vp8x_flags_match_the_chunks_written() {
        for alpha in [false, true] {
            for icc in [false, true] {
                for exif in [false, true] {
                    let mut extra: Vec<([u8; 4], &[u8])> = vec![(*b"XMP ", b"<x/>")];
                    if icc {
                        extra.push((*b"ICCP", ICC));
                    }
                    let rotated = rotated_exif();
                    if exif {
                        extra.push((*b"EXIF", &rotated));
                    }
                    let source = fx::with_chunks(&lossless(alpha), &extra);
                    let out = optimize_lossy(&source, q(75)).unwrap().bytes;
                    let names = fx::chunk_names(&out);
                    let what = format!("alpha {alpha}, icc {icc}, exif {exif}: {names:?}");
                    assert_eq!(names.contains(&"ALPH".to_owned()), alpha, "{what}");
                    assert_eq!(names.contains(&"ICCP".to_owned()), icc, "{what}");
                    assert_eq!(names.contains(&"EXIF".to_owned()), exif, "{what}");
                    assert!(!names.contains(&"XMP ".to_owned()), "{what}");
                    assert_eq!(names[0] == "VP8X", alpha || icc || exif, "{what}");
                    if names[0] != "VP8X" {
                        continue;
                    }
                    let mut expected = 0;
                    for (has, flag) in [
                        (alpha, fx::FLAG_ALPHA),
                        (icc, fx::FLAG_ICC),
                        (exif, fx::FLAG_EXIF),
                    ] {
                        if has {
                            expected |= flag;
                        }
                    }
                    assert_eq!(flags(&out), expected, "{what}");
                }
            }
        }
    }

    #[test]
    fn the_metadata_matches_the_lossless_path_for_every_source_shape() {
        let base = lossless(false);
        let image = fx::chunks_of(&base).remove(0);
        let exif = |orientation| {
            (
                *b"EXIF",
                fx::exif_with_orientation(orientation, false, false),
            )
        };
        let vp8x = (
            *b"VP8X",
            fx::vp8x_payload(fx::FLAG_ICC | fx::FLAG_EXIF, W, H),
        );
        let sources = [
            (
                "profile and rotation",
                fx::with_chunks(&base, &[(*b"ICCP", ICC), (*b"EXIF", &rotated_exif())]),
            ),
            (
                "a simple file with a profile after its image",
                fx::from_chunks(&[image.clone(), (*b"ICCP", ICC.to_vec())]),
            ),
            (
                "a simple file with a rotation",
                fx::from_chunks(&[image.clone(), exif(8)]),
            ),
            (
                "first EXIF rotated, second not",
                fx::from_chunks(&[vp8x.clone(), image.clone(), exif(6), exif(1)]),
            ),
            (
                "first EXIF not rotated, second rotated",
                fx::from_chunks(&[vp8x.clone(), image.clone(), exif(1), exif(6)]),
            ),
            (
                "two profiles",
                fx::from_chunks(&[
                    vp8x.clone(),
                    (*b"ICCP", ICC.to_vec()),
                    (*b"ICCP", vec![7; 12]),
                    image.clone(),
                ]),
            ),
            (
                "XMP and an unknown chunk only",
                fx::with_chunks(&base, &[(*b"XMP ", b"<x/>"), (*b"ABCD", &[1; 5])]),
            ),
        ];
        for (what, source) in sources {
            let stripped = riff::parse(&source).unwrap().stripped();
            let out = optimize_lossy(&source, q(75)).unwrap();
            assert_eq!(out.note, None, "{what}");
            for fourcc in [b"ICCP", b"EXIF", b"XMP "] {
                assert_eq!(
                    payloads(&out.bytes, fourcc),
                    payloads(&stripped, fourcc),
                    "{what}: {}",
                    String::from_utf8_lossy(fourcc)
                );
            }
            if fx::chunk_names(&stripped)[0] == "VP8X" {
                assert_eq!(
                    metadata_flags(&out.bytes),
                    metadata_flags(&stripped),
                    "{what}"
                );
            } else {
                // A simple source has no flags to keep. The lossy file needs
                // a VP8X for its metadata: its EXIF flag matches the chunk,
                // and its ICC flag is the source's, which is none.
                let has_exif = !payloads(&out.bytes, b"EXIF").is_empty();
                let expected = if has_exif { fx::FLAG_EXIF } else { 0 };
                assert_eq!(metadata_flags(&out.bytes), expected, "{what}");
            }
        }
    }

    #[test]
    fn the_icc_flag_stays_clear_when_the_source_had_it_clear() {
        let with_icc = fx::with_chunks(&lossless(false), &[(*b"ICCP", ICC)]);
        let cleared = with_flags(&with_icc, flags(&with_icc) & !fx::FLAG_ICC);
        let out = optimize_lossy(&cleared, q(75)).unwrap().bytes;
        assert_eq!(payloads(&out, b"ICCP"), [ICC]);
        assert_eq!(flags(&out) & fx::FLAG_ICC, 0);
        // The counterpart: a source with the flag set keeps it set.
        let out = optimize_lossy(&with_icc, q(75)).unwrap().bytes;
        assert_eq!(flags(&out) & fx::FLAG_ICC, fx::FLAG_ICC);
    }

    // --- quality ----------------------------------------------------------------------

    #[test]
    fn quality_30_is_smaller_than_quality_90() {
        let source = lossless(false);
        let low = optimize_lossy(&source, q(30)).unwrap();
        let high = optimize_lossy(&source, q(90)).unwrap();
        assert_eq!((low.note, high.note), (None, None));
        assert!(low.bytes.len() < high.bytes.len());
    }

    #[test]
    fn quality_90_is_not_smaller_than_quality_30() {
        let source = lossless(false);
        let low = optimize_lossy(&source, q(30)).unwrap();
        let high = optimize_lossy(&source, q(90)).unwrap();
        assert_eq!((low.note, high.note), (None, None));
        assert!(high.bytes.len() >= low.bytes.len());
    }

    #[test]
    fn quality_0_gives_a_valid_webp_of_the_same_size() {
        let lossy = optimize_lossy(&lossless(true), q(0)).unwrap();
        assert_eq!(lossy.note, None);
        let (w, h, _) = fx::decode_rgba(&lossy.bytes);
        assert_eq!((w, h), (W, H));
    }

    // --- codec doubles ------------------------------------------------------------------

    #[test]
    fn the_codec_double_gets_the_decoded_pixels_and_the_quality() {
        let codec = encoding_to(Err("recorded".to_owned()));
        let err = optimize_lossy_with(&lossless(true), q(42), &codec).unwrap_err();
        assert_eq!(err, "recorded");
        assert_eq!(*codec.decoded.borrow(), 1);
        assert_eq!(*codec.encoded.borrow(), [(W, H, (W * H * 4) as usize, 42)]);
    }

    #[test]
    fn a_decode_error_fails_with_its_reason_and_nothing_is_encoded() {
        let codec = double(
            |_| Err("decoder broke".to_owned()),
            |_, _| panic!("the encoder must not be called"),
        );
        let err = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap_err();
        assert_eq!(err, "decoder broke");
        assert_eq!(calls(&codec), (1, 0));
    }

    #[test]
    fn an_encode_double_returning_an_error_fails_with_its_reason() {
        let codec = encoding_to(Err("encoder broke".to_owned()));
        let err = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap_err();
        assert_eq!(err, "encoder broke");
    }

    #[test]
    fn an_encode_double_returning_another_size_fails() {
        let other = fx::lossy(&fx::photo_rgba(16, 16, false), 16, 16, 20.0);
        let codec = encoding_to(Ok(other));
        let err = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap_err();
        assert_eq!(
            err,
            "the lossy result has a different size (16×16, expected 256×192)"
        );
    }

    #[test]
    fn an_encode_double_returning_bytes_that_are_not_webp_fails() {
        for garbage in [b"not a WebP at all".to_vec(), Vec::new(), b"RIFF".to_vec()] {
            let codec = encoding_to(Ok(garbage.clone()));
            let err = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap_err();
            assert!(
                err.starts_with("the lossy result is not a readable WebP file ("),
                "{garbage:?}: {err}"
            );
        }
    }

    #[test]
    fn an_encode_double_returning_a_webp_libwebp_cannot_read_fails() {
        // A VP8 frame header with the "show frame" bit clear: the chunk walk
        // takes it, libwebp's header check does not.
        let mut hidden = vp8_payload(W as u16, H as u16);
        hidden[0] = 0x20;
        let codec = encoding_to(Ok(riff(&[chunk(b"VP8 ", &hidden)])));
        let err = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap_err();
        assert_eq!(err, "the lossy result is not a WebP libwebp can read");

        // The same header with the bit set passes.
        let shown = riff(&[chunk(b"VP8 ", &vp8_payload(W as u16, H as u16))]);
        let codec = encoding_to(Ok(shown.clone()));
        let lossy = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap();
        assert_eq!(lossy.bytes, shown);
    }

    #[test]
    fn a_valid_double_result_is_returned() {
        let small = fx::lossy(&photo(false), W, H, 20.0);
        let codec = encoding_to(Ok(small.clone()));
        let lossy = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap();
        assert_eq!(lossy.bytes, small);
        assert_eq!(lossy.note, None);
    }

    // --- the pixel limit and the kind of file --------------------------------------------

    #[test]
    fn a_header_over_max_pixels_is_refused_before_decoding() {
        let cases = [
            (riff(&[vp8x(0, 20_000, 20_000)]), "20000×20000"),
            (riff(&[vp8l(16_384, 16_384)]), "16384×16384"),
        ];
        for (bomb, size) in cases {
            let codec = recording();
            let started = Instant::now();
            let err = optimize_lossy_with(&bomb, q(75), &codec).unwrap_err();
            assert_eq!(
                err,
                format!("image too large: {size} exceeds 100 megapixels")
            );
            assert!(started.elapsed() < QUICK);
            assert_eq!(calls(&codec), (0, 0));
        }
    }

    #[test]
    fn a_header_within_max_pixels_is_passed_on_to_the_decoder() {
        // An 8x8 VP8L header over a broken bitstream: libwebp gets to refuse it.
        let codec = recording();
        let err = optimize_lossy_with(&riff(&[vp8l(8, 8)]), q(75), &codec).unwrap_err();
        assert_eq!(
            err,
            "could not read the WebP data: libwebp could not decode the image"
        );
        assert_eq!(calls(&codec), (1, 0));
    }

    /// Two different frames of the photo, as an animation.
    fn animation() -> Vec<u8> {
        let frames = [
            fx::lossless(&fx::photo_rgba(32, 32, true), 32, 32),
            fx::lossless(&fx::photo_rgba(32, 32, false), 32, 32),
        ];
        fx::animated(&frames, 32, 32)
    }

    #[test]
    fn an_animated_webp_is_kept_lossless_with_the_note_until_task_6() {
        let plain = animation();
        let with_metadata =
            fx::with_chunks(&plain, &[(*b"XMP ", &[1; 400]), (*b"EXIF", &[2; 400])]);
        for source in [plain, with_metadata] {
            let codec = recording();
            let lossy = optimize_lossy_with(&source, q(75), &codec).unwrap();
            assert_eq!(lossy.note, Some(Note::KeptLosslessAnimationTooLarge));
            assert_eq!(lossy.bytes, crate::webp::optimize(&source).unwrap());
            assert_eq!(calls(&codec), (0, 0));
        }
    }

    #[test]
    fn a_still_webp_goes_through_the_codec() {
        let codec = recording();
        let lossy = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(calls(&codec), (1, 1));
    }

    // --- hostile input (the WebP cases of tests/hostile_input.rs) -------------------------

    /// A PNG of one IHDR chunk, as hostile_input.rs builds it.
    fn one_pixel_png() -> Vec<u8> {
        crate::png::tests::file(&[crate::png::tests::ihdr(1, 1, 8, 0)])
    }

    /// The hostile WebP cases, the same bytes as in hostile_input.rs, and a
    /// part of the message each must fail with.
    fn hostile_cases() -> Vec<(&'static str, Vec<u8>, &'static str)> {
        const NOT_WEBP: &str = "not a WebP file (the content is not WebP data)";
        const UNREADABLE: &str = "could not read the WebP data";
        let mut wave = b"RIFF".to_vec();
        wave.extend_from_slice(&36u32.to_le_bytes());
        wave.extend_from_slice(b"WAVEfmt ");
        let lossless_8 = || chunk(b"VP8L", &vp8l_payload(8, 8));
        let whole = riff(&[lossless_8()]);
        vec![
            ("only RIFF", b"RIFF".to_vec(), NOT_WEBP),
            ("empty", Vec::new(), NOT_WEBP),
            ("a PNG", one_pixel_png(), NOT_WEBP),
            ("a WAVE file", wave, NOT_WEBP),
            (
                "a chunk of u32::MAX bytes",
                riff(&[raw_chunk(b"VP8L", u32::MAX, &vp8l_payload(8, 8))]),
                UNREADABLE,
            ),
            (
                "a RIFF size past the end",
                riff_declaring(u32::MAX, &lossless_8()),
                UNREADABLE,
            ),
            (
                "cut inside VP8L",
                whole[..whole.len() - 4].to_vec(),
                UNREADABLE,
            ),
            (
                "a VP8L payload shorter than its header",
                riff(&[chunk(b"VP8L", &vp8l_payload(8, 8)[..3])]),
                UNREADABLE,
            ),
            (
                "a misplaced VP8X",
                riff(&[lossless_8(), vp8x(0, 8, 8)]),
                UNREADABLE,
            ),
            (
                "a second VP8X",
                riff(&[vp8x(0, 8, 8), vp8x(0, 65_535, 65_535), lossless_8()]),
                UNREADABLE,
            ),
            (
                "a canvas over the pixel limit",
                riff(&[vp8x(0, 20_000, 20_000)]),
                "image too large: 20000×20000 exceeds 100 megapixels",
            ),
            (
                "a canvas at the limit with no image",
                riff(&[vp8x(0, 10_000, 10_000)]),
                UNREADABLE,
            ),
        ]
    }

    #[test]
    fn the_hostile_webp_cases_fail_in_a_lossy_run_too() {
        for (what, bytes, reason) in hostile_cases() {
            let started = Instant::now();
            let result = panic::catch_unwind(|| optimize_lossy(&bytes, q(75)));
            let err = result.expect("no panic").unwrap_err();
            assert!(err.contains(reason), "{what}: {err}");
            assert!(started.elapsed() < QUICK, "{what}");
            if what == "a canvas at the limit with no image" {
                assert!(!err.contains("too large"), "{what}: {err}");
            }
            // Refused from the container: nothing is decoded.
            let codec = recording();
            assert!(
                optimize_lossy_with(&bytes, q(75), &codec).is_err(),
                "{what}"
            );
            assert_eq!(calls(&codec), (0, 0), "{what}");
        }
    }

    #[test]
    fn a_normal_webp_is_optimised_in_a_lossy_run() {
        let original = normal_webp();
        let lossy = optimize_lossy(&original, q(75)).unwrap();
        let (w, h, _) = fx::decode_rgba(&lossy.bytes);
        assert_eq!((w, h), (32, 32));
    }

    #[test]
    fn a_webp_with_a_malformed_exif_chunk_is_lossy_without_the_chunk() {
        let le = |v: u32| v.to_le_bytes();
        let mut header_cut = fx::EXIF_PREFIX.to_vec();
        header_cut.extend_from_slice(b"II*");
        let mut offset_past_end = fx::tiff(false, &[(fx::ORIENTATION, fx::SHORT, 1, 6)]);
        offset_past_end[4..8].copy_from_slice(&le(u32::MAX));
        let mut offset_into_header = fx::tiff(true, &[(fx::ORIENTATION, fx::SHORT, 1, 6)]);
        offset_into_header[4..8].copy_from_slice(&2u32.to_be_bytes());
        let mut count_too_large = fx::tiff(false, &[]);
        count_too_large[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        let wrong_type = fx::tiff(true, &[(fx::ORIENTATION, 4, 1, 6)]);
        let cases: [(&str, Vec<u8>); 6] = [
            ("empty", Vec::new()),
            ("header cut short", header_cut),
            ("IFD0 offset past the end", offset_past_end),
            ("IFD0 offset into the header", offset_into_header),
            ("entry count larger than the payload", count_too_large),
            ("Orientation of type LONG", wrong_type),
        ];
        for (what, exif) in cases {
            // The bytes of hostile_input.rs: not refused, quick. At 32x32
            // the lossy file is not smaller, so the original comes back.
            let source = fx::with_chunks(&normal_webp(), &[(*b"EXIF", &exif)]);
            let started = Instant::now();
            let lossy = optimize_lossy(&source, q(75)).unwrap();
            assert!(started.elapsed() < QUICK, "{what}");
            assert_eq!(lossy.note, Some(Note::OriginalKept), "{what}");
            assert_eq!(lossy.bytes, source, "{what}");
            // The same EXIF on the photo, where the lossy file is written:
            // the chunk is dropped.
            let source = fx::with_chunks(&lossless(false), &[(*b"EXIF", &exif)]);
            let lossy = optimize_lossy(&source, q(75)).unwrap();
            assert_eq!(lossy.note, None, "{what}");
            assert_eq!(fx::chunk_names(&lossy.bytes), ["VP8 "], "{what}");
        }

        // The control: the same file with a well-formed rotation keeps it.
        let rotated = fx::exif_with_orientation(6, true, false);
        let source = fx::with_chunks(&lossless(false), &[(*b"EXIF", &rotated)]);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(payloads(&lossy.bytes, b"EXIF"), [rotated]);
    }
}

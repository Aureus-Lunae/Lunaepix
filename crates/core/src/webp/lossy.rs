//! Lossy WebP: the source, lossless or lossy, still or animated, is decoded
//! to RGBA and encoded again by libwebp as a lossy WebP at the run's
//! quality, with libwebp's default alpha quality of 100 ([`codec`]). An
//! animated source goes to [`optimize_animated_lossy`].
//!
//! The source's metadata is put back under today's lossless rules
//! ([`riff::with_metadata`], which shares its choice of chunks with
//! [`riff::WebpFile::stripped`]): every `ICCP` chunk byte for byte with the
//! source's ICC flag, the first `EXIF` chunk only when it turns the picture,
//! never `XMP `.
//!
//! Safe Rust: the libwebp calls are in [`codec`].

use super::codec::{self, Codec, LibWebp};
use super::riff::{self, Kind};
use crate::lossy::{Lossy, Note, Quality, Shape, check_shape, finish, kept_lossless};
use crate::plan::animation_within_caps;

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
        return optimize_animated_lossy(bytes, &file, quality, codec);
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
    check_shape(expected, shape_of(&result)?)?;
    Ok(finish(bytes, result))
}

/// An animated WebP in a lossy run: every frame decoded as a full canvas
/// and encoded again by libwebp's animation encoder, lossy at `quality`,
/// one frame at a time, with the source's loop count, background colour and
/// frame timings ([`codec`]); then the source's metadata as for a still
/// image ([`riff::with_metadata`]).
///
/// Before any decoder runs, the headers ([`riff::animation_header`]) are
/// checked against the animation caps ([`animation_within_caps`]) and
/// against the longest total duration libwebp can count
/// ([`codec::MAX_TIMESTAMP_MS`]). Over any of them the file gets the
/// lossless treatment with [`Note::KeptLosslessAnimationTooLarge`].
///
/// The result must have the source's canvas and total duration; it may
/// have fewer frames, because libwebp merges a frame that matches the one
/// before it into that frame.
fn optimize_animated_lossy(
    bytes: &[u8],
    file: &riff::WebpFile<'_>,
    quality: Quality,
    codec: &impl Codec,
) -> Result<Lossy, String> {
    let header = riff::animation_header(file)?;
    if !animation_within_caps(header.frames, header.width, header.height)
        || header.duration_ms > codec::MAX_TIMESTAMP_MS
    {
        return Ok(kept_lossless(
            super::optimize(bytes)?,
            Note::KeptLosslessAnimationTooLarge,
        ));
    }
    let expected = Shape {
        width: header.width,
        height: header.height,
        duration_ms: Some(header.duration_ms),
    };
    let (info, mut frames) = codec.decode_animation(bytes)?;
    let encoded = codec.encode_animation(&info, &mut frames, quality)?;
    drop(frames);
    let result = riff::with_metadata(&encoded, file)?;
    drop(encoded);
    check_shape(expected, shape_of(&result)?)?;
    Ok(finish(bytes, result))
}

/// The shape of a lossy result as libwebp reads it: for a still image its
/// size from the header ([`codec::info`]); for an animation the canvas
/// libwebp's animation decoder opens it with (which checks the whole
/// container and where every frame sits, without decoding a pixel) and the
/// total of its frame durations. A result libwebp cannot open is refused.
fn shape_of(result: &[u8]) -> Result<Shape, String> {
    let unreadable = || "the lossy result is not a WebP libwebp can read".to_owned();
    // `with_metadata` wrote `result` from a parsed file, so it parses.
    if riff::parse(result).map_err(|_| unreadable())?.kind == Kind::Animated {
        let (info, _frames) = LibWebp.decode_animation(result).map_err(|_| unreadable())?;
        return Ok(Shape {
            width: info.width,
            height: info.height,
            duration_ms: Some(u64::from(info.duration_ms)),
        });
    }
    let (width, height) = codec::info(result).ok_or_else(unreadable)?;
    Ok(Shape {
        width,
        height,
        duration_ms: None,
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::panic;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::webp::codec::{AnimationInfo, Frame, Rgba};
    use crate::webp::fixtures as fx;
    use crate::webp::riff::tests::{
        anmf_header, chunk, raw_chunk, riff, riff_declaring, vp8_payload, vp8l, vp8l_payload, vp8x,
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
    type FrameList = Vec<Result<Frame, String>>;
    type DecodeAnimation = Box<dyn Fn(&[u8]) -> Result<(AnimationInfo, FrameList), String>>;
    type EncodeAnimation =
        Box<dyn Fn(&AnimationInfo, FrameList, Quality) -> Result<Vec<u8>, String>>;

    /// What the animation encoder of a double was given.
    #[derive(Debug, Clone, PartialEq, Eq)]
    struct AnimationCall {
        info: AnimationInfo,
        frames: FrameList,
        quality: u8,
    }

    /// A codec double: the four closures answer, and every call is recorded
    /// (the still encoder's as width, height, pixel bytes and quality; the
    /// animation decoder's as the bytes it was given; the animation
    /// encoder's as everything it was given).
    struct Double {
        decode: Decode,
        encode: Encode,
        decode_animation: DecodeAnimation,
        encode_animation: EncodeAnimation,
        decoded: RefCell<usize>,
        encoded: RefCell<Vec<(u32, u32, usize, u8)>>,
        animations_decoded: RefCell<Vec<Vec<u8>>>,
        animations_encoded: RefCell<Vec<AnimationCall>>,
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

        fn decode_animation<'a>(
            &self,
            bytes: &'a [u8],
        ) -> Result<(AnimationInfo, codec::Frames<'a>), String> {
            self.animations_decoded.borrow_mut().push(bytes.to_vec());
            let (info, frames) = (self.decode_animation)(bytes)?;
            Ok((info, Box::new(frames.into_iter())))
        }

        fn encode_animation(
            &self,
            info: &AnimationInfo,
            frames: &mut dyn Iterator<Item = Result<Frame, String>>,
            quality: Quality,
        ) -> Result<Vec<u8>, String> {
            let frames: FrameList = frames.collect();
            self.animations_encoded.borrow_mut().push(AnimationCall {
                info: *info,
                frames: frames.clone(),
                quality: quality.get(),
            });
            (self.encode_animation)(info, frames, quality)
        }
    }

    /// libwebp's animation decoder, every frame taken.
    fn real_decode_animation(bytes: &[u8]) -> Result<(AnimationInfo, FrameList), String> {
        let (info, frames) = LibWebp.decode_animation(bytes)?;
        Ok((info, frames.collect()))
    }

    fn real_encode_animation(
        info: &AnimationInfo,
        frames: FrameList,
        quality: Quality,
    ) -> Result<Vec<u8>, String> {
        LibWebp.encode_animation(info, &mut frames.into_iter(), quality)
    }

    /// A double for still images; its animation half is libwebp.
    fn double(
        decode: impl Fn(&[u8]) -> Result<Rgba, String> + 'static,
        encode: impl Fn(&Rgba, Quality) -> Result<Vec<u8>, String> + 'static,
    ) -> Double {
        Double {
            decode: Box::new(decode),
            encode: Box::new(encode),
            decode_animation: Box::new(real_decode_animation),
            encode_animation: Box::new(real_encode_animation),
            decoded: RefCell::new(0),
            encoded: RefCell::new(Vec::new()),
            animations_decoded: RefCell::new(Vec::new()),
            animations_encoded: RefCell::new(Vec::new()),
        }
    }

    /// A double for animations; its still half is libwebp.
    fn animation_double(
        decode: impl Fn(&[u8]) -> Result<(AnimationInfo, FrameList), String> + 'static,
        encode: impl Fn(&AnimationInfo, FrameList, Quality) -> Result<Vec<u8>, String> + 'static,
    ) -> Double {
        Double {
            decode_animation: Box::new(decode),
            encode_animation: Box::new(encode),
            ..recording()
        }
    }

    /// The real animation decoder, with an encoder that returns `result`.
    fn animation_encoding_to(result: Result<Vec<u8>, String>) -> Double {
        animation_double(real_decode_animation, move |_, _, _| result.clone())
    }

    /// The animation calls a double recorded: decoder, encoder.
    fn animation_calls(codec: &Double) -> (usize, usize) {
        (
            codec.animations_decoded.borrow().len(),
            codec.animations_encoded.borrow().len(),
        )
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
    fn an_encode_double_returning_an_animation_for_a_still_image_fails() {
        // Of the still image's size, so only the kind of file differs.
        let codec = encoding_to(Ok(lossy_animation(W, H, &[100, 100])));
        let err = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap_err();
        assert_eq!(
            err,
            "the lossy result is an animation, but the source is not an animation"
        );
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

    #[test]
    fn a_still_webp_goes_through_the_codec() {
        let codec = recording();
        let lossy = optimize_lossy_with(&lossless(false), q(75), &codec).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(calls(&codec), (1, 1));
        assert_eq!(animation_calls(&codec), (0, 0));
    }

    // --- animations: fixtures --------------------------------------------------------------

    /// The canvas of the animation fixtures.
    const AW: u32 = 96;
    const AH: u32 = 64;
    const LOOPS: u16 = 3;
    const BACKGROUND: u32 = 0x8040_2010;

    /// Opaque RGBA of the photo with its colours shifted by `k`, so every `k`
    /// gives a different frame.
    fn anim_frame(k: u8) -> Vec<u8> {
        let mut pixels = fx::photo_rgba(AW, AH, false);
        for pixel in pixels.chunks_mut(4) {
            for channel in &mut pixel[..3] {
                *channel = channel.wrapping_add(k.wrapping_mul(50));
            }
        }
        pixels
    }

    /// An animation of these frames as lossless images (from `image`), with
    /// these durations, looping 3 times on a background colour.
    fn animation_of(frames: &[Vec<u8>], durations: &[u32]) -> Vec<u8> {
        let files: Vec<Vec<u8>> = frames.iter().map(|f| fx::lossless(f, AW, AH)).collect();
        fx::animated_with(&files, AW, AH, durations, LOOPS, BACKGROUND)
    }

    /// Three distinct frames, shown 100, 250 and 70 ms.
    fn animation() -> Vec<u8> {
        animation_of(
            &[anim_frame(0), anim_frame(1), anim_frame(2)],
            &[100, 250, 70],
        )
    }

    /// A lossy animation of `w`x`h` with these durations, as an encoder
    /// double's answer.
    fn lossy_animation(w: u32, h: u32, durations: &[u32]) -> Vec<u8> {
        let frames: Vec<Vec<u8>> = (0..durations.len() as u8)
            .map(|k| {
                let mut rgba = fx::photo_rgba(w, h, false);
                rgba[0] = k;
                fx::lossy(&rgba, w, h, 30.0)
            })
            .collect();
        fx::animated_with(&frames, w, h, durations, LOOPS, BACKGROUND)
    }

    /// Only headers: a `w`x`h` canvas and `frames` frame headers of 100 ms.
    /// Nothing in it can be decoded, so only a check before decoding can
    /// give it an answer without a decoder.
    fn declared_animation(w: u32, h: u32, frames: usize) -> Vec<u8> {
        declared_with_durations(w, h, &vec![100; frames])
    }

    fn declared_with_durations(w: u32, h: u32, durations: &[u32]) -> Vec<u8> {
        let mut chunks = vec![
            vp8x(fx::FLAG_ANIMATION, w, h),
            chunk(b"ANIM", &fx::anim_payload(0, 0)),
        ];
        chunks.extend(durations.iter().map(|&d| anmf_header(w, h, d)));
        riff(&chunks)
    }

    /// 128 frames of the longest 24-bit duration and one of `last` ms:
    /// `i32::MAX` in total for `last` = 127.
    fn durations_ending_in(last: u32) -> Vec<u32> {
        let mut durations = vec![0xFF_FFFF; 128];
        durations.push(last);
        durations
    }

    /// An animation double whose decoder answers with a recognisable error,
    /// and whose encoder must never be reached.
    fn decoding_refused() -> Double {
        animation_double(
            |_| Err("the double decoded it".to_owned()),
            |_, _, _| panic!("the encoder must not be called"),
        )
    }

    // --- animations: the real codec -------------------------------------------------------

    #[test]
    fn distinct_frames_keep_canvas_loop_count_frame_count_and_durations() {
        let source = animation();
        let lossy = optimize_lossy(&source, q(75)).unwrap();

        assert_eq!(lossy.note, None);
        assert!(lossy.bytes.len() < source.len());
        let out = &lossy.bytes;
        assert_eq!(codec::info(out), Some((AW, AH)));
        assert_eq!(flags(out) & fx::FLAG_ANIMATION, fx::FLAG_ANIMATION);
        assert_eq!(payloads(out, b"ANIM"), payloads(&source, b"ANIM"));
        assert_eq!(fx::anmf_durations(out), [100, 250, 70]);
        // The source's frames are lossless; the result's are lossy, so the
        // encoder wrote them.
        for chunks in fx::anmf_image_chunks(&source) {
            assert_eq!(chunks, ["VP8L"]);
        }
        for chunks in fx::anmf_image_chunks(out) {
            assert!(chunks.contains(&"VP8 ".to_owned()), "{chunks:?}");
        }
        // An independent decoder sees three frames of the canvas.
        let frames = fx::decode_frames(out);
        assert_eq!(frames.len(), 3);
        assert!(
            frames
                .iter()
                .all(|(rgba, _)| rgba.len() == (AW * AH * 4) as usize)
        );
    }

    #[test]
    fn the_animation_codec_gets_the_sources_frames_settings_and_quality() {
        let source = animation();
        let codec = animation_double(real_decode_animation, real_encode_animation);

        optimize_lossy_with(&source, q(42), &codec).unwrap();

        assert_eq!(
            *codec.animations_decoded.borrow(),
            std::slice::from_ref(&source)
        );
        assert_eq!(calls(&codec), (0, 0), "no still decode or encode");
        let encoded = codec.animations_encoded.borrow();
        assert_eq!(encoded.len(), 1);
        let call = &encoded[0];
        assert_eq!(
            call.info,
            AnimationInfo {
                width: AW,
                height: AH,
                loop_count: u32::from(LOOPS),
                background: BACKGROUND,
                duration_ms: 420,
            }
        );
        assert_eq!(call.quality, 42);
        let expected = [anim_frame(0), anim_frame(1), anim_frame(2)];
        let frames: Vec<&Frame> = call.frames.iter().map(|f| f.as_ref().unwrap()).collect();
        let timestamps: Vec<u32> = frames.iter().map(|f| f.timestamp_ms).collect();
        assert_eq!(timestamps, [0, 100, 350]);
        for (at, frame) in frames.iter().enumerate() {
            assert_eq!(frame.rgba, expected[at], "frame {at}");
        }
    }

    #[test]
    fn an_animated_webp_goes_through_the_animation_codec() {
        let codec = recording();
        let lossy = optimize_lossy_with(&animation(), q(75), &codec).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(animation_calls(&codec), (1, 1));
        assert_eq!(calls(&codec), (0, 0));
    }

    #[test]
    fn two_identical_frames_in_a_row_merge_into_one_with_the_same_total_duration() {
        let source = animation_of(&[anim_frame(0), anim_frame(0), anim_frame(1)], &[100; 3]);
        let lossy = optimize_lossy(&source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(fx::anmf_durations(&lossy.bytes), [200, 100]);
        assert_eq!(fx::decode_frames(&lossy.bytes).len(), 2);
    }

    #[test]
    fn iccp_kept_and_xmp_dropped_on_an_animation() {
        let source = fx::with_chunks(&animation(), &[(*b"ICCP", ICC), (*b"XMP ", &[b'x'; 400])]);
        let out = &lossy_animation_bytes(&source);
        assert_eq!(
            fx::chunk_names(out),
            ["VP8X", "ICCP", "ANIM", "ANMF", "ANMF", "ANMF"]
        );
        assert_eq!(payloads(out, b"ICCP"), [ICC]);
        assert_eq!(flags(out) & fx::FLAG_ICC, fx::FLAG_ICC);
        assert_eq!(flags(out) & fx::FLAG_XMP, 0);
        assert_eq!(flags(out) & fx::FLAG_ANIMATION, fx::FLAG_ANIMATION);
    }

    /// The bytes of a lossy run that went through the animation encoder:
    /// every frame of the result is lossy (the fixtures' frames are lossless,
    /// so the lossless path cannot give this).
    fn lossy_animation_bytes(source: &[u8]) -> Vec<u8> {
        let lossy = optimize_lossy(source, q(75)).unwrap();
        assert_eq!(lossy.note, None);
        for chunks in fx::anmf_image_chunks(&lossy.bytes) {
            assert!(chunks.contains(&"VP8 ".to_owned()), "{chunks:?}");
        }
        lossy.bytes
    }

    #[test]
    fn a_rotated_exif_is_kept_on_an_animation() {
        let source = fx::with_chunks(&animation(), &[(*b"EXIF", &rotated_exif())]);
        let out = lossy_animation_bytes(&source);
        assert_eq!(fx::chunk_names(&out).last().unwrap(), "EXIF");
        assert_eq!(payloads(&out, b"EXIF"), [rotated_exif()]);
        assert_eq!(flags(&out) & fx::FLAG_EXIF, fx::FLAG_EXIF);
    }

    #[test]
    fn an_unrotated_exif_is_dropped_on_an_animation() {
        let exif = fx::exif_with_orientation(1, true, true);
        let source = fx::with_chunks(&animation(), &[(*b"EXIF", &exif)]);
        let out = lossy_animation_bytes(&source);
        assert!(payloads(&out, b"EXIF").is_empty());
        assert_eq!(flags(&out) & fx::FLAG_EXIF, 0);
    }

    // --- animations: the caps -------------------------------------------------------------

    /// `source` in a lossy run with a double that refuses to decode: kept
    /// lossless with the note, and the double never called.
    fn assert_kept_lossless_without_decoding(source: &[u8]) {
        let codec = decoding_refused();
        let started = Instant::now();
        let lossy = optimize_lossy_with(source, q(75), &codec).unwrap();
        assert!(started.elapsed() < QUICK);
        assert_eq!(lossy.note, Some(Note::KeptLosslessAnimationTooLarge));
        assert_eq!(lossy.bytes, crate::webp::optimize(source).unwrap());
        assert_eq!(animation_calls(&codec), (0, 0));
        assert_eq!(calls(&codec), (0, 0));
    }

    /// `source` in a lossy run reaches the decoder (a double, which refuses).
    fn assert_decoded(source: &[u8]) {
        let codec = decoding_refused();
        let err = optimize_lossy_with(source, q(75), &codec).unwrap_err();
        assert_eq!(err, "the double decoded it");
        assert_eq!(*codec.animations_decoded.borrow(), [source.to_vec()]);
    }

    #[test]
    fn an_animation_just_over_the_cap_is_kept_lossless_with_the_note_and_never_decoded() {
        // 1001 frames of one megapixel: 1,001,000,000 frames x pixels.
        assert_kept_lossless_without_decoding(&declared_animation(1000, 1000, 1001));
    }

    #[test]
    fn an_animation_just_under_the_cap_is_decoded() {
        // 1000 frames of one megapixel: exactly the cap, which is allowed.
        assert_decoded(&declared_animation(1000, 1000, 1000));
        assert_decoded(&declared_animation(1000, 1000, 999));
    }

    #[test]
    fn a_canvas_just_over_50_mp_is_kept_lossless_with_the_note_and_never_decoded() {
        // 10001x5000: 50,005,000 pixels, in a single frame.
        assert_kept_lossless_without_decoding(&declared_animation(10_001, 5000, 1));
    }

    #[test]
    fn a_canvas_of_exactly_50_mp_within_the_frame_cap_is_decoded() {
        // 20 frames of 50 MP: both caps exactly.
        assert_decoded(&declared_animation(10_000, 5000, 20));
    }

    #[test]
    fn an_animation_longer_than_libwebp_counts_is_kept_lossless_and_never_decoded() {
        // i32::MAX + 1 ms in total, on a 1x1 canvas well within both caps.
        assert_kept_lossless_without_decoding(&declared_with_durations(
            1,
            1,
            &durations_ending_in(128),
        ));
    }

    #[test]
    fn an_animation_of_exactly_i32_max_ms_is_decoded() {
        assert_decoded(&declared_with_durations(1, 1, &durations_ending_in(127)));
    }

    #[test]
    fn an_animation_without_vp8x_or_with_a_cut_anmf_header_fails() {
        let anim = || chunk(b"ANIM", &fx::anim_payload(0, 0));
        let no_vp8x = riff(&[anim(), anmf_header(8, 8, 100)]);
        let mut cut = anmf_header(8, 8, 100);
        cut[4] = 15;
        cut.truncate(8 + 15);
        cut.push(0);
        let cut_anmf = riff(&[vp8x(fx::FLAG_ANIMATION, 8, 8), anim(), cut]);
        let cases = [
            (no_vp8x, "the animation has no VP8X chunk"),
            (cut_anmf, "an ANMF chunk is shorter than its 16-byte header"),
        ];
        for (source, why) in cases {
            let codec = decoding_refused();
            let err = optimize_lossy_with(&source, q(75), &codec).unwrap_err();
            assert_eq!(err, format!("could not read the WebP data: {why}"));
            assert_eq!(animation_calls(&codec), (0, 0));
        }
    }

    // --- animations: codec doubles ---------------------------------------------------------

    #[test]
    fn a_decode_error_and_an_encode_error_fail_with_their_reasons() {
        let codec = animation_double(
            |_| Err("decoder broke".to_owned()),
            |_, _, _| panic!("the encoder must not be called"),
        );
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(err, "decoder broke");
        assert_eq!(animation_calls(&codec), (1, 0));

        let codec = animation_encoding_to(Err("encoder broke".to_owned()));
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(err, "encoder broke");
        assert_eq!(animation_calls(&codec), (1, 1));

        // A frame that fails to decode reaches the real encoder, which stops.
        let codec = animation_double(
            |bytes| {
                let (info, mut frames) = real_decode_animation(bytes)?;
                frames[1] = Err("frame broke".to_owned());
                Ok((info, frames))
            },
            real_encode_animation,
        );
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(err, "frame broke");
    }

    #[test]
    fn a_valid_animation_double_result_is_returned() {
        let small = lossy_animation(AW, AH, &[100, 250, 70]);
        let codec = animation_encoding_to(Ok(small.clone()));
        let lossy = optimize_lossy_with(&animation(), q(75), &codec).unwrap();
        assert_eq!(lossy.note, None);
        assert_eq!(lossy.bytes, small);
    }

    #[test]
    fn an_encode_double_with_another_total_duration_fails_with_the_reason() {
        // Fewer frames is fine; another total is not. 300 ms instead of 420.
        let codec = animation_encoding_to(Ok(lossy_animation(AW, AH, &[100, 200])));
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(
            err,
            "the lossy result has a different duration (300 ms, expected 420 ms)"
        );
        // Fewer frames with the same total pass the check.
        let codec = animation_encoding_to(Ok(lossy_animation(AW, AH, &[350, 70])));
        assert!(optimize_lossy_with(&animation(), q(75), &codec).is_ok());
    }

    #[test]
    fn an_encode_double_with_another_canvas_fails() {
        let codec = animation_encoding_to(Ok(lossy_animation(48, 32, &[100, 250, 70])));
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(
            err,
            "the lossy result has a different size (48×32, expected 96×64)"
        );
    }

    #[test]
    fn an_encode_double_returning_a_still_image_fails() {
        let still = fx::lossy(&anim_frame(0), AW, AH, 30.0);
        let codec = animation_encoding_to(Ok(still));
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(
            err,
            "the lossy result is not an animation, but the source is an animation"
        );
    }

    #[test]
    fn an_encode_double_returning_an_animation_libwebp_cannot_read_fails() {
        let off_canvas = fx::with_frame_at(&lossy_animation(AW, AH, &[100, 250, 70]), 2, 2, 0);
        let codec = animation_encoding_to(Ok(off_canvas));
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(err, "the lossy result is not a WebP libwebp can read");
    }

    #[test]
    fn an_encode_double_returning_an_animation_with_a_cut_anmf_header_fails() {
        let mut cut = anmf_header(AW, AH, 420);
        cut[4] = 15;
        cut.truncate(8 + 15);
        cut.push(0);
        let result = riff(&[
            vp8x(fx::FLAG_ANIMATION, AW, AH),
            chunk(b"ANIM", &fx::anim_payload(0, 0)),
            cut,
        ]);
        let codec = animation_encoding_to(Ok(result));
        let err = optimize_lossy_with(&animation(), q(75), &codec).unwrap_err();
        assert_eq!(err, "the lossy result is not a WebP libwebp can read");
    }

    #[test]
    fn an_animation_whose_lossy_result_is_not_smaller_keeps_the_original() {
        // The encoder answers with the source itself: valid, same shape, not
        // smaller.
        let source = animation();
        let codec = animation_encoding_to(Ok(source.clone()));
        let lossy = optimize_lossy_with(&source, q(75), &codec).unwrap();
        assert_eq!(lossy.note, Some(Note::OriginalKept));
        assert_eq!(lossy.bytes, source);
    }

    // --- animations: hostile input --------------------------------------------------------

    #[test]
    fn an_animation_with_a_frame_running_off_the_canvas_fails_without_panic_within_5_s() {
        // The last frame, as wide as the canvas, moved 2 pixels right.
        let off = fx::with_frame_at(&animation(), 2, 2, 0);
        let started = Instant::now();
        let result = panic::catch_unwind(|| optimize_lossy(&off, q(75)));
        let err = result.expect("no panic").unwrap_err();
        assert!(started.elapsed() < QUICK);
        assert_eq!(
            err,
            "could not read the WebP data: libwebp could not decode the animation"
        );
        // In place, the same frames are optimised.
        assert!(optimize_lossy(&animation(), q(75)).is_ok());
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
            assert_eq!(animation_calls(&codec), (0, 0), "{what}");
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

//! WebP optimisation.
//!
//! The lossless path walks the container in safe Rust ([`riff`]); no image
//! is decoded, so it cannot change a pixel. The lossy path ([`lossy`])
//! decodes and encodes through libwebp, and so does a thumbnail
//! ([`thumbnail_rgba`]); libwebp's FFI is the one module here that may use
//! `unsafe` ([`codec`]).

#[allow(unsafe_code)]
mod codec;
mod exif;
mod lossy;
mod riff;

pub(crate) use lossy::optimize_lossy;

use codec::{Codec, LibWebp, Rgba};
use riff::Kind;

/// Check that `bytes` is WebP data with well-formed chunks whose declared
/// size is within [`crate::MAX_PIXELS`] ([`riff::parse`]); nothing is
/// decoded.
pub(crate) fn check_header(bytes: &[u8]) -> Result<(), String> {
    riff::parse(bytes).map(drop)
}

/// The pixels a thumbnail of `side` pixels is made from, as width, height
/// and 8-bit RGBA, after the container check ([`riff::parse`]):
/// - a still image, decoded by libwebp straight to the thumbnail's size
///   ([`crate::thumb::fit`] of the size in its header);
/// - an animation's first frame, at the canvas size ([`first_frame`]).
pub(crate) fn thumbnail_rgba(bytes: &[u8], side: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let image = if riff::parse(bytes)?.kind == Kind::Animated {
        first_frame(bytes, &LibWebp)?
    } else {
        codec::decode_scaled(bytes, |width, height| {
            crate::thumb::fit(width, height, side)
        })?
    };
    Ok((image.width, image.height, image.pixels))
}

/// The first frame of an animated WebP, as a full canvas: the first that
/// `codec`'s animation decoder gives. Only that frame is decoded.
fn first_frame(bytes: &[u8], codec: &impl Codec) -> Result<Rgba, String> {
    let (info, mut frames) = codec.decode_animation(bytes)?;
    let frame = frames.next().unwrap_or_else(|| {
        Err("could not read the WebP data: the animation has no frame".to_owned())
    })?;
    Ok(Rgba {
        width: info.width,
        height: info.height,
        pixels: frame.rgba,
    })
}

/// The fixture helpers of the integration tests, shared with the unit tests.
/// They call libwebp's encoder themselves, hence the `unsafe`, so a fixture
/// does not come from the code under test ([`codec`]).
#[cfg(test)]
#[path = "../tests/common/webp_fixtures.rs"]
#[allow(unsafe_code)]
mod fixtures;

/// Optimise WebP bytes losslessly: check the file, strip its metadata and
/// return the smaller of the result and the source.
///
/// The content must be WebP data, the chunks must be well-formed and the
/// declared image must be within [`crate::MAX_PIXELS`]. The stripped file
/// ([`riff::WebpFile::stripped`]) drops `XMP `, unknown chunks, any bytes after
/// the RIFF size and `EXIF`, unless that EXIF turns the picture (an
/// Orientation tag that is not 1). It keeps the image, alpha, animation and
/// colour profile byte for byte, so the picture is the same. When that is not
/// smaller than the source the source bytes themselves come back, so a file
/// with nothing to lose is copied as it is.
///
/// The error is a message for the user, from [`riff::parse`].
pub(crate) fn optimize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let file = riff::parse(bytes)?;
    let candidates = [file.stripped()];
    Ok(crate::smallest(bytes, &candidates).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use fixtures as fx;

    /// A real lossless WebP from `image`'s encoder.
    fn real_lossless() -> Vec<u8> {
        let pixels: Vec<u8> = (0..16 * 16 * 4).map(|i| (i * 7 % 251) as u8).collect();
        let mut out = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut out)
            .encode(&pixels, 16, 16, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    #[test]
    fn returns_the_original_bytes_after_a_successful_parse() {
        let bytes = real_lossless();
        assert_eq!(optimize(&bytes), Ok(bytes));
    }

    #[test]
    fn passes_on_the_parse_error() {
        assert_eq!(
            optimize(b"RIFF"),
            Err("not a WebP file (the content is not WebP data)".to_owned())
        );
        let mut cut = real_lossless();
        cut.truncate(cut.len() - 3);
        let err = optimize(&cut).unwrap_err();
        assert!(err.starts_with("could not read the WebP data:"), "{err}");
    }

    const SIZE: u32 = 16;

    fn clean() -> Vec<u8> {
        fx::lossless(&fx::photo_rgba(SIZE, SIZE, false), SIZE, SIZE)
    }

    #[test]
    fn returns_the_stripped_file_when_it_is_smaller() {
        let input = fx::with_chunks(&clean(), &[(*b"EXIF", &[9u8; 10_240])]);
        let out = optimize(&input).unwrap();
        assert!(out.len() < input.len() - 10_000, "{} bytes", out.len());
        assert_eq!(out, riff::parse(&input).unwrap().stripped());
        assert_eq!(fx::chunk_names(&out), ["VP8X", "VP8L"]);
    }

    #[test]
    fn returns_the_original_when_stripping_gains_nothing() {
        let input = clean();
        assert_eq!(optimize(&input), Ok(input));
        let with_icc = fx::with_chunks(&clean(), &[(*b"ICCP", &[1u8; 64])]);
        assert_eq!(optimize(&with_icc), Ok(with_icc));
    }

    #[test]
    fn returns_the_original_when_stripping_would_make_the_file_larger() {
        // The last chunk is odd-sized and lacks its pad byte: the stripped
        // file writes it, so it is one byte larger than the source.
        let mut chunks = fx::chunks_of(&clean());
        chunks.push((*b"ICCP", vec![1, 2, 3]));
        let mut input = fx::from_chunks(&chunks);
        input.pop();
        let size = u32::from_le_bytes(input[4..8].try_into().unwrap()) - 1;
        input[4..8].copy_from_slice(&size.to_le_bytes());
        let stripped = riff::parse(&input).unwrap().stripped();
        assert_eq!(stripped.len(), input.len() + 1);

        assert_eq!(optimize(&input), Ok(input));
    }

    // --- thumbnails ---------------------------------------------------------------

    use std::cell::RefCell;

    use codec::{AnimationInfo, Frame, Frames, LibWebp};

    use crate::lossy::Quality;

    /// A codec double for the first frame: it records the bytes it is
    /// handed and opens an animation of `frames`. Nothing else is called.
    struct FramesDouble {
        given: RefCell<Vec<Vec<u8>>>,
        frames: Vec<Result<Frame, String>>,
    }

    impl FramesDouble {
        fn with(frames: Vec<Result<Frame, String>>) -> Self {
            FramesDouble {
                given: RefCell::new(Vec::new()),
                frames,
            }
        }
    }

    impl Codec for FramesDouble {
        fn decode_rgba(&self, _: &[u8]) -> Result<Rgba, String> {
            panic!("not called")
        }

        fn encode_lossy(&self, _: &Rgba, _: Quality) -> Result<Vec<u8>, String> {
            panic!("not called")
        }

        fn decode_animation<'a>(
            &self,
            bytes: &'a [u8],
        ) -> Result<(AnimationInfo, Frames<'a>), String> {
            self.given.borrow_mut().push(bytes.to_vec());
            let info = AnimationInfo {
                width: 2,
                height: 1,
                loop_count: 0,
                background: 0,
                duration_ms: 200,
            };
            Ok((info, Box::new(self.frames.clone().into_iter())))
        }

        fn encode_animation(
            &self,
            _: &AnimationInfo,
            _: &mut dyn Iterator<Item = Result<Frame, String>>,
            _: Quality,
        ) -> Result<Vec<u8>, String> {
            panic!("not called")
        }
    }

    fn frame(rgba: [u8; 8]) -> Result<Frame, String> {
        Ok(Frame {
            rgba: rgba.to_vec(),
            timestamp_ms: 0,
        })
    }

    #[test]
    fn the_first_frame_is_the_first_the_decoder_gives_at_the_canvas_size() {
        let double = FramesDouble::with(vec![frame([1; 8]), frame([2; 8])]);
        let first = first_frame(b"the animation", &double).unwrap();
        assert_eq!(
            first,
            Rgba {
                width: 2,
                height: 1,
                pixels: vec![1; 8]
            }
        );
        assert_eq!(*double.given.borrow(), [b"the animation".to_vec()]);
    }

    #[test]
    fn the_first_frame_fails_when_the_decoder_gives_no_frame_or_an_error() {
        let none = FramesDouble::with(Vec::new());
        assert!(first_frame(b"x", &none).is_err());
        let broken = FramesDouble::with(vec![Err("frame 1 is broken".to_owned()), frame([2; 8])]);
        assert_eq!(first_frame(b"x", &broken).unwrap_err(), "frame 1 is broken");
    }

    #[test]
    fn the_real_first_frame_of_an_animation_is_its_first_canvas() {
        let red = fx::lossless(&[255, 0, 0, 255].repeat(16), 4, 4);
        let blue = fx::lossless(&[0, 0, 255, 255].repeat(16), 4, 4);
        let animation = fx::animated(&[red, blue], 4, 4);
        let first = first_frame(&animation, &LibWebp).unwrap();
        assert_eq!((first.width, first.height), (4, 4));
        assert_eq!(first.pixels, [255, 0, 0, 255].repeat(16));
    }

    #[test]
    fn a_still_webp_thumbnail_is_decoded_scaled_to_fit() {
        let still = fx::lossless(&fx::photo_rgba(400, 200, false), 400, 200);
        let (w, h, rgba) = thumbnail_rgba(&still, 64).unwrap();
        assert_eq!((w, h), (64, 32));
        assert_eq!(rgba.len(), 64 * 32 * 4);
    }

    #[test]
    fn an_animated_webp_thumbnail_is_its_first_frame_at_the_canvas_size() {
        let frames = [
            fx::lossless(&fx::photo_rgba(400, 200, false), 400, 200),
            fx::lossless(&fx::photo_rgba(400, 200, true), 400, 200),
        ];
        let animation = fx::animated(&frames, 400, 200);
        let (w, h, rgba) = thumbnail_rgba(&animation, 64).unwrap();
        assert_eq!((w, h), (400, 200));
        assert_eq!(rgba, fx::photo_rgba(400, 200, false));
    }

    #[test]
    fn a_webp_thumbnail_refuses_what_the_container_check_refuses() {
        assert_eq!(
            thumbnail_rgba(b"RIFF", 64).unwrap_err(),
            "not a WebP file (the content is not WebP data)"
        );
        assert_eq!(check_header(b"RIFF"), thumbnail_rgba(b"RIFF", 64).map(drop));
        assert_eq!(check_header(&clean()), Ok(()));
    }

    #[test]
    fn returns_the_file_without_its_trailing_bytes_when_that_is_all_there_is_to_save() {
        let clean = clean();
        let mut input = clean.clone();
        input.extend_from_slice(&[0xAB; 100]);
        assert_eq!(optimize(&input), Ok(clean));
    }
}

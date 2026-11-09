//! WebP optimisation.
//!
//! The container is walked in safe Rust ([`riff`]); no image is decoded, so
//! nothing here can change a pixel.

mod exif;
mod riff;

/// The fixture helpers of the integration tests, shared with the unit tests.
/// They call libwebp's encoder, hence the `unsafe`; no product code does.
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

    #[test]
    fn returns_the_file_without_its_trailing_bytes_when_that_is_all_there_is_to_save() {
        let clean = clean();
        let mut input = clean.clone();
        input.extend_from_slice(&[0xAB; 100]);
        assert_eq!(optimize(&input), Ok(clean));
    }
}

//! WebP optimisation.
//!
//! The container is walked in safe Rust ([`riff`]); no image is decoded.

mod riff;

/// Optimise WebP bytes losslessly. For now it only checks the file: the
/// content must be WebP data, the chunks must be well-formed and the declared
/// image must be within [`crate::MAX_PIXELS`]. The original bytes come back
/// unchanged.
///
/// The error is a message for the user, from [`riff::parse`].
pub(crate) fn optimize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    riff::parse(bytes)?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

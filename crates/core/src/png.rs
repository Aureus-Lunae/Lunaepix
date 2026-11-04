//! PNG optimisation with oxipng.

/// The eight bytes every PNG file starts with.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Optimise PNG bytes losslessly. The settings are today's
/// `oxipng -o 4 --strip safe`, as chosen in the Step 2 evaluation (which
/// measured byte-identical output to that command). If oxipng cannot make the
/// file smaller it returns the input unchanged.
///
/// The error is a message for the user: the content is not a PNG, or oxipng
/// could not decode it (truncated or corrupt data).
pub(crate) fn optimize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&SIGNATURE) {
        return Err("not a PNG file (the content is not PNG data)".to_owned());
    }
    // Refuse a decompression bomb before oxipng allocates for it: the size
    // comes from the header, never from decoding.
    if let Some((width, height)) = ihdr_dimensions(bytes) {
        crate::plan::check_pixels(width, height)?;
    }
    let mut options = oxipng::Options::from_preset(4);
    options.strip = oxipng::StripChunks::Safe;
    oxipng::optimize_from_memory(bytes, &options)
        .map_err(|error| format!("could not read the PNG data: {error}"))
}

/// Width and height from the IHDR chunk, which a PNG must have first, right
/// after the signature: length 13, type `IHDR`, then the big-endian width
/// (bytes 16..20) and height (20..24). `None` when there is no such chunk;
/// oxipng then reports the file as corrupt.
fn ihdr_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.get(12..16)? != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    Some((width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A signature and an IHDR chunk (CRC not checked here) for `w`x`h`.
    fn header(w: u32, h: u32) -> Vec<u8> {
        let mut bytes = SIGNATURE.to_vec();
        bytes.extend_from_slice(&13u32.to_be_bytes());
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&w.to_be_bytes());
        bytes.extend_from_slice(&h.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0, 0, 0, 0, 0]);
        bytes
    }

    #[test]
    fn reads_the_dimensions_from_the_ihdr() {
        assert_eq!(ihdr_dimensions(&header(640, 480)), Some((640, 480)));
        assert_eq!(
            ihdr_dimensions(&header(65_535, 1)),
            Some((65_535, 1)),
            "width and height must not be swapped"
        );
    }

    #[test]
    fn reads_no_dimensions_when_the_first_chunk_is_not_an_ihdr() {
        let mut other = header(640, 480);
        other[12..16].copy_from_slice(b"IDAT");
        assert_eq!(ihdr_dimensions(&other), None);
        assert_eq!(ihdr_dimensions(&header(640, 480)[..23]), None);
        assert_eq!(ihdr_dimensions(&SIGNATURE), None);
    }

    #[test]
    fn refuses_a_png_that_declares_too_many_pixels_before_oxipng_runs() {
        let err = optimize(&header(65_535, 65_535)).unwrap_err();
        assert!(err.contains("image too large"), "{err}");
    }

    #[test]
    fn passes_a_png_within_the_pixel_limit_on_to_oxipng() {
        // The header alone is not a whole PNG: oxipng, not the size check,
        // refuses it.
        let err = optimize(&header(640, 480)).unwrap_err();
        assert!(!err.contains("image too large"), "{err}");
        assert!(err.contains("could not read the PNG data"), "{err}");
    }
}

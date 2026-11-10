//! PNG optimisation with oxipng.

use crate::plan::{MAX_PIXELS, check_pixels};

mod lossy;

#[expect(
    unused_imports,
    reason = "the worker calls it once a run can be lossy (a later task)"
)]
pub(crate) use lossy::optimize_lossy;

/// The eight bytes every PNG file starts with.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The most raw (inflated) image data oxipng may decode, in bytes: 1 GB, the
/// raw size of the worst image within [`MAX_PIXELS`].
///
/// Derivation. oxipng's `IhdrData::raw_data_size` is the image's bytes plus
/// one filter byte per row. The widest pixel is 16-bit RGBA, 8 bytes, so the
/// image's bytes are at most `8 * MAX_PIXELS`. (For narrower pixels each row
/// rounds up to whole bytes, but that never exceeds 8 bytes per pixel.) Without
/// interlacing there are `height <= MAX_PIXELS` rows; with Adam7 the seven
/// passes have at most `1.875 * height + 7` rows together. So the raw size is
/// at most `8 * MAX_PIXELS + 1.875 * MAX_PIXELS + 7`, which is below
/// `10 * MAX_PIXELS`.
///
/// This bounds memory, not the pixel count. A 1-bit image of 8 gigapixels
/// also fits in 1 GB. The pixel count is [`check_chunks`]'s job; this limit is
/// the layer that holds whichever IHDR oxipng ends up using.
const MAX_DECOMPRESSED_BYTES: usize = 10 * MAX_PIXELS as usize;

/// Optimise PNG bytes losslessly. The settings are today's
/// `oxipng -o 4 --strip safe`, as chosen in the Step 2 evaluation (which
/// measured byte-identical output to that command). If oxipng cannot make the
/// file smaller it returns the input unchanged.
///
/// A decompression bomb (a small file that declares a huge image) is refused
/// twice over, without decoding anything: [`check_chunks`] checks the one
/// IHDR a valid PNG has, and oxipng itself refuses to inflate more than
/// [`MAX_DECOMPRESSED_BYTES`] for the IHDR it actually uses.
///
/// The error is a message for the user: the content is not a PNG, the image
/// is too large, or oxipng could not decode it (truncated or corrupt data).
pub(crate) fn optimize(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&SIGNATURE) {
        return Err("not a PNG file (the content is not PNG data)".to_owned());
    }
    check_chunks(bytes)?;
    run_oxipng(bytes)
}

/// One chunk header of a PNG file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkHeader {
    /// The offset of the chunk's length field in the file.
    at: usize,
    /// The declared length of the chunk's data.
    length: u32,
    kind: [u8; 4],
}

impl ChunkHeader {
    /// The whole chunk (length, type, data and CRC), if the file holds all
    /// of it.
    fn whole<'a>(&self, bytes: &'a [u8]) -> Option<&'a [u8]> {
        // Length, type and CRC are 12 bytes; `usize` holds any `u32`.
        let end = (self.at + 12).checked_add(self.length as usize)?;
        bytes.get(self.at..end)
    }
}

/// The chunk headers after the signature, in order, as oxipng reads them
/// (length and type only; the data is not read). The walk ends after IEND,
/// and also after a chunk that runs past the end of the file (that chunk is
/// still yielded, so its type is seen) or at a header cut short.
fn chunk_headers(bytes: &[u8]) -> impl Iterator<Item = ChunkHeader> + '_ {
    let mut next = Some(SIGNATURE.len());
    std::iter::from_fn(move || {
        let at = next?;
        let header = bytes.get(at..at + 8)?;
        let chunk = ChunkHeader {
            at,
            length: u32::from_be_bytes([header[0], header[1], header[2], header[3]]),
            kind: [header[4], header[5], header[6], header[7]],
        };
        next = match chunk.whole(bytes) {
            Some(whole) if &chunk.kind != b"IEND" => Some(at + whole.len()),
            _ => None,
        };
        Some(chunk)
    })
}

/// What [`check_chunks`] found besides the checks it passed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkCheck {
    /// The file has an `acTL` chunk: it is an animated PNG.
    has_actl: bool,
}

/// Walk every chunk header ([`chunk_headers`]) up to IEND. A PNG must have
/// exactly one IHDR, as its first chunk, and its declared size must be
/// within [`MAX_PIXELS`]. oxipng is more lenient: it accepts an IHDR anywhere
/// and keeps the last one. So a file that breaks the rule is refused here,
/// before a later IHDR can slip a huge size past the check.
///
/// The walk stops at IEND, and also at a chunk that runs past the end of the
/// file (or a header cut short), leaving that truncation to oxipng to report.
/// It also reports whether the file has an `acTL` chunk.
fn check_chunks(bytes: &[u8]) -> Result<ChunkCheck, String> {
    let mut seen_ihdr = false;
    let mut has_actl = false;
    for chunk in chunk_headers(bytes) {
        if &chunk.kind == b"IHDR" {
            if seen_ihdr {
                return Err(
                    "could not read the PNG data: it has more than one IHDR chunk".to_owned(),
                );
            }
            seen_ihdr = true;
            if let Some(size) = bytes.get(chunk.at + 8..chunk.at + 16) {
                let width = u32::from_be_bytes([size[0], size[1], size[2], size[3]]);
                let height = u32::from_be_bytes([size[4], size[5], size[6], size[7]]);
                check_pixels(width, height)?;
            }
        } else if !seen_ihdr {
            return Err("could not read the PNG data: the first chunk is not IHDR".to_owned());
        }
        has_actl |= &chunk.kind == b"acTL";
    }
    Ok(ChunkCheck { has_actl })
}

/// Run oxipng with the Step 2 settings and the decompressed-size limit.
fn run_oxipng(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut options = oxipng::Options::from_preset(4);
    options.strip = oxipng::StripChunks::Safe;
    // oxipng checks this against the IHDR it uses, before it inflates.
    options.max_decompressed_size = Some(MAX_DECOMPRESSED_BYTES);
    oxipng::optimize_from_memory(bytes, &options).map_err(|error| match error {
        // oxipng raises the same error when the stream overflows a small
        // IHDR's size (a corrupt file); only our own limit is "too large".
        oxipng::PngError::InflatedDataTooLong(max) if max == MAX_DECOMPRESSED_BYTES => format!(
            "image too large: its pixel data would exceed the {}-megapixel limit",
            MAX_PIXELS / 1_000_000
        ),
        other => format!("could not read the PNG data: {other}"),
    })
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

    /// One chunk with a valid CRC (oxipng checks them).
    pub(super) fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = (data.len() as u32).to_be_bytes().to_vec();
        let mut body = kind.to_vec();
        body.extend_from_slice(data);
        out.extend_from_slice(&body);
        out.extend_from_slice(&crc32fast::hash(&body).to_be_bytes());
        out
    }

    pub(super) fn ihdr(w: u32, h: u32, depth: u8, colour: u8) -> Vec<u8> {
        let mut data = w.to_be_bytes().to_vec();
        data.extend_from_slice(&h.to_be_bytes());
        data.extend_from_slice(&[depth, colour, 0, 0, 0]);
        chunk(b"IHDR", &data)
    }

    /// A zlib stream of one empty stored block.
    pub(super) const TOKEN_IDAT: [u8; 11] = [
        0x78, 0x01, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0x00, 0x00, 0x00, 0x01,
    ];

    /// Signature, the given chunks, IEND.
    pub(super) fn file(chunks: &[Vec<u8>]) -> Vec<u8> {
        let mut bytes = SIGNATURE.to_vec();
        for c in chunks {
            bytes.extend_from_slice(c);
        }
        bytes.extend(chunk(b"IEND", &[]));
        bytes
    }

    /// A real 32x32 greyscale PNG from the `png` encoder.
    fn real_png() -> Vec<u8> {
        let mut out = Vec::new();
        let mut encoder = png::Encoder::new(&mut out, 32, 32);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[77; 32 * 32]).unwrap();
        writer.finish().unwrap();
        out
    }

    /// What `check_chunks` reports for a PNG without `acTL`.
    const STILL: ChunkCheck = ChunkCheck { has_actl: false };

    /// An `acTL` chunk: 2 frames, played forever.
    fn actl() -> Vec<u8> {
        chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0])
    }

    // --- the chunk walk -----------------------------------------------------------

    #[test]
    fn check_chunks_reports_an_actl_chunk() {
        let png = file(&[ihdr(16, 16, 8, 0), actl(), chunk(b"IDAT", &TOKEN_IDAT)]);
        assert_eq!(check_chunks(&png), Ok(ChunkCheck { has_actl: true }));
        // Also when it is not the first chunk after IHDR.
        let png = file(&[
            ihdr(16, 16, 8, 0),
            chunk(b"tEXt", b"Comment\0x"),
            actl(),
            chunk(b"IDAT", &TOKEN_IDAT),
        ]);
        assert_eq!(check_chunks(&png), Ok(ChunkCheck { has_actl: true }));
    }

    #[test]
    fn check_chunks_reports_no_actl_for_a_still_png() {
        // A chunk type that only resembles acTL (another letter case, or the
        // name as data inside another chunk) is not one, and nothing after
        // IEND is read.
        let mut png = file(&[
            ihdr(16, 16, 8, 0),
            chunk(b"tEXt", b"acTL\0acTL"),
            chunk(b"aCTL", &[0, 0, 0, 2, 0, 0, 0, 0]),
            chunk(b"IDAT", &TOKEN_IDAT),
        ]);
        png.extend(actl());
        assert_eq!(check_chunks(&png), Ok(STILL));
    }

    #[test]
    fn accepts_an_ihdr_that_comes_first_and_only_once() {
        let png = file(&[
            ihdr(640, 480, 8, 6),
            chunk(b"tEXt", b"Comment\0x"),
            chunk(b"IDAT", &TOKEN_IDAT),
        ]);
        assert_eq!(check_chunks(&png), Ok(STILL));
        assert_eq!(check_chunks(&real_png()), Ok(STILL));
    }

    #[test]
    fn refuses_a_first_chunk_that_is_not_an_ihdr() {
        let png = file(&[chunk(b"tEXt", b"Comment\0x"), ihdr(640, 480, 8, 6)]);
        let err = check_chunks(&png).unwrap_err();
        assert!(err.contains("first chunk is not IHDR"), "{err}");
    }

    #[test]
    fn refuses_a_second_ihdr() {
        let png = file(&[ihdr(1, 1, 8, 0), ihdr(2, 2, 8, 0)]);
        let err = check_chunks(&png).unwrap_err();
        assert!(err.contains("more than one IHDR"), "{err}");
    }

    #[test]
    fn ignores_what_follows_iend_as_oxipng_does() {
        // The counterpart of the test above: after IEND nothing is read, by
        // oxipng or by the walk, so an IHDR there is not a second one.
        let mut png = file(&[ihdr(1, 1, 8, 0), chunk(b"IDAT", &TOKEN_IDAT)]);
        png.extend(ihdr(65_535, 65_535, 8, 6));
        assert_eq!(check_chunks(&png), Ok(STILL));
    }

    #[test]
    fn refuses_an_ihdr_that_declares_too_many_pixels() {
        let png = file(&[ihdr(65_535, 65_535, 8, 6)]);
        let err = check_chunks(&png).unwrap_err();
        assert_eq!(err, "image too large: 65535×65535 exceeds 100 megapixels");
    }

    #[test]
    fn stops_at_a_chunk_that_runs_past_the_end_of_the_file() {
        // A length of u32::MAX must neither overflow nor read out of bounds;
        // the truncation is oxipng's to report.
        let mut png = SIGNATURE.to_vec();
        png.extend(ihdr(16, 16, 8, 0));
        png.extend_from_slice(&u32::MAX.to_be_bytes());
        png.extend_from_slice(b"IDAT");
        png.extend_from_slice(&[0; 16]);
        assert_eq!(check_chunks(&png), Ok(STILL));
        let err = optimize(&png).unwrap_err();
        assert!(err.contains("could not read the PNG data"), "{err}");
    }

    #[test]
    fn leaves_a_file_without_a_whole_chunk_header_to_oxipng() {
        let mut png = SIGNATURE.to_vec();
        png.extend_from_slice(&[0, 0, 0]);
        assert_eq!(check_chunks(&png), Ok(STILL));
        assert!(optimize(&png).is_err());
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

    // --- oxipng's own limit, on its own -----------------------------------------------

    #[test]
    fn oxipng_refuses_a_last_ihdr_over_the_decompressed_size_limit() {
        // Bypasses the chunk walk: oxipng keeps the last IHDR (65535x65535
        // RGBA, 17 GB of raw data) and must refuse it before inflating.
        let bomb = file(&[
            ihdr(1, 1, 8, 0),
            ihdr(65_535, 65_535, 8, 6),
            chunk(b"IDAT", &TOKEN_IDAT),
        ]);
        let started = std::time::Instant::now();
        let err = run_oxipng(&bomb).unwrap_err();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(err.starts_with("image too large"), "{err}");
    }

    #[test]
    fn oxipng_still_optimises_a_normal_png_under_the_decompressed_size_limit() {
        assert!(run_oxipng(&real_png()).is_ok());
    }

    #[test]
    fn oxipng_reports_an_idat_longer_than_its_ihdr_as_unreadable_not_too_large() {
        // oxipng raises the same error kind when the stream overflows the
        // IHDR's size; only our own limit may read as "image too large".
        // A zlib stream of one stored block of 1024 zeros (LEN 0x0400, NLEN
        // 0xFBFF), whose Adler-32 is 0x0400_0001; the 1x1 IHDR holds 2 bytes.
        let mut stream = vec![0x78, 0x01, 0x01, 0x00, 0x04, 0xFF, 0xFB];
        stream.extend_from_slice(&[0; 1024]);
        stream.extend_from_slice(&0x0400_0001u32.to_be_bytes());
        let png = file(&[ihdr(1, 1, 8, 0), chunk(b"IDAT", &stream)]);
        let err = run_oxipng(&png).unwrap_err();
        assert!(!err.contains("image too large"), "{err}");
        assert!(err.contains("could not read the PNG data"), "{err}");
    }

    #[test]
    fn the_decompressed_size_limit_covers_16_bit_rgba_at_the_pixel_limit() {
        // 8 bytes per pixel, plus at most 1.875 rows' filter bytes per row
        // of the image (Adam7) and 7 rounding rows; the height is at most
        // MAX_PIXELS (one pixel wide).
        let worst = 8 * MAX_PIXELS + MAX_PIXELS * 15 / 8 + 7;
        assert!(MAX_DECOMPRESSED_BYTES as u64 >= worst);
    }
}

//! The probe: what a file would show in Single image mode, from its header.
//!
//! Pure: [`probe`] reads the file once, checks it and gives the size it is
//! shown at, or the reason Batch would give for refusing it. It decodes no
//! image data, so it is quick on a photo near the pixel limit, and it runs in
//! a worker process ([`crate::worker`], [`crate::run_probe`]) for the same
//! reason the optimiser and the thumbnail do.
//!
//! The checks are the optimiser's, made with the optimiser's own functions
//! and so with its messages:
//! - the planner's: the extension picks the format ([`Skip::UnsupportedType`]),
//!   a regular file ([`Skip::NotAFile`]) of at most [`MAX_FILE_BYTES`]
//!   ([`Skip::TooLarge`]), also when it grew while it was read;
//! - a PNG, JPG or WebP header: the signature, the chunks or the frame
//!   header, and the declared size within [`crate::MAX_PIXELS`]
//!   ([`decode::header_size`]);
//! - an SVG: UTF-8, the entity bound, well-formed XML within the node limit,
//!   an `<svg>` root and the depth limit ([`svg::check`]).
//!
//! What only a lossy run refuses (a WebP side over 16383) is not checked:
//! the probe does not know the run's setting, and the compression step gives
//! that reason.

use std::path::Path;

use crate::plan::{self, Format};
use crate::thumb::decode;
use crate::thumb::{SourceError, read_regular};
use crate::{MAX_FILE_BYTES, Skip, svg};

/// The size a file is shown at, in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// `None` for an SVG, which has no pixel size.
    pub width: Option<u32>,
    /// `None` for an SVG, which has no pixel size.
    pub height: Option<u32>,
}

/// The size `src` is shown at, or Batch's reason for refusing it.
///
/// The size is the one shown: a JPG or WebP whose EXIF Orientation is 5 to
/// 8 (turned a quarter) gives its width and height swapped, an animated PNG
/// or WebP gives its canvas, and an SVG gives none. A PNG is never turned.
/// `src` is opened only when it is a regular file, so a FIFO is never
/// opened.
pub fn probe(src: &Path) -> Result<Probe, String> {
    let format = plan::format_of(src).ok_or_else(|| Skip::UnsupportedType.to_string())?;
    let bytes = read_regular(src, MAX_FILE_BYTES).map_err(|error| match error {
        // As the planner says it for a path it cannot look at or use.
        SourceError::NotThere(_) | SourceError::NotRegular => Skip::NotAFile.to_string(),
        SourceError::TooLarge => Skip::TooLarge.to_string(),
        // As the optimiser says it for a file it cannot open or read.
        SourceError::Unreadable(cause) => cause.to_string(),
    })?;
    probe_bytes(format, &bytes)
}

/// [`probe`] of the content of a file of `format`.
pub(crate) fn probe_bytes(format: Format, bytes: &[u8]) -> Result<Probe, String> {
    if format == Format::Svg {
        svg::check(bytes)?;
        return Ok(Probe {
            width: None,
            height: None,
        });
    }
    let (width, height) = decode::header_size(format, bytes)?;
    let (width, height) = if turns_a_quarter(decode::orientation(format, bytes)) {
        (height, width)
    } else {
        (width, height)
    };
    Ok(Probe {
        width: Some(width),
        height: Some(height),
    })
}

/// Whether an EXIF Orientation turns the picture a quarter, so that its
/// shown width is the stored height: 5 to 8. 1 to 4 only flip it or turn it
/// half way; anything else is no orientation.
pub(crate) fn turns_a_quarter(orientation: Option<u8>) -> bool {
    matches!(orientation, Some(5..=8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Format, check_pixels};

    #[test]
    fn only_orientations_5_to_8_turn_the_picture_a_quarter() {
        for value in 5..=8 {
            assert!(turns_a_quarter(Some(value)), "{value}");
        }
        for value in [1, 2, 3, 4] {
            assert!(!turns_a_quarter(Some(value)), "{value}");
        }
        // A value outside 1 to 8, and no orientation at all, leave it alone.
        for value in [0, 9, 255] {
            assert!(!turns_a_quarter(Some(value)), "{value}");
        }
        assert!(!turns_a_quarter(None));
    }

    #[test]
    fn the_over_max_pixels_message_is_check_pixels_message() {
        let png = crate::png::tests::file(&[crate::png::tests::ihdr(10_000, 10_001, 8, 6)]);
        assert_eq!(
            probe_bytes(Format::Png, &png),
            Err(check_pixels(10_000, 10_001).unwrap_err())
        );
        // At the limit the same call answers.
        let at = crate::png::tests::file(&[crate::png::tests::ihdr(10_000, 10_000, 8, 6)]);
        assert_eq!(
            probe_bytes(Format::Png, &at),
            Ok(Probe {
                width: Some(10_000),
                height: Some(10_000)
            })
        );
    }

    #[test]
    fn an_svg_has_no_size_and_a_raster_has_one() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg"/>"#;
        assert_eq!(
            probe_bytes(Format::Svg, svg),
            Ok(Probe {
                width: None,
                height: None
            })
        );
        let png = crate::png::tests::file(&[crate::png::tests::ihdr(7, 3, 8, 6)]);
        assert_eq!(
            probe_bytes(Format::Png, &png),
            Ok(Probe {
                width: Some(7),
                height: Some(3)
            })
        );
        // Not an SVG: refused, not answered with no size.
        assert!(probe_bytes(Format::Svg, b"plain text").is_err());
    }
}

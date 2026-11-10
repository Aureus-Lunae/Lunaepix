//! The shared core of lossy compression: the quality setting, the notes shown
//! to the user, the shape check and the "never larger than the original" rule.
//!
//! The format modules (JPG, PNG, WebP) build on these; nothing here knows a
//! format.

use std::fmt;

/// A lossy quality, an integer from 0 to 100. A higher number keeps more
/// detail and gives a larger file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quality(u8);

/// The quality a run starts with.
pub const DEFAULT_QUALITY: Quality = Quality(75);

impl Quality {
    /// The highest quality.
    const MAX: u8 = 100;

    /// `None` above 100.
    pub fn new(value: u8) -> Option<Quality> {
        (value <= Self::MAX).then_some(Quality(value))
    }

    /// Reads a quality from text: ASCII digits only, so no sign, fraction or
    /// space, and 0 to 100. Anything else, including a number too long for
    /// any integer type, is `None`.
    pub fn parse(text: &str) -> Option<Quality> {
        if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        // Only digits remain, so an error here can only be an overflow.
        text.parse::<u8>().ok().and_then(Quality::new)
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// Why a lossy run kept a file's bytes as they were. The `Display` text is
/// shown to the user; the code travels in the worker's record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// An animated PNG is not compressed lossily.
    KeptLosslessAnimatedPng,
    /// A CMYK JPG is not compressed lossily.
    KeptLosslessCmyk,
    /// An animation over the frame or canvas limit got the lossless treatment.
    KeptLosslessAnimationTooLarge,
    /// The lossy result was not smaller, so the original bytes were written.
    OriginalKept,
}

impl Note {
    /// The short code in the worker's record.
    pub fn code(self) -> &'static str {
        match self {
            Note::KeptLosslessAnimatedPng => "apng",
            Note::KeptLosslessCmyk => "cmyk",
            Note::KeptLosslessAnimationTooLarge => "anim-too-large",
            Note::OriginalKept => "original-kept",
        }
    }

    /// The note for a [`code`](Note::code); `None` for any other text.
    pub fn from_code(code: &str) -> Option<Note> {
        [
            Note::KeptLosslessAnimatedPng,
            Note::KeptLosslessCmyk,
            Note::KeptLosslessAnimationTooLarge,
            Note::OriginalKept,
        ]
        .into_iter()
        .find(|note| note.code() == code)
    }
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Note::KeptLosslessAnimatedPng => "kept lossless: animated PNG",
            Note::KeptLosslessCmyk => "kept lossless: CMYK JPG",
            Note::KeptLosslessAnimationTooLarge => "kept lossless: animation too large for lossy",
            Note::OriginalKept => "original kept: lossy was not smaller",
        })
    }
}

/// What a lossy result must keep of its source: the size, and for an
/// animation the total duration. Frame count is deliberately not compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Shape {
    pub(crate) width: u32,
    pub(crate) height: u32,
    /// `Some(total)` for an animation only.
    pub(crate) duration_ms: Option<u64>,
}

/// Whether `output` has the shape of `source`. On a mismatch the message
/// names what differs.
pub(crate) fn check_shape(source: Shape, output: Shape) -> Result<(), String> {
    match (source.duration_ms, output.duration_ms) {
        (Some(_), None) => {
            return Err(
                "the lossy result is not an animation, but the source is an animation".to_owned(),
            );
        }
        (None, Some(_)) => {
            return Err(
                "the lossy result is an animation, but the source is not an animation".to_owned(),
            );
        }
        _ => {}
    }
    if (source.width, source.height) != (output.width, output.height) {
        return Err(format!(
            "the lossy result has a different size ({}×{}, expected {}×{})",
            output.width, output.height, source.width, source.height
        ));
    }
    if let (Some(expected), Some(found)) = (source.duration_ms, output.duration_ms)
        && expected != found
    {
        return Err(format!(
            "the lossy result has a different duration ({found} ms, expected {expected} ms)"
        ));
    }
    Ok(())
}

/// The bytes a lossy run writes, and the note to show for them, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Lossy {
    pub(crate) bytes: Vec<u8>,
    pub(crate) note: Option<Note>,
}

/// The never-larger rule: the candidate when it is strictly smaller than the
/// original, otherwise the original bytes with [`Note::OriginalKept`].
pub(crate) fn finish(original: &[u8], candidate: Vec<u8>) -> Lossy {
    if candidate.len() < original.len() {
        Lossy {
            bytes: candidate,
            note: None,
        }
    } else {
        Lossy {
            bytes: original.to_vec(),
            note: Some(Note::OriginalKept),
        }
    }
}

/// A file given the lossless treatment in a lossy run, with the reason.
pub(crate) fn kept_lossless(bytes: Vec<u8>, note: Note) -> Lossy {
    Lossy {
        bytes,
        note: Some(note),
    }
}

/// A zeroed pixel buffer of `len` bytes, or an error when that memory cannot
/// be had (instead of aborting the process).
pub(crate) fn reserve_pixels(len: usize) -> Result<Vec<u8>, String> {
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(len)
        .map_err(|_| "not enough memory for the pixels".to_owned())?;
    pixels.resize(len, 0);
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- quality -------------------------------------------------------------

    #[test]
    fn quality_accepts_0_75_and_100() {
        for value in [0u8, 1, 75, 99, 100] {
            assert_eq!(Quality::new(value).map(Quality::get), Some(value));
            assert_eq!(
                Quality::parse(&value.to_string()).map(Quality::get),
                Some(value)
            );
        }
        assert_eq!(DEFAULT_QUALITY.get(), 75);
        assert_eq!(Quality::parse("075").map(Quality::get), Some(75));
    }

    #[test]
    fn quality_rejects_101_and_text_signs_fractions_and_spaces() {
        assert_eq!(Quality::new(101), None);
        assert_eq!(Quality::new(255), None);
        for text in [
            "101", "-1", "+5", "7.5", "abc", "", " 75", "75 ", "1000", "7 5", "٧٥", "0x10", "1e1",
            "-0",
        ] {
            assert_eq!(Quality::parse(text), None, "{text:?}");
        }
        // A number too long for any integer type is rejected, not wrapped.
        assert_eq!(Quality::parse("99999999999999999999999999"), None);
    }

    // --- notes ---------------------------------------------------------------

    const NOTES: [(Note, &str, &str); 4] = [
        (
            Note::KeptLosslessAnimatedPng,
            "apng",
            "kept lossless: animated PNG",
        ),
        (Note::KeptLosslessCmyk, "cmyk", "kept lossless: CMYK JPG"),
        (
            Note::KeptLosslessAnimationTooLarge,
            "anim-too-large",
            "kept lossless: animation too large for lossy",
        ),
        (
            Note::OriginalKept,
            "original-kept",
            "original kept: lossy was not smaller",
        ),
    ];

    #[test]
    fn notes_read_as_the_exact_sentences() {
        for (note, _, sentence) in NOTES {
            assert_eq!(note.to_string(), sentence);
        }
    }

    #[test]
    fn note_codes_round_trip() {
        for (note, code, _) in NOTES {
            assert_eq!(note.code(), code);
            assert_eq!(Note::from_code(code), Some(note));
        }
    }

    #[test]
    fn an_unknown_note_code_is_none() {
        // A sentence is not a code, and the match is exact.
        for code in [
            "",
            "APNG",
            "apng ",
            "kept lossless: animated PNG",
            "none",
            "x",
        ] {
            assert_eq!(Note::from_code(code), None, "{code:?}");
        }
    }

    // --- shape ---------------------------------------------------------------

    fn still(width: u32, height: u32) -> Shape {
        Shape {
            width,
            height,
            duration_ms: None,
        }
    }

    fn animation(width: u32, height: u32, duration_ms: u64) -> Shape {
        Shape {
            width,
            height,
            duration_ms: Some(duration_ms),
        }
    }

    #[test]
    fn check_shape_accepts_the_same_size_and_duration() {
        assert_eq!(check_shape(still(100, 90), still(100, 90)), Ok(()));
        assert_eq!(
            check_shape(animation(100, 90, 1200), animation(100, 90, 1200)),
            Ok(())
        );
    }

    #[test]
    fn check_shape_rejects_another_width_height_or_duration() {
        let error = check_shape(still(100, 90), still(100, 80)).unwrap_err();
        assert_eq!(
            error,
            "the lossy result has a different size (100×80, expected 100×90)"
        );
        let error = check_shape(still(100, 90), still(99, 90)).unwrap_err();
        assert!(
            error.contains("99×90") && error.contains("100×90"),
            "{error}"
        );
        let error = check_shape(animation(100, 90, 1200), animation(100, 90, 1180)).unwrap_err();
        assert!(
            error.contains("duration") && error.contains("1180") && error.contains("1200"),
            "{error}"
        );
        // A size mismatch is reported even when the durations also differ.
        let error = check_shape(animation(100, 90, 1200), animation(50, 90, 900)).unwrap_err();
        assert!(error.contains("size"), "{error}");
    }

    #[test]
    fn check_shape_rejects_a_still_output_for_an_animation() {
        let error = check_shape(animation(100, 90, 1200), still(100, 90)).unwrap_err();
        assert!(error.contains("animation"), "{error}");
        let error = check_shape(still(100, 90), animation(100, 90, 1200)).unwrap_err();
        assert!(error.contains("animation"), "{error}");
    }

    // --- the never-larger rule -----------------------------------------------

    #[test]
    fn finish_keeps_a_smaller_candidate_without_note() {
        let original = vec![1u8; 10];
        let candidate = vec![2u8; 9];
        let lossy = finish(&original, candidate.clone());
        assert_eq!(lossy.bytes, candidate);
        assert_eq!(lossy.note, None);
        // Even a single byte smaller counts, and an empty candidate is smaller.
        assert_eq!(finish(&original, vec![]).note, None);
    }

    #[test]
    fn finish_keeps_the_original_with_the_note_when_the_candidate_is_equal_or_larger() {
        let original = vec![1u8; 10];
        for candidate in [vec![2u8; 10], vec![2u8; 11], vec![2u8; 5000]] {
            let lossy = finish(&original, candidate);
            assert_eq!(lossy.bytes, original);
            assert_eq!(lossy.note, Some(Note::OriginalKept));
        }
    }

    // --- pixel buffers ----------------------------------------------------------

    #[test]
    fn reserving_an_impossible_pixel_buffer_fails_with_a_reason() {
        let err = reserve_pixels(usize::MAX).unwrap_err();
        assert_eq!(err, "not enough memory for the pixels");
        assert_eq!(reserve_pixels(12).unwrap(), vec![0; 12]);
    }

    #[test]
    fn kept_lossless_carries_the_bytes_and_the_given_note() {
        let lossy = kept_lossless(vec![7, 8, 9], Note::KeptLosslessCmyk);
        assert_eq!(lossy.bytes, vec![7, 8, 9]);
        assert_eq!(lossy.note, Some(Note::KeptLosslessCmyk));
        let other = kept_lossless(vec![], Note::KeptLosslessAnimatedPng);
        assert_eq!(other.note, Some(Note::KeptLosslessAnimatedPng));
    }
}

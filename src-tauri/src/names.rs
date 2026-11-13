//! The rules for the name typed in Save as.
//!
//! The window checks the same rules in `ui/names.js`, so a name is refused
//! before Save is pressed; this copy is the one that decides, because the
//! window is not trusted. Both are tested against one fixture,
//! `tests/fixtures/save-names.json`, so they cannot drift apart. The checks run
//! in the order of [`NameProblem`], and the first that fails is the one
//! reported.

/// Most characters (Unicode code points) in a whole name, extension included.
pub const MAX_NAME_CHARS: usize = 200;

/// Most bytes (UTF-8) in a whole name, extension included: Linux allows 255
/// bytes per file name, and a name of 63 emoji is only 67 characters.
pub const MAX_NAME_BYTES: usize = 255;

/// What is wrong with a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    /// Nothing, or only white space, was typed.
    Empty,
    /// One of `/ \ : * ? " < > |`, or a control character (U+0000 to U+001F,
    /// U+007F).
    ForbiddenCharacter,
    /// The name ends in a space or a dot, which Windows drops.
    TrailingSpaceOrDot,
    /// A device name Windows does not allow as a file name: `con`, `prn`,
    /// `aux`, `nul`, `com1` to `com9` and `com¹ ² ³`, `lpt1` to `lpt9` and
    /// `lpt¹ ² ³`, with or without an extension, in any case.
    Reserved,
    /// More than [`MAX_NAME_CHARS`] characters or [`MAX_NAME_BYTES`] bytes.
    TooLong,
}

/// Whether the name `stem` (what the user typed) saved with `extension` (the
/// original's, with its dot) is allowed. Forbidden characters and a trailing
/// space or dot are looked for in the stem; the device names and the length are
/// checked on the whole name.
pub fn check_save_name(stem: &str, extension: &str) -> Result<(), NameProblem> {
    if stem.chars().all(is_blank) {
        return Err(NameProblem::Empty);
    }
    if stem.chars().any(is_forbidden) {
        return Err(NameProblem::ForbiddenCharacter);
    }
    if stem.ends_with([' ', '.']) {
        return Err(NameProblem::TrailingSpaceOrDot);
    }
    let full = format!("{stem}{extension}");
    if is_reserved(&full) {
        return Err(NameProblem::Reserved);
    }
    if full.chars().count() > MAX_NAME_CHARS || full.len() > MAX_NAME_BYTES {
        return Err(NameProblem::TooLong);
    }
    Ok(())
}

/// White space as the window sees it. JavaScript's `\s` is Rust's
/// `is_whitespace` with U+FEFF added and U+0085 left out; the window adds
/// U+0085 back, so both say the same for every character.
fn is_blank(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

/// A character no file name may hold on Windows: the nine below and the
/// control characters U+0000 to U+001F and U+007F (not the whole `Cc` class,
/// which the window's pattern does not cover either).
fn is_forbidden(c: char) -> bool {
    matches!(
        c,
        '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\u{0}'..='\u{1F}' | '\u{7F}'
    )
}

/// Whether the part of `name` before its first dot is a device name, in any
/// case (`Con.txt` and `nul.tar` are, `console` and `com10` are not). Windows
/// also reserves `com` and `lpt` followed by the superscript digits ¹ ² ³
/// (U+00B9, U+00B2, U+00B3), so `COM¹` is refused, and `com⁴` is not.
fn is_reserved(name: &str) -> bool {
    let lower = name.to_lowercase();
    let head = lower.split('.').next().unwrap_or_default();
    match head {
        "con" | "prn" | "aux" | "nul" => true,
        _ => ["com", "lpt"].iter().any(|device| {
            head.strip_prefix(device).is_some_and(|number| {
                let mut chars = number.chars();
                matches!(
                    (chars.next(), chars.next()),
                    (Some('1'..='9' | '\u{B9}' | '\u{B2}' | '\u{B3}'), None)
                )
            })
        }),
    }
}

#[cfg(test)]
#[path = "../tests/common/save_names.rs"]
pub(crate) mod save_names;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::save_names::{self, expand};
    use super::*;

    fn problem_named(name: &str) -> NameProblem {
        match name {
            "Empty" => NameProblem::Empty,
            "ForbiddenCharacter" => NameProblem::ForbiddenCharacter,
            "TrailingSpaceOrDot" => NameProblem::TrailingSpaceOrDot,
            "Reserved" => NameProblem::Reserved,
            "TooLong" => NameProblem::TooLong,
            other => panic!("the fixture names a problem that does not exist: {other}"),
        }
    }

    #[test]
    fn every_accepted_fixture_name_passes() {
        let names = save_names::load();
        for stem in &names.accepted {
            assert_eq!(
                check_save_name(stem, &names.extension),
                Ok(()),
                "accepted name {stem:?}"
            );
        }
    }

    #[test]
    fn every_refused_fixture_name_gives_its_problem() {
        let names = save_names::load();
        for (problem, stems) in &names.refused {
            for stem in stems {
                assert_eq!(
                    check_save_name(stem, &names.extension),
                    Err(problem_named(problem)),
                    "refused name {stem:?} under {problem}"
                );
            }
        }
    }

    #[test]
    fn a_reserved_word_inside_a_name_is_accepted() {
        for stem in ["console", "com10", "com0", "icon", "con-x"] {
            assert_eq!(check_save_name(stem, ".jpg"), Ok(()), "{stem}");
        }
    }

    #[test]
    fn the_fixture_covers_every_problem_and_both_caps() {
        let names = save_names::load();
        let listed: Vec<&str> = names.refused.iter().map(|(p, _)| p.as_str()).collect();
        for problem in [
            "Empty",
            "ForbiddenCharacter",
            "TrailingSpaceOrDot",
            "Reserved",
            "TooLong",
        ] {
            let stems = names.refused.iter().find(|(p, _)| p == problem);
            assert!(
                stems.is_some_and(|(_, stems)| !stems.is_empty()),
                "{problem} has no names; the fixture has {listed:?}"
            );
        }
        let full = |stem: &String| format!("{stem}{}", names.extension);
        let too_long = &names
            .refused
            .iter()
            .find(|(p, _)| p == "TooLong")
            .unwrap()
            .1;
        // A name only the byte cap refuses: within 200 code points, over 255 bytes.
        assert!(
            too_long
                .iter()
                .any(|s| full(s).chars().count() <= 200 && full(s).len() > 255)
        );
        // A name only the code point cap refuses: over 200 code points, within 255 bytes.
        assert!(
            too_long
                .iter()
                .any(|s| full(s).chars().count() > 200 && full(s).len() <= 255)
        );
        // Names on the caps are accepted.
        assert!(
            names
                .accepted
                .iter()
                .any(|s| full(s).chars().count() == 200)
        );
        assert!(names.accepted.iter().any(|s| full(s).len() == 255));
    }

    #[test]
    fn a_repeat_rule_expands_to_the_text_repeated() {
        assert_eq!(expand(&json!("plain")).unwrap(), "plain");
        assert_eq!(
            expand(&json!({ "repeat": "ab", "times": 3 })).unwrap(),
            "ababab"
        );
        assert_eq!(expand(&json!({ "repeat": "ab", "times": 0 })).unwrap(), "");
    }

    #[test]
    fn a_repeat_rule_may_end_with_a_suffix() {
        assert_eq!(
            expand(&json!({ "repeat": "x", "times": 2, "then": "abc" })).unwrap(),
            "xxabc"
        );
    }

    #[test]
    fn a_malformed_rule_is_an_error() {
        for bad in [
            json!({ "repeat": "a" }),
            json!({ "times": 2 }),
            json!({ "repeat": "a", "times": -1 }),
            json!({ "repeat": "a", "times": 1.5 }),
            json!({ "repeat": "a", "times": 10_001 }),
            json!({ "repeat": 7, "times": 2 }),
            json!({ "repeat": "a", "times": 2, "then": 1 }),
            json!({ "repeat": "a", "times": 2, "before": 1 }),
            json!({ "repeat": "a", "times": 2, "extra": "x" }),
            json!(42),
            json!(["a"]),
            json!(null),
        ] {
            assert!(expand(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_rule_may_start_with_a_before_text() {
        assert_eq!(
            expand(&json!({ "before": "con.", "repeat": "x", "times": 2, "then": "!" })).unwrap(),
            "con.xx!"
        );
    }

    /// The `times` of a rule written as JSON text, so that how it is written
    /// (`2`, `2.0`) reaches the loader as it would from the file.
    fn expand_times(written: &str) -> Result<String, String> {
        let rule: serde_json::Value =
            serde_json::from_str(&format!(r#"{{"repeat": "a", "times": {written}}}"#)).unwrap();
        expand(&rule)
    }

    #[test]
    fn a_times_written_as_a_plain_integer_is_accepted() {
        assert_eq!(expand_times("2").unwrap(), "aa");
        assert_eq!(expand_times("0").unwrap(), "");
    }

    #[test]
    fn a_times_written_with_a_decimal_point_or_exponent_is_an_error() {
        // The window's loader refuses these spellings too (names.test.js).
        for written in ["2.0", "2e0", "1e1", "1.5", "-0", "-1"] {
            assert!(expand_times(written).is_err(), "{written}");
        }
    }

    /// The characters of `text` that are not printable ASCII or a line break.
    fn not_printable_ascii(text: &str) -> Vec<char> {
        text.chars()
            .filter(|c| !(' '..='~').contains(c) && *c != '\n')
            .collect()
    }

    #[test]
    fn the_fixture_file_is_printable_ascii_only() {
        // Every other character is a \uXXXX escape, so an editor cannot change
        // an invisible or look-alike one unnoticed.
        assert_eq!(not_printable_ascii(&save_names::raw()), Vec::<char>::new());
    }

    #[test]
    fn a_character_outside_printable_ascii_in_a_fixture_text_is_found() {
        assert_eq!(
            not_printable_ascii("a\u{E9}\u{A0}\u{FEFF}\t\"x\"\n"),
            ['\u{E9}', '\u{A0}', '\u{FEFF}', '\t']
        );
        assert!(not_printable_ascii("{\"a\": \"\\u00E9\"}\n").is_empty());
    }

    #[test]
    fn the_fixture_keeps_composed_and_decomposed_accents_apart() {
        let names = save_names::load();
        let composed = "vakantie-caf\u{E9}";
        let decomposed = "vakantie-cafe\u{301}";
        assert_ne!(composed, decomposed);
        assert!(names.accepted.iter().any(|s| s == composed));
        assert!(names.accepted.iter().any(|s| s == decomposed));
    }

    #[test]
    fn a_fixture_that_is_not_in_the_documented_shape_is_an_error() {
        assert!(save_names::parse("not json").is_err());
        assert!(save_names::parse(r#"{"accepted": [], "refused": {}}"#).is_err());
        assert!(
            save_names::parse(r#"{"extension": ".jpg", "accepted": "a", "refused": {}}"#).is_err()
        );
        assert!(
            save_names::parse(r#"{"extension": ".jpg", "accepted": [], "refused": []}"#).is_err()
        );
        assert!(
            save_names::parse(
                r#"{"extension": ".jpg", "accepted": [], "refused": {"Empty": [1]}}"#
            )
            .is_err()
        );
        let ok = save_names::parse(
            r#"{"extension": ".jpg", "accepted": ["a"], "refused": {"Empty": [""]}}"#,
        )
        .unwrap();
        assert_eq!(ok.accepted, ["a"]);
        assert_eq!(ok.refused, [("Empty".to_owned(), vec![String::new()])]);
    }
}

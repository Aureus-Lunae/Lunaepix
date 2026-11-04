//! What the CLI prints for each file. The success layout is the one the shell
//! function `optimg` prints today (an `awk` `printf`), byte for byte.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::path::Path;

use lunaepix_core::{FileResult, Outcome};

/// The colour switched on before the saved percentage, and off after it.
const GREEN: &str = "\x1b[32m";
const RESET: &str = "\x1b[0m";

/// The text for one optimised file:
///
/// ```text
/// <name>:
/// Done in <ms> ms!
/// <before> KiB - <saved>% = <after> KiB
/// <blank line>
/// ```
///
/// Sizes are KiB with three decimals, the saving a percentage with one; both
/// are rounded as `printf` does. `colour` puts the saving in green. A
/// zero-byte input saves `0.0%`. A file that grew has a negative saving. The
/// name is for display; bytes that are not UTF-8 are replaced.
pub fn format_success(name: &OsStr, outcome: &Outcome, colour: bool) -> String {
    let (green, reset) = if colour { (GREEN, RESET) } else { ("", "") };
    let before = outcome.before as f64;
    let after = outcome.after as f64;
    let saved = if outcome.before > 0 {
        (before - after) * 100.0 / before
    } else {
        0.0
    };
    format!(
        "{}:\nDone in {} ms!\n{:.3} KiB - {green}{saved:.1}%{reset} = {:.3} KiB\n\n",
        escape_control(&name.to_string_lossy()),
        outcome.millis,
        before / 1024.0,
        after / 1024.0,
    )
}

/// `text` with every control character replaced by a visible escape, so that
/// a file name, or text from a file or a worker, cannot drive the terminal
/// it is printed on. C0 (below 0x20, so tab, CR and LF too) and DEL become
/// `\x1b`; C1 (U+0080 to U+009F) become `\u{9b}`. Nothing else changes.
fn escape_control(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if !c.is_control() {
            escaped.push(c);
        } else if (c as u32) < 0x80 {
            escaped.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            escaped.push_str(&format!("\\u{{{:x}}}", c as u32));
        }
    }
    escaped
}

/// A path for display: lossy, and escaped.
fn display_path(path: &Path) -> String {
    escape_control(&path.to_string_lossy())
}

/// Print `lunaepix: failed (not a folder): <dir>` to `err`: a `--dir` value
/// that does not exist or is not a folder.
pub fn report_not_a_folder(dir: &Path, err: &mut dyn Write) {
    let _ = writeln!(
        err,
        "lunaepix: failed (not a folder): {}",
        display_path(dir)
    );
}

/// [`report_to`] on the real stdout and stderr. They are locked for this one
/// call only, not for the whole run.
pub fn report(src: &Path, result: &FileResult, colour: bool) -> bool {
    report_to(
        src,
        result,
        colour,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}

/// Print the result of one file: a success to `out`, a skip or a failure to
/// `err`. Returns whether the file failed (a skip is not a failure). Nothing
/// here can stop the run: a write that fails (a closed pipe) is ignored.
pub fn report_to(
    src: &Path,
    result: &FileResult,
    colour: bool,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> bool {
    // Write errors are ignored on purpose: a closed pipe must not stop the
    // run or hide a failure from the exit code.
    match result {
        FileResult::Optimized(outcome) => {
            let name = src.file_name().unwrap_or(src.as_os_str());
            let _ = out.write_all(format_success(name, outcome, colour).as_bytes());
            let _ = out.flush();
            false
        }
        FileResult::Skipped(reason) => {
            let reason = escape_control(&reason.to_string());
            let _ = writeln!(err, "lunaepix: skipped ({reason}): {}", display_path(src));
            false
        }
        FileResult::Failed(error) => {
            let reason = escape_control(&error.reason());
            let _ = writeln!(err, "lunaepix: failed ({reason}): {}", display_path(src));
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunaepix_core::{OptimizeError, RunError, Skip};

    fn outcome(before: u64, after: u64, millis: u64) -> Outcome {
        Outcome {
            before,
            after,
            millis,
        }
    }

    fn plain(before: u64, after: u64, millis: u64) -> String {
        format_success(
            OsStr::new("my file.png"),
            &outcome(before, after, millis),
            false,
        )
    }

    // The expected texts below come from the shell function's own awk
    // `printf`, run by hand with the same numbers.

    #[test]
    fn formats_a_known_outcome_as_the_shell_function_does() {
        assert_eq!(
            plain(123_456, 45_678, 1234),
            "my file.png:\nDone in 1234 ms!\n120.562 KiB - 63.0% = 44.607 KiB\n\n"
        );
    }

    #[test]
    fn puts_the_escape_codes_around_the_percentage_only_when_asked() {
        let coloured = format_success(
            OsStr::new("my file.png"),
            &outcome(123_456, 45_678, 1234),
            true,
        );
        assert_eq!(
            coloured,
            "my file.png:\nDone in 1234 ms!\n120.562 KiB - \x1b[32m63.0%\x1b[0m = 44.607 KiB\n\n"
        );
        // And not when colour is off.
        assert!(!plain(123_456, 45_678, 1234).contains('\x1b'));
    }

    #[test]
    fn a_zero_byte_input_saves_0_0_percent_without_dividing_by_zero() {
        assert_eq!(
            plain(0, 0, 0),
            "my file.png:\nDone in 0 ms!\n0.000 KiB - 0.0% = 0.000 KiB\n\n"
        );
        // Also when something was written for an empty input.
        let text = plain(0, 10, 5);
        assert_eq!(
            text,
            "my file.png:\nDone in 5 ms!\n0.000 KiB - 0.0% = 0.010 KiB\n\n"
        );
        assert!(!text.contains("NaN") && !text.contains("inf"));
    }

    #[test]
    fn a_non_empty_input_is_not_reported_as_zero_percent() {
        // The zero-byte guard must not catch real files.
        assert!(plain(100, 50, 1).contains(" - 50.0% = "));
        assert!(plain(1, 0, 1).contains(" - 100.0% = "));
    }

    #[test]
    fn a_file_that_grew_has_a_negative_percentage() {
        assert_eq!(
            plain(100, 150, 9),
            "my file.png:\nDone in 9 ms!\n0.098 KiB - -50.0% = 0.146 KiB\n\n"
        );
        // A growth that rounds to nothing keeps awk's "-0.0".
        assert!(plain(100_000, 100_001, 1).contains(" - -0.0% = "));
    }

    #[test]
    fn rounds_sizes_and_the_percentage_as_printf_does() {
        // 64 / 1024 = 0.0625 exactly, a tie: printf rounds it to even.
        assert!(
            plain(64, 63, 1)
                .starts_with("my file.png:\nDone in 1 ms!\n0.062 KiB - 1.6% = 0.062 KiB")
        );
        // 1_000_000 / 1024 = 976.5625, a tie, to even: 976.562.
        assert!(plain(1_000_000, 999_999, 2).contains("976.562 KiB - 0.0% = 976.562 KiB"));
        // 100_000 / 1024 = 97.65625, a tie, to even: 97.656 (even is 6).
        assert!(plain(100_000, 99_996, 2).contains("97.656 KiB - 0.0% = 97.652 KiB"));
        // 99_999 / 1024 = 97.6552..., not a tie: up to 97.655.
        assert!(plain(99_999, 12_345, 77).contains("97.655 KiB - 87.7% = 12.056 KiB"));
        // 1 / 16 saved = 6.25 percent, a tie: 6.2.
        assert!(plain(16, 15, 1).contains(" - 6.2% = "));
        // 12.5 needs no rounding; 1/3 and 2/3 do.
        assert!(plain(8, 7, 1).contains(" - 12.5% = "));
        assert!(plain(3, 2, 3).contains("0.003 KiB - 33.3% = 0.002 KiB"));
        assert!(plain(3, 1, 3).contains("0.003 KiB - 66.7% = 0.001 KiB"));
    }

    #[test]
    fn prints_the_milliseconds_the_core_measured_as_a_whole_number() {
        assert!(plain(2048, 1024, 987_654).contains("Done in 987654 ms!"));
    }

    #[test]
    fn shows_the_file_name_without_its_folder() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let result = FileResult::Optimized(outcome(2048, 1024, 1));
        report_to(
            Path::new("some/where/a b.png"),
            &result,
            false,
            &mut out,
            &mut err,
        );
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("a b.png:\n"), "{text}");
        assert!(!text.contains("where"), "{text}");
    }

    fn run_report(src: &str, result: &FileResult) -> (bool, String, String) {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let failed = report_to(Path::new(src), result, false, &mut out, &mut err);
        (
            failed,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn a_success_goes_to_stdout_only_and_is_not_a_failure() {
        let (failed, out, err) =
            run_report("a.png", &FileResult::Optimized(outcome(2048, 1024, 3)));
        assert!(!failed);
        assert_eq!(
            out,
            "a.png:\nDone in 3 ms!\n2.000 KiB - 50.0% = 1.000 KiB\n\n"
        );
        assert_eq!(err, "");
    }

    #[test]
    fn a_skip_goes_to_stderr_with_the_reason_and_is_not_a_failure() {
        let (failed, out, err) =
            run_report("dir/notes.txt", &FileResult::Skipped(Skip::UnsupportedType));
        assert!(!failed);
        assert_eq!(out, "");
        assert_eq!(
            err,
            "lunaepix: skipped (Not a PNG, JPG or SVG file): dir/notes.txt\n"
        );
    }

    #[test]
    fn a_failure_goes_to_stderr_with_the_bare_reason_and_counts_as_failed() {
        let error = RunError::Optimize(OptimizeError::Invalid {
            path: "bad.png".into(),
            message: "not a PNG".to_owned(),
        });
        let (failed, out, err) = run_report("bad.png", &FileResult::Failed(error));
        assert!(failed);
        assert_eq!(out, "");
        // The reason does not repeat the path that ends the line.
        assert_eq!(err, "lunaepix: failed (not a PNG): bad.png\n");
    }

    #[test]
    fn a_crash_reason_does_not_repeat_the_path_either() {
        let error = RunError::Crashed {
            path: "dir/b.svg".into(),
            cause: lunaepix_core::Crash::NoValidResult,
            stderr: String::new(),
        };
        let (_, _, err) = run_report("dir/b.svg", &FileResult::Failed(error));
        assert_eq!(
            err,
            "lunaepix: failed (the optimiser crashed on this file (it gave no valid result)): dir/b.svg\n"
        );
        assert_eq!(err.matches("dir/b.svg").count(), 1);
    }

    // --- control characters ---------------------------------------------------

    /// Every byte is printable, except exactly `lines` newlines: nothing a
    /// terminal would act on.
    fn assert_no_control_bytes(text: &str, lines: usize) {
        assert_eq!(text.matches('\n').count(), lines, "{text:?}");
        // `is_control` is C0, DEL and C1.
        for c in text.chars().filter(|&c| c != '\n') {
            assert!(!c.is_control(), "raw control character {c:?} in {text:?}");
        }
    }

    /// Names that a terminal would act on: set the title, clear the screen,
    /// ring the bell, move the cursor to the line start or down a line, a tab,
    /// DEL, and a single-character CSI (C1).
    const HOSTILE: [&str; 8] = [
        "\x1b]0;pwned\x07a.png",
        "\x1b[2Ja.png",
        "bell\x07.png",
        "cr\ra.png",
        "lf\na.png",
        "tab\ta.png",
        "del\x7fa.png",
        "csi\u{9b}2Ja.png",
    ];

    #[test]
    fn leaves_a_normal_non_ascii_name_unchanged() {
        for name in [
            "Mobilis Logo – zwart é.png",
            "写真 (1).svg",
            "a b.jpg",
            "ß\\.png",
        ] {
            assert_eq!(escape_control(name), name);
        }
        let (_, out, _) = run_report(
            "dir/Mobilis Logo – zwart é.png",
            &FileResult::Optimized(outcome(2048, 1024, 1)),
        );
        assert!(out.starts_with("Mobilis Logo – zwart é.png:\n"), "{out}");
        let (_, _, err) = run_report(
            "dir/Mobilis Logo – zwart é.txt",
            &FileResult::Skipped(Skip::UnsupportedType),
        );
        assert!(err.ends_with(": dir/Mobilis Logo – zwart é.txt\n"), "{err}");
    }

    #[test]
    fn escapes_each_kind_of_control_character_visibly() {
        assert_eq!(escape_control("\x1b"), "\\x1b");
        assert_eq!(escape_control("\x07"), "\\x07");
        assert_eq!(escape_control("\r\n\t"), "\\x0d\\x0a\\x09");
        assert_eq!(escape_control("\x00"), "\\x00");
        assert_eq!(escape_control("\x7f"), "\\x7f");
        assert_eq!(
            escape_control("\u{80}\u{9b}\u{9f}"),
            "\\u{80}\\u{9b}\\u{9f}"
        );
        // The characters on either side of the C1 range are not control
        // characters.
        assert_eq!(escape_control("\u{a0}\u{7e}\u{20}"), "\u{a0}\u{7e}\u{20}");
        assert_eq!(
            escape_control("\x1b]0;pwned\x07a.png"),
            "\\x1b]0;pwned\\x07a.png"
        );
    }

    #[test]
    fn a_hostile_name_reaches_no_terminal_in_a_success_block() {
        for name in HOSTILE {
            let text = format_success(OsStr::new(name), &outcome(2048, 1024, 1), false);
            assert_no_control_bytes(&text, 4);
            let (_, out, err) = run_report(
                &format!("dir/{name}"),
                &FileResult::Optimized(outcome(2048, 1024, 1)),
            );
            assert_no_control_bytes(&out, 4);
            assert_eq!(err, "");
        }
    }

    #[test]
    fn a_hostile_path_reaches_no_terminal_in_a_skip_or_failure_line() {
        for name in HOSTILE {
            let path = format!("dir/{name}");
            let (_, _, err) = run_report(&path, &FileResult::Skipped(Skip::UnsupportedType));
            assert_no_control_bytes(&err, 1);
            let error = RunError::Optimize(OptimizeError::Invalid {
                path: path.clone().into(),
                message: "x".to_owned(),
            });
            let (_, _, err) = run_report(&path, &FileResult::Failed(error));
            assert_no_control_bytes(&err, 1);
        }
    }

    #[test]
    fn untrusted_text_in_a_failure_reason_reaches_no_terminal() {
        // The reason can carry text from a file (a parser's message) or from
        // the worker.
        for text in HOSTILE {
            let error = RunError::Optimize(OptimizeError::Invalid {
                path: "bad.png".into(),
                message: text.to_owned(),
            });
            let (failed, _, err) = run_report("bad.png", &FileResult::Failed(error));
            assert!(failed);
            assert_no_control_bytes(&err, 1);
        }
    }

    #[test]
    fn our_own_colour_codes_survive_the_escaping() {
        let text = format_success(OsStr::new("\x1b[2Ja.png"), &outcome(2048, 1024, 1), true);
        assert_eq!(
            text,
            "\\x1b[2Ja.png:\nDone in 1 ms!\n2.000 KiB - \x1b[32m50.0%\x1b[0m = 1.000 KiB\n\n"
        );
    }

    // --- a --dir that is not a folder -----------------------------------------

    #[test]
    fn says_a_dir_that_is_not_a_folder_failed() {
        let mut err = Vec::new();
        report_not_a_folder(Path::new("pics/none"), &mut err);
        assert_eq!(
            String::from_utf8(err).unwrap(),
            "lunaepix: failed (not a folder): pics/none\n"
        );
    }

    #[test]
    fn a_hostile_dir_name_reaches_no_terminal() {
        for name in HOSTILE {
            let mut err = Vec::new();
            report_not_a_folder(Path::new(name), &mut err);
            assert_no_control_bytes(&String::from_utf8(err).unwrap(), 1);
        }
    }

    /// A writer whose pipe is closed.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn a_closed_stdout_neither_panics_nor_hides_a_failure() {
        let ok = FileResult::Optimized(outcome(10, 5, 1));
        assert!(!report_to(
            Path::new("a.png"),
            &ok,
            false,
            &mut Closed,
            &mut Closed
        ));
        let error = RunError::Optimize(OptimizeError::Invalid {
            path: "bad.png".into(),
            message: "x".to_owned(),
        });
        let failed = FileResult::Failed(error);
        assert!(report_to(
            Path::new("bad.png"),
            &failed,
            false,
            &mut Closed,
            &mut Closed
        ));
    }
}

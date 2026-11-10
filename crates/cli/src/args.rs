//! The command line: what to optimise, and how.

use std::ffi::OsString;
use std::path::PathBuf;

use lunaepix_core::{Compression, DEFAULT_QUALITY, Quality};

/// What the user asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// `lunaepix <file>...`: these files (a folder among them is planned like
    /// `--dir`).
    Files(Vec<PathBuf>),
    /// `lunaepix --dir <dir>`: the files directly inside the folder.
    Dir(PathBuf),
}

/// All the options of one command line.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub request: Request,
    /// Lossless unless `--lossy` was given.
    pub compression: Compression,
    /// `--out <folder>`: where the outputs go instead of next to the
    /// originals (or into `<dir>/optimized/`).
    pub out: Option<PathBuf>,
}

/// The arguments do not make sense; the caller prints [`USAGE`].
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError;

/// The usage text, printed to stderr on a usage error.
pub const USAGE: &str = "usage: lunaepix [--lossy [--quality <0-100>]] [--out <folder>] [--] <file>...\n       lunaepix [--lossy [--quality <0-100>]] [--out <folder>] --dir <dir>";

/// Read the arguments after the program name.
///
/// `--dir <dir>`, `--out <folder>` and `--quality <0-100>` take the next
/// argument as their value, whatever it looks like. `--` ends option parsing,
/// so a file named `--dir` can be passed after it. Without `--lossy` the run
/// is lossless; with it the quality is `--quality`, or [`DEFAULT_QUALITY`].
///
/// A usage error is: no arguments, an unknown option (also the `--quality=50`
/// spelling), an option without its value, `--dir`, `--lossy`, `--quality` or
/// `--out` given twice, `--dir` mixed with files, `--quality` without
/// `--lossy`, or a quality that is not a whole number from 0 to 100.
/// Arguments stay `OsString`; nothing is turned into text to decide anything
/// except the quality, which a non-UTF-8 value cannot be.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, UsageError> {
    let mut args = args.into_iter();
    let mut files = Vec::new();
    let mut dir: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut lossy = false;
    let mut quality: Option<Quality> = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            files.extend(args.by_ref().map(PathBuf::from));
        } else if arg == "--dir" {
            let value = args.next().ok_or(UsageError)?;
            if dir.replace(PathBuf::from(value)).is_some() {
                return Err(UsageError);
            }
        } else if arg == "--out" {
            let value = args.next().ok_or(UsageError)?;
            if out.replace(PathBuf::from(value)).is_some() {
                return Err(UsageError);
            }
        } else if arg == "--lossy" {
            if std::mem::replace(&mut lossy, true) {
                return Err(UsageError);
            }
        } else if arg == "--quality" {
            let value = args.next().ok_or(UsageError)?;
            let value = value.to_str().and_then(Quality::parse).ok_or(UsageError)?;
            if quality.replace(value).is_some() {
                return Err(UsageError);
            }
        } else if arg != "-" && arg.as_encoded_bytes().starts_with(b"-") {
            return Err(UsageError);
        } else {
            files.push(PathBuf::from(arg));
        }
    }
    let compression = match (lossy, quality) {
        (true, quality) => Compression::Lossy(quality.unwrap_or(DEFAULT_QUALITY)),
        (false, None) => Compression::Lossless,
        // A quality means nothing to a lossless run: say so, do not ignore it.
        (false, Some(_)) => return Err(UsageError),
    };
    let request = match (dir, files.is_empty()) {
        (Some(dir), true) => Request::Dir(dir),
        (None, false) => Request::Files(files),
        _ => return Err(UsageError),
    };
    Ok(Args {
        request,
        compression,
        out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lunaepix_core::{DEFAULT_QUALITY, Quality};

    fn parse_args(args: &[&str]) -> Result<Args, UsageError> {
        parse(args.iter().map(OsString::from))
    }

    fn parse_strs(args: &[&str]) -> Result<Request, UsageError> {
        parse_args(args).map(|args| args.request)
    }

    fn lossy(quality: u8) -> Compression {
        Compression::Lossy(Quality::new(quality).unwrap())
    }

    fn files(names: &[&str]) -> Result<Request, UsageError> {
        Ok(Request::Files(names.iter().map(PathBuf::from).collect()))
    }

    #[test]
    fn takes_files_in_the_order_given() {
        assert_eq!(parse_strs(&["b.png", "a.svg"]), files(&["b.png", "a.svg"]));
    }

    #[test]
    fn takes_a_folder_after_dir() {
        assert_eq!(
            parse_strs(&["--dir", "pics"]),
            Ok(Request::Dir(PathBuf::from("pics")))
        );
    }

    #[test]
    fn rejects_no_arguments() {
        assert_eq!(parse_strs(&[]), Err(UsageError));
    }

    #[test]
    fn rejects_an_unknown_option() {
        assert_eq!(parse_strs(&["--bogus", "a.png"]), Err(UsageError));
        assert_eq!(parse_strs(&["a.png", "-x"]), Err(UsageError));
        assert_eq!(parse_strs(&["--dir=pics"]), Err(UsageError));
    }

    #[test]
    fn rejects_dir_without_a_value() {
        assert_eq!(parse_strs(&["--dir"]), Err(UsageError));
        assert_eq!(parse_strs(&["a.png", "--dir"]), Err(UsageError));
    }

    #[test]
    fn rejects_dir_given_twice_or_mixed_with_files() {
        assert_eq!(parse_strs(&["--dir", "a", "--dir", "b"]), Err(UsageError));
        assert_eq!(parse_strs(&["--dir", "a", "b.png"]), Err(UsageError));
        assert_eq!(parse_strs(&["b.png", "--dir", "a"]), Err(UsageError));
    }

    #[test]
    fn a_double_dash_ends_option_parsing() {
        assert_eq!(parse_strs(&["--", "--dir"]), files(&["--dir"]));
        assert_eq!(
            parse_strs(&["a.png", "--", "--dir", "-x"]),
            files(&["a.png", "--dir", "-x"])
        );
        // Without it the same name is the option, and has no value.
        assert_eq!(parse_strs(&["--dir"]), Err(UsageError));
    }

    #[test]
    fn a_double_dash_with_nothing_after_it_is_a_usage_error() {
        assert_eq!(parse_strs(&["--"]), Err(UsageError));
    }

    #[test]
    fn a_lone_dash_is_a_file_name() {
        assert_eq!(parse_strs(&["-"]), files(&["-"]));
    }

    #[cfg(unix)]
    #[test]
    fn keeps_a_file_name_that_is_not_valid_utf8() {
        use std::os::unix::ffi::OsStringExt;
        let name = OsString::from_vec(vec![b'a', 0xff, b'.', b'p', b'n', b'g']);
        assert_eq!(
            parse([name.clone()]).map(|args| args.request),
            Ok(Request::Files(vec![PathBuf::from(name)]))
        );
    }

    // --- --lossy, --quality, --out ------------------------------------------

    #[test]
    fn parses_lossy_quality_0_and_100_out_with_files_and_with_dir() {
        let parsed = |args: &[&str]| parse_args(args).unwrap();
        assert_eq!(
            parsed(&["--lossy", "a.png"]),
            Args {
                request: Request::Files(vec![PathBuf::from("a.png")]),
                compression: lossy(75),
                out: None,
            }
        );
        assert_eq!(
            parsed(&["--lossy", "--quality", "0", "a.png"]).compression,
            lossy(0)
        );
        assert_eq!(
            parsed(&["--lossy", "--quality", "100", "a.png"]).compression,
            lossy(100)
        );
        // The order of the options does not matter, nor where the files sit.
        let both = parsed(&["a.png", "--quality", "30", "--out", "o", "--lossy", "b.jpg"]);
        assert_eq!(both.compression, lossy(30));
        assert_eq!(both.out, Some(PathBuf::from("o")));
        assert_eq!(
            both.request,
            Request::Files(vec![PathBuf::from("a.png"), PathBuf::from("b.jpg")])
        );
        // `--out` alone is a lossless run into a folder.
        assert_eq!(
            parsed(&["--out", "o", "a.png"]),
            Args {
                request: Request::Files(vec![PathBuf::from("a.png")]),
                compression: Compression::Lossless,
                out: Some(PathBuf::from("o")),
            }
        );
        // With `--dir`.
        assert_eq!(
            parsed(&["--lossy", "--quality", "50", "--out", "o", "--dir", "pics"]),
            Args {
                request: Request::Dir(PathBuf::from("pics")),
                compression: lossy(50),
                out: Some(PathBuf::from("o")),
            }
        );
        // The value of `--out` is taken whatever it looks like, like `--dir`'s.
        assert_eq!(
            parsed(&["--out", "--lossy", "a.png"]).out,
            Some(PathBuf::from("--lossy"))
        );
        // After `--` everything is a file, also the options.
        let after = parsed(&["--lossy", "--", "--out", "-x"]);
        assert_eq!(after.compression, lossy(75));
        assert_eq!(after.out, None);
        assert_eq!(
            after.request,
            Request::Files(vec![PathBuf::from("--out"), PathBuf::from("-x")])
        );
    }

    #[test]
    fn usage_error_for_quality_without_lossy_101_minus_1_7_5_abc_repeated_quality_lossy_or_out_and_out_without_value()
     {
        let bad: &[&[&str]] = &[
            // `--quality` without `--lossy`, in either order.
            &["--quality", "50", "a.png"],
            &["a.png", "--quality", "50"],
            &["--quality", "50", "--out", "o", "a.png"],
            // A value that is not an integer from 0 to 100.
            &["--lossy", "--quality", "101", "a.png"],
            &["--lossy", "--quality", "256", "a.png"],
            &["--lossy", "--quality", "-1", "a.png"],
            &["--lossy", "--quality", "+5", "a.png"],
            &["--lossy", "--quality", "7.5", "a.png"],
            &["--lossy", "--quality", "abc", "a.png"],
            &["--lossy", "--quality", "", "a.png"],
            &["--lossy", "--quality", " 5", "a.png"],
            &["--lossy", "--quality", "99999999999999999999", "a.png"],
            // Given twice, also with the same value.
            &["--lossy", "--quality", "50", "--quality", "50", "a.png"],
            &["--lossy", "--quality", "50", "--quality", "60", "a.png"],
            &["--lossy", "--lossy", "a.png"],
            &["--out", "o", "--out", "o", "a.png"],
            &["--out", "o", "--out", "p", "--dir", "d"],
            // Without a value.
            &["--lossy", "--quality"],
            &["a.png", "--out"],
            &["--lossy", "a.png", "--quality"],
            // The `=` spelling is not an option.
            &["--lossy", "--quality=50", "a.png"],
            &["--out=o", "a.png"],
            // The options alone name nothing to optimise.
            &["--lossy"],
            &["--lossy", "--out", "o"],
        ];
        for args in bad {
            assert_eq!(parse_args(args), Err(UsageError), "{args:?}");
        }
    }

    #[test]
    fn lossy_alone_uses_75() {
        let args = parse_args(&["--lossy", "a.png"]).unwrap();
        assert_eq!(args.compression, Compression::Lossy(DEFAULT_QUALITY));
        assert_eq!(DEFAULT_QUALITY.get(), 75);
    }

    #[test]
    fn lossy_with_a_quality_uses_that_quality_and_no_lossy_means_lossless() {
        assert_eq!(
            parse_args(&["--lossy", "--quality", "30", "a.png"])
                .unwrap()
                .compression,
            lossy(30)
        );
        assert_ne!(lossy(30), Compression::Lossy(DEFAULT_QUALITY));
        for args in [&["a.png"][..], &["--dir", "d"], &["--out", "o", "a.png"]] {
            assert_eq!(
                parse_args(args).unwrap().compression,
                Compression::Lossless,
                "{args:?}"
            );
        }
    }
}

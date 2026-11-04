//! The command line: what to optimise.

use std::ffi::OsString;
use std::path::PathBuf;

/// What the user asked for.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    /// `lunaepix <file>...`: these files (a folder among them is planned like
    /// `--dir`).
    Files(Vec<PathBuf>),
    /// `lunaepix --dir <dir>`: the files directly inside the folder.
    Dir(PathBuf),
}

/// The arguments do not make sense; the caller prints [`USAGE`].
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError;

/// The usage text, printed to stderr on a usage error.
pub const USAGE: &str = "usage: lunaepix [--] <file>...\n       lunaepix --dir <dir>";

/// Read the arguments after the program name.
///
/// `--dir <dir>` takes the next argument as the folder, whatever it looks
/// like. `--` ends option parsing, so a file named `--dir` can be passed after
/// it. A usage error is: no arguments, an unknown option, `--dir` without a
/// value or given twice, or `--dir` mixed with files. Arguments stay
/// `OsString`; nothing is turned into text to decide anything.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Request, UsageError> {
    let mut args = args.into_iter();
    let mut files = Vec::new();
    let mut dir: Option<PathBuf> = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            files.extend(args.by_ref().map(PathBuf::from));
        } else if arg == "--dir" {
            let value = args.next().ok_or(UsageError)?;
            if dir.replace(PathBuf::from(value)).is_some() {
                return Err(UsageError);
            }
        } else if arg != "-" && arg.as_encoded_bytes().starts_with(b"-") {
            return Err(UsageError);
        } else {
            files.push(PathBuf::from(arg));
        }
    }
    match (dir, files.is_empty()) {
        (Some(dir), true) => Ok(Request::Dir(dir)),
        (None, false) => Ok(Request::Files(files)),
        _ => Err(UsageError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Request, UsageError> {
        parse(args.iter().map(OsString::from))
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
            parse([name.clone()]),
            Ok(Request::Files(vec![PathBuf::from(name)]))
        );
    }
}

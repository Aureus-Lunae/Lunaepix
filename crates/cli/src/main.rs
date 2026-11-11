//! `lunaepix`: PNG, JPG, SVG and WebP optimisation from the command line,
//! lossless by default and lossy on request.
//!
//! ```text
//! lunaepix [--lossy [--quality <0-100>]] [--out <folder>] [--] <file>...
//!                          optimise these files; output next to each one,
//!                          or into <folder> under their own names
//! lunaepix [--lossy [--quality <0-100>]] [--out <folder>] --dir <dir>
//!                          optimise the files in a folder into
//!                          <dir>/optimized/, or into <folder>
//! ```
//!
//! Without `--lossy` the run is lossless, exactly as it always was. `--lossy`
//! re-encodes JPG, PNG and WebP files at `--quality` (a whole number from 0 to
//! 100, 75 when it is not given; it needs `--lossy`); an SVG gets the
//! lossless treatment. `--out` creates its folder when it is missing, and
//! never overwrites a file in it: a name that is taken gets a number.
//!
//! Prints one block per optimised file to stdout, as the shell function
//! `optimg` does, and one line per skipped or failed file to stderr. A lossy
//! run first prints `Lossy, quality <q>`, and a file that was kept as it was
//! (or kept lossless) has its note on the line under its sizes. The exit code
//! is 0 unless a file failed (1), or the usage was wrong (2). A skipped file
//! is not a failure.

mod args;
mod report;

use std::fs;
use std::io::{self, IsTerminal};
use std::process::ExitCode;

use lunaepix_core::{Options, Worker, WriteMode, plan_files_to, plan_folder_to, run};

use args::{Args, Request, USAGE, UsageError};

fn main() -> ExitCode {
    // The runner starts this same binary as the worker for each file (and as
    // the thumbnail worker for each thumbnail); without this first line every
    // worker would start a run of its own.
    if let Some(code) = lunaepix_core::worker::serve_if_requested() {
        return code;
    }

    let Args {
        request,
        compression,
        out,
    } = match args::parse(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(UsageError) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    // A folder the user named that is not there is a failure, not a skip. It
    // is checked before `--out` is created, so a failed run leaves no folder.
    if let Request::Dir(dir) = &request
        && !dir.is_dir()
    {
        report::report_not_a_folder(dir, &mut io::stderr().lock());
        return ExitCode::FAILURE;
    }
    if let Some(out) = &out
        && let Err(error) = fs::create_dir_all(out)
    {
        report::report_cannot_create_out(out, &error, &mut io::stderr().lock());
        return ExitCode::FAILURE;
    }
    let jobs = match &request {
        Request::Files(files) => plan_files_to(files, out.as_deref()),
        Request::Dir(dir) => plan_folder_to(dir, out.as_deref()),
    };
    let options = Options {
        compression,
        // An output folder is never replaced into: a taken name gets a number.
        write: if out.is_some() {
            WriteMode::NoReplace
        } else {
            WriteMode::Replace
        },
    };
    let worker = match Worker::current_exe() {
        Ok(worker) => worker.with_options(&options),
        Err(error) => {
            eprintln!("lunaepix: cannot find its own program file: {error}");
            return ExitCode::FAILURE;
        }
    };

    let colour = io::stdout().is_terminal();
    report::report_mode(compression, &mut io::stdout().lock());
    let mut failed = false;
    run(&jobs, &worker, |_, src, result| {
        failed |= report::report(src, result, colour);
    });
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

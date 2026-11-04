//! `lunaepix`: lossless PNG, JPG and SVG optimisation from the command line.
//!
//! ```text
//! lunaepix <file>...       optimise these files; output next to each one
//! lunaepix --dir <dir>     optimise the files in a folder into <dir>/optimized/
//! ```
//!
//! Prints one block per optimised file to stdout, as the shell function
//! `optimg` does, and one line per skipped or failed file to stderr. The exit
//! code is 0 unless a file failed (1), or the usage was wrong (2). A skipped
//! file is not a failure.

mod args;
mod report;

use std::io::{self, IsTerminal};
use std::process::ExitCode;

use lunaepix_core::{Worker, plan_files, plan_folder, run};

use args::{Request, USAGE, UsageError};

fn main() -> ExitCode {
    // The runner starts this same binary as the worker for each file; without
    // this first line every worker would start a run of its own.
    if let Some(code) = lunaepix_core::worker::serve_if_requested() {
        return code;
    }

    let request = match args::parse(std::env::args_os().skip(1)) {
        Ok(request) => request,
        Err(UsageError) => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let jobs = match &request {
        Request::Files(files) => plan_files(files),
        Request::Dir(dir) if dir.is_dir() => plan_folder(dir),
        // A folder the user named that is not there is a failure, not a skip.
        Request::Dir(dir) => {
            report::report_not_a_folder(dir, &mut io::stderr().lock());
            return ExitCode::FAILURE;
        }
    };
    let worker = match Worker::current_exe() {
        Ok(worker) => worker,
        Err(error) => {
            eprintln!("lunaepix: cannot find its own program file: {error}");
            return ExitCode::FAILURE;
        }
    };

    let colour = io::stdout().is_terminal();
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

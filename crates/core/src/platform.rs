//! The two system calls the worker makes for itself at start-up, one per
//! system, each a no-op elsewhere. The only `unsafe` code outside `jpg`.

/// Linux: ask the kernel to kill this worker when the thread that started it
/// ends: the app's run thread, which waits on its worker, ends only with the
/// run or with the app. So closing or killing the window never leaves a
/// worker writing on. If the parent is already gone by the time this runs (a
/// gap of microseconds after the start), the worker finishes its one file.
#[cfg(target_os = "linux")]
pub(crate) fn die_with_parent() {
    // SAFETY: `prctl(PR_SET_PDEATHSIG, signal)` takes two integers, reads no
    // memory of ours, and only sets an attribute of this process. Best
    // effort: a failure leaves the default, which is to carry on.
    unsafe {
        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn die_with_parent() {}

/// Windows: a worker that crashes must end at once, without the "has stopped
/// working" dialog of Windows Error Reporting, which would keep it alive
/// until someone dismisses it or the runner's timeout stops it. The error
/// mode also covers the "insert a disk" kind of dialog.
#[cfg(windows)]
pub(crate) fn suppress_crash_dialogs() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };
    // SAFETY: `SetErrorMode` takes a flag word and only sets the error mode
    // of this process; it returns the previous mode, which is not needed.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX);
    }
}

#[cfg(not(windows))]
pub(crate) fn suppress_crash_dialogs() {}

#[cfg(all(test, windows))]
mod tests {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SetErrorMode,
    };

    use super::suppress_crash_dialogs;

    const BOTH: u32 = SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX;

    #[test]
    fn turns_off_the_crash_and_error_dialogs_of_windows() {
        // SAFETY: as in `suppress_crash_dialogs`; this test process only.
        let before = unsafe {
            SetErrorMode(0);
            GetErrorMode()
        };
        // The negative: with the mode cleared, neither flag is set.
        assert_eq!(before & BOTH, 0, "{before:#x}");

        suppress_crash_dialogs();

        // SAFETY: reads the error mode of this process.
        let after = unsafe { GetErrorMode() };
        assert_eq!(after & BOTH, BOTH, "{after:#x}");
    }
}

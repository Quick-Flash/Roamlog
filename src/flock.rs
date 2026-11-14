//! Advisory whole-file locks (flock) shared between Android UI and recorder processes.
//! std's `File::lock` is unsupported on Android, so call flock directly.
use std::{fs::File, os::fd::AsRawFd};

fn flock(file: &File, operation: i32) -> std::io::Result<()> {
    loop {
        // SAFETY: fd is valid for file's lifetime; flock has no memory effects.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Blocks until exclusive lock is held.
pub fn lock(file: &File) -> Result<(), String> {
    flock(file, libc::LOCK_EX).map_err(|e| format!("Journal lock: {e}"))
}

pub fn unlock(file: &File) {
    let _ = flock(file, libc::LOCK_UN);
}

/// Takes exclusive lock if free; false when another open file holds it.
#[cfg(any(feature = "ui", target_os = "android", test))]
pub fn try_lock(file: &File) -> bool {
    flock(file, libc::LOCK_EX | libc::LOCK_NB).is_ok()
}

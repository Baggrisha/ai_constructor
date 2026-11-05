use std::{io, path::Path};

#[cfg(test)]
thread_local! {
    pub(super) static FAIL_SWAP: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rename(from: &Path, to: &Path, swap: bool) -> io::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};

    #[cfg(test)]
    if swap && FAIL_SWAP.with(|flag| flag.replace(false)) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Simulated unsupported atomic swap",
        ));
    }

    let from = CString::new(from.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in source path"))?;
    let to = CString::new(to.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in destination path"))?;

    // The C strings live through the syscall, and both flag values are documented.
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            if swap {
                libc::RENAME_SWAP
            } else {
                libc::RENAME_EXCL
            },
        )
    };
    // The C strings live through the syscall, and both flag values are documented.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            from.as_ptr(),
            libc::AT_FDCWD,
            to.as_ptr(),
            if swap {
                libc::RENAME_EXCHANGE
            } else {
                libc::RENAME_NOREPLACE
            } as _,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn rename(_from: &Path, _to: &Path, _swap: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Safe atomic publication is unavailable on this platform",
    ))
}

pub(super) fn swap(from: &Path, to: &Path) -> io::Result<()> {
    rename(from, to, true)
}

pub(super) fn move_no_replace(from: &Path, to: &Path) -> io::Result<()> {
    rename(from, to, false)
}

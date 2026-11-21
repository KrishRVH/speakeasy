use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io,
    os::{
        fd::{AsRawFd as _, FromRawFd as _, IntoRawFd as _},
        unix::{
            ffi::{OsStrExt as _, OsStringExt as _},
            fs::{MetadataExt as _, OpenOptionsExt as _},
        },
    },
    path::Path,
    ptr::NonNull,
};

const FLAGS: libc::c_int = libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

pub(super) fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(FLAGS | libc::O_DIRECTORY)
        .open(path)
}

pub(super) fn child(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
    let name = CString::new(name.as_bytes())?;
    let flags = FLAGS | libc::O_RDONLY | if directory { libc::O_DIRECTORY } else { 0 };
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        let error = io::Error::last_os_error();
        if !directory && error.raw_os_error() == Some(libc::ELOOP) {
            return Err(io::Error::other(
                "not a regular file: symbolic links are refused",
            ));
        }
        return Err(error);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn linked(metadata: &Metadata) -> bool {
    metadata.is_symlink()
}

pub(super) fn same(first: &File, second: &File) -> io::Result<bool> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    Ok((first.dev(), first.ino()) == (second.dev(), second.ino()))
}

pub(super) fn visit(directory: &File, mut visit: impl FnMut(OsString)) -> io::Result<()> {
    let fd = child(directory, OsStr::new("."), true)?.into_raw_fd();
    let Some(entries) = NonNull::new(unsafe { libc::fdopendir(fd) }) else {
        let error = io::Error::last_os_error();
        drop(unsafe { File::from_raw_fd(fd) });
        return Err(error);
    };
    let entries = Entries(entries);
    loop {
        unsafe { errno().write(0) };
        let entry = unsafe { libc::readdir(entries.0.as_ptr()) };
        let Some(entry) = (unsafe { entry.as_ref() }) else {
            let error = unsafe { errno().read() };
            return if error == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(error))
            };
        };
        let name = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) };
        visit(OsString::from_vec(name.to_bytes().to_vec()));
    }
}

struct Entries(NonNull<libc::DIR>);

impl Drop for Entries {
    fn drop(&mut self) {
        unsafe { libc::closedir(self.0.as_ptr()) };
    }
}

fn errno() -> *mut libc::c_int {
    #[cfg(target_os = "linux")]
    unsafe {
        libc::__errno_location()
    }
    #[cfg(target_os = "macos")]
    unsafe {
        libc::__error()
    }
}

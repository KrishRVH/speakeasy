use std::{
    ffi::{CStr, CString, OsStr, OsString},
    fs::{File, OpenOptions},
    io,
    mem::MaybeUninit,
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

use super::OpenKind;

const FLAGS: libc::c_int = libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK;

#[cfg(target_os = "linux")]
const SEARCH: libc::c_int = libc::O_PATH | libc::O_DIRECTORY;
#[cfg(target_os = "macos")]
const SEARCH: libc::c_int = libc::O_SEARCH;

pub(super) fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(FLAGS | SEARCH)
        .open(path)
}

pub(super) fn child(parent: &File, name: &OsStr, kind: OpenKind) -> io::Result<File> {
    let (flags, refused, reason) = match kind {
        OpenKind::Directory => (
            SEARCH,
            io::ErrorKind::NotADirectory,
            "not a plain directory: symbolic links are refused",
        ),
        OpenKind::File | OpenKind::StableFile => (
            libc::O_RDONLY,
            io::ErrorKind::Other,
            "not a regular file: symbolic links are refused",
        ),
    };
    open_at(parent, name, flags).map_err(|error| {
        if error.raw_os_error() == Some(libc::ELOOP) {
            io::Error::new(refused, reason)
        } else {
            error
        }
    })
}

pub(super) fn lookup(parent: &File, name: &OsStr) -> io::Result<()> {
    let name = CString::new(name.as_bytes())?;
    let mut status = MaybeUninit::<libc::stat>::uninit();
    let found = unsafe {
        libc::fstatat(
            parent.as_raw_fd(),
            name.as_ptr(),
            status.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if found == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn open_at(parent: &File, name: &OsStr, flags: libc::c_int) -> io::Result<File> {
    let name = CString::new(name.as_bytes())?;
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), FLAGS | flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

pub(super) fn same(first: &File, second: &File) -> io::Result<bool> {
    let first = first.metadata()?;
    let second = second.metadata()?;
    Ok((first.dev(), first.ino()) == (second.dev(), second.ino()))
}

pub(super) fn visit(directory: &File, mut visit: impl FnMut(OsString)) -> io::Result<()> {
    let fd = open_at(
        directory,
        OsStr::new("."),
        libc::O_RDONLY | libc::O_DIRECTORY,
    )?
    .into_raw_fd();
    let Some(entries) = NonNull::new(unsafe { libc::fdopendir(fd) }) else {
        let error = io::Error::last_os_error();
        drop(unsafe { File::from_raw_fd(fd) });
        return Err(error);
    };
    let entries = Entries(entries);
    loop {
        unsafe { errno().write(0) };
        let entry = unsafe { libc::readdir(entries.0.as_ptr()) };
        if entry.is_null() {
            let error = unsafe { errno().read() };
            return if error == 0 {
                Ok(())
            } else {
                Err(io::Error::from_raw_os_error(error))
            };
        }
        let name = unsafe { CStr::from_ptr((&raw const (*entry).d_name).cast()) };
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

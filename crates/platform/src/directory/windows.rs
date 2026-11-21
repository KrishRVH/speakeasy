use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::OpenOptionsExt as _,
        io::{AsRawHandle as _, FromRawHandle as _},
    },
    path::Path,
    ptr,
};

use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_DIRECTORY_FILE, FILE_NAMES_INFORMATION, FILE_NON_DIRECTORY_FILE,
            FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT, FileNamesInformation,
            NtOpenFile, NtQueryDirectoryFile,
        },
    },
    Win32::{
        Foundation::{
            ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, NTSTATUS,
            OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_NO_MORE_FILES,
            STATUS_NO_SUCH_FILE, UNICODE_STRING,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            FILE_GENERIC_READ, FILE_ID_INFO, FILE_LIST_DIRECTORY, FILE_NAME_NORMALIZED,
            FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
            FILE_TRAVERSE, FileIdInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
            GetFinalPathNameByHandleW, SYNCHRONIZE, VOLUME_NAME_NONE,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

use super::OpenKind;

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const SEARCH: u32 = FILE_TRAVERSE | FILE_READ_ATTRIBUTES | SYNCHRONIZE;
const LIST: u32 = FILE_LIST_DIRECTORY | SYNCHRONIZE;

pub(super) fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(SEARCH)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

pub(super) fn child(parent: &File, name: &OsStr, kind: OpenKind) -> io::Result<File> {
    let access = match kind {
        OpenKind::Directory => SEARCH,
        OpenKind::File | OpenKind::StableFile => FILE_GENERIC_READ,
    };
    let file = open_relative(parent, name, kind, access)?;
    if !final_name(&file)?.eq_ignore_ascii_case(name) {
        return Err(io::ErrorKind::NotFound.into());
    }
    Ok(file)
}

pub(super) fn lookup(parent: &File, name: &OsStr) -> io::Result<()> {
    open_at(
        parent,
        name,
        FILE_READ_ATTRIBUTES | SYNCHRONIZE,
        SHARE,
        FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
    )
    .map(drop)
}

fn open_relative(parent: &File, name: &OsStr, kind: OpenKind, access: u32) -> io::Result<File> {
    let options = FILE_OPEN_REPARSE_POINT
        | FILE_SYNCHRONOUS_IO_NONALERT
        | if matches!(kind, OpenKind::Directory) {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
    let share = match kind {
        OpenKind::Directory | OpenKind::File => SHARE,
        OpenKind::StableFile => FILE_SHARE_READ | FILE_SHARE_DELETE,
    };
    open_at(parent, name, access, share, options)
}

fn open_at(parent: &File, name: &OsStr, access: u32, share: u32, options: u32) -> io::Result<File> {
    let mut text: Vec<_> = name.encode_wide().collect();
    if text.iter().any(|unit| [0, u16::from(b':')].contains(unit)) {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let length = u16::try_from(text.len().saturating_mul(size_of::<u16>()))
        .map_err(|_overflow| io::ErrorKind::InvalidInput)?;
    let name = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: if text.is_empty() {
            ptr::null_mut()
        } else {
            text.as_mut_ptr()
        },
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: u32::try_from(size_of::<OBJECT_ATTRIBUTES>())
            .map_err(|_overflow| io::ErrorKind::InvalidInput)?,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &raw const name,
        Attributes: OBJ_CASE_INSENSITIVE | OBJ_DONT_REPARSE,
        ..OBJECT_ATTRIBUTES::default()
    };
    let mut status = IO_STATUS_BLOCK::default();
    let mut handle = ptr::null_mut();
    let result = unsafe {
        NtOpenFile(
            &raw mut handle,
            access,
            &raw const attributes,
            &raw mut status,
            share,
            options,
        )
    };
    if result < 0 {
        return Err(status_error(result));
    }
    if handle.is_null() {
        return Err(io::Error::other(
            "opening a directory entry returned no handle",
        ));
    }
    Ok(unsafe { File::from_raw_handle(handle) })
}

fn final_name(file: &File) -> io::Result<OsString> {
    let mut path = vec![0; 260];
    loop {
        let capacity = u32::try_from(path.len()).map_err(|_overflow| io::ErrorKind::InvalidData)?;
        let length = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                path.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_NONE,
            )
        };
        let length = usize::try_from(length).map_err(|_overflow| io::ErrorKind::InvalidData)?;
        if length == 0 {
            return Err(io::Error::last_os_error());
        }
        if length < path.len() {
            path.truncate(length);
            break;
        }
        path.resize(length, 0);
    }
    let name = path
        .rsplit(|unit| *unit == u16::from(b'\\'))
        .next()
        .unwrap_or_default();
    Ok(OsString::from_wide(name))
}

#[derive(PartialEq, Eq)]
enum Identity {
    Full { volume: u64, file: [u8; 16] },
    Indexed([u32; 3]),
}

pub(super) fn same(first: &File, second: &File) -> io::Result<bool> {
    Ok(identity(first)? == identity(second)?)
}

fn identity(file: &File) -> io::Result<Identity> {
    or_indexed(full_identity(file), || indexed_identity(file))
}

fn or_indexed(
    full: io::Result<Identity>,
    indexed: impl FnOnce() -> io::Result<Identity>,
) -> io::Result<Identity> {
    match full {
        Err(error)
            if error
                .raw_os_error()
                .and_then(|code| u32::try_from(code).ok())
                .is_some_and(|code| {
                    matches!(
                        code,
                        ERROR_INVALID_PARAMETER | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION
                    )
                }) =>
        {
            indexed()
        },
        full => full,
    }
}

fn full_identity(file: &File) -> io::Result<Identity> {
    let mut full = FILE_ID_INFO::default();
    let size =
        u32::try_from(size_of::<FILE_ID_INFO>()).map_err(|_overflow| io::ErrorKind::InvalidData)?;
    let read = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&raw mut full).cast(),
            size,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Identity::Full {
        volume: full.VolumeSerialNumber,
        file: full.FileId.Identifier,
    })
}

fn indexed_identity(file: &File) -> io::Result<Identity> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Identity::Indexed([
        info.dwVolumeSerialNumber,
        info.nFileIndexHigh,
        info.nFileIndexLow,
    ]))
}

#[repr(align(8))]
struct DirectoryBuffer([u8; 16 << 10]);

pub(super) fn visit(directory: &File, mut visit: impl FnMut(OsString)) -> io::Result<()> {
    let directory = open_relative(directory, OsStr::new(""), OpenKind::Directory, LIST)?;
    let mut buffer = DirectoryBuffer([0; 16 << 10]);
    let capacity =
        u32::try_from(buffer.0.len()).map_err(|_overflow| io::ErrorKind::InvalidInput)?;
    let mut restart = true;
    loop {
        let mut status = IO_STATUS_BLOCK::default();
        let result = unsafe {
            NtQueryDirectoryFile(
                directory.as_raw_handle(),
                ptr::null_mut(),
                None,
                ptr::null(),
                &raw mut status,
                buffer.0.as_mut_ptr().cast(),
                capacity,
                FileNamesInformation,
                false,
                ptr::null(),
                restart,
            )
        };
        if matches!(result, STATUS_NO_MORE_FILES | STATUS_NO_SUCH_FILE) {
            return Ok(());
        }
        if result < 0 {
            return Err(status_error(result));
        }
        let entries = buffer
            .0
            .get(..status.Information)
            .ok_or(io::ErrorKind::InvalidData)?;
        visit_entries(entries, &mut visit)?;
        restart = false;
    }
}

fn visit_entries(mut entries: &[u8], visit: &mut impl FnMut(OsString)) -> io::Result<()> {
    const OFFSET_BYTES: usize = offset_of!(FILE_NAMES_INFORMATION, NextEntryOffset);
    const LENGTH_BYTES: usize = offset_of!(FILE_NAMES_INFORMATION, FileNameLength);
    const NAME_BYTES: usize = offset_of!(FILE_NAMES_INFORMATION, FileName);

    loop {
        let next = field(entries, OFFSET_BYTES)?;
        let length = field(entries, LENGTH_BYTES)?;
        let end = NAME_BYTES
            .checked_add(length)
            .ok_or(io::ErrorKind::InvalidData)?;
        let name = entries
            .get(NAME_BYTES..end)
            .ok_or(io::ErrorKind::InvalidData)?;
        let (units, tail) = name.as_chunks::<2>();
        if !tail.is_empty() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let units: Vec<_> = units.iter().map(|unit| u16::from_ne_bytes(*unit)).collect();
        visit(OsString::from_wide(&units));
        if next == 0 {
            return Ok(());
        }
        if next < end {
            return Err(io::ErrorKind::InvalidData.into());
        }
        entries = entries.get(next..).ok_or(io::ErrorKind::InvalidData)?;
    }
}

fn field(entry: &[u8], offset: usize) -> io::Result<usize> {
    let bytes = entry
        .get(offset..)
        .and_then(|field| field.first_chunk::<4>())
        .ok_or(io::ErrorKind::InvalidData)?;
    usize::try_from(u32::from_ne_bytes(*bytes))
        .map_err(|_overflow| io::ErrorKind::InvalidData.into())
}

fn status_error(status: NTSTATUS) -> io::Error {
    let code = unsafe { RtlNtStatusToDosError(status) };
    io::Error::from_raw_os_error(code.cast_signed())
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;

    use super::*;

    fn failed(code: u32) -> io::Result<Identity> {
        Err(io::Error::from_raw_os_error(code.cast_signed()))
    }

    #[test]
    fn only_a_file_system_without_full_ids_falls_back_to_the_file_index() {
        let indexed = || Ok(Identity::Indexed([1, 2, 3]));
        for code in [
            ERROR_INVALID_PARAMETER,
            ERROR_NOT_SUPPORTED,
            ERROR_INVALID_FUNCTION,
        ] {
            assert!(matches!(
                or_indexed(failed(code), indexed),
                Ok(Identity::Indexed(_))
            ));
        }
        let denied = or_indexed(failed(ERROR_ACCESS_DENIED), indexed);
        assert!(
            denied.is_err_and(|error| error.raw_os_error() == Some(ERROR_ACCESS_DENIED.cast_signed())),
            "A failed 128-bit query fell back to an index that may not be unique"
        );
    }
}

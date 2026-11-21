use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata, OpenOptions},
    io,
    mem::{offset_of, size_of},
    os::windows::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::{MetadataExt as _, OpenOptionsExt as _},
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
            NTSTATUS, OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError,
            STATUS_NO_MORE_FILES, STATUS_NO_SUCH_FILE, UNICODE_STRING,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, GetFileInformationByHandle,
        },
        System::IO::IO_STATUS_BLOCK,
    },
};

const SHARE: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

pub(super) fn directory(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
}

pub(super) fn child(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
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
    let flags = FILE_OPEN_REPARSE_POINT
        | FILE_SYNCHRONOUS_IO_NONALERT
        | if directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        };
    let share = if directory {
        SHARE
    } else {
        FILE_SHARE_READ | FILE_SHARE_DELETE
    };
    let result = unsafe {
        NtOpenFile(
            &raw mut handle,
            FILE_GENERIC_READ,
            &raw const attributes,
            &raw mut status,
            share,
            flags,
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

pub(super) fn linked(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

pub(super) fn same(first: &File, second: &File) -> io::Result<bool> {
    fn identity(file: &File) -> io::Result<[u32; 3]> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &raw mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok([
            info.dwVolumeSerialNumber,
            info.nFileIndexHigh,
            info.nFileIndexLow,
        ])
    }
    Ok(identity(first)? == identity(second)?)
}

#[repr(align(8))]
struct DirectoryBuffer([u8; 16 << 10]);

pub(super) fn visit(directory: &File, mut visit: impl FnMut(OsString)) -> io::Result<()> {
    let directory = child(directory, OsStr::new(""), true)?;
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

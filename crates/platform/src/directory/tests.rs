use std::{fs, io::Read as _};

use super::*;

fn read(directory: &ReadDirectory, name: &str) -> anyhow::Result<String> {
    let (mut file, _) = directory.file(Path::new(name))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

#[test]
fn reads_keep_the_open_directory_across_a_rename() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let original = root.path().join("original");
    let renamed = root.path().join("renamed");
    fs::create_dir(&original)?;
    fs::write(original.join("public.txt"), "original")?;
    let opened = ReadDirectory::open(&original)?;
    fs::rename(&original, &renamed)?;
    fs::create_dir(&original)?;
    fs::write(original.join("public.txt"), "replacement")?;
    fs::write(original.join("new.txt"), "replacement")?;
    assert!(!opened.is_at(&original)?);
    assert!(opened.is_at(&renamed)?);
    let mut entries = Vec::new();
    for _ in 0..2 {
        opened.visit(|name| {
            if name != "." && name != ".." {
                entries.push(name);
            }
        })?;
    }
    assert_eq!(entries, vec![OsString::from("public.txt"); 2]);
    assert_eq!(read(&opened, "public.txt")?, "original");
    Ok(())
}

#[cfg(target_os = "windows")]
#[test]
fn only_stable_reads_exclude_writers() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let name = Path::new("content");
    let path = root.path().join(name);
    let writer = File::create_new(&path)?;
    let directory = ReadDirectory::open(root.path())?;
    assert!(directory.file(name).is_ok());
    assert!(directory.stable_file(name).is_err());
    drop(writer);

    let (reading, _) = directory.stable_file(name)?;
    assert!(fs::OpenOptions::new().write(true).open(&path).is_err());
    drop(reading);
    assert!(fs::OpenOptions::new().write(true).open(&path).is_ok());
    Ok(())
}

#[cfg(target_os = "windows")]
#[test]
fn a_short_name_cannot_reach_a_differently_named_entry() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let hidden = root.path().join(".expired-short-name-probe");
    fs::create_dir(&hidden)?;
    fs::write(hidden.join("public.txt"), "hidden")?;
    let directory = ReadDirectory::open(root.path())?;
    let opened = directory.child(OsStr::new(".EXPIRED-Short-Name-Probe"))?;
    assert_eq!(read(&opened, "PUBLIC.TXT")?, "hidden");

    let short = windows::short_name(&hidden)?;
    if short == hidden.file_name().unwrap_or_default() {
        eprintln!(
            "The temporary volume does not generate short names; the alias check is unexercised"
        );
        return Ok(());
    }
    assert!(directory.child(&short).is_err());
    assert!(
        directory
            .file(&Path::new(&short).join("public.txt"))
            .is_err()
    );
    Ok(())
}

#[cfg(target_os = "windows")]
#[test]
fn only_name_surrogate_reparse_points_are_refused() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let placeholder = root.path().join("placeholder.txt");
    fs::write(&placeholder, "placeholder")?;
    windows::tag(&placeholder, windows::PLACEHOLDER)?;
    let folder = root.path().join("folder");
    fs::create_dir(&folder)?;
    windows::tag(&folder, windows::PLACEHOLDER_FOLDER)?;
    fs::write(folder.join("inside.txt"), "inside")?;
    let link = root.path().join("link.txt");
    fs::write(&link, "link")?;
    windows::tag(&link, windows::SURROGATE)?;

    let directory = ReadDirectory::open(root.path())?;
    assert_eq!(read(&directory, "placeholder.txt")?, "placeholder");
    assert_eq!(
        read(&directory.child(OsStr::new("folder"))?, "inside.txt")?,
        "inside"
    );
    assert_eq!(
        read(&ReadDirectory::open(&folder)?, "inside.txt")?,
        "inside"
    );
    assert!(directory.file(Path::new("link.txt")).is_err());
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_searchable_directory_opens_without_list_permission() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir()?;
    let inner = root.path().join("inner");
    fs::create_dir(&inner)?;
    fs::write(inner.join("public.txt"), "public")?;
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o300))?;
    let directory = ReadDirectory::open(root.path())?;
    let opened = directory
        .child(OsStr::new("inner"))
        .map(|inner| read(&inner, "public.txt"));
    let rooted = ReadDirectory::open(&inner).map(|inner| read(&inner, "public.txt"));
    let listed = ReadDirectory::open(&inner).map(|inner| inner.visit(|_| {}).is_ok());
    fs::set_permissions(&inner, fs::Permissions::from_mode(0o700))?;
    assert_eq!(opened??, "public");
    assert_eq!(rooted??, "public");
    assert!(!listed?, "Listing a directory needs permission to read it");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_fifo_is_refused_without_waiting_for_a_writer() -> anyhow::Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt as _, sync::mpsc, thread, time::Duration};

    let root = tempfile::tempdir()?;
    let fifo = CString::new(root.path().join("fifo").as_os_str().as_bytes())?;
    if unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) } != 0 {
        return Err(io::Error::last_os_error().into());
    }
    let directory = ReadDirectory::open(root.path())?;
    let (opened, outcome) = mpsc::channel();
    thread::spawn(move || opened.send(directory.file(Path::new("fifo")).is_err()));
    assert_eq!(
        outcome.recv_timeout(Duration::from_secs(5)),
        Ok(true),
        "Opening a FIFO waited for a writer"
    );
    Ok(())
}

#[cfg(target_os = "windows")]
mod windows {
    use std::{
        ffi::OsString,
        fs::OpenOptions,
        io,
        os::windows::{
            ffi::{OsStrExt as _, OsStringExt as _},
            fs::OpenOptionsExt as _,
            io::AsRawHandle as _,
        },
        path::Path,
        ptr,
    };

    use windows_sys::Win32::{
        Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, GetShortPathNameW,
        },
        System::IO::DeviceIoControl,
    };

    const FSCTL_SET_REPARSE_POINT: u32 = 0x0009_00A4;
    pub(super) const PLACEHOLDER: u32 = 0x0000_7E57;
    pub(super) const PLACEHOLDER_FOLDER: u32 = 0x1000_7E57;
    pub(super) const SURROGATE: u32 = 0x2000_7E57;

    pub(super) fn short_name(path: &Path) -> io::Result<OsString> {
        let long: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
        let mut short = vec![0_u16; 1024];
        let length = unsafe {
            GetShortPathNameW(
                long.as_ptr(),
                short.as_mut_ptr(),
                u32::try_from(short.len()).unwrap(),
            )
        };
        if length == 0 || usize::try_from(length).unwrap() >= short.len() {
            return Err(io::Error::last_os_error());
        }
        short.truncate(usize::try_from(length).unwrap());
        let name = short
            .rsplit(|unit| *unit == u16::from(b'\\'))
            .next()
            .unwrap();
        Ok(OsString::from_wide(name))
    }

    pub(super) fn tag(path: &Path, tag: u32) -> io::Result<()> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut buffer = Vec::from(tag.to_le_bytes());
        buffer.extend([0; 4]);
        buffer.extend(*b"speakeasy probe!");
        let mut returned = 0;
        let set = unsafe {
            DeviceIoControl(
                file.as_raw_handle(),
                FSCTL_SET_REPARSE_POINT,
                buffer.as_ptr().cast(),
                u32::try_from(buffer.len()).unwrap(),
                ptr::null_mut(),
                0,
                &raw mut returned,
                ptr::null_mut(),
            )
        };
        if set == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

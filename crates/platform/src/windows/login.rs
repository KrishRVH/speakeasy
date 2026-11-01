use std::{env, io, iter, os::windows::ffi::OsStrExt as _, path::Path, ptr::null_mut};

use anyhow::anyhow;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
    System::Registry::{
        HKEY_CURRENT_USER, REG_ROUTINE_FLAGS, REG_SZ, REG_VALUE_TYPE, RRF_RT_ANY,
        RRF_RT_REG_BINARY, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    },
};

use super::to_wide;

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

const VALUE: &str = "Speakeasy";

pub fn start_at_login(on: bool) -> anyhow::Result<()> {
    if on {
        env::current_exe()
            .and_then(|exe| write(RUN, REG_SZ, &command(&exe)))
            .map_err(|error| {
                anyhow!(
                    "Cannot turn on Start at login: {error}. Allow Speakeasy in your security software, or turn off Start at login in Settings."
                )
            })
    } else {
        delete(RUN).map_err(|error| {
            anyhow!(
                "Cannot turn off Start at login: {error}. Turn Speakeasy off in Windows' Startup apps."
            )
        })
    }
}

#[must_use]
pub fn startup_approved() -> bool {
    approved(APPROVED)
}

fn approved(key: &str) -> bool {
    let mut data = [0; 12];
    approves(read(key, RRF_RT_REG_BINARY, &mut data).ok().flatten())
}

fn approves(value: Option<&[u8]>) -> bool {
    value
        .and_then(<[u8]>::first)
        .is_none_or(|flags| flags & 1 == 0)
}

fn command(exe: &Path) -> Vec<u8> {
    let quote = u16::from(b'"');
    iter::once(quote)
        .chain(exe.as_os_str().encode_wide())
        .chain([quote, 0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

fn write(key: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> io::Result<()> {
    let size = u32::try_from(data.len()).map_err(io::Error::other)?;
    let (key, value) = (to_wide(key), to_wide(VALUE));

    checked(unsafe {
        RegSetKeyValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            kind,
            data.as_ptr().cast(),
            size,
        )
    })
}

fn delete(key: &str) -> io::Result<()> {
    if !present(key)? {
        return Ok(());
    }
    let (key, value) = (to_wide(key), to_wide(VALUE));

    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
        ERROR_FILE_NOT_FOUND => Ok(()),
        status => checked(status),
    }
}

fn present(key: &str) -> io::Result<bool> {
    let (key, value) = (to_wide(key), to_wide(VALUE));

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_ANY,
            null_mut(),
            null_mut(),
            null_mut(),
        )
    };
    match status {
        ERROR_FILE_NOT_FOUND => Ok(false),
        status => checked(status).map(|()| true),
    }
}

fn read<'a>(
    key: &str,
    types: REG_ROUTINE_FLAGS,
    data: &'a mut [u8],
) -> io::Result<Option<&'a [u8]>> {
    let mut size = u32::try_from(data.len()).map_err(io::Error::other)?;
    let (key, value) = (to_wide(key), to_wide(VALUE));

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            types,
            null_mut(),
            data.as_mut_ptr().cast(),
            &raw mut size,
        )
    };
    match status {
        ERROR_FILE_NOT_FOUND => Ok(None),
        status => {
            checked(status)?;
            Ok(data.get(..usize::try_from(size).map_err(io::Error::other)?))
        },
    }
}

fn checked(status: WIN32_ERROR) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.cast_signed()))
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use windows_sys::Win32::System::Registry::{
        REG_BINARY, RRF_RT_REG_SZ, RegDeleteKeyW, RegDeleteTreeW,
    };

    use super::*;

    const PARENT: &str = r"Software\Speakeasy-tests";

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16().flat_map(u16::to_le_bytes).collect()
    }

    #[test]
    fn the_run_command_quotes_a_path_with_spaces() {
        assert_eq!(
            command(Path::new(r"C:\Program Files\Speakeasy\speakeasy.exe")),
            utf16("\"C:\\Program Files\\Speakeasy\\speakeasy.exe\"\0")
        );
    }

    #[test]
    fn only_the_off_flag_turns_the_sign_in_launch_off() {
        let disabled_at = [0x00, 0xA0, 0x0C, 0x06, 0x9D, 0x67, 0xDD, 0x01];
        let value = |flags: u8, time: [u8; 8]| [&[flags, 0, 0, 0][..], &time].concat();
        assert!(approves(None), "No value read as off");
        assert!(approves(Some(&value(2, [0; 8]))));
        assert!(approves(Some(&value(6, [0; 8]))));
        assert!(!approves(Some(&value(3, disabled_at))));
        assert!(approves(Some(&[])), "An empty value read as off");
    }

    struct Scratch(String);

    impl Scratch {
        fn new() -> anyhow::Result<Self> {
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
            let id = std::process::id();
            Ok(Self(format!(r"{PARENT}\login-{id}-{now}")))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let (key, parent) = (to_wide(&self.0), to_wide(PARENT));

            unsafe {
                RegDeleteTreeW(HKEY_CURRENT_USER, key.as_ptr());
                RegDeleteKeyW(HKEY_CURRENT_USER, parent.as_ptr());
            }
        }
    }

    #[test]
    #[ignore = "Writes, then deletes, a scratch key under HKCU\\Software\\Speakeasy-tests; never the Run or StartupApproved keys"]
    fn the_run_value_follows_the_switch_and_startup_apps_reads_back() -> anyhow::Result<()> {
        let scratch = Scratch::new()?;
        let (run, approval) = (
            format!(r"{}\Run", scratch.0),
            format!(r"{}\Approved", scratch.0),
        );
        let exe = Path::new(r"C:\Program Files\Speakeasy\speakeasy.exe");
        let mut data = [0; 512];

        delete(&run)?;
        write(&run, REG_SZ, &command(exe))?;
        assert_eq!(
            read(&run, RRF_RT_REG_SZ, &mut data)?,
            Some(&command(exe)[..]),
            "Turning on did not write the quoted path as a string"
        );
        delete(&run)?;
        assert_eq!(
            read(&run, RRF_RT_REG_SZ, &mut data)?,
            None,
            "Turning off left the value"
        );
        delete(&run)?;

        assert!(approved(&approval), "A missing approval read as off");
        for (flags, on) in [(2, true), (3, false), (6, true)] {
            write(
                &approval,
                REG_BINARY,
                &[flags, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            )?;
            assert_eq!(approved(&approval), on, "flags {flags}");
        }
        Ok(())
    }
}

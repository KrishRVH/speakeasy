//! Starting Speakeasy when the user signs in: the per-user Run value Windows launches, and the
//! `StartupApproved` value through which Windows' Startup apps page turns that launch off.

use std::{env, io, iter, os::windows::ffi::OsStrExt as _, path::Path, ptr::null_mut};

use anyhow::anyhow;
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR},
    System::Registry::{
        HKEY_CURRENT_USER, REG_ROUTINE_FLAGS, REG_SZ, REG_VALUE_TYPE, RRF_RT_REG_BINARY,
        RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW,
    },
};

use super::to_wide;

/// The per-user key whose values Windows runs at sign-in, each a command line.
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Windows' Startup apps page records which values of [`RUN`] it lets start.
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
/// The value Speakeasy owns in both keys.
const VALUE: &str = "Speakeasy";

/// Makes Windows start this executable when the user signs in, or stop doing so.
///
/// Turning it on writes the running executable's path, so the entry follows a moved or updated
/// copy; turning it off deletes the entry if there is one.
///
/// # Errors
/// Fails, with a remedy, when this executable's path is unavailable or the registry refuses the
/// change, as a security policy may.
pub fn start_at_login(on: bool) -> anyhow::Result<()> {
    let result = if on {
        env::current_exe().and_then(|exe| set_run(RUN, Some(&exe)))
    } else {
        set_run(RUN, None)
    };
    result.map_err(|error| {
        if on {
            anyhow!(
                "Cannot turn on Start at login: {error}. Allow Speakeasy in your security software, or turn off Start at login in Settings."
            )
        } else {
            anyhow!(
                "Cannot turn off Start at login: {error}. Turn Speakeasy off in Windows' Startup apps."
            )
        }
    })
}

/// Whether Windows' Startup apps page lets the sign-in launch start. An entry the page never
/// toggled has no approval value, which counts as allowed, and so does one Speakeasy cannot read.
#[must_use]
pub fn startup_approved() -> bool {
    approved(APPROVED)
}

/// Writes the command that starts `exe` as [`VALUE`] under the Run `key`, or deletes the value
/// when there is no `exe` to start.
fn set_run(key: &str, exe: Option<&Path>) -> io::Result<()> {
    match exe {
        Some(exe) => write(key, REG_SZ, &command(exe)),
        None => delete(key),
    }
}

/// Whether the `StartupApproved` value under `key` lets the Run value start, as [`approves`]
/// decodes it. A value longer than Explorer writes, or of another type, cannot be read, so it
/// counts as allowed.
fn approved(key: &str) -> bool {
    let mut data = [0; 12];
    approves(read(key, RRF_RT_REG_BINARY, &mut data).ok().flatten())
}

/// Whether a `StartupApproved` `value` lets its Run value start. Explorer writes 12 bytes: a
/// little-endian DWORD of flags, 2 or 6 while the entry is on and 3 once the user turns it off,
/// then the FILETIME at which they did, zero while it is on. Bit 0 marks it off; no value means
/// the page never toggled it, so it is on.
fn approves(value: Option<&[u8]>) -> bool {
    value
        .and_then(<[u8]>::first)
        .is_none_or(|flags| flags & 1 == 0)
}

/// The `REG_SZ` data that starts `exe`, as NUL-terminated UTF-16: its path in quotes, so a path
/// with spaces names one program. Windows paths cannot contain quotes, so none needs escaping.
fn command(exe: &Path) -> Vec<u8> {
    let quote = u16::from(b'"');
    iter::once(quote)
        .chain(exe.as_os_str().encode_wide())
        .chain([quote, 0])
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Writes `data` as [`VALUE`] of type `kind` under `key`, creating the key if it is missing.
fn write(key: &str, kind: REG_VALUE_TYPE, data: &[u8]) -> io::Result<()> {
    let size = u32::try_from(data.len()).map_err(io::Error::other)?;
    let (key, value) = (to_wide(key), to_wide(VALUE));
    // SAFETY: both names are NUL-terminated, and `size` bytes of `data` are readable; all outlive
    // the call.
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

/// Deletes [`VALUE`] under `key`; a value or key that is already gone counts as deleted.
fn delete(key: &str) -> io::Result<()> {
    let (key, value) = (to_wide(key), to_wide(VALUE));
    // SAFETY: both names are NUL-terminated and outlive the call.
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) } {
        ERROR_FILE_NOT_FOUND => Ok(()),
        status => checked(status),
    }
}

/// Reads [`VALUE`] under `key` into `data` if it has a type `types` allows and fits, returning
/// the bytes read, or `None` when the key or value is absent.
fn read<'a>(
    key: &str,
    types: REG_ROUTINE_FLAGS,
    data: &'a mut [u8],
) -> io::Result<Option<&'a [u8]>> {
    let mut size = u32::try_from(data.len()).map_err(io::Error::other)?;
    let (key, value) = (to_wide(key), to_wide(VALUE));
    // SAFETY: both names are NUL-terminated, `size` is the length of the writable `data`, and a
    // null type pointer skips the type; all outlive the call.
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

    /// Holds every scratch key, so a test never names a key Windows reads.
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
        // 2026-10-29 12:00 UTC as a FILETIME.
        let disabled_at = [0x00, 0xA0, 0x0C, 0x06, 0x9D, 0x67, 0xDD, 0x01];
        let value = |flags: u8, time: [u8; 8]| [&[flags, 0, 0, 0][..], &time].concat();
        assert!(approves(None), "No value read as off");
        assert!(approves(Some(&value(2, [0; 8]))));
        assert!(approves(Some(&value(6, [0; 8]))));
        assert!(!approves(Some(&value(3, disabled_at))));
        assert!(approves(Some(&[])), "An empty value read as off");
    }

    /// A uniquely named key under [`PARENT`], deleted with everything in it, and the parent once
    /// empty, on drop.
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
            // SAFETY: both names are NUL-terminated and outlive the calls. Given a subkey name,
            // RegDeleteTreeW deletes that key and its contents; RegDeleteKeyW deletes the parent
            // only once no other run's key remains in it.
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

        set_run(&run, Some(exe))?;
        assert_eq!(
            read(&run, RRF_RT_REG_SZ, &mut data)?,
            Some(&command(exe)[..]),
            "Turning on did not write the quoted path as a string"
        );
        set_run(&run, None)?;
        assert_eq!(
            read(&run, RRF_RT_REG_SZ, &mut data)?,
            None,
            "Turning off left the value"
        );
        set_run(&run, None)?;

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

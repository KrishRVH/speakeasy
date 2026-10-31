use std::{ffi::OsString, io, os::windows::ffi::OsStringExt as _, path::PathBuf};

use anyhow::{Context as _, ensure};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW},
};

use super::to_wide;

/// Reads the per-user Windows installer's registered directory without changing it.
/// A portable copy has no registration. Call off the UI thread; registry access can block.
///
/// # Errors
/// Fails when the registry is inaccessible or contains an invalid installation path.
pub fn installed_directory() -> anyhow::Result<Option<PathBuf>> {
    let key = to_wide(
        r"Software\Microsoft\Windows\CurrentVersion\Uninstall\io.github.krvh.speakeasy_is1",
    );
    let value = to_wide("InstallLocation");
    let mut buffer = vec![0_u16; 32_768];
    let mut length = u32::try_from(buffer.len().saturating_mul(2))?;
    // SAFETY: names are NUL-terminated and the aligned buffer has `length` writable bytes.
    // All pointers remain valid for this synchronous, read-only registry call.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &raw mut length,
        )
    };
    if status == ERROR_FILE_NOT_FOUND {
        return Ok(None);
    }
    if status != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(status.cast_signed())).context(
            "Cannot read the Windows installation. Open the release page to update manually.",
        );
    }
    ensure!(
        length >= 2 && length.is_multiple_of(2),
        "The Windows installation path is invalid."
    );
    let units = usize::try_from(length / 2)?;
    let text = buffer
        .get(..units)
        .context("The Windows installation path is too long.")?;
    ensure!(
        text.last() == Some(&0),
        "The Windows installation path is invalid."
    );
    let path = text
        .get(..units.saturating_sub(1))
        .context("The Windows installation path is invalid.")?;
    ensure!(
        !path.contains(&0),
        "The Windows installation path is invalid."
    );
    let path = PathBuf::from(OsString::from_wide(path));
    ensure!(
        path.is_absolute(),
        "The Windows installation path is not absolute."
    );
    Ok(Some(path))
}

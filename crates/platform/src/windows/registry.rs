use std::{ffi::OsString, io, os::windows::ffi::OsStringExt as _, ptr::null_mut};

use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
    System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW},
};

use super::to_wide;

const LONGEST_UNITS: usize = 32_768;

pub(crate) fn text(key: &str, value: &str) -> io::Result<Option<OsString>> {
    let (key, value) = (to_wide(key), to_wide(value));
    let mut buffer = vec![0_u16; LONGEST_UNITS];
    let mut length = u32::try_from(buffer.len().saturating_mul(2)).map_err(io::Error::other)?;

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            null_mut(),
            buffer.as_mut_ptr().cast(),
            &raw mut length,
        )
    };
    match status {
        ERROR_SUCCESS => {},
        ERROR_FILE_NOT_FOUND => return Ok(None),
        status => return Err(io::Error::from_raw_os_error(status.cast_signed())),
    }
    let units = usize::try_from(length / 2).map_err(io::Error::other)?;
    let terminated = buffer
        .get(..units)
        .filter(|_| length.is_multiple_of(2))
        .and_then(<[u16]>::split_last);
    match terminated {
        Some((0, text)) if !text.contains(&0) => Ok(Some(OsString::from_wide(text))),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "The registry value is not one terminated string",
        )),
    }
}

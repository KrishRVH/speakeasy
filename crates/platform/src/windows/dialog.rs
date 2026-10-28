//! The Windows open-file dialog, run on a thread of its own.

use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf, ptr::null, thread};

use anyhow::bail;
use windows_sys::Win32::{
    Foundation::{FALSE, HWND},
    System::Com::{
        COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
    },
    UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_HIDEREADONLY,
        OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    },
};

use super::to_wide;
use crate::{FileFilter, NativeWindow};

/// The extended-length path limit: 32,767 UTF-16 units plus NUL.
const LONGEST_PATH: usize = 32_768;

impl NativeWindow {
    /// Asks for one existing file in a dialog this window owns, offering `filter` before all files,
    /// without blocking the UI owner.
    ///
    /// The dialog runs its own message loop on a dedicated thread; shown from GPUI's UI thread, it
    /// stays unpainted while GPUI is idle.
    ///
    /// # Errors
    /// Fails when the native dialog or its thread fails; cancellation returns no path.
    pub fn choose_file(
        &self,
        title: &str,
        filter: FileFilter<'_>,
    ) -> impl Future<Output = anyhow::Result<Option<PathBuf>>> + Send + use<> {
        // A raw HWND is not Send, so the dialog thread receives its value.
        let owner = self.native.hwnd as isize;
        let title = to_wide(title);
        let FileFilter { name, pattern } = filter;
        // The empty entry and to_wide's NUL end the list with the double NUL lpstrFilter requires.
        let filter = to_wide(&[name, pattern, "All files", "*.*", ""].join("\0"));
        async move {
            let (reply, selection) = async_channel::bounded(1);
            // Detached: the modal dialog cannot be cancelled, so dropping the future must not join
            // it; process exit reclaims the thread.
            thread::Builder::new()
                .name("file-dialog".into())
                .spawn(move || {
                    #[expect(
                        clippy::let_underscore_must_use,
                        reason = "A closed completion lane means the settings owner no longer needs the dialog result"
                    )]
                    let _ = reply.send_blocking(open_file(owner as HWND, &title, &filter));
                })?;
            selection.recv().await?
        }
    }
}

fn open_file(owner: HWND, title: &[u16], filter: &[u16]) -> anyhow::Result<Option<PathBuf>> {
    let mut file = vec![0_u16; LONGEST_PATH];
    let mut dialog = OPENFILENAMEW {
        lStructSize: u32::try_from(size_of::<OPENFILENAMEW>())?,
        hwndOwner: owner,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: u32::try_from(file.len())?,
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_FILEMUSTEXIST
            | OFN_PATHMUSTEXIST
            | OFN_HIDEREADONLY
            | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    // SAFETY: this thread owns the dialog's apartment and message loop, and every buffer in
    // `dialog` outlives the synchronous call.
    let error = unsafe {
        let apartment = CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).cast_unsigned(),
        );
        let error = if GetOpenFileNameW(&raw mut dialog) == FALSE {
            CommDlgExtendedError()
        } else {
            0
        };
        if apartment >= 0 {
            CoUninitialize();
        }
        error
    };
    if error != 0 {
        bail!("The file dialog failed with code {error:#x}");
    }
    Ok(file
        .split(|&unit| unit == 0)
        .next()
        .filter(|path| !path.is_empty())
        .map(|path| OsString::from_wide(path).into()))
}

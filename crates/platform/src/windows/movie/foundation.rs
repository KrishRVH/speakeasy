use std::marker::PhantomData;

use windows::Win32::{
    Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup},
    System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
};

pub(super) struct Foundation {
    _thread: PhantomData<*mut ()>,
}

impl Foundation {
    pub(super) fn start() -> anyhow::Result<Self> {
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };

        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            unsafe { CoUninitialize() };
            return Err(error.into());
        }
        Ok(Self {
            _thread: PhantomData,
        })
    }
}

impl Drop for Foundation {
    fn drop(&mut self) {
        unsafe {
            #[expect(
                clippy::let_underscore_must_use,
                reason = "Teardown has no observer on partial initialization or unwind; the checkpoint retains any partial movie"
            )]
            let _ = MFShutdown();
            CoUninitialize();
        }
    }
}

//! Media Foundation and its COM apartment outlive all native codec objects on their owner thread.

use std::marker::PhantomData;

use windows::Win32::{
    Media::MediaFoundation::{MF_VERSION, MFSTARTUP_FULL, MFShutdown, MFStartup},
    System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoUninitialize},
};

pub(super) struct Foundation {
    // COM apartment cleanup must run on its initializing thread, including on partial open failure.
    _thread: PhantomData<*mut ()>,
}

impl Foundation {
    pub(super) fn start() -> anyhow::Result<Self> {
        // SAFETY: this owned capture thread uses the MTA. Every successful initialization has a
        // matching uninitialize on this thread, after its Media Foundation objects are released.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
        // SAFETY: balanced with MFShutdown after every object this startup creates is released.
        if let Err(error) = unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) } {
            // SAFETY: the preceding CoInitializeEx succeeded on this thread.
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
        // SAFETY: field/local drop order has released every native object before this guard.
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

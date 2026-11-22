use std::{
    fs::File,
    os::unix::fs::FileExt as _,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Mutex, MutexGuard, PoisonError},
};

use anyhow::Context as _;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{
    AllocAnyThread, DefinedClass, define_class, msg_send, rc::Retained, runtime::ProtocolObject,
};
use objc2_av_foundation::{
    AVAssetResourceLoader, AVAssetResourceLoaderDelegate,
    AVAssetResourceLoadingContentInformationRequest, AVAssetResourceLoadingDataRequest,
    AVAssetResourceLoadingRequest, AVFileTypeMPEG4, AVURLAsset,
};
use objc2_foundation::{NSData, NSError, NSObject, NSObjectProtocol, NSURL, ns_string};
use speakeasy_show::MoviePrefix;

const RESOURCE: &str = "speakeasy-movie://local/recording.mp4";

const READ_CHUNK: usize = 64 * 1024;

struct Input {
    #[expect(
        clippy::disallowed_types,
        reason = "Native resource callbacks may still be reading when the owner retires the validated handle; this lock acknowledges each read before closing it"
    )]
    file: Mutex<Option<File>>,
    bytes: u64,
}

impl Input {
    fn read(&self, buffer: &mut [u8], at: u64) -> anyhow::Result<()> {
        let lease = self.lease();
        let file = lease.as_ref().context("Video input has closed")?;
        let result = file.read_exact_at(buffer, at);
        drop(lease);
        result?;
        Ok(())
    }

    fn close(&self) {
        *self.lease() = None;
    }

    fn lease(&self) -> MutexGuard<'_, Option<File>> {
        self.file.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

define_class!(
    #[unsafe(super = NSObject)]
    #[ivars = Input]
    struct Reader;

    unsafe impl NSObjectProtocol for Reader {}

    unsafe impl AVAssetResourceLoaderDelegate for Reader {
        #[unsafe(method(resourceLoader:shouldWaitForLoadingOfRequestedResource:))]
        fn load(&self, _: &AVAssetResourceLoader, request: &AVAssetResourceLoadingRequest) -> bool {
            if !matches!(
                catch_unwind(AssertUnwindSafe(|| self.respond(request))),
                Ok(Ok(()))
            ) {
                let error = unsafe {
                    NSError::errorWithDomain_code_userInfo(
                        ns_string!("SpeakeasyMovieRead"),
                        1,
                        None,
                    )
                };

                unsafe {
                    request.finishLoadingWithError(Some(&error));
                }
            }
            true
        }
    }
);

impl Reader {
    #[expect(
        clippy::disallowed_types,
        reason = "The native file lease must synchronize callback reads with explicit owner teardown"
    )]
    fn new(file: File, prefix: &MoviePrefix) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Input {
            file: Mutex::new(Some(file)),
            bytes: prefix.bytes(),
        });

        unsafe { msg_send![super(this), init] }
    }

    fn respond(&self, request: &AVAssetResourceLoadingRequest) -> anyhow::Result<()> {
        if let Some(info) = unsafe { request.contentInformationRequest() } {
            self.describe(&info)?;
        }
        if let Some(data) = unsafe { request.dataRequest() } {
            self.serve(&data, request)?;
        }
        if !unsafe { request.isCancelled() } {
            unsafe { request.finishLoading() };
        }
        Ok(())
    }

    fn describe(
        &self,
        info: &AVAssetResourceLoadingContentInformationRequest,
    ) -> anyhow::Result<()> {
        let kind = unsafe { AVFileTypeMPEG4 }.context("The native MP4 type is unavailable")?;
        let length = i64::try_from(self.ivars().bytes)?;
        unsafe {
            info.setContentType(Some(kind));
            info.setContentLength(length);
            info.setByteRangeAccessSupported(true);
            info.setEntireLengthAvailableOnDemand(true);
        }
        Ok(())
    }

    fn serve(
        &self,
        data: &AVAssetResourceLoadingDataRequest,
        request: &AVAssetResourceLoadingRequest,
    ) -> anyhow::Result<()> {
        let input = self.ivars();
        let start = u64::try_from(unsafe { data.requestedOffset() })?;
        let mut position = u64::try_from(unsafe { data.currentOffset() })?;
        let end = if unsafe { data.requestsAllDataToEndOfResource() } {
            input.bytes
        } else {
            start
                .checked_add(u64::try_from(unsafe { data.requestedLength() })?)
                .context("Video byte range overflowed")?
                .min(input.bytes)
        };
        let mut buffer = vec![0; READ_CHUNK];
        while position < end && !unsafe { request.isCancelled() } {
            let count = usize::try_from(end.saturating_sub(position))?.min(READ_CHUNK);
            let bytes = buffer
                .get_mut(..count)
                .context("Video read exceeded its buffer")?;
            input.read(bytes, position)?;
            unsafe { data.respondWithData(&NSData::with_bytes(bytes)) };
            position = position
                .checked_add(u64::try_from(count)?)
                .context("Video byte position overflowed")?;
        }
        Ok(())
    }
}

pub(super) struct Asset {
    pub(super) native: Retained<AVURLAsset>,
    reader: Retained<Reader>,
    _queue: DispatchRetained<DispatchQueue>,
}

impl Asset {
    pub(super) fn new(file: File, prefix: &MoviePrefix) -> anyhow::Result<Self> {
        let reader = Reader::new(file, prefix);
        let queue = DispatchQueue::new("Speakeasy movie input", None);
        let url = NSURL::URLWithString(ns_string!(RESOURCE))
            .context("Cannot create the local movie resource")?;

        let native = unsafe {
            let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
            asset
                .resourceLoader()
                .setDelegate_queue(Some(ProtocolObject::from_ref(&*reader)), Some(&queue));
            asset
        };
        Ok(Self {
            native,
            reader,
            _queue: queue,
        })
    }
}

impl Drop for Asset {
    fn drop(&mut self) {
        unsafe {
            self.native.cancelLoading();
            self.native.resourceLoader().setDelegate_queue(None, None);
        }
        self.reader.ivars().close();
    }
}

use std::{
    fs::File,
    os::unix::fs::FileExt as _,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Mutex, PoisonError},
};

use anyhow::{Context as _, ensure};
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::{
    AllocAnyThread, DefinedClass, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, ProtocolObject},
};
use objc2_av_foundation::{
    AVAssetReferenceRestrictions, AVAssetResourceLoader, AVAssetResourceLoaderDelegate,
    AVAssetResourceLoadingRequest, AVFileTypeMPEG4, AVURLAsset, AVURLAssetReferenceRestrictionsKey,
};
use objc2_foundation::{
    NSData, NSDictionary, NSError, NSNumber, NSObject, NSObjectProtocol, NSURL, ns_string,
};
use speakeasy_show::MoviePrefix;

struct Input {
    #[expect(
        clippy::disallowed_types,
        reason = "Native resource callbacks may still be reading when the owner retires the validated handle; this lock acknowledges each read before closing it"
    )]
    file: Mutex<Option<File>>,
    bytes: u64,
}

impl Input {
    fn read(&self, bytes: &mut [u8], at: u64) -> anyhow::Result<()> {
        let lease = self.file.lock().unwrap_or_else(PoisonError::into_inner);
        let file = lease.as_ref().context("Video input has closed")?;
        ensure!(file.metadata()?.len() == self.bytes, "Video length changed");
        let result = file.read_exact_at(bytes, at);
        drop(lease);
        result?;
        Ok(())
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
            bytes: prefix.bytes,
        });

        unsafe { msg_send![super(this), init] }
    }

    fn respond(&self, request: &AVAssetResourceLoadingRequest) -> anyhow::Result<()> {
        let input = self.ivars();

        unsafe {
            ensure!(
                request.request().URL().and_then(|url| url.absoluteString())
                    .is_some_and(|url| *url == *ns_string!("speakeasy-movie://local/recording.mp4")),
                "Video requested an unsupported resource"
            );
            if let Some(info) = request.contentInformationRequest() {
                let kind = AVFileTypeMPEG4.context("The native MP4 type is unavailable")?;
                ensure!(
                    info.allowedContentTypes()
                        .is_none_or(|types| types.is_empty()
                            || types.iter().any(|allowed| *allowed == *kind)),
                    "Video requested an unsupported content type"
                );
                info.setContentType(Some(kind));
                info.setContentLength(i64::try_from(input.bytes)?);
                info.setByteRangeAccessSupported(true);
                info.setEntireLengthAvailableOnDemand(true);
            }
            if let Some(data) = request.dataRequest() {
                let start = u64::try_from(data.requestedOffset())?;
                let mut position = u64::try_from(data.currentOffset())?;
                let end = if data.requestsAllDataToEndOfResource() {
                    input.bytes
                } else {
                    start
                        .checked_add(u64::try_from(data.requestedLength())?)
                        .context("Video byte range overflowed")?
                        .min(input.bytes)
                };
                ensure!(position >= start, "Video byte range moved backwards");
                let mut buffer = vec![0; 64 * 1024];
                while position < end && !request.isCancelled() {
                    let count = usize::try_from(end.saturating_sub(position).min(64 * 1024))?;
                    let bytes = buffer
                        .get_mut(..count)
                        .context("Video read exceeded its buffer")?;
                    input.read(bytes, position)?;
                    data.respondWithData(&NSData::with_bytes(bytes));
                    position = position
                        .checked_add(u64::try_from(count)?)
                        .context("Video byte position overflowed")?;
                }
            }
            if !request.isCancelled() {
                request.finishLoading();
            }
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
        let url = NSURL::URLWithString(ns_string!("speakeasy-movie://local/recording.mp4"))
            .context("Cannot create the local movie resource")?;
        let restrictions = NSNumber::new_usize(AVAssetReferenceRestrictions::ForbidAll.0);

        let native = unsafe {
            let options = NSDictionary::from_slices(
                &[AVURLAssetReferenceRestrictionsKey],
                &[&*restrictions as &AnyObject],
            );
            let asset = AVURLAsset::URLAssetWithURL_options(&url, Some(&options));
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
        *self
            .reader
            .ivars()
            .file
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }
}

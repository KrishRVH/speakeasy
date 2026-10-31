//! A byte-capped native movie stream. Media Foundation's asynchronous writes and seeks share one
//! cursor; the cap is enforced at every actual write, including writes deferred by the encoder.

use std::{
    convert::Infallible,
    ffi::c_void,
    fs::{File, OpenOptions},
    io::{Read as _, Seek as _, SeekFrom, Write as _},
    path::Path,
    sync::{Mutex, MutexGuard},
};

use windows::Win32::{
    Foundation::{
        E_FAIL, E_NOTIMPL, E_POINTER, S_FALSE, S_OK, STG_E_ACCESSDENIED, STG_E_INVALIDFUNCTION,
        STG_E_MEDIUMFULL, STG_E_READFAULT, STG_E_WRITEFAULT,
    },
    Media::MediaFoundation::{IMFByteStream, MFCreateMFByteStreamOnStream},
    System::Com::{
        ISequentialStream_Impl, IStream, IStream_Impl, LOCKTYPE, STATFLAG, STATSTG, STGC,
        STGM_READ, STGM_READWRITE, STGTY_STREAM, STREAM_SEEK, STREAM_SEEK_CUR, STREAM_SEEK_END,
        STREAM_SEEK_SET,
    },
};
use windows_core::{Error, HRESULT, Ref, Result};

pub(super) struct Stream {
    pub native: IMFByteStream,
    // Finalize closes IMFByteStream. This owned duplicate can still sync its last fragment, and
    // drops before the closure receipt, so no movie file handle survives the checkpoint lease.
    pub sync: File,
    // Released after the native stream: an asynchronous callback may still own its IStream.
    _closed: Closed,
}

impl Drop for Stream {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Finalize may already close the stream; failed initialization/finalization still explicitly cancels outstanding I/O before waiting for actual file closure"
        )]
        // SAFETY: this stream is still owned; all writer submissions have stopped before teardown.
        let _ = unsafe { self.native.Close() };
    }
}

pub(super) struct ReadStream {
    pub native: IMFByteStream,
    _closed: Closed,
}

impl Drop for ReadStream {
    fn drop(&mut self) {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "Reader teardown must cancel asynchronous I/O before waiting for the last callback to release its file"
        )]
        // SAFETY: the reader is gone; this owner retains the stream until all callbacks release.
        let _ = unsafe { self.native.Close() };
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    Write,
}

#[expect(
    clippy::disallowed_types,
    reason = "Media Foundation reads and seeks on its own I/O threads; those callbacks must share the validated file's cursor"
)]
pub(super) fn read(mut file: File) -> anyhow::Result<ReadStream> {
    let limit = file.metadata()?.len();
    file.rewind()?;
    let (released, closed) = async_channel::bounded(1);
    let stream: IStream = CappedFile {
        file: Mutex::new(file),
        limit,
        access: Access::Read,
        _released: released,
    }
    .into();
    Ok(ReadStream {
        // SAFETY: the wrapper retains the owned file and serializes native I/O callbacks.
        native: unsafe { MFCreateMFByteStreamOnStream(&stream) }?,
        _closed: Closed(closed),
    })
}

struct Closed(async_channel::Receiver<Infallible>);

impl Drop for Closed {
    fn drop(&mut self) {
        let Err(_closed) = self.0.recv_blocking();
    }
}

#[expect(
    clippy::disallowed_types,
    reason = "Native stream callbacks run on Media Foundation I/O threads; seek and capped write must share one atomic cursor operation"
)]
pub(super) fn create(path: &Path, limit: u64) -> anyhow::Result<Stream> {
    let file = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .open(path)?;
    let sync = file.try_clone()?;
    let (released, closed) = async_channel::bounded(1);
    let stream: IStream = CappedFile {
        file: Mutex::new(file),
        limit,
        access: Access::Write,
        _released: released,
    }
    .into();
    // SAFETY: the COM stream owns its file and serializes every native callback. The returned
    // wrapper retains its own reference until all asynchronous I/O has finished.
    Ok(Stream {
        // SAFETY: the wrapper retains this owned COM stream through all asynchronous callbacks.
        native: unsafe { MFCreateMFByteStreamOnStream(&stream) }?,
        sync,
        _closed: Closed(closed),
    })
}

#[expect(
    clippy::ref_as_ptr,
    reason = "windows::implement generates COM vtable pointer conversions"
)]
#[expect(
    clippy::inline_always,
    reason = "windows::implement generates inline COM interface conversions"
)]
mod com {
    use windows_core::implement;

    use super::{Access, File, IStream, Infallible, Mutex};

    #[implement(IStream)]
    pub(super) struct CappedFile {
        // Native I/O callbacks share an atomic seek, cap check and write; the capture owner cannot
        // order work performed inside Media Foundation's asynchronous codec/stream calls.
        #[expect(
            clippy::disallowed_types,
            reason = "Native callbacks need one cursor and cap check held across each file operation"
        )]
        pub(super) file: Mutex<File>,
        pub(super) limit: u64,
        pub(super) access: Access,
        // Field order closes the actual file before acknowledging asynchronous COM teardown.
        pub(super) _released: async_channel::Sender<Infallible>,
    }
}

use com::{CappedFile, CappedFile_Impl};

impl CappedFile_Impl {
    fn file(&self) -> Result<MutexGuard<'_, File>> {
        self.file
            .lock()
            .map_err(|_poisoned| Error::from_hresult(E_FAIL))
    }
}

fn io_error(code: HRESULT) -> impl FnOnce(std::io::Error) -> Error {
    move |_private_error| Error::from_hresult(code)
}

impl ISequentialStream_Impl for CappedFile_Impl {
    fn Read(&self, destination: *mut c_void, count: u32, read: *mut u32) -> HRESULT {
        let result = (|| {
            if !read.is_null() {
                // SAFETY: ISequentialStream permits a null count; a non-null pointer is writable.
                unsafe { read.write(0) };
            }
            if count == 0 {
                return Ok(S_OK);
            }
            if destination.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            let count = usize::try_from(count)
                .map_err(|_invalid_count| Error::from_hresult(STG_E_READFAULT))?;
            // SAFETY: the native caller supplies a writable buffer of count bytes for this call.
            let bytes = unsafe { std::slice::from_raw_parts_mut(destination.cast(), count) };
            let read_bytes = self
                .file()?
                .read(bytes)
                .map_err(io_error(STG_E_READFAULT))?;
            if !read.is_null() {
                // SAFETY: same caller-owned output pointer; the read is no larger than count.
                unsafe {
                    read.write(
                        u32::try_from(read_bytes)
                            .map_err(|_invalid_count| Error::from_hresult(STG_E_READFAULT))?,
                    );
                };
            }
            Ok(if read_bytes == count { S_OK } else { S_FALSE })
        })();
        result.unwrap_or_else(|error: Error| error.code())
    }

    fn Write(&self, source: *const c_void, count: u32, written: *mut u32) -> HRESULT {
        let result = (|| {
            if !written.is_null() {
                // SAFETY: ISequentialStream permits a null count; a non-null pointer is writable.
                unsafe { written.write(0) };
            }
            if self.access == Access::Read {
                return Err(Error::from_hresult(STG_E_ACCESSDENIED));
            }
            if count == 0 {
                return Ok(());
            }
            if source.is_null() {
                return Err(Error::from_hresult(E_POINTER));
            }
            let mut file = self.file()?;
            let position = file.stream_position().map_err(io_error(STG_E_WRITEFAULT))?;
            if position
                .checked_add(u64::from(count))
                .is_none_or(|end| end > self.limit)
            {
                return Err(Error::from_hresult(STG_E_MEDIUMFULL));
            }
            let length = usize::try_from(count)
                .map_err(|_invalid_count| Error::from_hresult(STG_E_WRITEFAULT))?;
            // SAFETY: the native caller supplies count readable bytes; the slice never escapes.
            let bytes = unsafe { std::slice::from_raw_parts(source.cast(), length) };
            file.write_all(bytes).map_err(io_error(STG_E_WRITEFAULT))?;
            drop(file);
            if !written.is_null() {
                // SAFETY: caller-owned output pointer is valid for this synchronous call.
                unsafe { written.write(count) };
            }
            Ok(())
        })();
        result.map_or_else(|error: Error| error.code(), |()| S_OK)
    }
}

impl IStream_Impl for CappedFile_Impl {
    fn Seek(&self, offset: i64, origin: STREAM_SEEK, position: *mut u64) -> Result<()> {
        let mut file = self.file()?;
        let base = match origin {
            STREAM_SEEK_SET => 0,
            STREAM_SEEK_CUR => file.stream_position().map_err(io_error(STG_E_READFAULT))?,
            STREAM_SEEK_END => file.metadata().map_err(io_error(STG_E_READFAULT))?.len(),
            _ => return Err(Error::from_hresult(STG_E_INVALIDFUNCTION)),
        };
        let next = base
            .checked_add_signed(offset)
            .filter(|next| *next <= self.limit)
            .ok_or_else(|| Error::from_hresult(STG_E_INVALIDFUNCTION))?;
        file.seek(SeekFrom::Start(next))
            .map_err(io_error(STG_E_WRITEFAULT))?;
        drop(file);
        if !position.is_null() {
            // SAFETY: IStream's optional position output is writable when supplied.
            unsafe { position.write(next) };
        }
        Ok(())
    }

    fn SetSize(&self, length: u64) -> Result<()> {
        if self.access == Access::Read {
            return Err(Error::from_hresult(STG_E_ACCESSDENIED));
        }
        if length > self.limit {
            return Err(Error::from_hresult(STG_E_MEDIUMFULL));
        }
        self.file()?
            .set_len(length)
            .map_err(io_error(STG_E_WRITEFAULT))
    }

    fn Commit(&self, _: &STGC) -> Result<()> {
        if self.access == Access::Read {
            return Ok(());
        }
        self.file()?.sync_data().map_err(io_error(STG_E_WRITEFAULT))
    }

    fn Stat(&self, output: *mut STATSTG, _: &STATFLAG) -> Result<()> {
        if output.is_null() {
            return Err(Error::from_hresult(E_POINTER));
        }
        let size = self
            .file()?
            .metadata()
            .map_err(io_error(STG_E_READFAULT))?
            .len();
        // SAFETY: IStream's caller owns this writable output; no allocated name needs releasing.
        unsafe {
            output.write(STATSTG {
                r#type: STGTY_STREAM.0.cast_unsigned(),
                cbSize: size,
                grfMode: match self.access {
                    Access::Read => STGM_READ,
                    Access::Write => STGM_READWRITE,
                },
                ..STATSTG::default()
            });
        };
        Ok(())
    }

    fn CopyTo(&self, _: Ref<'_, IStream>, _: u64, _: *mut u64, _: *mut u64) -> Result<()> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn Revert(&self) -> Result<()> {
        Err(Error::from_hresult(E_NOTIMPL))
    }

    fn LockRegion(&self, _: u64, _: u64, _: &LOCKTYPE) -> Result<()> {
        Err(Error::from_hresult(STG_E_INVALIDFUNCTION))
    }

    fn UnlockRegion(&self, _: u64, _: u64, _: u32) -> Result<()> {
        Err(Error::from_hresult(STG_E_INVALIDFUNCTION))
    }

    fn Clone(&self) -> Result<IStream> {
        Err(Error::from_hresult(E_NOTIMPL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_read_access_preserves_bytes_and_bounds_seeks() -> anyhow::Result<()> {
        let mut file = tempfile::tempfile()?;
        file.write_all(&[1, 2, 3, 4])?;
        file.rewind()?;
        let (released, closed) = async_channel::bounded(1);
        #[expect(
            clippy::disallowed_types,
            reason = "The test supplies a writable file so the native adapter, rather than the OS handle, must enforce read-only access"
        )]
        let stream: IStream = CappedFile {
            file: Mutex::new(file),
            limit: 4,
            access: Access::Read,
            _released: released,
        }
        .into();
        let mut bytes = [0_u8; 4];
        let mut count = 0;
        // SAFETY: every buffer and output is owned and valid for these synchronous COM calls.
        unsafe {
            assert_eq!(
                stream.Write(bytes.as_ptr().cast(), 4, Some(&raw mut count)),
                STG_E_ACCESSDENIED
            );
            assert_eq!(count, 0);
            assert_eq!(stream.SetSize(0).unwrap_err().code(), STG_E_ACCESSDENIED);
            assert!(stream.Seek(5, STREAM_SEEK_SET, None).is_err());
            stream
                .Read(bytes.as_mut_ptr().cast(), 4, Some(&raw mut count))
                .ok()?;
            assert_eq!(bytes, [1, 2, 3, 4]);
            assert_eq!(count, 4);
        }
        drop(stream);
        assert!(closed.is_closed());
        Ok(())
    }

    #[test]
    fn native_stream_caps_every_write_and_retains_its_file_until_the_last_reference()
    -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("synthetic.mp4");
        let file = File::create_new(&path)?;
        let (released, closed) = async_channel::bounded(1);
        #[expect(
            clippy::disallowed_types,
            reason = "Exercise the native callback implementation with a real owned file and bounded synthetic bytes"
        )]
        let stream: IStream = CappedFile {
            file: Mutex::new(file),
            limit: 16,
            access: Access::Write,
            _released: released,
        }
        .into();
        let held = stream.clone();
        let data = [42_u8; 12];
        let mut written = 0;
        // SAFETY: this test owns the COM stream and every supplied buffer/output for each call.
        unsafe {
            stream
                .Write(data.as_ptr().cast(), 12, Some(&raw mut written))
                .ok()?;
            assert_eq!(written, 12);
            assert_eq!(
                stream.Write(data.as_ptr().cast(), 12, Some(&raw mut written)),
                STG_E_MEDIUMFULL
            );
            assert_eq!(written, 0);
            stream.Seek(4, STREAM_SEEK_SET, None)?;
            stream.Write(data.as_ptr().cast(), 12, None).ok()?;
            assert!(stream.SetSize(17).is_err());
            assert!(stream.Seek(17, STREAM_SEEK_SET, None).is_err());
            assert!(stream.Seek(-1, STREAM_SEEK_SET, None).is_err());
        }
        drop(stream);
        assert!(
            !closed.is_closed(),
            "The native callback's reference lost its file lease"
        );
        drop(held);
        assert!(closed.is_closed());
        assert_eq!(std::fs::read(path)?, [42; 16]);
        Ok(())
    }
}

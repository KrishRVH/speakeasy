//! In-process recognition through NeMo-Speech.cpp's stable C ABI.
//!
//! The engine library is opened at runtime from its installation, so the application neither links
//! nor bundles it. Status codes, not engine messages, become errors: engine diagnostics can name
//! private files.

use std::{
    ffi::{CStr, CString, c_char, c_void},
    marker::{PhantomData, PhantomPinned},
    os::unix::ffi::OsStrExt,
    path::Path,
    ptr::{self, NonNull},
};

use anyhow::{Context, anyhow, ensure};
use libloading::Library;

/// Where the engine executes the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Accelerator {
    /// The first GPU the engine finds; Metal on Apple silicon.
    Gpu,
    /// The CPU backend.
    Cpu,
}

/// A loaded model that recognizes one utterance at a time.
///
/// Dropping it releases the model before the engine library is unloaded.
pub struct Recognizer {
    api: Api,
    handle: NonNull<RawRecognizer>,
    // Unloaded last: `api` points into it and `handle` belongs to it.
    _library: Library,
}

// SAFETY: the C ABI accepts calls on a recognizer from any thread, and `&mut self` serializes this
// value's calls, so moving it to another thread cannot race its handle.
unsafe impl Send for Recognizer {}

impl Recognizer {
    /// Loads `model` through the engine library at `library`, which may take seconds.
    ///
    /// # Errors
    /// Returns an error if the library or one of its symbols is missing, or the engine rejects the
    /// model or accelerator.
    pub fn load(library: &Path, model: &Path, accelerator: Accelerator) -> anyhow::Result<Self> {
        // SAFETY: the engine library's initializers only register its backends.
        let library = unsafe { Library::new(library) }
            .context("Cannot open the speech engine library. Run automatic setup again.")?;
        let api = Api::resolve(&library)?;
        let model = CString::new(model.as_os_str().as_bytes())
            .context("The model path contains a NUL byte")?;
        let backend = BackendConfig {
            size: size_of::<BackendConfig>(),
            gpu: match accelerator {
                Accelerator::Gpu => 0,
                Accelerator::Cpu => -1,
            },
        };
        let model = ModelConfig {
            size: size_of::<ModelConfig>(),
            path: model.as_ptr(),
            name: ptr::null(),
        };
        let config = RecognizerConfig {
            size: size_of::<RecognizerConfig>(),
            backend: &raw const backend,
            model: &raw const model,
            ..RecognizerConfig::DEFAULTS
        };
        let mut handle = ptr::null_mut();
        // SAFETY: every pointer in `config` refers to a live local; the engine copies them.
        let status = unsafe { (api.create)(&raw const config, &raw mut handle) };
        check(status, "The speech engine could not load the model")?;
        let handle = NonNull::new(handle).context("The speech engine returned no recognizer")?;
        Ok(Self {
            api,
            handle,
            _library: library,
        })
    }

    /// Recognizes mono `samples` at `rate` Hz; the engine resamples 8–96 kHz to the model's rate.
    ///
    /// # Errors
    /// Returns an error for empty or unsupported audio and for failed inference.
    pub fn recognize(&mut self, samples: &[f32], rate: u32) -> anyhow::Result<String> {
        ensure!(!samples.is_empty(), "No audio to recognize");
        ensure!(
            (8_000..=96_000).contains(&rate),
            "Parakeet needs 8–96 kHz audio. Set your microphone to 48 kHz in Audio MIDI Setup."
        );
        let rate = i32::try_from(rate)?;
        let options = RecognitionOptions {
            // The engine's HTTP route punctuates by default; the C ABI's zeroed default does not.
            enable_automatic_punctuation: true,
            ..RecognitionOptions::default()
        };
        let mut result = ptr::null_mut();
        // SAFETY: the handle is live, `options` and `samples` outlive the call, and the engine only
        // reads `samples.len()` floats.
        let status = unsafe {
            (self.api.recognize)(
                self.handle.as_ptr(),
                &raw const options,
                samples.as_ptr(),
                samples.len(),
                rate,
                &raw mut result,
            )
        };
        check(status, "Local transcription failed")?;
        let result = NonNull::new(result).context("The speech engine returned no result")?;
        let result = OwnedResult {
            api: &self.api,
            result,
        };
        Ok(result.transcript())
    }
}

impl Drop for Recognizer {
    fn drop(&mut self) {
        // SAFETY: the handle came from `create` and is destroyed exactly once, here.
        unsafe { (self.api.destroy)(self.handle.as_ptr()) }
    }
}

/// Destroys a result on every path out of [`Recognizer::recognize`].
struct OwnedResult<'a> {
    api: &'a Api,
    result: NonNull<RawResult>,
}

impl OwnedResult<'_> {
    fn transcript(&self) -> String {
        // SAFETY: the result is live; alternative 0 always exists for a successful recognition.
        let text = unsafe { (self.api.transcript)(self.result.as_ptr(), 0) };
        if text.is_null() {
            return String::new();
        }
        // SAFETY: the engine returns a NUL-terminated string owned by the live result.
        unsafe { CStr::from_ptr(text) }
            .to_string_lossy()
            .into_owned()
    }
}

impl Drop for OwnedResult<'_> {
    fn drop(&mut self) {
        // SAFETY: the result came from `recognize` and is destroyed exactly once, here.
        unsafe { (self.api.destroy_result)(self.result.as_ptr()) }
    }
}

fn check(status: i32, failure: &str) -> anyhow::Result<()> {
    const OK: i32 = 0;
    const INVALID_ARGUMENT: i32 = 1;
    const OUT_OF_MEMORY: i32 = 2;
    match status {
        OK => Ok(()),
        INVALID_ARGUMENT => Err(anyhow!("{failure}: the model or audio is not supported.")),
        OUT_OF_MEMORY => Err(anyhow!(
            "{failure}: not enough memory. Close other apps and try again."
        )),
        _ => Err(anyhow!(
            "{failure}. Check the model and engine, or run automatic setup again."
        )),
    }
}

/// The engine's opaque `nemo_speech_asr_recognizer`: unsized in spirit, neither `Send` nor `Unpin`.
#[repr(C)]
struct RawRecognizer {
    _opaque: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// The engine's opaque `nemo_speech_asr_result`.
#[repr(C)]
struct RawResult {
    _opaque: [u8; 0],
    _marker: PhantomData<(*mut u8, PhantomPinned)>,
}

/// The engine entry points, valid while the library that resolved them stays loaded.
struct Api {
    create: unsafe extern "C" fn(*const RecognizerConfig, *mut *mut RawRecognizer) -> i32,
    destroy: unsafe extern "C" fn(*mut RawRecognizer),
    recognize: unsafe extern "C" fn(
        *mut RawRecognizer,
        *const RecognitionOptions,
        *const f32,
        usize,
        i32,
        *mut *mut RawResult,
    ) -> i32,
    transcript: unsafe extern "C" fn(*const RawResult, usize) -> *const c_char,
    destroy_result: unsafe extern "C" fn(*mut RawResult),
}

impl Api {
    fn resolve(library: &Library) -> anyhow::Result<Self> {
        // SAFETY: each symbol is declared in the engine's v1 `asr.h` with exactly this signature.
        unsafe {
            Ok(Self {
                create: *library.get(c"nemo_speech_asr_create")?,
                destroy: *library.get(c"nemo_speech_asr_destroy")?,
                recognize: *library.get(c"nemo_speech_asr_recognize_f32")?,
                transcript: *library.get(c"nemo_speech_asr_result_transcript")?,
                destroy_result: *library.get(c"nemo_speech_asr_result_destroy")?,
            })
        }
    }
}

/// `nemo_speech_asr_backend_config`.
#[repr(C)]
struct BackendConfig {
    size: usize,
    gpu: i32,
}

/// `nemo_speech_asr_model_config`.
#[repr(C)]
struct ModelConfig {
    size: usize,
    path: *const c_char,
    name: *const c_char,
}

/// `nemo_speech_asr_recognizer_config`; null subsystem configs select the library defaults, which
/// include unbatched single-stream inference.
#[repr(C)]
struct RecognizerConfig {
    size: usize,
    backend: *const BackendConfig,
    model: *const ModelConfig,
    streaming: *const c_void,
    decoder: *const c_void,
    vad: *const c_void,
    endpointing: *const c_void,
    postproc: *const c_void,
    diar: *const c_void,
    batching: *const c_void,
}

impl RecognizerConfig {
    const DEFAULTS: Self = Self {
        size: 0,
        backend: ptr::null(),
        model: ptr::null(),
        streaming: ptr::null(),
        decoder: ptr::null(),
        vad: ptr::null(),
        endpointing: ptr::null(),
        postproc: ptr::null(),
        diar: ptr::null(),
        batching: ptr::null(),
    };
}

/// `nemo_speech_asr_recognition_options`; null strings select the engine's automatic language.
#[repr(C)]
struct RecognitionOptions {
    size: usize,
    request_id: *const c_char,
    language_code: *const c_char,
    interim_results: bool,
    enable_word_time_offsets: bool,
    enable_automatic_punctuation: bool,
    verbatim_transcripts: bool,
    profanity_filter: bool,
    stop_history_eou_ms: i32,
    speech_contexts: *const c_void,
    speech_context_count: usize,
    max_alternatives: i32,
    enable_speaker_diarization: bool,
    max_speaker_count: i32,
}

impl Default for RecognitionOptions {
    fn default() -> Self {
        Self {
            size: size_of::<Self>(),
            request_id: ptr::null(),
            language_code: ptr::null(),
            interim_results: false,
            enable_word_time_offsets: false,
            enable_automatic_punctuation: false,
            verbatim_transcripts: false,
            profanity_filter: false,
            stop_history_eou_ms: 0,
            speech_contexts: ptr::null(),
            speech_context_count: 0,
            max_alternatives: 1,
            enable_speaker_diarization: false,
            max_speaker_count: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::mem::offset_of;

    use super::*;

    /// The engine reads fields by offset; a drifted layout would silently misconfigure requests.
    #[test]
    fn layouts_match_the_lp64_c_abi() {
        assert_eq!(size_of::<BackendConfig>(), 16);
        assert_eq!(size_of::<ModelConfig>(), 24);
        assert_eq!(size_of::<RecognizerConfig>(), 80);
        assert_eq!(offset_of!(RecognizerConfig, batching), 72);
        assert_eq!(offset_of!(RecognitionOptions, interim_results), 24);
        assert_eq!(
            offset_of!(RecognitionOptions, enable_automatic_punctuation),
            26
        );
        assert_eq!(offset_of!(RecognitionOptions, stop_history_eou_ms), 32);
        assert_eq!(offset_of!(RecognitionOptions, speech_contexts), 40);
        assert_eq!(offset_of!(RecognitionOptions, max_alternatives), 56);
        assert_eq!(
            offset_of!(RecognitionOptions, enable_speaker_diarization),
            60
        );
        assert_eq!(offset_of!(RecognitionOptions, max_speaker_count), 64);
        assert_eq!(size_of::<RecognitionOptions>(), 72);
    }
}

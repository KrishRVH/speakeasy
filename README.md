# speakeasy

Speakeasy is a native Rust dictation app for Windows and macOS. It uses a shared
GPUI interface and native OS adapters, with hold-to-talk, hands-free dictation,
Escape cancellation, and a small animated pill. It needs no account and works
offline after downloading a local speech engine and model. Speech inference
runs in an owned native whisper.cpp or NeMo-Speech.cpp process. The Windows
Release demo and prerecorded recognition are verified;
live microphone-to-editor acceptance remains. Native macOS builds, tests, and
packaging pass; interactive Mac acceptance remains.

## Build and run

Use the pinned Rust toolchain. Windows builds need Visual Studio C++ Build Tools
and the Windows SDK; macOS builds need Xcode command line tools. Native
targets are Windows 11 x64 and macOS 14+ on Apple Silicon.

```sh
cargo build --locked -p speakeasy
cargo run --locked -p speakeasy -- --demo
```

`--demo` runs a simulated motion preview without microphone, global shortcut,
model, or clipboard access. Linux supports this preview only; GPUI's X11 build
needs ALSA, fontconfig, X11/XCB, xkbcommon, and Vulkan development dependencies.
The preview includes hands-free, the limit countdown, submission, interrupted
dismissal, cancellation, silence, and an error. **Reduce motion** takes effect
immediately in the preview. On Windows/macOS, `--demo-tray` adds the native tray
and Settings hide/reopen behavior while keeping dictation controls disabled.

For dictation, run the app, choose an engine and its executable and model in
Settings, and select **Enable dictation**. Whisper is the default engine. Settings
also select your microphone, language, GPU use, clipboard behavior, and reduced
motion. Changes apply when you save; appearance, microphone, and language changes
keep the loaded model warm.

For a separate configuration, copy `settings.example.json`, set its paths,
and run:

```sh
cargo run --locked -p speakeasy -- --config settings.json
```

## Speech engines

Whisper uses a local whisper.cpp server and a compatible GGML model. The verified
Windows CUDA engine is [whisper.cpp b5130](https://github.com/ggml-org/whisper.cpp/releases/tag/b5130).
Extract the complete runtime and select `Release/whisper-server.exe` in Settings.
Choose an engine built for your hardware; **Prefer GPU** cannot add acceleration
to a CPU-only executable. Disable it when using a CPU-only engine. The `threads`
JSON setting applies to Whisper and defaults to four.

The app owns a loopback-only worker and keeps its model warm. GPU setup includes
a synthetic-silence request to initialize kernels before dictation. Cancellation
responds immediately; the app checks GPU worker recovery in the background and
replaces the worker if recovery fails or exceeds two seconds. CPU cancellation
replaces the worker. **Pause dictation** releases the shortcut and model.

Microphone capture starts only when you trigger dictation. A missing selected
microphone produces an error. No audio, transcripts, or provider output are
logged. Relative engine/model paths resolve beside the settings file.
See [performance measurements and limits](docs/performance-audit.md).

### Parakeet on Windows

The self-contained Windows NVIDIA-GPU bundle is generated locally at
`artifacts/rust/speakeasy-windows-parakeet-x64.zip`. Extract its entire
`Speakeasy` folder to a Windows drive and run **Speakeasy.cmd**. It includes the
verified engine, model and relative-path settings; no download or path editing
is needed. It uses its own settings file and enables dictation in the tray on
launch. Open **Settings…** from the tray to check model readiness or change your
microphone. `Speakeasy.cmd --demo` opens the simulated motion preview;
`Speakeasy.cmd --demo-tray` also previews tray and Settings behavior. Quit the
regular app before starting the tray preview from the same folder. The smaller
`speakeasy-windows-x64.zip` contains the app and settings example.
Generated packages and models are excluded from Git.

To recreate the full bundle from the built executable and assets listed below:

```powershell
./scripts/package-parakeet-windows.ps1 -RuntimeDirectory <extracted-nemo-release> -Model <parakeet-v3-q8.gguf>
```

**Engine: Parakeet** uses NVIDIA's NeMo-Speech.cpp with a Parakeet v3 model.
It requires GPU acceleration and detects the language automatically. Choose the
matching executable and GGUF model, then save. Switching engines is explicit.
The JSON setting is `"engine": "parakeet"`; `engine_executable` selects the worker.

Verified on Windows with an RTX 4090:

- [NeMo-Speech.cpp v0.1.0 CUDA ZIP](https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cuda.zip).
  Extract the complete archive and choose `bin/nemo-speech.exe`.
  SHA-256: `ba024204e76ca2fa4eefa8787506c3c49e418147f627f60cf9206a582b60089c`.
- [NVIDIA Parakeet TDT 0.6B v3 q8 GGUF](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.q8_0.gguf),
  by NVIDIA, distributed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
  SHA-256: `e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e`.

Parakeet requires GPU acceleration and microphone audio at 8–96 kHz. The measured
worker held about 3.8 GB of GPU memory after a five-minute recording; Pause
releases it. CPU-only Parakeet and Parakeet on Mac are unverified.

## Everyday use

Without `--config`, settings are read from `%APPDATA%/speakeasy/settings.json`
on Windows or `~/Library/Application Support/speakeasy/settings.json` on Mac.
Once configured, native launches stay in the tray/menu bar. First setup and
invalid configuration open Settings. Closing Settings hides its window and keeps
unsaved edits; Windows minimize does the same. Reopening Speakeasy or choosing
**Settings…** in the tray restores that window. A brief hint appears the first
time Settings is hidden while the pill is idle.

The tray icon and menu show model loading, readiness, recording, processing,
pause, or an error. **Pause dictation** releases the shortcut and model;
**Resume dictation** uses the saved configuration. **Quit Speakeasy** stops the
app. Error details remain in Settings after the pill disappears.

Hold **Ctrl+Alt+Space** to dictate, double-tap for hands-free, tap again to finish,
or press **Escape** to cancel. On Mac, Alt is Option. Text goes to the app focused
when insertion occurs. Standard mode leaves the transcript on the clipboard;
`preserve_clipboard: true` uses direct Unicode input instead, which some editors
reject. Protected fields and elevated Windows apps may reject either method.
An OS input submission cannot confirm that an editor accepted the text.
The pill's brief check mark means input was submitted. Silence instead shows
**No speech detected**, and cancellation dismisses quietly. Hands-free shows a
lock symbol and timer; the last 30 seconds show the remaining recording time.

## Package and verify

For local packaging, run `scripts/package-windows.ps1` on Windows or
`bash scripts/package-macos.sh` on Mac. The Mac bundle includes the microphone
usage declaration and a local signature; grant Microphone and Accessibility
permissions to that bundle. These scripts package the app, not models or the
Whisper runtime. Mac distribution signing/notarization is still pending.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Default tests use mocked microphone, inference, and insertion ports around the
real session owner. They never record, install a global hook, or modify the
clipboard. A separate ignored provider test accepts public prerecorded audio:

```sh
SPEAKEASY_FIXTURE_CONFIG=/path/to/settings.json \
SPEAKEASY_FIXTURE_WAV=/path/to/whisper.cpp/samples/jfk.wav \
cargo test -p speakeasy local_worker_recognizes_fixture_and_stops -- --ignored
```

With the same variables, `cargo test -p speakeasy profile_fixture_dictation --
--ignored --nocapture` measures the real controller and provider through fake
capture and insertion. It excludes microphone teardown, audio preparation, and
OS paste latency; it is not a microphone-to-editor acceptance check.

On an available Windows desktop, `scripts/check-demo-windows.ps1
-Executable <path-to-speakeasy.exe>` checks the visible demo's focus and hit
testing without recording, injecting input, or changing the clipboard.
Native build/package jobs are in `.github/workflows/ci.yml`. See
[handoff.md](handoff.md) for the checks actually run and remaining native limits.
`scripts/check-tray-windows.ps1 -Executable <path-to-speakeasy.exe>` checks
minimize, close, relaunch, and configuration preservation using owned
`--demo-tray` windows and a temporary configuration directory.

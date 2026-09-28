<p align="center">
  <img src="docs/assets/banner.png" alt="Speakeasy. Say the word." width="760">
</p>

<p align="center">
  <strong>Private dictation for Windows and macOS.</strong><br>
  Hold a shortcut, speak, and your words appear wherever you are typing.<br>
  Local speech engines. No account, no upload, no history.
</p>

<p align="center">
  <a href="#get-started">Get started</a>&nbsp;&nbsp;◆&nbsp;&nbsp;<a href="#speech-engines">Speech engines</a>&nbsp;&nbsp;◆&nbsp;&nbsp;<a href="#build-and-verify">Build and verify</a>&nbsp;&nbsp;◆&nbsp;&nbsp;<a href="docs/architecture.md">Architecture</a>
</p>

## Say the word

A speakeasy was a room behind an ordinary door. You spoke at a small slot, the
door opened, and what was said inside stayed inside. Speakeasy works the same
way: it waits in the tray, opens a small slot on screen while you speak, and
keeps every word on your machine.

<p align="center">
  <img src="docs/assets/pill.png" alt="The pill opening the microphone, listening, recording hands-free, and confirming typed text" width="760">
</p>

| Gesture | Windows | Mac |
| --- | --- | --- |
| Dictate while held; release to type | <kbd>Ctrl</kbd> + <kbd>Win</kbd> | <kbd>Fn</kbd> |
| Hands-free; press the shortcut again to finish | <kbd>Ctrl</kbd> + <kbd>Win</kbd> + <kbd>Space</kbd> | <kbd>Fn</kbd> + <kbd>Space</kbd> |
| Cancel; the key still reaches your app | <kbd>Escape</kbd> | <kbd>Escape</kbd> |

The shortcuts match Wispr Flow's defaults, and double-tapping the shortcut also
starts hands-free. On Mac, set **System Settings › Keyboard › Press 🌐 key to**
to **Do Nothing** so Fn only dictates. Recordings stop at five minutes, with a
countdown for the last 30 seconds. A check means the text was submitted to your
app; **No speech detected** means nothing was sent.

## What's said here stays here

- Speech runs in a local whisper.cpp or NeMo-Speech.cpp process that Speakeasy
  owns and stops.
- The microphone opens only while you dictate. Audio, transcripts, and engine
  output are never logged.
- There are no accounts, cloud providers, telemetry, automatic editing, or
  transcript history.

## Get started

Speakeasy targets Windows 11 x64 and macOS 14+ on Apple Silicon. Use the pinned
Rust toolchain. Windows builds need the Visual Studio C++ Build Tools and the
Windows SDK; macOS builds need the Xcode command line tools.

```sh
cargo run --locked -p speakeasy -- --demo   # simulated preview
cargo run --locked -p speakeasy             # the app
```

1. In **Settings**, choose an engine, then its executable and model.
2. Select **Enable dictation**. Speakeasy moves to the tray or menu bar.
3. Hold <kbd>Ctrl</kbd> + <kbd>Win</kbd> (Windows) or <kbd>Fn</kbd> (Mac) in any editor and speak.

Settings also choose the microphone, language, GPU use, clipboard behavior,
reduced motion, and theme: **Jet & Champagne**, **Emerald Lounge**, **Iris**, or
**Midnight Chrome**. Changes apply when you save. Appearance, microphone, and
language changes keep the model loaded.

`--demo` uses simulated audio without the microphone, global shortcut, model, or
clipboard. It previews hands-free, the countdown, submission, interrupted
dismissal, cancellation, silence, and an error. On Windows and macOS,
`--demo-tray` adds the tray and the Settings hide-and-reopen behavior with
dictation disabled. Linux runs the preview only; GPUI's X11 build needs ALSA,
fontconfig, X11/XCB, xkbcommon, and Vulkan development packages.

## Speech engines

**Whisper** is the default. It runs a local whisper.cpp server with a GGML model;
the known-good Windows CUDA build is
[whisper.cpp b5130](https://github.com/ggml-org/whisper.cpp/releases/tag/b5130).
Extract the complete runtime and choose `Release/whisper-server.exe`. **Prefer
GPU** cannot accelerate a CPU-only build, so turn it off there. The `threads`
setting applies to Whisper and defaults to four.

**Parakeet** runs NVIDIA's NeMo-Speech.cpp with a Parakeet v3 model. It needs GPU
acceleration and microphone audio at 8–96 kHz, and it detects the language
automatically. Set `"engine": "parakeet"` or choose it in Settings; switching
engines is always explicit.

<details>
<summary>Known-good Parakeet setup and the portable Windows bundle</summary>

<br>

On Windows with an RTX 4090:

- [NeMo-Speech.cpp v0.1.0 CUDA ZIP](https://github.com/NVIDIA/NeMo-Speech.cpp/releases/download/v0.1.0/nemo-speech-0.1.0-windows-x86_64-cuda.zip).
  Extract the complete archive and choose `bin/nemo-speech.exe`.
  SHA-256: `ba024204e76ca2fa4eefa8787506c3c49e418147f627f60cf9206a582b60089c`.
- [NVIDIA Parakeet TDT 0.6B v3 q8 GGUF](https://huggingface.co/nvidia/parakeet-tdt-0.6b-v3/resolve/541d1f99c6b0c3cd0b11a95167540bb8edefd82b/parakeet-tdt-0.6b-v3.q8_0.gguf),
  by NVIDIA, distributed under [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).
  SHA-256: `e3880d0aaaaf2c308ea2c35016b2b895c423eb3fda924c1b463d1c19b7f4d32e`.

After a five-minute recording the worker holds about 3.8 GB of GPU memory until
**Pause** releases it. CPU-only Parakeet and Parakeet on Mac are untested.

The self-contained bundle packs the app, engine, model, notices, and
relative-path settings:

```powershell
./scripts/package-parakeet-windows.ps1 -RuntimeDirectory <extracted-nemo-release> -Model <parakeet-v3-q8.gguf>
```

It writes `artifacts/speakeasy-windows-parakeet-x64.zip`. Extract the whole
`Speakeasy` folder to a Windows drive and run **Speakeasy.cmd**. It starts in the
tray with dictation enabled and keeps its own settings. `Speakeasy.cmd --demo`
and `Speakeasy.cmd --demo-tray` preview it safely; quit the running app first.

</details>

The worker stays warm on loopback. With a GPU, one silent request initializes
its kernels before the first dictation. Cancelling GPU work keeps a healthy
worker and replaces it if a two-second recovery check fails; CPU cancellation
always replaces the worker. **Pause dictation** releases the shortcut and the
model. Relative engine and model paths resolve beside the settings file.

## Everyday use

Settings live in `%APPDATA%/speakeasy/settings.json` on Windows and
`~/Library/Application Support/speakeasy/settings.json` on Mac. Start from
`settings.example.json` and pass `--config <path>` to use another file.
Configured launches go straight to the tray or menu bar. Closing or minimizing
Settings hides it and keeps unsaved edits; reopening Speakeasy or choosing
**Settings…** in the tray brings it back.

The tray icon shows loading, ready, recording, processing, paused, and attention
states. Its menu pauses or resumes dictation, starts or finishes a recording,
cancels, opens Settings, and quits. Errors stay in Settings after the pill fades.

By default the text is pasted through the clipboard and stays there. **Keep
clipboard** types it as direct input instead, which some editors reject.
Protected fields and elevated Windows apps may refuse either method, and no OS
submission can confirm that an editor accepted the text.

## Build and verify

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Default tests drive the real session owner with mocked microphone, inference,
and insertion. They never record, install a global hook, or touch the clipboard.

<details>
<summary>Packaging, fixture tests, and native checks</summary>

<br>

- `scripts/package-windows.ps1` and `bash scripts/package-macos.sh` package the
  app into `artifacts/`, without models or engines. The Mac bundle carries its
  microphone usage declaration and a local signature; grant it Microphone and
  Accessibility access. Distribution needs Developer ID signing and notarization.
- With `SPEAKEASY_FIXTURE_CONFIG` pointing at a settings file and
  `SPEAKEASY_FIXTURE_WAV` at public audio such as whisper.cpp's `samples/jfk.wav`:

  ```sh
  cargo test -p speakeasy local_worker_recognizes_fixture_and_stops -- --ignored
  cargo test -p speakeasy profile_fixture_dictation -- --ignored --nocapture
  ```

  The first checks recognition and worker shutdown. The second times the real
  controller and engine with fake capture and insertion; it excludes microphone
  teardown and OS paste latency.
- `scripts/check-demo-windows.ps1 -Executable <speakeasy.exe>` checks that the
  demo pill stays nonactivating and passes clicks through.
  `scripts/check-tray-windows.ps1 -Executable <speakeasy.exe>` checks minimize,
  close, relaunch, and configuration preservation with owned `--demo-tray` windows.
- `.github/workflows/release.yml` builds the Windows and macOS packages on each
  push to `main` and publishes them to the GitHub release for the workspace
  version.

Live microphone-to-editor dictation and interactive macOS use still need
hands-on acceptance.

</details>

## Documentation

- [Architecture](docs/architecture.md): module ownership, sessions, recognition, and privacy.
- [Interaction design](docs/interaction-design.md): the brand, the pill, the tray, and interaction contracts.
- [Performance](docs/performance.md): measured latency and memory, and the approaches not taken.
- [Remote dictation](docs/remote-dictation.md): routing a remote microphone to Speakeasy.

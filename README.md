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

- Speech runs in a local NeMo-Speech.cpp or whisper.cpp process that Speakeasy
  owns and stops. Setup downloads the engine and model once; nothing is uploaded.
- The microphone opens only while you dictate. Audio, transcripts, and engine
  output are never logged.
- There are no accounts, cloud providers, telemetry, automatic editing, or
  transcript history.

## Get started

Download the Windows or Mac zip from the
[latest release](https://github.com/KrishRVH/speakeasy/releases/latest), unzip
it, and open Speakeasy. On a Mac, allow Microphone and Accessibility access when
asked.

The first launch sets everything up. Speakeasy checks the machine, downloads the
matching [NeMo-Speech.cpp](https://github.com/NVIDIA/NeMo-Speech.cpp) build and
NVIDIA's Parakeet v3 model, about 0.8 GB in all, and turns dictation on:

| Machine | Engine build | Speech runs on |
| --- | --- | --- |
| Windows, NVIDIA GPU with 6 GB or more | CUDA | GPU |
| Windows, another GPU with 6 GB or more | Vulkan | GPU |
| Any other Windows PC | Vulkan, or CPU without a Vulkan driver | CPU |
| Mac with Apple silicon | Metal | GPU |

Then hold <kbd>Ctrl</kbd> + <kbd>Win</kbd> (Windows) or <kbd>Fn</kbd> (Mac) in any
editor and speak.

Downloads come from pinned GitHub and Hugging Face URLs and are checked against
their SHA-256 before use. They live in `%LOCALAPPDATA%\speakeasy` on Windows and
`~/Library/Application Support/speakeasy` on Mac. **Cancel** pauses setup, and
the next attempt resumes where it stopped.

<details>
<summary>Build from source</summary>

<br>

Speakeasy targets Windows 11 x64 and macOS 14+ on Apple Silicon. Use the pinned
Rust toolchain. Windows builds need the Visual Studio C++ Build Tools and the
Windows SDK; macOS builds need the Xcode command line tools.

```sh
cargo run --locked -p speakeasy -- --demo   # simulated preview
cargo run --locked -p speakeasy             # the app
```

`--demo` uses simulated audio without the microphone, global shortcut, model, or
clipboard. It previews hands-free, the countdown, submission, interrupted
dismissal, cancellation, silence, and an error. On Windows and macOS,
`--demo-tray` adds the tray and the Settings hide-and-reopen behavior with
dictation disabled. Linux runs the preview only; GPUI's X11 build needs ALSA,
fontconfig, X11/XCB, xkbcommon, and Vulkan development packages.

</details>

Settings also choose the microphone, GPU use, clipboard behavior, reduced motion,
and theme: **Jet & Champagne**, **Emerald Lounge**, **Iris**, or **Midnight
Chrome**. Changes apply when you save. Appearance, microphone, and language
changes keep the model loaded.

## Speech engines

**Parakeet** is the default: NVIDIA's Parakeet TDT 0.6B v3, run by
NeMo-Speech.cpp. It detects the language automatically and needs microphone
audio at 8–96 kHz. Setup chooses its build, and **Prefer GPU** switches between
the GPU and the CPU. After a five-minute recording the GPU worker holds about
3.8 GB of memory until **Pause** releases it. The model is by NVIDIA, under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).

**Whisper** runs a local whisper.cpp server with a GGML model. Switch the engine
under **Local speech**, choose its executable and model, and save; switching is
always explicit. The known-good Windows build is
[whisper.cpp b5130](https://github.com/ggml-org/whisper.cpp/releases/tag/b5130):
extract the complete runtime and choose `Release/whisper-server.exe`. Turn off
**Prefer GPU** for a CPU-only build. The `threads` setting applies to Whisper and
defaults to four.

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
  app into `artifacts/`. Packages carry no engine or model; setup downloads
  them on first launch. The Mac bundle carries its
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
  teardown and OS paste latency. With only `SPEAKEASY_FIXTURE_WAV`,
  `cargo test -p speakeasy install_chooses -- --ignored` runs setup into a
  temporary directory, downloading about 0.8 GB, and recognizes the fixture with
  the build this machine would get.
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

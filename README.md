<p align="center">
  <img src="docs/assets/banner.png" alt="Speakeasy. Say the word." width="760">
</p>

<p align="center">
  <strong>Private dictation for Apple silicon Macs.</strong><br>
  Hold Fn, speak, and your words appear wherever you are typing.<br>
  Local speech. No account, no upload, no history.
</p>

<p align="center">
  <a href="#get-started">Get started</a>&nbsp;&nbsp;◆&nbsp;&nbsp;
  <a href="#speech-engines">Speech engines</a>&nbsp;&nbsp;◆&nbsp;&nbsp;
  <a href="#build-and-verify">Build and verify</a>&nbsp;&nbsp;◆&nbsp;&nbsp;
  <a href="docs/architecture.md">Architecture</a>
</p>

This branch is the macOS-only performance fork of Speakeasy 0.3.3. It drops Windows and Linux to
optimize one target, Apple silicon, and to measure whether people can feel the difference. The
[fork PRD](docs/apple-silicon-prd.md) owns its goals, measurements, and acceptance gates.

## Say the word

A speakeasy was a room behind an ordinary door. You spoke at a small slot, the door opened, and what
was said inside stayed inside. Speakeasy works the same way: it waits in the menu bar, opens a small
slot on screen while you speak, and keeps every word on your Mac.

<p align="center">
  <img src="docs/assets/pill.png"
    alt="The pill opening the microphone, listening, recording hands-free, and confirming typed text" width="760">
</p>

| Gesture                                        | Shortcut                         |
| ---------------------------------------------- | -------------------------------- |
| Dictate while held; release to type            | <kbd>Fn</kbd>                    |
| Hands-free; press the shortcut again to finish | <kbd>Fn</kbd> + <kbd>Space</kbd> |
| Cancel; the key still reaches your app         | <kbd>Escape</kbd>                |

Double-tapping Fn also starts hands-free. Set **System Settings › Keyboard › Press 🌐 key to** to
**Do Nothing** so Fn only dictates. Recordings stop at five minutes, with a countdown for the last
30 seconds. A check means the text was submitted to your app; **No speech detected** means nothing
was sent. **Remove um / uh** is on by default and removes standalone English hesitation words
locally before insertion. Speech containing only these fillers sends nothing. Turn it off for
non-English Parakeet dictation or literal transcription; Whisper removes fillers only when its
language is set to English.

## What's said here stays here

- Speech runs in a local NeMo-Speech.cpp or whisper.cpp process that Speakeasy owns and stops. Setup
  downloads the model once; nothing is uploaded.
- The microphone opens only while you dictate. Audio, transcripts, and engine output are never
  logged.
- There are no accounts, cloud providers, telemetry, or transcript history.

## Get started

Build the app bundle on a Mac with Apple silicon, macOS 14 or later, Xcode with its Metal toolchain,
and CMake, then open it and allow Microphone and Accessibility access when asked:

```sh
bash scripts/package-macos.sh
open artifacts/Speakeasy.app
```

The app carries its own Metal build of [NeMo-Speech.cpp](https://github.com/NVIDIA/NeMo-Speech.cpp),
tuned for Apple silicon. The first launch installs it and downloads NVIDIA's Parakeet v3 model,
about 0.7 GB, into `~/Library/Application Support/speakeasy`, then turns dictation on. The model
comes from a pinned Hugging Face revision and is checked against its SHA-256 before use. **Cancel**
pauses setup, and the next attempt resumes where it stopped. Then hold <kbd>Fn</kbd> in an editor
and speak.

Settings also choose the microphone, GPU use, filler removal, clipboard behavior, reduced motion,
and theme: **Jet & Champagne**, **Emerald Lounge**, **Iris**, or **Midnight Chrome**. Changes apply
when you save. Appearance, microphone, language, and filler removal changes keep the model loaded.
Saving stays responsive and preserves edits made while the save is running. Pausing during a save
keeps dictation paused; Quit finishes requested saves.

## Speech engines

**Parakeet** is the default: NVIDIA's Parakeet TDT 0.6B v3, run by NeMo-Speech.cpp on Metal. It
detects the language automatically and needs microphone audio at 8–96 kHz. **Prefer GPU** switches
between the GPU and the CPU. After a five-minute recording the worker holds several gigabytes of
unified memory until **Pause** releases it. The model is by NVIDIA, under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).

**Whisper** runs a local whisper.cpp server with a GGML model. Switch the engine under **Local
speech**, choose its executable and model, and save; switching is always explicit. Turn off **Prefer
GPU** for a CPU-only build. The `threads` setting applies to Whisper and defaults to four.

Parakeet runs in Speakeasy's own helper process, which loads the engine installation's library and
receives audio over a pipe; an installation without that library runs the engine's server instead.
The worker stays warm. With the GPU, one silent request initializes its kernels before the first
dictation. In hands-free dictation on the GPU, Speakeasy recognizes the audio so far whenever you
pause, so when you stop without saying more, the text is often ready at once. Cancelling GPU work
keeps a healthy worker and replaces it if a two-second recovery check fails. Cancelling active CPU
inference replaces the worker. **Pause dictation** releases the shortcut and the model. Relative
engine and model paths resolve beside the settings file.

## Everyday use

Settings live in `~/Library/Application Support/speakeasy/settings.json`. Start from
`settings.example.json` and pass `--config <path>` to use another file. Configured launches go
straight to the menu bar. Closing Settings hides it and keeps unsaved edits. Reopening Speakeasy or
choosing **Settings…** in the menu brings it back. `speakeasy --toggle` and `speakeasy --cancel`
start, finish, or discard dictation in the running app, for scripts and launchers.

The menu bar icon shows loading, ready, recording, processing, paused, and attention states by
shape. Its menu pauses or resumes dictation, starts or finishes a recording, cancels, opens
Settings, and quits. Errors stay in Settings after the pill fades.

By default the text is pasted through the clipboard and stays there. **Keep clipboard** types it as
direct input instead, which some editors reject. Protected fields may refuse either method, and no
OS submission can confirm that an editor accepted the text.

## Build and verify

Use the pinned nightly Rust toolchain in `rust-toolchain.toml`; workspace dependencies are declared
centrally in the root `Cargo.toml`.

```sh
mise install
mise run standards:check
cargo run --locked -p speakeasy -- --demo   # simulated preview
cargo run --locked -p speakeasy             # the app
```

The pinned toolchain and lockfiles make the local and CI gate the same. It checks Rust formatting,
Clippy, docs, tests, and unused dependencies; Python formatting, lint, types, and fixture tests;
shell formatting, lint, and syntax; and Markdown formatting, structure, local links, and spelling.
`mise run standards` applies safe formatters and autofixes. `mise tasks` lists focused checks such
as `mise run rust:lint` and `mise run py:test`.

`--demo` uses simulated audio without the microphone, global shortcut, model, or clipboard. It
previews hands-free, the countdown, submission, interrupted dismissal, cancellation, silence, and an
error. `--demo-tray` adds the menu bar item and the Settings hide-and-reopen behavior with dictation
disabled.

Default tests drive the real session owner with mocked microphone, inference, and insertion. A
paused clock exercises gesture deadlines and the five-minute cap. They never record, install a
global hook, or touch the clipboard. The GPUI-free crates also build and test on other Unix hosts;
the app itself builds only on macOS. See the
[architecture change and test map](docs/architecture.md#change-and-test-map).

<details>
<summary>Packaging, fixture tests, and native checks</summary>

<br>

- `bash scripts/package-macos.sh` packages the app into `artifacts/`. It runs
  `scripts/build-engine.sh`, which fetches pinned NeMo-Speech.cpp, ggml, and SentencePiece sources,
  applies [Speakeasy's engine patches](packaging/engine/README.md), and builds for M1 and macOS 14.
  The bundle carries that engine but no model, its microphone usage declaration, and a local
  signature. Distribution needs Developer ID signing and notarization.
- With `SPEAKEASY_FIXTURE_CONFIG` pointing at a settings file and `SPEAKEASY_FIXTURE_WAV` at public
  audio such as whisper.cpp's `samples/jfk.wav`:

  ```sh
  cargo test -p speakeasy-dictation local_worker_recognizes_fixture_and_stops -- --ignored
  cargo test -p speakeasy-dictation profile_fixture_dictation -- --ignored --nocapture
  ```

  The first checks recognition and worker shutdown. The second times the real session owner and
  engine with fake capture and insertion; it excludes microphone teardown and OS paste latency. With
  only `SPEAKEASY_FIXTURE_WAV`, `cargo test -p speakeasy-dictation install_chooses -- --ignored`
  runs setup into a temporary directory and recognizes the fixture; `SPEAKEASY_FIXTURE_ENGINE` names
  a `scripts/build-engine.sh` build to install instead of downloading NVIDIA's. A test binary cannot
  run the Parakeet helper, so set `SPEAKEASY_FIXTURE_HELPER` to a built `speakeasy` binary.

- `cargo run --release -p speakeasy-platform --example engine_profile` times warm recognition of
  public WAV fixtures through the engine's C library or its HTTP server; see
  [performance](docs/performance.md#measuring).
- `cargo run --locked -p speakeasy --example check_native_rendering -- --native-gui` opens an owned
  non-activating window, renders an embedded SVG and paths, resizes, and repeats show/hide. It runs
  on the process main thread for AppKit and accesses no microphone, global hook, or clipboard.
- `.github/workflows/checks.yml` runs the gate, the owned-window rendering check, and packaging on a
  macOS runner. This fork publishes no releases.

Live microphone-to-editor dictation still needs hands-on acceptance.

</details>

## Documentation

- [Architecture](docs/architecture.md): module ownership, sessions, recognition, and privacy.
- [Interaction design](docs/interaction-design.md): the brand, the pill, the menu bar item, and
  interaction contracts.
- [Performance](docs/performance.md): constraints, profiling methods, and native measurement limits.
- [Apple silicon fork PRD](docs/apple-silicon-prd.md): feature parity, performance measurements,
  perceptibility testing, and native acceptance, with
  [platform research](docs/apple-silicon-research.md) and the
  [comparison baseline](docs/apple-silicon-baseline.md).

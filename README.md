<p align="center">
  <img src="docs/assets/banner.png" alt="Speakeasy. Say the word." width="760">
</p>

<p align="center">
  <strong>Private dictation for Windows, macOS, and Linux.</strong><br>
  Hold a shortcut, speak, and your words appear wherever you are typing.<br>
  Local speech engines. No account, no upload, no history.
</p>

<p align="center">
  <a href="#get-started">Get started</a>&nbsp;&nbsp;◆&nbsp;&nbsp;
  <a href="#speech-engines">Speech engines</a>&nbsp;&nbsp;◆&nbsp;&nbsp;
  <a href="#build-and-verify">Build and verify</a>&nbsp;&nbsp;◆&nbsp;&nbsp;
  <a href="docs/architecture.md">Architecture</a>
</p>

## Say the word

A speakeasy was a room behind an ordinary door. You spoke at a small slot, the door opened, and what
was said inside stayed inside. Speakeasy works the same way: it waits in the tray, opens a small
slot on screen while you speak, and keeps every word on your machine.

<p align="center">
  <img src="docs/assets/pill.png"
    alt="The pill opening the microphone, listening, recording hands-free, and confirming typed text" width="760">
</p>

| Gesture                                        | Windows                                             | Mac                              |
| ---------------------------------------------- | --------------------------------------------------- | -------------------------------- |
| Dictate while held; release to type            | <kbd>Ctrl</kbd> + <kbd>Win</kbd>                    | <kbd>Fn</kbd>                    |
| Hands-free; press the shortcut again to finish | <kbd>Ctrl</kbd> + <kbd>Win</kbd> + <kbd>Space</kbd> | <kbd>Fn</kbd> + <kbd>Space</kbd> |
| Cancel; the key still reaches your app         | <kbd>Escape</kbd>                                   | <kbd>Escape</kbd>                |

The shortcuts match Wispr Flow's defaults, and double-tapping the shortcut also starts hands-free.
On Mac, set **System Settings › Keyboard › Press 🌐 key to** to **Do Nothing** so Fn only dictates.
Recordings stop at five minutes, with a countdown for the last 30 seconds. A check means the text
was submitted to your app; **No speech detected** means nothing was sent. **Remove um / uh** is on
by default and removes standalone English hesitation words locally before insertion. Speech
containing only these fillers sends nothing. Turn it off for non-English Parakeet dictation or
literal transcription; Whisper removes fillers only when its language is set to English.

## What's said here stays here

- Speech runs in a local NeMo-Speech.cpp or whisper.cpp process that Speakeasy owns and stops. Setup
  downloads the engine and model once; nothing is uploaded.
- The microphone opens only while you dictate. Audio, transcripts, and engine output are never
  logged.
- There are no accounts, cloud providers, telemetry, or transcript history.

## Get started

Download the Windows or Mac zip from the
[latest release](https://github.com/KrishRVH/speakeasy/releases/latest), unzip it, and open
Speakeasy. On a Mac, allow Microphone and Accessibility access when asked. Linux's experimental
AppImage and tar packages are described below.

The first launch sets everything up. Speakeasy checks the machine, downloads the matching
[NeMo-Speech.cpp](https://github.com/NVIDIA/NeMo-Speech.cpp) build and NVIDIA's Parakeet v3 model,
about 0.8 GB in all, and turns dictation on:

| Machine                                | Engine build                                    | Speech runs on |
| -------------------------------------- | ----------------------------------------------- | -------------- |
| Windows, NVIDIA GPU with 6 GB or more  | CUDA                                            | GPU            |
| Windows, another GPU with 6 GB or more | Vulkan                                          | GPU            |
| Any other Windows PC                   | Vulkan, or CPU without a Vulkan driver          | CPU            |
| Mac with Apple silicon                 | Metal                                           | GPU            |
| Linux x86_64                           | CUDA, Vulkan, or CPU after compatibility checks | GPU or CPU     |

Then hold <kbd>Ctrl</kbd> + <kbd>Win</kbd> (Windows), <kbd>Fn</kbd> (Mac), or <kbd>Ctrl</kbd> +
<kbd>Super</kbd> + <kbd>Space</kbd> (Linux) in an editor and speak.

Downloads come from pinned GitHub and Hugging Face URLs and are checked against their SHA-256 before
use. They live in `%LOCALAPPDATA%\speakeasy` on Windows and
`~/Library/Application Support/speakeasy` on Mac. **Cancel** pauses setup, and the next attempt
resumes where it stopped.

<details>
<summary>Build from source</summary>

<br>

Speakeasy targets Windows 11 x64, macOS 14+ on Apple silicon, and experimental Linux x86_64 builds
with glibc 2.35 or later. Use the pinned nightly Rust toolchain in `rust-toolchain.toml` (including
rustfmt and Clippy). Workspace dependencies are declared centrally in the root `Cargo.toml`. Windows
builds need the Visual Studio C++ Build Tools and the Windows SDK; macOS builds need the Xcode
command line tools.

```sh
cargo run --locked -p speakeasy -- --demo   # simulated preview
cargo run --locked -p speakeasy             # the app
```

`--demo` uses simulated audio without the microphone, global shortcut, model, or clipboard. It
previews hands-free, the countdown, submission, interrupted dismissal, cancellation, silence, and an
error. On Windows and macOS, `--demo-tray` adds the tray and the Settings hide-and-reopen behavior
with dictation disabled. Linux has experimental desktop support; see the Linux instructions below.
GPUI's X11 build needs ALSA, fontconfig, X11/XCB, xkbcommon, and Vulkan development packages.

</details>

Settings also choose the microphone, GPU use, filler removal, clipboard behavior, reduced motion,
and theme: **Jet & Champagne**, **Emerald Lounge**, **Iris**, or **Midnight Chrome**. Changes apply
when you save. Appearance, microphone, language, and filler removal changes keep the model loaded.
Saving stays responsive and preserves edits made while the save is running. Pausing during a save
keeps dictation paused; Quit finishes requested saves.

## Linux (experimental)

Download the x86_64 AppImage or tar archive from the release. The application targets glibc 2.35 or
later and needs X11 or Xwayland, Vulkan, and desktop audio support. It shares audio, engines, the
session owner, Settings and the pill with Windows and Mac. Native GNOME/KDE/X11 acceptance is
pending; see [implementation and acceptance](docs/linux-implementation.md) for support limits.

The defaults are **Ctrl+Super+Space** to hold or double-tap for hands-free and **Ctrl+Super+Escape**
to cancel. Linux's cancel chord is reserved; bare Escape is not globally captured. Wayland requests
desktop shortcut and keyboard permissions; Settings shows the actual accepted bindings. Install the
supplied `io.github.krvh.speakeasy.desktop` file in `~/.local/share/applications` (or the
corresponding XDG data directory), and make the executable available as `speakeasy` on your PATH.
Portals use this installed launcher identity. For source builds:

```sh
cargo build --release --locked -p speakeasy
mkdir -p "$HOME/.local/bin"
ln -s "$PWD/target/release/speakeasy" "$HOME/.local/bin/speakeasy"
install -Dm644 packaging/linux/io.github.krvh.speakeasy.desktop \
  "${XDG_DATA_HOME:-$HOME/.local/share}/applications/io.github.krvh.speakeasy.desktop"
```

For the tar package, link its absolute `Speakeasy.AppDir/AppRun` path instead, and install the
desktop file from `usr/share/applications` inside that directory. For AppImage, make it executable,
link its absolute path as `speakeasy`, and install the desktop file from this repository or its
`--appimage-extract` output. The executable stays in the chosen location. Automatic paste needs
compositor modifier feedback and libei; **Keep clipboard** additionally needs direct text support.
Choose **Copy for manual paste** if automatic insertion is unavailable. For terminals, select
**Ctrl+Shift+V** as the paste shortcut. X11 automatic paste uses the first keyboard layout with an
unshifted V key; other layouts copy and explain how to paste manually.

For a desktop without suitable shortcut permissions, enable **Desktop bindings** and assign
`speakeasy --toggle` and `speakeasy --cancel` in its shortcut settings. This mode starts/finishes
hands-free. Enable manual paste as well to avoid requesting keyboard-control access. On X11, native
chords can be set in the optional `linux` settings object: `shortcut`, `cancel`, `terminal_paste`,
`manual_paste`, and `external_shortcut`. Ordinary chord keys use the portal syntax, such as
`CTRL+LOGO+space`. Save and resume after changing bindings or keyboard maps.

Rendering uses X11 or Xwayland with Vulkan. A tray host is optional: without one, closing Settings
quits and the launcher reopens the app. Configuration lives under `$XDG_CONFIG_HOME/speakeasy`,
downloads under `$XDG_DATA_HOME/speakeasy`, and private portal restore credentials under
`$XDG_STATE_HOME/speakeasy`, with standard home directory defaults. The pinned upstream Linux CPU
engine has a reported illegal-instruction failure on some CPUs without AVX-512. A synthetic
inference check precedes readiness, and an incompatible build produces an error; choose a compatible
engine in Settings. This check does not establish a universal CPU fallback. Engine and desktop
compatibility remain separate from the app's glibc baseline.

`bash scripts/package-linux.sh` prepares an x86_64 AppImage and bundled tar on an older builder,
checking glibc 2.35. `--native-tar` makes an unbundled archive for the current machine; extract it
and run `Speakeasy.AppDir/AppRun`. The release workflow builds Linux on Ubuntu 22.04. Local native
graphics checks run those binaries in an Ubuntu 24.04 container with stock Mesa, preserving the
build ABI while exercising a corrected software driver. CPU, memory, latency, and frame pacing
checks are described in [performance](docs/performance.md).

## Speech engines

**Parakeet** is the default: NVIDIA's Parakeet TDT 0.6B v3, run by NeMo-Speech.cpp. It detects the
language automatically and needs microphone audio at 8–96 kHz. Setup chooses its build, and **Prefer
GPU** switches between the GPU and the CPU. After a five-minute recording the GPU worker holds about
3.8 GB of memory until **Pause** releases it. The model is by NVIDIA, under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/).

**Whisper** runs a local whisper.cpp server with a GGML model. Switch the engine under **Local
speech**, choose its executable and model, and save; switching is always explicit. The known-good
Windows build is [whisper.cpp b5130](https://github.com/ggml-org/whisper.cpp/releases/tag/b5130):
extract the complete runtime and choose `Release/whisper-server.exe`. Turn off **Prefer GPU** for a
CPU-only build. The `threads` setting applies to Whisper and defaults to four.

The worker stays warm on loopback. With a GPU, one silent request initializes its kernels before the
first dictation; Linux CPU workers also validate inference before readiness. Cancelling GPU work
keeps a healthy worker and replaces it if a two-second recovery check fails. Cancelling active CPU
inference replaces the worker. **Pause dictation** releases the shortcut and the model. Relative
engine and model paths resolve beside the settings file.

## Everyday use

Settings live in `%APPDATA%\speakeasy\settings.json` on Windows and
`~/Library/Application Support/speakeasy/settings.json` on Mac. Start from `settings.example.json`
and pass `--config <path>` to use another file. Configured launches go straight to the tray or menu
bar. Closing Settings hides it and keeps unsaved edits; Windows minimize also hides it. Reopening
Speakeasy or choosing **Settings…** in the tray brings it back.

The tray icon shows loading, ready, recording, processing, paused, and attention states. Its menu
pauses or resumes dictation, starts or finishes a recording, cancels, opens Settings, and quits.
Errors stay in Settings after the pill fades.

By default the text is pasted through the clipboard and stays there. **Keep clipboard** types it as
direct input instead, which some editors reject. Protected fields and elevated Windows apps may
refuse either method, and no OS submission can confirm that an editor accepted the text.

## Build and verify

```sh
mise install
mise run standards:check
```

The pinned toolchain and lockfiles make local checks reproducible. The gate checks Rust formatting,
Clippy, docs, tests, and unused dependencies; Python formatting, lint, types, and fixture tests;
shell formatting, lint, and syntax; and Markdown formatting, structure, local links, and spelling.
`mise run standards` applies safe formatters and autofixes. `mise tasks` lists focused checks such
as `mise run rust:lint` and `mise run py:test`.

Default tests drive the real session owner with mocked microphone, inference, and insertion. A
paused clock exercises gesture deadlines and the five-minute cap. They never record, install a
global hook, or touch the clipboard. Run checks locally before merging. See the
[architecture change and test map](docs/architecture.md#change-and-test-map).

<details>
<summary>Packaging, fixture tests, and native checks</summary>

<br>

- `scripts/package-windows.ps1`, `bash scripts/package-macos.sh`, and
  `bash scripts/package-linux.sh` package the app into `artifacts/`. Packages carry no engine or
  model; setup downloads them on first launch. The Mac bundle carries its microphone usage
  declaration and a local signature; grant it Microphone and Accessibility access. Distribution
  needs Developer ID signing and notarization.
- With `SPEAKEASY_FIXTURE_CONFIG` pointing at a settings file and `SPEAKEASY_FIXTURE_WAV` at public
  audio such as whisper.cpp's `samples/jfk.wav`:

  ```sh
  cargo test -p speakeasy local_worker_recognizes_fixture_and_stops -- --ignored
  cargo test -p speakeasy profile_fixture_dictation -- --ignored --nocapture
  ```

  The first checks recognition and worker shutdown. The second times the real session owner and
  engine with fake capture and insertion; it excludes microphone teardown and OS paste latency. With
  only `SPEAKEASY_FIXTURE_WAV`, `cargo test -p speakeasy install_chooses -- --ignored` runs setup
  into a temporary directory, downloading about 0.8 GB, and recognizes the fixture with the build
  this machine would get.

- `scripts/check-demo-windows.ps1 -Executable <speakeasy.exe>` checks that the demo pill stays
  non-activating and passes clicks through.
  `scripts/check-tray-windows.ps1 -Executable <speakeasy.exe>` checks minimize, close, relaunch, and
  configuration preservation with owned `--demo-tray` windows. On Windows,
  `cargo test -p speakeasy-platform hidden_pill_consumes_paint -- --ignored` checks hidden and
  visible paint dispatch with an owned native window and fake renderer, without microphone, hook,
  clipboard, or input access.
- On Linux, `mise exec -- python scripts/check_demo_linux.py target/release/speakeasy` opens a
  private Xvfb display, checks rendered Settings and the pill's input shape, and waits for the
  simulated preview to hide. It needs Xvfb, x11-utils, and a Vulkan driver. Headless Xvfb checks
  need Mesa's X11 software presentation fix, included in Mesa 24.1.
  `bash scripts/check-rendering-linux.sh` builds the native fixtures and runs them, plus the release
  binary that `package-linux.sh` builds, in an Ubuntu 24.04 Docker container with stock Mesa;
  `--packages` checks the AppImage and tar launchers locally. The separate
  `idle_native_pill_starts_unmapped_and_can_show_and_hide` opt-in test verifies the pill starts
  hidden and supports native show/hide on that private display.
- `cargo run --locked -p speakeasy --example check_native_rendering -- --native-gui` opens an owned
  non-activating window, renders an embedded SVG and paths, resizes, and repeats show/hide. It runs
  on the process main thread for AppKit. Linux needs a private display, such as `xvfb-run -a`;
  Windows and Mac use their native desktop. It accesses no microphone, global hook, or clipboard.
- `.github/workflows/release.yml` builds and uploads Windows, macOS, and Linux packages without
  running checks or tests. A change to `Cargo.toml` on `main`, or a manual workflow dispatch, starts
  packaging. Only `main` publishes the GitHub release for the workspace version; manual branch runs
  upload build artifacts. Bump the workspace version and update `docs/release-notes.md` for each
  release.

Live microphone-to-editor dictation and interactive macOS use still need hands-on acceptance.

</details>

## Documentation

- [Architecture](docs/architecture.md): module ownership, sessions, recognition, and privacy.
- [Interaction design](docs/interaction-design.md): the brand, the pill, the tray, and interaction
  contracts.
- [Performance](docs/performance.md): constraints, profiling methods, and native measurement limits.
- [Remote dictation](docs/remote-dictation.md): routing a remote microphone to Speakeasy.

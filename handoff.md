# Speakeasy handoff

## Current product

Speakeasy is a native Rust app for Windows and macOS. `crates/core` owns gestures
and motion, `crates/app` owns GPUI and dictation, and `crates/platform` owns native
adapters. Speech inference runs in an owned local whisper.cpp or NeMo-Speech.cpp
worker. Read [README.md](README.md) for setup,
[architecture](docs/architecture.md) for ownership, and
[interaction design](docs/interaction-design.md) for the implemented UI.

The scope is dictate-into-anything: hold-to-talk, double-tap hands-free, passive
Escape, a five-minute cap, tray/settings, and clipboard or Unicode insertion.
No automatic editing, transcript history, accounts, telemetry, or silent engine
switching. Configuration uses `settings.json` and `engine_executable`.

The app provides quiet configured launches, a live status icon and
menu, Pause/Resume, Windows minimize-to-tray, hidden Settings that retain edits,
and relaunch-to-reveal. The pill distinguishes startup, capture, hands-free,
processing, submitted input, silence, cancellation, and errors. It includes a
last-30-seconds countdown and respects reduced motion. The behavior contract
is [ux-plan.md](docs/ux-plan.md); `--demo-tray` safely previews native lifecycle
behavior with dictation disabled.

## Windows delivery

The local NVIDIA GPU bundle is `artifacts/rust/speakeasy-windows-parakeet-x64.zip`.
Extract its entire `Speakeasy` folder to a Windows drive and run **Speakeasy.cmd**.
It includes the app, CUDA worker, Parakeet model, license notices, and its own
relative-path settings. It enables dictation in the tray on launch; open Settings
from the tray to check readiness. `Speakeasy.cmd --demo` opens a simulated motion
preview; `Speakeasy.cmd --demo-tray` also previews the native tray lifecycle.
Quit the regular app before starting the tray preview from the same folder.
The smaller `speakeasy-windows-x64.zip` contains the
app and settings example. Both packages are generated and excluded from Git.

The source commit, build target, checksums and sizes are recorded beside the
bundles in `latest-windows-build.json`. Recreate packages with the scripts
documented in README; models and native engine binaries remain separate
dependencies.

## Verification

The repo-wide finishing pass covers all 52 active human-maintained files against
`49b381c`; Cargo.lock and ignored generated outputs are preserved. New tests have
executed regression evidence, including native lifecycle faults in isolated
copies and a working alternative minimize path.

The UX pass passes formatting, Linux and Windows GNU all-target Clippy, 18
default tests (three opt-in tests remain ignored), and a Windows GNU Release
build. The macOS platform crate cross-checks successfully; interactive macOS UX
has not been verified. Both Windows ZIPs contain the fresh GNU Release app.

On Windows, the extracted bundle passes demo visibility, focus preservation, and
click-through checks. `scripts/check-tray-windows.ps1` verifies native command
and direct minimize paths, close-to-tray, relaunch restoring the same window,
unchanged configuration, and orderly reopen-listener cleanup at quit. Captured
pill states were inspected at 175% display scale. A settled, hidden tray demo
used 15.63 ms measured CPU over three seconds and a 63.7 MiB working set; this is a
short simulated idle sample, not a dictation performance benchmark. Evidence is
under ignored `artifacts/rust/polish/` and `artifacts/rust/ux/`. Live
microphone/input/clipboard acceptance was not run.

Earlier Windows bundle acceptance used public prerecorded audio, fake
capture/insertion, and owned demo windows. It checked extraction under a path
containing spaces, the actual launcher from an
unrelated directory, visible-pill focus/hit testing, production controller and
worker inference, unchanged settings, and worker shutdown.

Native Windows MSVC and macOS builds, tests, and packages are checked by
`.github/workflows/ci.yml`. Interactive macOS operation and live
microphone-to-editor behavior remain unverified, as do elevated/protected fields,
sleep/lock, display changes, screen readers, and displayed frame pacing.
The transitive `proc-macro-error2` future-compatibility advisory remains.

## Performance

Fable 5.1 xhigh informed the Tally interaction design. Opus 5.5 xhigh audited
performance; [the audit record](docs/performance-audit.md) preserves measured
latency, memory, UX limits, and rejected approaches. The requested additional
app-wide 10× gain remains unachieved. Component gains do not establish that claim.

## Working locally

Follow the scoped standards and native Cargo commands in [AGENTS.md](AGENTS.md).
Linux development libraries restored inside this workspace require:

```sh
export PKG_CONFIG_PATH="$PWD/artifacts/rust/build-libs/root/usr/lib/x86_64-linux-gnu/pkgconfig"
export LIBRARY_PATH="$PWD/artifacts/rust/build-libs/root/usr/lib/x86_64-linux-gnu"
```

The Windows GNU target and release shaders are cached under `target/`. Native
Windows packaging uses MSVC and Windows SDK FXC. Stage reviewed WSL PowerShell
scripts into a unique Windows-local temporary directory under the existing
RemoteSigned policy. Await GUI probes with `Diagnostics.Process.WaitForExit`.

The user requested mocks while unavailable. Continue using public fixtures and
owned `--demo` windows; do not start live capture, hooks, clipboard changes, or
input injection without renewed direction. Preserve user settings. Local evidence
and review ledgers live under ignored `artifacts/rust/`.

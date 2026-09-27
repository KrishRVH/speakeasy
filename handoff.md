# Speakeasy Rust handoff — September 27, 2026

## Windows delivery

The Rust app is implemented in `crates/`. The available Windows delivery is
complete with mocked capture/insertion and public-audio verification. Missing
live-device or Mac measurements do not block this Windows package.

| Local artifact | Contents | Size |
| --- | --- | ---: |
| `artifacts/rust/speakeasy-windows-parakeet-x64.zip` | App, verified CUDA engine/model, notices, launcher and relative-path settings | 792,175,054 bytes |
| `artifacts/rust/speakeasy-windows-x64.zip` | App and settings example; choose an engine/model separately | 4,119,441 bytes |

Extract the full bundle's `Speakeasy` folder to a Windows drive and run
**Speakeasy.cmd**. It uses its own settings and enables dictation on launch.
It requires an NVIDIA GPU; this machine has an RTX 4090. For a simulated preview,
run `Speakeasy.cmd --demo`. Close older instances before normal dictation.
These generated artifacts and models are excluded from Git.

Full bundle SHA-256: `815c0a9f6d5d3d42aa726de5b32f817dad59505fce427e13b985ed40b5a25f23`.
Application SHA-256: `ecc0046737d2effc47ee76b56e5ea90d2d3644da935e60731f99d25579021e2c`.
Machine-readable metadata: `artifacts/rust/windows-delivery.json` and
`artifacts/rust/parakeet-package.json`. Rebuild instructions and upstream assets
are in [README.md](README.md#optional-parakeet-engine).

## Scope and ownership

Core dictation only: hold-to-talk, double-tap hands-free, passive Escape,
five-minute cap, tray/settings, and clipboard or direct Unicode insertion.
Premium Tally motion comes from the Fable 5.1 xhigh consultation. No automatic
editing, transcript history, accounts, telemetry, or silent provider switching.
The existing .NET app remains supported separately and retains its own features.

- `core` owns pure gestures and analytic motion; `app` owns GPUI, capture and
  the session owner; `platform` owns native adapters and unsafe FFI.
- Tagged sessions, bounded input and the native commit gate reject stale results.
  Pause retires the owner asynchronously; Quit waits for owned cleanup.
- The audio callback uses a bounded ring without locks or allocations. First
  samples trigger Ready. The consumer owns native-rate PCM16, grows its initial
  ten-second allocation up to five minutes, and prepares the WAV in memory.
- The audible gate requires 100 ms in 20 ms RMS windows. Quiet edges of at least
  one second are trimmed with 500 ms padding; interior pauses remain intact.
- `local_speech.rs` owns one loopback worker, defaults to Whisper, and supports
  explicitly selected Parakeet. GPU cancellation disconnects, then performs a
  bounded silent inference check before reuse; failure stops the worker before
  replacement. CPU cancellation reloads. Failed warmup does not retry-loop.
- Keep Whisper's normal timestamp decoding to avoid long-recording omissions.
  Inserted text has normalized whitespace and no timestamps.
- The pill is nonactivating/click-through and stops requesting frames when
  settled. Upstream GPUI's Windows VSync thread still wakes while idle.

See the [module map](docs/rust-rewrite-plan.md),
[interaction direction](docs/interaction-design.md), and
[scoped standards](docs/standards.md). No ADR, fuzzing, mutation or coverage gates.

## Verification

The final Windows bundle was extracted into a path containing spaces. Its app
passed visible-pill, focus and hit-testing checks; its actual launcher worked
from an unrelated directory. Packaged settings, engine and model recognized
public JFK audio through the real controller with fake capture/insertion.
Settings were unchanged, all owned workers exited, and temporary files were
removed. Evidence: `artifacts/rust/windows-delivery-final.log`.

- Rust formatting, Linux and Windows all-target Clippy, and Windows GNU Release
  build passed. All 12 default tests passed on Linux and actual Windows.
- The existing .NET app passed locked restore, Release build with no warnings or
  errors, all 168 tests, and whitespace verification using Windows SDK 10.0.100.
- Six setup scenarios with fake downloads/settings passed. PowerShell, Bash and
  configuration parsing passed. No live microphone, hook, clipboard or input
  was used in these checks.
- Repo-wide `$polish` covered 104 human-maintained paths against base `d619fc0`
  plus the working tree; nine generated locks/binary assets were preserved.
  It fixed Windows startup error preservation, strengthened demo bounds checks,
  corrected CI paths and clarified current versus historical documentation.
  No tests or runtime dependencies were added by the polish pass.

Native MSVC packaging, native Mac execution, actual microphone-to-editor
behavior, broad target-app/elevated-field compatibility, sleep/lock, display
changes, screen readers and displayed frame pacing remain unverified. Submission
intervals under GPU load were measured, but DXGI returned no display timestamps
and PresentMon tracing lacked permission. These limits are explicit evidence
boundaries. The transitive `proc-macro-error2` future-compatibility advisory
remains; there are no first-party compiler/Clippy warnings.

## Performance findings

Opus 5.5 xhigh completed the comprehensive performance audit and follow-ups.
[The audit](docs/performance-audit.md) preserves measurements, corrections,
source links and rejected candidates. The additional app-wide 10× goal remains
unachieved; do not present component gains as that result.

Parakeet reduced measured stop-to-fake-insertion from about 42/96/2,489 ms to
11/20/929 ms for 3.3/10.4/293.7-second fixtures. These exclude capture preparation
and actual OS insertion. Word errors improved overall, with different formatting.
The worker retained about 3.8 GB dedicated GPU memory after long audio; Pause
releases it. Original 16 kHz versus synthetic 48 kHz input added a median 4 ms
short/131 ms long in the HTTP screen, with matching normalized word hashes.

Rejected candidates include independent pause chunks and tested continuous
streaming models (formatting/quality loss), whole-recording speculation (stale
work can delay Stop), and tested CUDA/cuBLAS/Q8/F16 switches (no useful gain or
higher memory). Do not reopen them without new evidence. Current upstream GPUI
still has the unconditional Windows VSync loop; NeMo has no newer packaged release.

## Continuing locally

Use the native Cargo commands in AGENTS.md. Linux development libraries restored
inside this workspace need these environment paths:

```sh
export PKG_CONFIG_PATH="$PWD/artifacts/rust/build-libs/root/usr/lib/x86_64-linux-gnu/pkgconfig"
export LIBRARY_PATH="$PWD/artifacts/rust/build-libs/root/usr/lib/x86_64-linux-gnu"
```

Windows GNU target and release shader output are cached under `target/`.
The normal native packaging script uses MSVC and Windows SDK FXC. Do not commit
SDK, model, binary or diagnostic output. Stage reviewed WSL PowerShell scripts
into a unique Windows-local temporary directory under the existing RemoteSigned
policy; no policy bypass is needed. Await GUI test processes with
`Diagnostics.Process.WaitForExit`, not a launcher that waits for its descendants.

The user requested mocks while unavailable. Continue using public fixtures and
owned demo windows; do not reopen live capture/hook/clipboard/input testing
without renewed direction. User settings remain untouched. Historical probe
recipes and evidence live under ignored `artifacts/rust/profiling/`; old `/tmp`
references may be absent after the earlier WSL restart. All delivery checks and
review workers are complete.

# Ownership refactor validation

Validated on Linux on 2026-10-01 UTC against baseline `2ffed9a`, with the pre-existing Apple silicon
draft preserved separately at `high-perf-macos` commit `65a477d`.

## Assessment and design

The assessment identified useful ownership and test-seam problems, with two
corrections: device-level microphone errors already reached the UI, and the
meter's −60 to −6 dBFS range was not an established defect. The refactor preserves
that range with an explanation and retains the real-time ring and callback limit.
Thread-spawn and runtime-initialization failures now retain their causes.

The architecture replaces loose session flags with owned session, microphone,
worker, and service lifecycle states. Cancel revokes authority before cleanup;
replacement waits for native retirement. Snapshots project the owner's state.
Portable keyboard decisions and insertion eligibility are shared and tested.
Linux refuses insertion into its own X11/Xwayland windows; portal focus queries
run on an owned worker so blocking X calls cannot block cancellation.

Claude Opus 5.5 reviewed both the proposed architecture and implementation at
`xhigh` effort. Its findings led to regression fixes for failed warmup during
capture stop, resume with a dead owner, stale warmup feedback, failed recovery
while capture is queued, and capture attempts after input closes. The second
review also prompted explicit shutdown and GPUI instance-lock ordering contracts.
The [architecture](architecture.md) is the authoritative map of these invariants.

## Toolchain and dependencies

The latest complete nightly manifest available during verification was
`nightly-2026-10-01`: `rustc 1.101.0-nightly (21b707e3f 2026-09-30)`. It is pinned
with rustfmt, Clippy, and Windows/macOS adapter targets. Workspace direct packages
use the latest stable releases checked against crates.io; Cargo regenerated the
lockfiles. Repository-managed action, engine-download, and packaging-tool pins
were checked and their immutable hashes retained. Installed system tools and
user-selected engine executables were not replaced.

Upstream constraints still require older incompatible transitive versions,
including libloading, sha2, x11rb, and xkbcommon alongside the latest direct
versions. Forcing those upgrades would require unrelated upstream changes.
GPUI and gpui_media's exact Core Foundation constraints required a narrow
[media manifest patch](../vendor/gpui_media/README.speakeasy.md), with provenance
and removal criteria. Cargo reports an upstream future-compatibility advisory
for the current stable `proc-macro-error2 2.0.1`; strict workspace Clippy passes.

## Results

| Check | Result |
| --- | --- |
| Workspace formatting and strict Clippy | Passed. |
| Workspace tests | 120 passed, 8 ignored; baseline was 96 passed, 8 ignored. |
| Windows and Apple silicon platform adapter Clippy | Passed for all targets with warnings denied. |
| Linux release build | Passed; binary is 27,978,376 bytes. |
| Release simulated GUI | Startup, rendered pixels, click-through pill, and preview show/hide passed. |
| Release shutdown | Hidden display disconnect and repeated application-owned Quit requests exited cleanly. |

The production owner is tested with paused Tokio time for desktop readiness,
hold/release, Space lock, double-tap, and the five-minute deadline. Explicit fake
cleanup gates exercise cancellation, stale results, device retirement, worker
recovery, configuration, and shutdown without changing the user's desktop.

The existing release profile measured ready-text-to-fake-insertion latency over
100 samples: baseline median/p95 **51/60 μs**, refactor **52/67 μs**. This includes
toolchain/dependency changes and scheduler noise; it establishes neither a speed
improvement nor a statistically established regression. It excludes real audio,
inference, native input, UI frames, and energy use. Timing assertions were not
added to CI.

This host's private X server could not bind a filesystem Unix listener. The
simulated GUI checks therefore used an authenticated private TCP X display with
Mesa's software renderer. The existing CI rendering script remains unchanged.

## Remaining acceptance

PR verification now includes Linux, Windows, and macOS. The new Windows/macOS
jobs have not run remotely; Linux cross-target adapter checks do not build the
SDK-dependent GPUI applications or validate native callbacks. Live microphone,
hook, clipboard, editor, and compositor acceptance was not run.

Application-owned Quit awaits native owners, setup, insertion, and requested
saves without a timeout while the UI continues rendering. A stuck driver leaves
Quitting visible. Forced native termination retains synchronous disposal; changing
that GPUI contract is deferred pending native acceptance. Engine diagnostics use
exit status and local remedies while discarding potentially private stderr.

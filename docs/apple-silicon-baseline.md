# Apple silicon fork: comparison baseline

The [draft PRD](apple-silicon-prd.md) freezes the feature and performance comparison at released
Speakeasy **0.3.3**, tag `v0.3.3`, commit `f30791273cc4130c2073077df703794263a675a0`. The
`high-perf-macos` branch carries the candidate; keep the control checkout at the frozen commit.

## Released 0.3.3 evidence and remaining baseline work

The [cross-platform checks](https://github.com/KrishRVH/speakeasy/actions/runs/37025868825) and
[release verification](https://github.com/KrishRVH/speakeasy/actions/runs/37025868693) passed at the
frozen commit. Linux passed 155 Rust tests and 19 tooling tests; Windows and macOS each passed 132
Rust tests. Native opt-in tests remained skipped: 8 on Linux, 7 on Windows, and 6 on macOS.
Verification also covered owned-window rendering on Windows/macOS and Linux private-display pixels,
and all three platform packages passed before
[0.3.3 was published](https://github.com/KrishRVH/speakeasy/releases/tag/v0.3.3). These checks
establish build and owned-window behavior; they do not measure native Mac dictation or user
perceptibility.

Use the control's `rust-toolchain.toml`, `.config/mise/config.toml`, manifests, lockfiles, packaging
settings, and model/runtime artifact hashes. Run `mise run standards:check` for the complete gate,
including Rust formatting, strict Clippy, public docs, nextest/doctests, dependency-use and
compiler-policy checks, and tooling validation. Keep checked release arithmetic and the inherited
workspace lint policy in extracted Rust crates. Record candidate compiler/SDK differences as
separate variables; framework comparisons need matched inference and capture paths.

The [architecture change and test map](architecture.md#change-and-test-map) identifies current
contracts and their regression seams. Historical
[0.3.0 refactor validation](https://github.com/KrishRVH/speakeasy/blob/2dbaaf835fdc0f79d735eb554932b7cf995a2bbe/docs/refactor-validation.md)
recorded ready-text-to-fake-insertion median/p95 of 52/67 µs over 100 Linux requests, against 51/60
µs before that refactor. Toolchain and scheduler differences prevent treating this as an established
regression. It is neither a 0.3.3 nor a native Mac measurement; rerun relevant components on the
reference Macs.

No native Mac microphone-onset, stop-to-visible-text, energy, memory, inference, or perceptibility
baseline has been collected here. Stage 1 must produce those distributions on M1-class and newer
hardware, using matched settings, public fixtures, and native-test opt-in. Report absolute user
latency and resource costs separately; carry out the PRD's blinded tasks before claiming a perceived
gain.

## Historical 0.2.2 audit

The following audit was recorded 2026-09-30 at `2ffed9af5f209ab9a0c527cffdcc58a271d74ff2` (0.2.2).
Its values and artifacts remain historical evidence, not the 0.3.3 comparison baseline. They cannot
establish Mac performance or the effect of a native rewrite.

### Environment and measurement limits

The available host is WSL2 Linux x86_64, kernel `6.18.33.2-microsoft-standard-WSL2`, on an AMD Ryzen
9 9950X3D with 32 logical processors. That audit used Rust and Cargo 1.98.1, thin LTO, and a single
codegen unit. Measurements were not affinity-pinned, and thermal state and competing host load were
not controlled.

No live microphone, global hook, real clipboard, focused editor, private recording, or real-model
inference was used. Native macOS tools and Apple silicon hardware were unavailable. These
observations cannot substitute for the frozen release's gate or native baseline.

### Component measurements at 0.2.2

The opt-in component probes used synthetic audio and fake capture, speech, and insertion.
[Performance](performance.md#measuring) owns current measurement commands.

| Component                                 | Measurement                                  | Interpretation                                                                                      |
| ----------------------------------------- | -------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| 480-frame, 48 kHz mono packet             | 0.805 µs median; 0.835 µs p95 of batch means | 21 runs of 3,000 packets; packet processing, not the native callback or driver.                     |
| Same packet, stereo                       | 0.809 µs median; 0.823 µs p95 of batch means | Downmix/ring/consumer work is already small on this CPU.                                            |
| Same packet, eight channels               | 0.996 µs median; 1.004 µs p95 of batch means | Not evidence about USB interfaces or native channel layouts.                                        |
| Quiet-edge trim of five minutes at 48 kHz | 2.152 ms median; 2.368 ms p95                | 21 runs; relevant to stop latency, but not the entire capture teardown.                             |
| Ready-text event through fake insertion   | 51 µs median; 60 µs p95                      | 100 requests; excludes model inference, AppKit dispatch, modifier waits, clipboard, and the editor. |

Packet timings divide each run's elapsed time by 3,000, then take percentiles across 21 run means.
They do not describe individual packet latency tails and cannot be compared to a native callback p99
requirement. Trim and controller timings are percentiles of individual operations/requests.

These are empirical sample percentiles from one host/session. They are neither macOS budgets nor
statistically established improvements over previous runs. Use them to select hypotheses about
larger costs before rewriting the session controller or spring arithmetic, then measure the current
Mac implementation.

The 0.2.2 executable also passed a private-Xvfb demo using simulated levels with no speech worker.
An untracked harness sampled its process every 0.5 seconds, then held both windows unmapped for ten
seconds. The following sampled values are retained only as historical context; that temporary
harness is not a reproducible current benchmark.

| Demo phase                             | Observations                                                                                                                                                 | Limitation                                                                                                           |
| -------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------- |
| Scripted preview with Settings visible | 48 samples over 23.63 seconds; 8.27 CPU-seconds, averaging 35.0% of one logical core. Peak sampled RSS 139.21 MiB, PSS 107.22 MiB, private memory 83.12 MiB. | Mixed animation states plus CPU rendering, excluding Xvfb's own process cost. Not a single-state renderer benchmark. |
| Both windows unmapped after preview    | 20 samples over 9.55 seconds; no increment in app CPU accounting. Same sampled memory high-water values.                                                     | CPU accounting has 10 ms resolution; zero observed ticks does not establish zero CPU work or zero wakeups.           |

The app had 107 threads in both phases, including the software graphics stack's threads. Thread
count alone does not diagnose app scheduling overhead. Mesa's CPU renderer and retained
Settings/resources explain why these measurements cannot predict a Mac GPU's footprint or energy.
The hidden interval suggests the 0.2.2 already stops substantial demo drawing; it does not establish
Mac idle power or model-worker behavior.

## Constraints and historical evidence

[Performance](performance.md) owns current implementation constraints and measurement methods.
Earlier native Windows results and rejected experiments remain in the
[0.3.0 performance record](https://github.com/KrishRVH/speakeasy/blob/2dbaaf835fdc0f79d735eb554932b7cf995a2bbe/docs/performance.md).
Useful constraints are:

- Real inference ranges from tens of milliseconds for short GPU fixtures to seconds for long
  recordings or CPU execution. Model work dominates the synthetic controller latency; Windows
  results do not rank Mac inference paths.
- Parakeet retains substantial accelerator scratch after long recordings. Unloading it releases
  resources but loses warm readiness. Total app, helper, model, and accelerator memory must all be
  reported.
- Disabling Whisper timestamps improved speed but caused severe skipped speech in the existing
  corpus. Chunking and speculative requests also failed existing quality or total-work comparisons.
  These are rejected results, not defaults to restore in a new language.
- Hidden-window scheduling and render-resource ownership have caused meaningful defects. Recent
  patches already fix multiple such problems. Historical pre-fix CPU/memory figures must not be used
  as the new app's comparison baseline.
- Finish and Cancel already `unpark` the capture consumer immediately. Its 5 ms data polling
  interval is not a mandatory 5 ms stop delay.

## Source audit and experiments

The following maps the frozen 0.3.3 implementation, whose owned threads, native monitors, and engine
processes are joined, woken, or killed and reaped before a replacement starts. Existing tests remain
the behavioral reference when moving these modules into a native shell.

| Area and source                                                                         | Present behavior                                                                                                                                                                                                                                                                                                                                                                                                                      | Candidate and proof required                                                                                                                                                                                                                                                                                              |
| --------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Session: `crates/app/src/runtime/{mod,owner,session,microphone,worker}.rs`, `ports.rs`  | One authority with typed stages, separate microphone/worker retirement, publisher epochs, and generation-bound insertion. `runtime/tests.rs` drives the production owner with paused Tokio time and explicit cleanup gates.                                                                                                                                                                                                           | Reuse this implementation and tests in a GPUI-free library; it currently resides in the app crate. Preserve stale-completion and cancellation/retirement contracts before measuring bridge cost.                                                                                                                          |
| Capture: `crates/app/src/audio.rs`, `audio/control.rs`                                  | CPAL callback downmixes into a bounded float ring with nonzero channel count and typed Recording/Finishing/Cancelled control. Cancel cannot be replaced by Finish. Consumer polls every 5 ms while recording, converts to PCM16, gates speech, trims, and prepares in-memory WAV. Meter range is −60 to −6 dBFS; owned PCM is zeroed on drop, best effort. A panicked capture thread reports an actionable failure and still retires. | Compare wake strategies, float staging, and capture adapters independently. Preserve checked rate/length validation and failure ordering. Count wakeups, callback work, stop latency, and audio loss. CPAL already uses Core Audio's HAL Output AudioUnit on macOS; direct HAL is not automatically faster.               |
| Inference: `crates/app/src/local_speech.rs`, `runtime/worker.rs`                        | One warm owned process, WAV multipart over loopback HTTP, bounded response, accelerator warmup, and cancellation recovery. Exit diagnostics retain actionable causes while discarding potentially private stderr.                                                                                                                                                                                                                     | Compare HTTP with the pinned engine's float-PCM ABI inside an owned helper. Isolate transport, resampling, model time, and warmup. Preserve explicit engine selection and local errors.                                                                                                                                   |
| Buffering: `audio.rs`, `local_speech.rs`                                                | PCM16 payload with checked WAV headers, sample alignment, lengths, and rates; selective capacity compaction after heavy trimming; requests retain audio while needed.                                                                                                                                                                                                                                                                 | Float32 avoids one quantization/decode route but uses twice PCM16 storage. At 48 kHz for 300 seconds, raw mono payloads are 28.8 MB PCM16 versus 57.6 MB float32. These are byte counts, not measured footprint. Count copies and allocations before adding shared memory; invalid input must remain an actionable error. |
| Process lifecycle: `crates/platform/src/process.rs`, `crates/app/src/child.rs`          | Engine and setup children are hidden, killed when their owner drops, and reaped before replacement. Orderly Unix shutdown cleans the owned process group with validated nonzero identity; abrupt app death can leave a worker. Cancellation may require terminating and replacing the worker.                                                                                                                                         | Add a parent-liveness contract for the new owned helper; test parent crash, kill, inference hang, and replacement. A launchd XPC service changes lifecycle semantics and must be assessed separately from transport speed.                                                                                                |
| UI: `crates/app/src/{pill,shell,tray}.rs` and their submodules, `vendor/gpui`           | GPUI Settings and pill, continuous springs, coalesced levels, nonactivation, patched frame/resource lifetime, and current template-icon APIs.                                                                                                                                                                                                                                                                                         | Compare AppKit/Core Animation with frozen GPUI on the same Mac/display. Measure displayed frames, WindowServer work, hidden wakeups, and retained Settings resources; preserve identical appearance in blinded trials.                                                                                                    |
| Lifecycle: `crates/app/src/shell/{lifecycle,services,shutdown}.rs`                      | Explicit disabled/validating/running/stopping/quitting states; latest pending configuration; retained native owners. App-owned Quit asynchronously awaits cleanup and requested saves. Forced native termination retains synchronous disposal.                                                                                                                                                                                        | Port coordination, acknowledgement ordering, validation epochs, saves, and instance-lock lifetime to AppKit. Test delayed teardown and repeated Quit; parent death remains a separate helper contract.                                                                                                                    |
| Native input/insertion: `crates/platform/src/{macos,keyboard,insertion,monitor,lib}.rs` | Owned event tap whose stop and handle publication share one lock, so a stop before start stays effective; portable Fn/Space/Escape policy; cancellation authority; shared focus-before-modifier eligibility; clipboard ownership and Unicode input. An incomplete result can include already-submitted input.                                                                                                                         | Reuse tested policy and generation-bound commit; never retry a partially submitted insertion automatically. Measure event-to-owner and owner-to-submission separately; keep blocking preparation off the tap and AppKit thread.                                                                                           |
| Gestures: `crates/core/src/gesture.rs`                                                  | Private state transitions with an explicit clock and checked tap/recording deadlines; an unrepresentable deadline expires immediately.                                                                                                                                                                                                                                                                                                | Reuse the state machine and boundary/property tests. Presentation or bridge scheduling cannot remove the recording cap.                                                                                                                                                                                                   |
| Instance ownership: `crates/app/src/instance.rs`                                        | Configuration-directory lock and verified loopback rendezvous; fixed requests use a bounded lane. The dedicated listener can wait for UI readiness; shutdown closes the lane and wakes/joins the listener before releasing the lock.                                                                                                                                                                                                  | Preserve protocol compatibility, command ordering, backpressure, startup/relaunch behavior, and shutdown wakeup. Native shell replacement must not introduce unbounded queues or lose Toggle/Cancel.                                                                                                                      |
| Setup/settings: `crates/app/src/{setup,config}.rs`, `shell/{settings,save}.rs`          | Verified resumable artifacts; setup and each save on an owned thread joined before replacement; off-UI validation/coalesced durable saves; Quit waits for writes.                                                                                                                                                                                                                                                                     | Preserve artifact integrity, cancellation, explicit engine choice, unsaved drafts, later edits, and Pause intent.                                                                                                                                                                                                         |

The highest-value initial experiments are native baseline collection, direct PCM inference versus
HTTP, helper death/cancellation, and native UI scheduling/resource cost. Capture polling and model
scratch reuse follow once native traces identify their contribution. Core ML/Neural Engine
conversion, custom Metal kernels, quantization, and buffered inference remain experiments rather
than assumed wins.

## Native gaps

No measurement here answers these questions:

- Stop-to-visible-text latency and cold/warm onset on M1 or later hardware.
- Metal inference latency, peak unified-memory pressure, and repeated long-input scratch retention
  with the pinned model.
- Energy or idle wakeups on a Mac, including WindowServer and the helper.
- Actual frame presentation on 60/120 Hz and mixed-refresh displays.
- Permission loss, sleep/wake, microphone route/device failure, Spaces/fullscreen, native clipboard
  ownership, Unicode insertion, or force-quit containment.
- Performance and signing behavior of the proposed native bundle.

The [PRD](apple-silicon-prd.md) defines native and perceptibility experiment gates, with
distribution acceptance separately optional. The [platform research](apple-silicon-research.md)
records first-party API constraints and the experiments needed to choose between competing
implementations.

# Apple silicon edition: current baseline

Recorded 2026-09-30 against `2ffed9af5f209ab9a0c527cffdcc58a271d74ff2`
(Speakeasy 0.2.2). This audit supports the [draft PRD](apple-silicon-prd.md).
It describes the current implementation, safe local measurements, and hypotheses
to test on a Mac. It does not establish the performance of a native macOS rewrite.

## Environment and verification

The available host is WSL2 Linux x86_64, kernel
`6.18.33.2-microsoft-standard-WSL2`, on an AMD Ryzen 9 9950X3D with 32 logical
processors. Rust and Cargo are 1.98.1. Release uses the repository's existing
thin LTO and single codegen unit. Measurements were not affinity-pinned, and
thermal state and competing host load were not controlled.

The workspace passed these checks before documentation changes:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo build --release --locked -p speakeasy
```

There were 96 passing tests and eight ignored opt-in fixtures. Existing GPUI
dependency warnings and a dependency future-incompatibility notice remain; the
workspace Clippy check passed. No live microphone, global hook, real clipboard,
focused editor, or private recording was used. No real-model inference was run
for this audit. Native macOS tools and Apple silicon hardware were unavailable.

## Fresh component measurements

These commands exercise production component logic with synthetic audio or fake
capture, speech, and insertion:

```sh
cargo test --release --locked -p speakeasy profile_audio_pipeline -- --ignored --nocapture
cargo test --release --locked -p speakeasy profile_ready_text_latency -- --ignored --nocapture
```

| Component | Measurement | Interpretation |
| --- | --- | --- |
| 480-frame, 48 kHz mono packet | 0.805 µs median; 0.835 µs p95 of batch means | 21 runs of 3,000 packets; packet processing, not the native callback or driver. |
| Same packet, stereo | 0.809 µs median; 0.823 µs p95 of batch means | Downmix/ring/consumer work is already small on this CPU. |
| Same packet, eight channels | 0.996 µs median; 1.004 µs p95 of batch means | Not evidence about USB interfaces or native channel layouts. |
| Quiet-edge trim of five minutes at 48 kHz | 2.152 ms median; 2.368 ms p95 | 21 runs; relevant to stop latency, but not the entire capture teardown. |
| Ready-text event through fake insertion | 51 µs median; 60 µs p95 | 100 requests; excludes model inference, AppKit dispatch, modifier waits, clipboard, and the editor. |

Packet timings divide each run's elapsed time by 3,000, then take percentiles
across 21 run means. They do not describe individual packet latency tails and
cannot be compared to a native callback p99 requirement. Trim and controller
timings are percentiles of individual operations/requests.

These are empirical sample percentiles from one host/session. They are neither
macOS budgets nor statistically established improvements over previous runs.
The similar results in [Performance](performance.md) reinforce prioritizing
larger costs before rewriting the session controller or spring arithmetic.

The rebuilt release executable also underwent the complete private-Xvfb demo
check, including rendering, click-through/nonactivating pill behavior, show/hide,
and clean exit after the hidden display disconnects:

```sh
python3 scripts/check-demo-linux.py target/release/speakeasy
```

The demo uses simulated levels with no speech worker. A temporary, untracked
harness additionally sampled the explicitly launched app every 0.5 seconds
using `scripts/profile-linux.py`'s process reader, and held both windows unmapped
for ten seconds before disconnect. Its report is a development artifact outside
Git at `/tmp/speakeasy-apple-research/demo-profile.json`, produced by
`python3 /tmp/speakeasy-apple-research/profile_demo.py`.

| Demo phase | Observations | Limitation |
| --- | --- | --- |
| Scripted preview with Settings visible | 48 samples over 23.63 seconds; 8.27 CPU-seconds, averaging 35.0% of one logical core. Peak sampled RSS 139.21 MiB, PSS 107.22 MiB, private memory 83.12 MiB. | Mixed animation states plus CPU rendering, excluding Xvfb's own process cost. Not a single-state renderer benchmark. |
| Both windows unmapped after preview | 20 samples over 9.55 seconds; no increment in app CPU accounting. Same sampled memory high-water values. | CPU accounting has 10 ms resolution; zero observed ticks does not establish zero CPU work or zero wakeups. |

The app had 107 threads in both phases, including the software graphics stack's
threads. Thread count alone does not diagnose app scheduling overhead. Mesa's
CPU renderer and retained Settings/resources explain why these measurements
cannot predict a Mac GPU's footprint or energy. The hidden interval suggests the
current app already stops substantial demo drawing; it does not establish Mac
idle power or model-worker behavior.

## Existing evidence to retain

[Performance](performance.md) contains earlier native Windows inference results,
source-level audits, and accepted/rejected experiments. Useful constraints are:

- Real inference ranges from tens of milliseconds for short GPU fixtures to
  seconds for long recordings or CPU execution. Model work dominates the
  synthetic controller latency; Windows results do not rank Mac inference paths.
- Parakeet retains substantial accelerator scratch after long recordings.
  Unloading it releases resources but loses warm readiness. Total app, helper,
  model, and accelerator memory must all be reported.
- Disabling Whisper timestamps improved speed but caused severe skipped speech
  in the existing corpus. Chunking and speculative requests also failed existing
  quality or total-work comparisons. These are rejected results, not defaults
  to restore in a new language.
- Hidden-window scheduling and render-resource ownership have caused meaningful
  defects. Recent patches already fix multiple such problems. Historical
  pre-fix CPU/memory figures must not be used as the new app's comparison baseline.
- Finish and Cancel already `unpark` the capture consumer immediately. Its 5 ms
  data polling interval is not a mandatory 5 ms stop delay.

## Source audit and experiments

| Area and source | Present behavior | Candidate and proof required |
| --- | --- | --- |
| Session: `crates/app/src/runtime.rs`, `ports.rs` | Single owner, session identities, asynchronous cleanup, generation-bound insertion permit, fakeable effect boundaries. | Extract GPUI-free ownership for a native shell. A language rewrite is justified only if native profiling finds a material cost. Preserve cancellation races before measuring speed. |
| Capture: `crates/app/src/audio.rs` | CPAL callback downmixes into a bounded float ring. Consumer polls every 5 ms while recording, converts to PCM16, collects levels, gates speech, trims, and prepares in-memory WAV. | Compare wake strategies, float staging, and capture adapters independently. Count wakeups, callback work, stop latency, and audio loss. CPAL already uses Core Audio's HAL Output AudioUnit on macOS; direct HAL is not automatically a different or faster path. |
| Inference: `crates/app/src/local_speech.rs` | One warm owned process, WAV multipart over loopback HTTP, bounded response, silent accelerator warmup, cancellation recovery. | Compare existing HTTP with the pinned engine's native float-PCM ABI inside an owned helper. Isolate codec/transport, resampling, model time, and warmup. Do not attribute all inference time to HTTP. |
| Buffering: `audio.rs`, `local_speech.rs` | PCM16 payload, selective capacity compaction after heavy trimming; requests retain audio while needed. | Float32 avoids one quantization/decode route but uses twice PCM16 storage. At 48 kHz for 300 seconds, raw mono payloads are 28.8 MB PCM16 versus 57.6 MB float32. These are byte counts, not measured footprint. Count copies and allocations before adding shared memory. |
| Process lifecycle: `crates/platform/src/process.rs`, `local_speech.rs` | Orderly Unix shutdown cleans process groups; abrupt app death can leave a worker. Cancellation may require terminating and replacing the worker. | Add a parent-liveness contract for the new owned helper; test parent crash, kill, inference hang, and replacement. A launchd XPC service changes lifecycle semantics and must be assessed separately from transport speed. |
| UI: `pill.rs`, `shell.rs`, `tray.rs`, `vendor/gpui` | GPUI Settings and pill, analytic continuous springs, coalesced levels, native nonactivation, patched frame/resource lifetime. | Compare AppKit/Core Animation with current GPUI on the same Mac and display. Measure presented frames, app/render-server work, hidden wakeups, and retained Settings resources. Framework branding is not performance evidence. |
| Native input/insertion: `crates/platform/src/macos.rs` | Owned event tap; Fn/Space gesture; passive Escape; target and modifier checks; guarded clipboard paste or Unicode events. | Preserve delivery/commit ordering. Measure event-to-owner and owner-to-submission separately. Avoid moving blocking preparation to the tap or AppKit thread. |
| Setup/settings: `setup.rs`, `config.rs`, `shell.rs` | Pinned verified resumable artifacts; owned cancellable setup; off-UI validation and coalesced durable saves. | Package a signed helper without weakening cancellation, artifact integrity, or manual engine choice. Existing configuration and unsaved drafts are parity requirements. |

The highest-value initial experiments are native baseline collection, direct PCM
inference versus HTTP, helper death/cancellation, and native UI scheduling/resource
cost. Capture polling and model scratch reuse follow once native traces identify
their contribution. Core ML/ANE conversion, custom Metal kernels, quantization,
and buffered inference remain experiments rather than assumed wins.

## Native gaps

No measurement here answers these questions:

- Stop-to-visible-text latency and cold/warm onset on M1 or later hardware.
- Metal inference latency, peak unified-memory pressure, and repeated long-input
  scratch retention with the pinned model.
- Energy or idle wakeups on a Mac, including WindowServer and the helper.
- Actual frame presentation on 60/120 Hz and mixed-refresh displays.
- Permission loss, sleep/wake, microphone route/device failure, Spaces/fullscreen,
  native clipboard ownership, Unicode insertion, or force-quit containment.
- Performance and signing behavior of the proposed native bundle.

The [PRD](apple-silicon-prd.md) makes these release gates. The
[platform research](apple-silicon-research.md) records first-party API constraints
and the experiments needed to choose between competing implementations.

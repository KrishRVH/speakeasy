# Performance

Speakeasy optimizes the time from finishing speech to usable text. Recognition
quality, immediate cancellation, smooth animation, idle cost, and memory are
constraints on that goal.

Figures below are reference measurements on Windows 11 with a Ryzen 9 9950X3D,
an RTX 4090, and 64 GB of RAM, using public JFK and LibriSpeech recordings.
Unless stated, they exclude live microphone capture and editor insertion.
Recognition scores use case- and punctuation-normalized word edit distance;
they are corpus screens, not guarantees for arbitrary dictation.

## Recognition

Whisper keeps full audio context and default timestamp decoding. Disabling
timestamp tokens was 26% faster on long recordings but skipped speech: 508
long-form errors per 7,488 words versus 130. Returned text contains no timecodes.

| Whisper small.en | CPU median / p95 | CUDA median / p95 |
| --- | ---: | ---: |
| 120 clips, 2–20 seconds | 1,273 / 1,958 ms | 74 / 151 ms |
| Six chapters, 129–294 seconds | 28,243 / 31,911 ms | 2,438 / 2,732 ms |

CPU runs use eight AVX2/FMA/F16C threads; CUDA runs use the b5130 runtime with
four. Both backends made nearly the same word errors.

Parakeet, through NeMo-Speech.cpp v0.1.0 with the v3 q8 model, is the faster GPU
option. These times run the real session owner with fake capture and insertion:

| Audio duration | Whisper stop to insertion | Parakeet stop to insertion |
| --- | ---: | ---: |
| 3.3 seconds | 42 ms | 11 ms |
| 10.4 seconds | 96 ms | 20 ms |
| 293.7 seconds | 2,489 ms | 929 ms |

Setup picks the Parakeet backend from what the machine can run. On the reference
machine, the 11-second JFK clip took 17–44 ms per request on CUDA, about 50 ms on
Vulkan after a 1.3-second first request and a five-second startup, and about
680 ms on the CPU.

Across the corpus, Parakeet made fewer word errors than Whisper (124 versus 212
per 5,230 short words; 100 versus 130 per 7,488 long words), with slightly
weaker long-form punctuation. It accepts native 48 kHz capture: the added
upload and resampling time, about 4 ms short and 131 ms long, does not justify
an app-side resampler.

## Responsiveness

- Ready text begins owned asynchronous insertion when the controller receives
  it; fake-device timings add tens of microseconds before native work.
- Cancelling GPU inference keeps the worker when a bounded silent request proves
  it healthy within two seconds. This cut the median cancel-to-next-result from
  637 to 87 ms. A failed check terminates and replaces the worker; CPU
  cancellation always does.
- Pause retires the runtime off the UI thread, since worker shutdown takes about
  84 ms. Quit waits for owned cleanup.
- The pill paints its grille in one canvas and requests frames only while it
  moves, with a 200 FPS budget synchronized to native display frames. Meter
  updates coalesce into the pending frame; session changes redraw immediately.
  Settled and hidden views request none.

## Memory and idle cost

- Capture reserves ten seconds of mono PCM and grows on its consumer thread up
  to the five-minute limit; the audio callback never allocates. A one-second
  recording used 1.8 MB of private memory in the reference measurement; an
  up-front five-minute reservation used 29.7 MB.
- Edge trimming uses an exact integer energy scan: 2.4 ms for five minutes of
  48 kHz audio.
- Parakeet's worker retains its largest GPU buffers, about 3.8 GB after a
  five-minute recording, until Pause releases the model.
- GPUI's Windows VSync loop wakes even when drawing has settled. Hiding Settings
  more than halves idle CPU cycles; dormant NVIDIA driver threads account for
  most of the thread count.

## Approaches not taken

Measurements and cost tradeoffs:

- Chunking at pauses was faster on CPU but degraded punctuation and casing at
  chunk boundaries. Whole-recording recognition remains the policy.
- Shortened Whisper context gave a small median gain with attention-cache side
  effects.
- Streaming models did not preserve punctuation quality. Word-error rate alone
  cannot choose a dictation engine.
- Parakeet cuBLAS substitution, cached F16 weights, stock Q8 kernels, and graph
  optimization produced no useful combined latency and memory gain.
- Whole-recording speculation repeats work and can delay the final request on
  the single worker.
- An always-on microphone, a worker pool, and a background service are not
  justified by these measurements.

## Not yet measured

Representative microphone onset and teardown, real editor insertion, displayed
frame pacing, mixed-DPI and mixed-refresh displays, accessibility, idle power,
and macOS runtime performance. Default tests and `--demo` do not cover these boundaries.

## Measuring

Record hardware, model and runtime, sample rate, request boundaries, warmup, and
competing load. Compare paired runs and check word accuracy and formatting as
well as latency. Keep transcripts out of logs. The opt-in tests
`profile_fixture_dictation` and `profile_ready_text_latency` exercise the real
controller with public audio or fake devices; the README lists the fixture
variables. Use ordinary native profilers rather than a permanent benchmark suite.

## Native capture with Valorant

On 2026-09-29, explicit native acceptance on Windows compared the same 48 kHz,
stereo microphone route with Discord, Voicemeeter, and Focusrite Scarlett drivers
in use. Speakeasy was already running before Valorant opened, matching the
reported trigger. Temporary metadata probes measured callbacks and buffer
occupancy without saving audio, transcripts, or device names.

| Competing load | Delivered audio | Native discontinuities | Maximum callback work | Maximum consumer interval | Ring high-water mark |
| --- | ---: | --- | ---: | ---: | ---: |
| Valorant closed | 5.76 s | None | 8.8 microseconds | 6.00 ms | 960 / 48,000 frames (2%) |
| Valorant open | 26.02 s | One, before the first packet | 9.5 microseconds | 6.12 ms | 960 / 48,000 frames (2%) |

With Valorant open, CPAL reported `Xrun` 3.6 microseconds before delivering the
first packet, and no further stream notifications occurred. All delivered frames
reached PCM; neither application ring overflowed. Full capture timestamp spans
and delivered frame durations agreed to within one sample in both recordings.
Small positive per-packet timestamp gaps also occurred with Valorant closed and
corrected over the full capture; their sum alone would exaggerate missing audio.
Timestamps are estimates and cannot prove that the driver delivered every sample.

Capture allows `Xrun` only before its data callback queues the first sample.
A later `Xrun` fails with an audio-interruption explanation, avoiding silent
transcription of potentially incomplete speech. Driver failures retain their
cause, and a full application ring has a separate message. These measurements do
not establish whether Valorant, Vanguard, or the audio route generates the startup
notification, and do not establish performance under every gaming load. Production
builds do not collect these measurements.

## Windows runtime measurements

Release-build checks on 2026-09-29 used the same reference machine and installed
Parakeet GPU backend. CPU percentages below describe one logical core, not the
whole machine.

### Idle and rendering

The configured app, with Settings and the pill hidden, consumed 7,359 ms of CPU
over ten seconds before the hidden-paint fix. Thread descriptions identified the
main UI thread; samples located repeated Windows message dispatch and paint
validation for the pill. The dictation and shortcut threads were idle. Completing
one native `BeginPaint`/`EndPaint` cycle cleared the pending paint region and
stopped the loop in that running process; `ValidateRect` alone did not.

With the hidden-paint adapter installed, a fresh native launch consumed 15.62 ms
of app CPU over ten seconds: about 0.16% of one core, versus 74% before the fix.
The worker consumed another 15.62 ms. Settings consumed 109.38 ms over five
seconds while visible, and 46.88 ms after hiding: about 2.2% and 0.94% of a core.
These short samples have timer quantization and competing desktop load; they
measure CPU time, not watts or GPU energy.

The owned tray preview consumed 640.63 ms of CPU over five seconds of active
animation with Settings hidden, about 13% of one core. After settling, it used no
measurable CPU over three seconds and held a 73.4 MiB working set. Native checks
confirmed click-through, nonactivation, Settings hide/reopen, configuration
preservation, and listener cleanup. Hidden and visible paint dispatch has a
separate opt-in native regression test with a fake renderer.

### Recognition and controller latency

The controller submitted 100 fake recognition results: ready text to fake
insertion took 18 microseconds median and 54 microseconds p95. Public-fixture
runs used the real controller and worker with fake capture and insertion, five
requests per run, and one second of silent GPU warmup before readiness:

| Fixture | Backend | Stop to fake insertion, median / maximum |
| --- | --- | ---: |
| 11-second JFK, 16 kHz | GPU | 53 / 67 ms |
| 11-second JFK, 48 kHz | GPU | 55 / 63 ms |
| 294-second repeated JFK, 16 kHz | GPU | 1,753 / 1,819 ms |
| 294-second repeated JFK, 48 kHz | GPU | 1,946 / 1,981 ms |
| 11-second JFK, 16 kHz | CPU | 974 / 998 ms |

The 48 kHz fixture repeats each source sample three times; the long fixture
repeats public speech. These are transport and duration checks, not representative
accuracy corpora. Each result passed the fixture phrase and whitespace checks;
transcripts were not printed. Worker startup took 0.52–0.78 seconds, and shutdown
35–274 ms. Microphone teardown and OS insertion are excluded. Concurrent desktop
load varied, so rate differences do not isolate resampling cost.

### Memory and remaining costs

Before Settings was created, the fresh tray app used 84.8 MiB private memory and
51.7 MiB resident working set. Opening Settings increased private memory to
102.4 MiB, retained on hide to preserve unsaved edits. The warm worker held a
183.7 MiB working set; this excludes GPU memory. There is no measured memory
reduction from the paint fix. The Windows executable is about 11.1 MiB, excluding
the separately installed model and engine.

Capture callback work and queue occupancy are bounded in the native comparison
above. Controller latency is small compared with recognition. The warm model
retains accelerator buffers for subsequent requests; Pause releases them.
These checks support fixing hidden paint dispatch while preserving full-context
recognition, native-rate capture, and animation synchronized to native frames.
Real editor timing, GPU energy attribution, mixed-display frame pacing, and
macOS remain unmeasured.

### Active animation and GPUI Fast comparison

An isolated tray demo supplied identical changing meter levels every 32 ms and
stayed in hands-free recording for 40 seconds. Settings was hidden; no
microphone, hook, model, or clipboard was used. A temporary view probe counted
actual renders and their intervals during a ten-second window after five seconds
of settling. Three runs per variant alternated order, using release Windows GNU
builds and the same application rendering code. Probes and dependency adapters
stayed outside the repository and installed app.

[GPUI Fast](https://github.com/longbridge/gpui-fast) was pinned to
`7ab23f46f2ba3a040ceb27d387383a2896bc5ae1`. Its newer GPUI defaults throttle
inactive windows to about 30 FPS; the nonactivating pill rendered at 28.6 FPS
with that default. Setting `inactive_frame_interval: None` matched the existing
build's 400 FPS for the retention comparison. `GPUI_VIEW_RETENTION=0` disabled
retention in the same fork binary.

| Framework, at matched cadence | Renders/s, median | Process cycles/render, median | Private memory, median |
| --- | ---: | ---: | ---: |
| GPUI 0.2.2 | 400 | 1.58 million | 81.1 MiB |
| GPUI Fast, retention disabled | 400 | 1.50 million | 87.3 MiB |
| GPUI Fast, retention enabled | 400 | 1.56 million | 86.3 MiB |

Retention enabled was about 1% below the pinned framework and 4% above retention
disabled, with overlapping run ranges. This does not establish a retention
benefit for the animated pill. The comparison against 0.2.2 also includes newer
upstream code and dependencies; the same-binary toggle isolates retention.
Settings interaction and macOS were not compared, so these measurements cannot
rule out gains elsewhere.

A separate three-pair comparison kept GPUI 0.2.2 and budgeted all pill animation
at 200 FPS. Opening, closing, and processing share the same budget. Session
changes remain immediate. Frame callbacks read the current view through a weak
handle, and settled views stop scheduling them. Meter updates change history
without triggering an extra render while animation has a pending frame.

| Animation | Renders/s, median | Process cycles/s, median | Render interval, median / p95 |
| --- | ---: | ---: | ---: |
| Native 400 Hz cadence | 399.92 | 664.2 million | 2.50 / 2.82 ms |
| 200 FPS budget | 199.97 | 424.9 million | 5.00 / 5.27 ms |

The budget cut measured process cycles per second by 36%; pairwise reductions
were 34–41%. Memory showed no consistent reduction. Windows message dispatch and
the framework's VSync provider continue at the display cadence, so halving
renders does not halve total app work. Springs use actual elapsed time and do
not change their response constants. Lower-refresh displays remain eligible on
every native frame, though those displays were not tested natively here.

A separate temporary phase profile measured about 47 microseconds per frame for
render/layout/prepaint, 23 for painting, and 61 for GPU submission. These are
elapsed timings with probe overhead, not isolated CPU costs. They support
reducing repeated frames before adding view or geometry caches.

The complete simulated preview exercised startup, recording, the countdown,
stopping, processing, submission, cancellation with an immediate new session,
silence, and errors. Fifteen state changes reached the view's render in 1.00 ms
median and 2.33 ms maximum. Every animated phase had a median interval near
5 ms; no renders occurred in the final four settled seconds. This measures
state observation to render, excluding the OS display and real input path.

After installation, with Valorant running and both app windows hidden, a fresh
native launch used 62.50 ms of CPU and 373.0 million process cycles over ten
seconds. It held 85.2 MiB private memory and a 51.4 MiB working set; the warm
worker held a 183.7 MiB working set. The executable and settings hashes were
verified across replacement. The measurement required no recording or editor
insertion; ongoing gameplay can deliver events to the app's shortcut observer.

Reported process CPU milliseconds ranged widely even between equivalent runs;
these comparisons use
[QueryProcessCycleTime](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-queryprocesscycletime)
for process-wide user and kernel cycle counts. These are same-machine comparisons,
not instruction counts, watts, GPU time, or a conversion to CPU utilization.
Counter behavior can depend on the CPU and its frequency policy, as described by
[Microsoft](https://devblogs.microsoft.com/oldnewthing/20160429-00/?p=93385/).
Render intervals measure view submission, not displayed frame pacing.

The fork separates platform creation, changes timer/context and paint APIs, and
requires dependency updates from Speakeasy's pinned 0.2.2. Native platform crates
must disable their default framework manifest when using Speakeasy's own
manifest. Adoption needs broader native regression checks and a benefit beyond
this pill workload; the production dependency stays pinned.

## Linux port and shared audio pass

Release profiles on September 29, 2026 used Rust 1.98.1, official GPUI 0.2.2,
an AMD Ryzen 9 9950X3D, and WSL2 with 32 logical CPUs. These are synthetic
component timings, not native application utilization. The audio fixture calls
the production callback and PCM consumer with 480-frame f32 packets at 48 kHz;
each run takes the median of 21 groups of 3,000 packets. Three alternating before/after
runs used retained release binaries, each pinned to logical CPU 31. The table reports the median of those
three medians; no microphone, hook, clipboard, or renderer was opened.

| Callback plus PCM work | Before | After | Reduction |
| --- | ---: | ---: | ---: |
| Mono | 2.028 µs/packet | 1.104 µs/packet | 46% |
| Stereo | 2.132 µs/packet | 1.128 µs/packet | 47% |
| Eight channels | 2.145 µs/packet | 1.294 µs/packet | 40% |

The producer publishes a packet once through rtrb's safe chunk iterator, and the
consumer releases a read chunk once after conversion. Both remain allocation-free
in the callback; rate, channel averaging, PCM conversion, meter cadence, ring
capacity, cancellation and the hard recording limit are unchanged. There is no
additional batching timer. Tests cover wraparound, partial overflow, invalid
samples, the sample limit, cancellation and native stream notifications.
The gain is small in absolute CPU time and cannot be presented as a 47% reduction
in app CPU. Native capture timing under gaming load remains a separate check.

Ready text to fake insertion was 63 µs median before and after, with p95 97 µs
and 93 µs respectively (100 results each). Five-minute edge trimming remained
about 2.4 ms; it was not changed. Immutable shortcut labels are shared between
snapshots instead of allocating strings on every level update. Linux tray
subscriptions filter host and watcher changes instead of receiving every bus
signal. Native insertion owns bounded/serialized work so cancelled queued requests
do not create overlapping clipboard operations or blocking threads. These latter
changes have no claimed native CPU or memory percentage yet.

The native Linux release binary was 24,825,240 bytes and its unbundled development
tar was 9,384,485 bytes at this measurement. It requires glibc 2.43. This is disk
size, not resident memory or a portable package size; the speech engine and model
are separate. The release packaging workflow targets glibc 2.35 and checks
bundled dependencies before producing distribution artifacts.

### Native process sampling

`profile-linux.py` reads numeric `/proc` metadata, including CPU time, RSS, PSS,
private resident pages, threads and descriptors. It detects process exit and PID
reuse. Linux context-switch counters describe the main thread, not all threads.
The sampler's synthetic single-core check measured 99.7% CPU and detected exit;
this validates sampling, not Speakeasy utilization.

`profile-windows.ps1` samples process CPU, working set, private committed memory,
threads and handles. Windows private committed memory and Linux private resident
pages are different metrics; compare each on the same OS. The Windows script
has not been executed here because WSL interop is denied in this sandbox.

Pass explicit app and worker PIDs, reported separately. For example:

```sh
python3 scripts/profile-linux.py --pid 1234 --pid 1235 --label idle-hidden \
  --seconds 30 --output artifacts/profiles/linux-idle.json
cargo test -p speakeasy --release --locked profile_audio_pipeline -- --ignored --nocapture
cargo test -p speakeasy --release --locked profile_ready_text_latency -- --ignored --nocapture
```

```powershell
./scripts/profile-windows.ps1 -ProcessIds 1234,1235 -Label idle-hidden `
  -Seconds 30 -OutputPath artifacts/profiles/windows-idle.json
```

100% CPU means one logical core; machine-normalized samples divide by the logical
CPU count. Use at least 30 seconds for low idle CPU, where OS tick quantization
matters. Keep raw reports under ignored `artifacts/`. The scripts read no command
lines, audio, transcripts, keystrokes or engine output. Do not add app and worker
RSS as unique physical memory: shared pages can be counted twice. Linux PSS can
be apportioned; report Windows process working sets individually.

For each candidate, collect paired runs after warmup with the same hardware,
model, backend, display refresh, competing load and settings:

| Phase | Footprint | UX checks |
| --- | --- | --- |
| Launch and warmup | CPU time, elapsed readiness, peak memory, worker lifetime | Settings and Cancel remain responsive |
| Hidden idle and visible Settings | CPU, resident/private memory, threads, descriptors/handles | Reopen and save latency |
| Recording and pill motion | App CPU, memory growth, GPU activity, displayed frame pacing | Audio onset, 60/144/200/high-refresh pacing and no lost speech |
| Short and long inference | Worker CPU/GPU memory, peak app PCM memory | Stop-to-insertion p50/p95 and recognition quality |
| Cancel, Pause and Quit | Cleanup time, returning memory and resource counts | No stale paste, stuck keys or surviving owned work |

Process sampling cannot measure GPU energy, displayed frames, microphone onset
or editor acceptance. Use the native platform's frame/CPU/GPU profiler for those
checks, and report live microphone/input/clipboard acceptance separately. Keep
the 200-FPS ceiling, native display pacing, speech quality and interaction policy
as constraints. No cache, worker pool or background microphone is introduced.

The full workspace fake checks pass, including loopback download and instance
tests. Native Windows process sampling and Linux GNOME/KDE/X11 acceptance were
not performed for this pass. Whole-app CPU, GPU, memory and energy improvements
from this candidate remain unmeasured.


## Performance audit for 0.2.1

The audit examined application CPU/rendering, memory/resource lifetime, native
interaction latency, setup, and distribution across Windows, macOS, and Linux.
Findings were reviewed against the code at `bf3582b` and challenged individually
before implementation. The additional measurements below used release builds
on Linux/WSL2, Rust 1.98.1, synthetic PCM, fake devices, loopback HTTP, and owned
fixture subprocesses. They did not exercise a live microphone, global hook,
clipboard, focused editor, real model, or displayed frame pacing.

### Implemented findings

| Driver | Evidence and decision |
| --- | --- |
| Settings storage on the UI thread | Validation, canonicalization, durable writes, and sync ran synchronously. Save and Resume validate off the UI thread; requested writes serialize and preserve later edits and Pause intent. Controlled tests hold save completion pending and exercise queued saves and lifecycle generations. |
| Windows/macOS shortcut startup | Native monitor constructors waited synchronously for initialization. Constructors return an owned monitor immediately; readiness arrives as an input message after native setup. Stop-before-ready remains effective, and completion precedes UI-thread resource disposal. The session service retains retiring monitors directly, so Quit does not depend on canceled UI tasks being dispatched again. Native startup latency is unmeasured. |
| Capture startup and teardown | The capture thread was detached, with cancellation checked late in startup. Capture owns completion and its join; retries wait for device teardown, and startup checks cancellation between native stages. Tests hold teardown pending while input and Pause proceed. Actual driver contention and onset latency are unmeasured. |
| Speech startup cancellation and replacement | Abort bypassed explicit process reaping. An isolated production-code probe left 10/10 children as zombies after retiring its Tokio runtime; cooperative cancellation and explicit stop left 0/10. Other live Linux runtimes can reap Tokio's orphan queue, so this does not establish a permanent app-wide leak. Zombies retain no model RSS or VRAM. Replacement waits for actual exit while input remains responsive. |
| Shutdown serialization | Microphone teardown could delay inference cancellation and model release. Shutdown signals cancellation immediately and cleans independent resources concurrently, then acknowledges completion. Controlled fixtures verify both cleanups begin before either is released. |
| X11 manual-copy delay | Manual copy shared automatic paste's physical modifier wait, up to 800 ms. Manual copy skips that wait while preserving cancellation and clipboard ownership checks. The focused test forbids even querying held modifiers in this mode. |
| X11 paste round trips | Each fresh XTEST cookie check synchronized separately: four round trips normally, six for terminal paste. Paste queues its complete key sequence and releases before one synchronization, then checks every cookie. Partial enqueue failures still attempt all releases; ambiguous input is never replayed. Native elapsed savings are unmeasured. |
| Wayland/Xwayland clipboard blocking | Native X11 clipboard initialization, copy, and ownership checks shared the current-thread desktop runtime. Foreign-owner reads can wait for another application. One bounded, serialized, owned clipboard worker keeps these calls off that runtime; initialization still gates readiness. An in-flight native call can delay cleanup, but cannot run after a newer queued copy. |
| Portal insertion and clipboard transfers | Insertion preparation and selection RPC/pipe waits blocked desktop-service progress. Preparation and bounded transfers remain pending in the existing select loop, with RPC/pipe deadlines and backpressure. Selection lifetime is separate from insertion commitment, so editors can read committed clipboard data; replacement invalidates only the preceding selection's transfers. |
| Completed setup partials | A correctly sized `.part` reset download progress and issued a full GET. A loopback fixture reproduced this; completed data is verified and promoted locally. Corrupt completed data is replaced and verified. This can avoid downloading the 714 MB model again. |
| Resume hashing and cancellation | Synchronous SHA-256 over a 512 MiB cached sparse file took 218 ms median across five runs, blocking cancellation on the setup runtime. Hashing uses asynchronous reads and 64 KiB chunks with an explicit yield between chunks. Cancellation preserves the partial for retry. This bounds uninterrupted hashing work, not physical disk latency. |
| Setup cancellation and Quit | Setup's detached thread could outlive Quit while cleaning an extraction or detection child. Cancel now retains an owned thread until completion without waiting on the UI thread; Quit signals every active/retiring setup and joins their cleanup. Controlled tests hold cleanup pending, and a fixture subprocess verifies its child and setup-directory locks are released before disposal returns. Native OS cleanup duration remains unmeasured. |
| Trimmed audio capacity | A five-minute 48 kHz allocation retaining ten seconds of speech plus padding held 28,800,044 bytes for 1,056,044 bytes of audio. Selective compaction releases 27,744,000 bytes (26.5 MiB) of capacity when spare space is at least 8 MiB and capacity is at least four times length. Ordinary captures keep their allocation. Cost varied by allocator state: one separate shrink probe took 1.49 ms median; a paired trim probe found no consistent added cost. HTTP upload or queued startup can retain this capacity; it is not necessarily held for all inference. RSS savings remain unmeasured. |

### Findings deferred or rejected

| Candidate | Adversarial conclusion |
| --- | --- |
| Settled/hidden framework wakeups | GPUI's native scheduling can wake even when application drawing has stopped. The app already stops requesting settled frames; the measured Windows hidden idle cost is small. Changing upstream scheduling needs native energy and visibility checks, so no framework fork or idle service was added. |
| Linux refresh-rate selection | Pinned GPUI's X11 client chooses the first active RandR CRTC and does not track window/display changes. Wrong-output cadence is a real source-level risk; it belongs in a tested upstream platform change. Mixed-display native verification is unavailable here. |
| Slower processing-highlight animation | A subtle periodic highlight keeps Processing animated at the existing frame budget. A lower cadence could reduce render count, but smoothness and whole-app savings need displayed-frame checks. Animation behavior is preserved. |
| Frame-budget divisors on high-refresh displays | Nondivisor refresh rates can produce alternating eligible-frame intervals. Source cadence alone does not establish visible judder. No display policy changed without native evidence. |
| Filtering meter-only Settings/tray notifications | Observers perform small snapshot/string work around 30 times per second, while equality checks already prevent unchanged redraws. A naive filter breaks theme refresh because theme changes republish the same snapshot; running and retiring state also matter. No separate publication framework was justified by profiling. |
| Spring/clock calculations | Thirty production spring evaluations cost roughly 0.08 microseconds per frame; clock formatting about 0.03 microseconds per call. Caches or extra settled checks would add complexity for negligible savings and can worsen moving frames. |
| Tray raster and grille caches | Tray state updates are already filtered, and the grille already paints in one canvas. These are not established dominant costs; extra caches were rejected. |
| Destroying hidden Settings | Reference measurements attribute about 17.6 MiB private memory to opening Settings. Destroying the view loses unsaved edits and adds reopen/device-enumeration work. Retaining the view preserves the interaction contract. |
| Capture polling and finish wakeups | The consumer wakes every 5 ms while recording. Changing its wake strategy could reduce wakeups or shave polling latency, but native callback/driver costs and power are unmeasured. The bounded callback path is preserved. |
| Automatic model unloading or recycling | Parakeet's large retained accelerator buffers trade memory for warm latency. Pause releases them. Automatic unloading would introduce cold starts and policy complexity without a demonstrated UX gain. |
| macOS dependency default features | AppKit/Foundation default features pull a broad dependency graph. Linker elimination and native footprint gains are unknown; selective feature changes would need macOS validation. No dependency policy changed. |
| More aggressive distribution stripping | A fresh Linux release executable shrank from 24,875,400 to 20,586,832 bytes with symbol stripping (17.2%); gzip output shrank 7.2%. This measures disk/download size, not RSS or launch speed; packaging may already strip, and symbols aid profiling. The release profile is retained. |
| Native startup allocation failure | If the dictation thread cannot be created after its monitor starts, constructor cleanup still joins the monitor synchronously. This rare resource-exhaustion path retains native ownership; removing the join or adding a startup broker would add risk and complexity without measured benefit. Normal startup and Pause remain asynchronous. |
| Linux pill visibility connections | X11 visibility work opens a connection and waits for native replies. Frequency and user-visible cost are unmeasured. A persistent connection/cache adds resource and display-lifecycle ownership; no speculative cache was added. |
| Recognition/resampling/worker pools | Existing corpus measurements favor full-context recognition, native-rate capture, and one warm worker. Timestamp disabling, chunking, speculative requests, app resampling, always-on capture, and provider switching risk quality, latency, privacy, or footprint. No recognition policy changed. |

The safe pre-change profiles put 480-frame 48 kHz packet work at roughly
0.90 microseconds for mono/stereo and 1.02 microseconds for eight channels;
five-minute trimming took 2.09 ms median and 2.25 ms p95. Fake ready-text insertion
took 52 microseconds median and 67 microseconds p95 over 100 requests. These
component figures guide prioritization; native interaction, energy, model memory,
and displayed-frame measurements remain separate acceptance work.

The final release fixtures measured 0.78/0.81/1.00 microseconds per packet for
mono/stereo/eight channels, 2.08 ms median and 2.23 ms p95 for five-minute
trimming, and 55 microseconds median and 62 microseconds p95 for fake ready-text
insertion. These single-run component results show no material regression and
do not isolate a whole-app improvement. The local workspace passed 83 tests
with six opt-in fixtures ignored, formatting, and Clippy. Windows GNU workspace
and Apple silicon platform cross-Clippy also passed; native release builds and
packaging are checked by the release workflow. Live native acceptance remains
separate.

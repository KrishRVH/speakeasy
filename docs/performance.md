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

- Ready text is submitted in the owner turn that receives it when modifiers are
  already released; the controller adds tens of microseconds before OS insertion.
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
  recording uses 1.8 MB of private memory instead of 29.7 MB for an up-front
  five-minute reservation.
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

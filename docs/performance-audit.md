# Performance

The performance audit used Opus 5.5 through Claude Code at xhigh, followed by
native Windows measurements and review of the implemented changes. This document
keeps the measurements relevant to the current architecture and decoding policy.
The detailed experiment history is retained in Git; raw metrics and recipes are
ignored local artifacts under `artifacts/rust/`.

The priority is time from finishing speech to usable text, with recognition
quality, immediate cancellation, smooth animation, idle cost, and memory as
constraints. The evidence supports large gains in inference and specific app
operations. **An app-wide 10× improvement is not established.**

## Measurement scope

Results below were collected September 26–27, 2026 on Windows 11, Ryzen 9 9950X3D,
RTX 4090, and 64 GB RAM. Speech fixtures are public JFK and LibriSpeech recordings.
No live microphone, global hook, clipboard, or target-editor insertion was used.

- **HTTP timing** includes upload, worker decoding/resampling, inference, and
  response handling. It excludes capture, app audio preparation, and insertion.
- **Controller timing** runs the production session owner and speech adapter with
  fake capture and insertion. It measures Finish to fake insertion acknowledgement.
- **Draw/submission timing** measures CPU rendering or frame submission. It does
  not measure when pixels appear on the display.
- Recognition scores use case/punctuation-normalized word edit distance. They
  are corpus screens, not official benchmark scores or arbitrary-dictation guarantees.

## Recognition latency and quality

### Whisper

Current Whisper requests retain full audio context and default timestamp
decoding. CPU and CUDA use the same upstream commit
`927cfce34f31707e17f2bff35c349632fb9e2c3a` and small.en fp16. CPU uses
AVX2/FMA/F16C with eight threads; CUDA uses the official b5130 runtime and four.
Warmup is excluded. CPU and GPU were measured in separate runs.

| Workload | CPU median / p95 | CUDA median / p95 | Median paired speedup |
| --- | ---: | ---: | ---: |
| 120 clips, 2–20 seconds | 1,273.34 / 1,957.63 ms | 74.21 / 151.30 ms | 16.84× |
| Six chapters, 129–294 seconds | 28,242.97 / 31,911.33 ms | 2,437.71 / 2,732.18 ms | 11.67× |

One short pair was below 10×; all six long pairs exceeded 11.46×. p95 uses nearest
rank, so the six-chapter p95 is simply its maximum. CPU/CUDA errors were 105/106
per 2,615 short reference words and 64/65 per 3,744 long words. Normalized hashes
matched 118/120 short and 5/6 long outputs. This is close observed accuracy,
not exact backend parity. Evidence: `corrected-decoding-performance.json`.

Default timestamp decoding is deliberate. A paired screen found 130 long-form
errors per 7,488 words versus 508 with timestamp tokens disabled, at a 26% latency
cost. Short errors increased by six per 5,230 words. The retained mode reduces
long-form omissions; returned text still contains no timecodes.
Evidence: `long-decoding-performance.json`.

### Parakeet

The explicit GPU option uses NeMo-Speech.cpp v0.1.0 and Parakeet v3 q8. In a
paired controller screen, five requests ran for each case and engine, with
engine order alternated by case:

| Audio duration | Whisper stop to fake insertion | Parakeet stop to fake insertion |
| --- | ---: | ---: |
| 3.275 seconds | 42.43 ms | 11.11 ms |
| 10.435 seconds | 95.89 ms | 19.99 ms |
| 293.7 seconds | 2,488.78 ms | 928.69 ms |

Cancellation feedback was about 0.19 ms for both. Immediately dictating another
long fixture took 2,566.97 ms with Whisper and 1,708.75 ms with Parakeet. A
confirmation observed no reload after cancellation for either engine and no
owned worker left running. GPU interference was not continuously monitored.
Evidence: `nemo-runtime-performance.json`.

The broader Parakeet corpus screen produced 124/5,230 short word errors versus
Whisper's 212, and 100/7,488 long errors versus 130. Long punctuation matches
fell by 30/4,494 shared aligned positions and casing by ten; short formatting
improved slightly. External GPU activity limits the latency comparison from
that run. Evidence: `nemo-corpus-performance.json`.

Native capture commonly supplies 48 kHz. Two paired passes through one Parakeet
worker compared original 16 kHz input with offline-generated 48 kHz versions:

| Input | 16 kHz median HTTP | 48 kHz median HTTP | Median paired added time |
| --- | ---: | ---: | ---: |
| Three short clips, 3.3–10.4 seconds | 17.03 ms | 23.58 ms | 4.23 ms |
| Six chapters, 129–294 seconds | 635.42 ms | 771.91 ms | 130.62 ms |

All 18 paired normalized word hashes matched. The extra cost includes upload,
resampling, and any changed recognition work; it cannot all be attributed to
resampling. This screen does not justify adding a resampler to the app.
Evidence: `nemo-rate-performance.json`.

## Responsiveness and rendering

- **Ready-text dispatch:** insertion starts in the owner turn when modifiers are
  already released. Native Windows fake-device median/p95 was 18/57 µs across
  100 requests. This excludes OS insertion. Evidence:
  `optimization-measurements.json`.
- **Cancel and redo:** retaining a recoverable GPU worker reduced the median
  cancel-to-next-fake-insertion from 637.02 to 87.39 ms (7.29×), across three
  sequential runs of each policy with Whisper small.en. Recovery uses silent
  inference with a two-second cap, then terminates and waits before replacement
  on failure. Evidence: `cancel-retention-performance.json`.
- **Pause:** runtime retirement is asynchronous. Native worker shutdown measured
  83.8 ms through the Rust Windows adapter, enough to motivate removing that wait
  from the UI thread. Quit still owns and waits for cleanup. Evidence:
  `ui-audio-performance.json`.
- **Meter:** one canvas paints all 24 bars. Two comparison orders measured
  75.65/146.7 and 76.9/148.6 µs median/p95 CPU draw time, reducing the median by
  23–35% versus separate layout elements. Each run contained 1,600 samples.
  Evidence: `meter-performance.json`.
- **GPU contention:** four demo runs in idle/loaded/loaded/idle order measured
  median submission intervals of 16.64–16.66 ms, p95 17.28–17.55 ms, and maximum
  18.04 ms. Loaded runs repeated five-minute Whisper requests. No sampled interval
  exceeded 25 ms. Evidence: `submission-cadence-performance.json`.

Actual display pacing remains unverified: PresentMon could not start its ETW
session with the available privileges, and DXGI returned no usable presentation
timestamps. No privilege or security policy was changed for those probes.

## Startup, memory, and idle cost

A stable-path Whisper provider check measured readiness including silent warmup
at 6,493.28 ms on first use and 572.38/573.95 ms on immediate repeats. Inference
and shutdown remained about 700 and 43 ms. File, driver, and security caches were
uncontrolled. These are provider times, not full app launch times.
Evidence: `provider-restart-performance.json`.

Parakeet's Windows worker used about 9.995 GB private commit and 545 MB peak
working set. Separate per-process GPU counters showed 1.208 GB dedicated after
readiness and 3.773 GB after a 293.7-second request, still held 60 seconds later;
shared GPU memory was 69 MB. Commit is not resident GPU memory. The engine retains
its largest intermediate buffers; subsequent short requests do not release them.
Evidence: `nemo-memory-performance.json`, `nemo-stage-performance.json`.

Capture starts with a ten-second allocation and grows on its consumer thread.
A Windows allocation probe touching one second at 48 kHz reduced private memory
from 29.66 to 1.77 MB versus reserving five minutes immediately. This isolates the
capture buffer, not total app memory. A full-length probe preserved all samples;
the longest growth operation was 2.108 ms. The callback does no allocation.
Evidence: `optimization-measurements.json`.

An exact integer RMS scan preserved trimming thresholds and reduced five-minute
synthetic scan time from 7.58 to 2.37 ms at 48 kHz and 29.40 to 9.03 ms at 192 kHz
in a Linux probe. Evidence: `ui-audio-performance.json`.

An idle Windows Release demo used 62.5 ms CPU over 5.02 seconds, 58.9 MiB working
set, and 97.2 MiB private memory. A separate thread probe attributed 103 of 114
threads to the NVIDIA driver; most were dormant. GPUI's Windows VSync loop still
wakes even when app drawing has settled. Hiding the demo's Settings window reduced
five-second process cycles from 104.7 to 45.9 million, but this is not a complete
tray-plus-loaded-model measurement. Idle power remains unmeasured.
Evidence: `optimization-measurements.json`, `idle-attribution.json`.

## Boundaries for further optimization

Keep recognition quality and ownership intact when evaluating a faster path:

- Independent pause chunks reduced CPU stop latency but regressed punctuation
  and casing at chunk boundaries. Whole-recording recognition remains the policy.
- Shortened Whisper context introduced attention-cache interactions and did not
  justify its small median gain. Keep full context and decoder fallback.
- Streaming model screens did not preserve punctuation quality. Word-error rate
  alone is insufficient to choose a dictation engine.
- Parakeet cuBLAS substitution, cached F16 weights, stock Q8 kernels, and optional
  graph optimization produced no useful combined latency/memory improvement.
- Whole-recording speculation adds repeated work and can delay the final request
  on the single worker; the existing corpus supplied no qualifying final pauses.
- No custom GPUI fork, automatic frame-rate reduction, always-on microphone,
  worker pool, or persistent background service is warranted by current evidence.

## Remaining measurements

Native microphone onset and teardown, actual editor insertion, mixed-DPI and
mixed-refresh display behavior, accessibility, battery/idle power, and macOS
runtime performance need native acceptance. Default tests and `--demo` do not
cover these boundaries. Reuse public fixtures and owned demo processes for
unattended checks, and obtain explicit opt-in before using a live microphone,
global input, clipboard, or editor.

For new measurements, record hardware, model/runtime identity, sample rate,
request boundaries, warmup, and competing load. Compare paired runs while
checking word accuracy and formatting. Keep transcripts out of diagnostic logs.
Use the existing fixture seams and ordinary native profilers; no permanent
benchmark framework or extra test gate is needed.

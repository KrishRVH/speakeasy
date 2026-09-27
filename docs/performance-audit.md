# Opus 5.5 performance audit — September 26, 2026

Requested by the user at the final delivery checkpoint. Claude Code was invoked
with `--model claude-opus-5-5 --effort xhigh`; returned metadata confirms Opus 5.5.
The audit inspected the Rust implementation and upstream dependencies and used
public fixture audio. No live microphone, hook, clipboard, or editor insertion
was involved.

This is a chronological audit record. Each measurement, review verdict, and
delivery checkpoint describes its stated build and conditions; later sections
record corrections and follow-up decisions. The original reports at the end are
preserved, including recommendations that were subsequently implemented or rejected.
Some evidence paths refer to ignored local artifacts or temporary files, which
are not part of the repository and may no longer be available.

For the current Windows packages, including the complete Parakeet bundle, see
the [README](../README.md#optional-parakeet-engine) and [handoff](../handoff.md).
The additional app-wide 10× target remains unachieved; it does not prevent
delivering the available verified Windows work. Native microphone/editor acceptance,
displayed frame pacing, and native Mac execution remain separate verification gaps.

## Outcome and follow-through

**Current decoding correction:** the app now keeps Whisper's default timestamp
decoding to reduce omissions in long recordings. Earlier timings in this audit
used `--no-timestamps`; they remain historical measurements, not current-build
latency claims. See “Long-form decoding and chunking” for the accuracy/latency
tradeoff and “Corrected decoding: CPU versus CUDA” for the corrected Whisper
inference baseline. The later Parakeet sections cover the additional engine.

The measured **13.17×** gain is warm CPU inference on one public clip with tiny.en
on a Ryzen 9 9950X3D: an AVX2 engine with eight threads versus the temporary scalar
engine with four. It is **not** a claim of app-wide 10× or 10× over an already
optimized engine. Model and decoding settings stayed the same; fixture text hashes
matched. Raw measurements are in `artifacts/rust/performance-measurements.json`.

The delivery fixes from the audit have been implemented:

- Rewarm the worker after cancelled or failed transcription; failed warmup does
  not retry-loop. Existing controller checks cover both recovery paths.
- Normalize Whisper segment whitespace before insertion.
- Retry Windows clipboard acquisition with a short wall-clock budget, preserving
  the immediate cancellation gate. No live clipboard test was run.
- Reduce health-request timeout to 200 ms and readiness polling to 25 ms.
- Restore the 100 ms audible gate in 20 ms RMS windows, trim only quiet edges of
  at least one second, retain 500 ms padding and all interior pauses. A focused
  fixture test protects those boundaries and rejects a short click.
- Document optimized engine selection and label GPU use as a preference.

Shortening Whisper's context window, disabling decoder fallback, streaming chunk
merging, always-on capture, GPUI forks, and per-machine thread-default changes
were not introduced. The audit identified quality or evidence gaps for them.
No new runtime dependency, module framework, fuzzing, or ADR gate was added.

## Further optimization measurements

The next pass compared against the optimized CPU engine on the same machine,
with an RTX 4090 and driver 591.86 confirmed directly. Windows-local v1.8.3,
tiny.en, public 11-second JFK audio, eight threads, and unchanged decoding gave
warm medians of **346.38 ms CPU versus 31.72 ms CUDA (10.92×)**. All recognized
text hashes matched. This is one fixture and model, not a broad accuracy result.
GPU working set was about 451 MiB versus 169 MiB CPU; speed did not improve memory.
Repeating in reverse order gave 406.91 ms CPU and 34.11 ms CUDA (11.93×).
The earlier optimized CPU run was faster, at 290.44 ms; against that baseline,
31.72 ms is 9.16×. These runs show roughly 9–12× depending on baseline, so they
do not establish a universal minimum 10×. No affinity or system power settings
were changed to tune the comparison.

A unique process-only `CUDA_CACHE_PATH` reproduced a **9.99-second** first request.
With one second of synthetic silence first, initialization took 10.71 seconds
and the first real fixture took **48.77 ms**. The implementation now runs this
warmup during background loading when GPU preference is enabled, with a 120-second
cap and discarded output. The initialization cost moves earlier; it is not
eliminated, and dictation before loading completes still waits. Cache isolation
uses NVIDIA's documented [CUDA environment variables](https://docs.nvidia.com/cuda/cuda-programming-guide/05-appendices/environment-variables.html).
No user cache or provider settings were changed.

Ready text previously waited for a 10 ms controller timer even with modifiers
released. It now passes through the existing cancellation/commit gate in the same
owner turn. A 100-request fake-device diagnostic measured Linux median/p95
**11,322/11,479 µs before versus 87/145 µs after**; native Windows after measured
**18/57 µs**. These measure simulated insertion, not OS clipboard/paste completion.
Language is now sent per request, avoiding a model reload when it changes.

The capture buffer now initially reserves ten seconds and grows on its consumer
thread up to the five-minute limit. A native Windows allocation probe touching
one second at 48 kHz measured **29.66 MB private memory before versus 1.77 MB**
with the smaller reservation; working set stayed about 5.3 MB. This isolates the
buffer allocation, not total app memory. A five-minute synthetic sample check
preserved all samples with five growth operations, the longest **2.108 ms**.
Compared with upfront reservation, that full-length probe added about 1 MB of
private memory and 4.4 ms total allocation/conversion/verification time. No work
was added to the real-time callback, and microphone acceptance remains separate.

A newer idle Release demo sample measured 129 million process cycles and 62.5 ms
CPU time over 5.02 seconds, with 58.9 MiB working set and 97.2 MiB private memory.
This revises the earlier coarse zero-CPU-time smoke observation: idle work exists.
It does not measure wakeup count or power, or isolate GPUI's VSync loop from other
work. No GPUI fork was introduced on the strength of this one sample.

Raw samples are in `artifacts/rust/optimization-measurements.json`. This is progress
toward the requested further 10× gain; it does **not** establish another 10× across
the app. Capture onset, frame pacing, shutdown stalls, idle wakeups/power, and total
memory remain separate areas to measure. No extra runtime dependency was added.

Opus 5.5 at xhigh reviewed this pass and its finishing changes in session
`d20a788b-f918-4bb1-97ad-1a6bf7b6687a`, returning **Approve** with no blockers.
Its notes led to a clear CPU-only preference instruction, distinct warmup errors,
retention of a worker whose inference finished just before cancellation, and a
language assertion in the existing fake controller scenario. The final review
also inspected bounded buffer growth. These changes passed ten default tests and
Clippy on Linux and actual Windows; no additional default tests were added.
Raw reviews are `/tmp/speakeasy-opus-optimization-{review,final}.json`.

## UI and audio follow-up

Further measurements are in `artifacts/rust/ui-audio-performance.json`:

- GPUI's existing `ZED_MEASUREMENTS` instrumentation measured 431 demo draws:
  median **0.155 ms**, p95 **0.295 ms**, maximum **2.96 ms**. This covers CPU draw
  and the present call, not when pixels reach the display. Arrival timestamps of
  a second demo's stderr lines gave median 16.56 ms and p95 17.89 ms between lines;
  multiple windows, pipe delivery, and reader scheduling make these a proxy only.
- Native thread-start addresses attributed **103 of 114 threads** to NVIDIA's
  `nvwgf2umx.dll`. Most were dormant. The app's idle sample used about 0.9–1.2%
  of one CPU core; limiting an app executor would not remove those driver threads.
- Owned Windows engine processes took **9.0 ms CPU / 58.4 ms CUDA** to terminate
  after fixture inference. This is `Kill` plus `WaitForExit`, not the Rust Job
  Object adapter. The Linux Rust adapter reported **13.5 ms** shutdown. Synchronous
  runtime destruction on the UI thread was therefore a concrete concern. A later
  run through the actual Rust Windows GPU adapter measured **83.8 ms** shutdown.
  Pause now requests cancellation immediately, retains the retiring runtime, and
  waits asynchronously for its completion before applying any queued enable.
  Quit retains synchronous owned cleanup. One focused fake test holds native
  cleanup pending and verifies the acknowledgement boundary without timing gates.
- The PCM trim scan now sums exact squared integers, scaling once per 20 ms
  window. At the 192 kHz cap the sum is below 2^42, so its wrapping reduction
  cannot overflow and its f64 conversion is exact. This permits vectorization
  while preserving the previous threshold. A five-minute synthetic Linux probe
  improved from **7.58 to 2.37 ms at 48 kHz**, and **29.40 to 9.03 ms at 192 kHz**.
  Thirty-six fixed full-scale, threshold, rate, and padding cases matched the
  previous output byte for byte. No new default test or SIMD dependency was added.

Windows security checks limited native measurements during this pass.
Smart App Control rejected one rebuilt debug test executable (Code Integrity
events 3033/3077); the policy was left intact. A subsequent normal build with the
Pause changes was accepted and all **eleven** Windows tests passed, as did Linux
tests and Clippy on both targets. The portable
[PresentMon console](https://github.com/GameTechDev/PresentMon/blob/v2.6.0/README-ConsoleApplication.md)
ran but could not start an ETW session because the caller lacks the required
privilege/group membership. No elevation or group changes were attempted. This
ETW restriction prevents claiming measured display pacing. Native shutdown was
subsequently exercised through the accepted Rust test executable, as noted above.

Opus 5.5 at xhigh approved the integer scan and Pause ownership changes with no
blocking findings (`/tmp/speakeasy-opus-pause-review.json`, same review session).
It checked cancellation, acknowledgement ordering, queued enable/Pause behavior,
quit ownership, and exact arithmetic. Its note to release the retirement task's
own handle after completion work was applied.

A separate CUDA thread-count check found warm medians of 41.25/34.86/33.38/33.90 ms
at 1/2/4/8 threads for the same fixture. The four-thread default stayed unchanged.
After warmup, requests separated by 1/5/15 idle seconds took 38.59/31.90/34.55 ms;
all recognized text hashes matched. This small probe found no large idle penalty,
but does not establish behavior for other models, GPUs, or longer idle periods.

## Controller and provider measurements

The user delegated the performance criteria: prioritize stop-to-text latency,
with recognition quality, immediate cancellation, animation smoothness, idle cost,
and memory as guardrails. Freeze the optimized CPU baseline rather than comparing
against the earlier scalar build or choosing a slower CPU sample. A universal 10×
reduction in every resource metric is not supported by these measurements.

The opt-in `runtime::fixture::profile_fixture_dictation` drives the real Rust
controller and Whisper adapter with public audio and fake capture/insertion.
Timing starts at the finish gesture and ends at the fake insertion acknowledgement.
It includes dispatch, HTTP, inference, response parsing, and the cancellation gate.
It excludes device teardown, trimming/WAV creation, and OS paste/editor processing.
This fixture is 16 kHz; native capture commonly supplies 48 kHz, increasing upload
size and adding server resampling inside the timed request.
Five requests run per worker; the table uses the median of the last four.
Workers and models reside on Windows-local storage. The fixture is the public
11-second JFK clip and a 33-second concatenation of three copies, using tiny.en
fp16. CPU uses AVX2/eight threads; CUDA uses four threads on the same RTX 4090.

| Worker | 11-second stop to fake insertion | 33-second stop to fake insertion |
| --- | ---: | ---: |
| v1.8.3 optimized CPU baseline | 291.89 ms | 627.25 ms |
| v1.8.3 CUDA 12.4 | 33.13 ms (8.81×) | 83.04 ms (7.55×) |
| v1.9.4 / b5130 CUDA 12.4 | 27.75 ms (10.52×) | 75.92 ms (8.26×) |

The newer engine improves the existing GPU path by about 16% short / 9% long;
the 10.52× figure compares against CPU, not against the preceding GPU build.
[v1.9.4](https://github.com/ggml-org/whisper.cpp/releases/tag/v1.9.4) links its
official binaries through [b5130](https://github.com/ggml-org/whisper.cpp/releases/tag/b5130).
GitHub labels b5130 as a prerelease. The measured asset is
`whisper-cublas-12.4.0-bin-x64.zip`, SHA-256
`af520ddd034d985b55dfeea3e465ed93653ba2aee1a55e865033edc548c272a7`.
The app works with both versions without protocol or production-code changes.
Separate HTTP checks produced identical normalized transcript hashes across both
versions on the long clip; this is compatibility evidence, not broad accuracy
coverage. Recognition/segmentation quality across accents, languages, noise, and
models remains unmeasured.

The newer CUDA bundle is about 643 MiB compressed / 1.2 GiB extracted, versus
about 419 MiB / 694 MiB before. Separate long-fixture processes used about
438 MiB working set / 1,522 MiB private memory with the new engine, compared with
424 MiB / 1,690 MiB before. These counters exclude a separately measured VRAM
total and are not whole-app memory. First observed controller startup was 19.47 s
for the new engine, followed by 309 ms for another worker; DLL/kernel caches make
this an uncontrolled startup comparison. Background warmup remains necessary.
New-engine shutdown took 39–41 ms. User settings and system drivers were not changed.

New-engine long-clip thread sweeps at 2/4/8/16 threads gave warm HTTP medians
104.96/79.76/85.61/89.01 ms. Disabling flash attention gave 86.20 ms at four
threads. All normalized hashes matched. Keep the current four-thread/default
attention settings; neither experiment warrants new configuration or scheduling
logic. Raw samples and probe scripts are in ignored
`artifacts/rust/controller-performance.json` and `artifacts/rust/profiling/`.
The longer fixture still falls short of 10×; no app-wide minimum is claimed.

Opus 5.5 at xhigh approved this diagnostic and measurement pass, with no blocking
findings (`/tmp/speakeasy-opus-controller-review.json`, same review session).
Its notes led to accurate fixture failure messages, explicit sample-rate and
shutdown-version scopes, and the binary prerelease label/asset digest above.
At that checkpoint it found no further high-impact change supported by evidence that would
avoid the previously identified quality and scope tradeoffs.

Following the review, the Auto language request also passed both controller
fixtures. tiny.en forces English, so this checks request compatibility rather
than multilingual detection. Later 16 kHz Auto runs measured **33.88/105.64 ms**
short/long, slower than the table's English runs. The server source forces `en`
for this model; separate runs do not isolate a language-setting penalty.
The 10.52× short result is consequently one observed run, not a guaranteed minimum.

A synthetic 48 kHz variant, made by repeating each public PCM sample three times,
passed Auto recognition and measured **38.80/150.19 ms**. This exercises larger
uploads and real server resampling, but is not real microphone audio or a quality
benchmark. Resampling can also change decoder work, so the difference cannot all
be assigned to transport or resampling CPU cost. A matching CPU 48 kHz pass took
426.76/924.73 ms; it is recorded separately rather than replacing the frozen
16 kHz baseline with these slower samples.

An interleaved follow-up used one warm v1.9.4 CUDA worker, alternating the order of
16/48 kHz and en/auto requests across five cycles and excluding the first cycle:

| Input | English short / long | Auto short / long |
| --- | ---: | ---: |
| 16 kHz | 34.13 / 93.24 ms | 33.35 / 88.67 ms |
| 48 kHz | 38.43 / 112.89 ms | 37.83 / 110.97 ms |

Normalized text hashes matched across rates/language settings for each clip.
This supports a sample-rate cost and rules out a large Auto penalty on tiny.en
in this run. It also reinforces that timings vary across runs. A separate Windows
utility using the same upstream miniaudio header measured decode/resample medians
of 0.033/2.245 ms at 16/48 kHz for the short clip and 0.258/8.237 ms for the long
clip (24 warm iterations, GCC `-O3`, in-memory WAV to f32/16 kHz). This is not the
official server's compiler/build, and does not explain the entire request delta.
The measured conversion cost does not justify introducing a new streaming
resampler or changing capture quality to pursue a 10× claim.

## Idle refresh attribution

A later Windows Release demo probe measured **105.7 million process cycles over
about five seconds** after animation settled. Its UI thread used 54.0 million,
one busy app background thread 40.3 million, and one NVIDIA driver thread
10.4 million. Temporarily suspending only that owned preview's busy background
thread dropped total cycles to 10.7 million; resuming restored 97.5 million.
The thread was resumed before closing the owned process. This is a diagnostic
intervention, not a shipped optimization or a proposed production mechanism.
The existing draw log also contains no draws after nine seconds; its last draw
was at 7.41 seconds. App animation is not continuing through the idle sample.

GPUI 0.2.2's private VSync loop calls `DwmFlush`, checks for device loss, and
invalidates every tracked window each refresh. GPUI skips actual drawing when
clean, but still enters a UI callback. This source behavior and the probe strongly
implicate that loop. The native thread query did not expose its Rust thread name.
Current upstream [Windows platform code](https://github.com/zed-industries/zed/blob/main/crates/gpui_windows/src/platform.rs)
still contains this unconditional loop; upgrading would not remove it.

The demo keeps Settings visible. A separate probe hiding only its owned Settings
window reduced cycles from **104.7 to 45.9 million** per five seconds. The real
tray path closes Settings and hides the pill, so visible-demo idle overstates
normal UI work. Hiding a window is still not the full tray/loaded-model state.
The second suspension probe reached 12.3 million cycles, then 96.5 million after
resuming. Driver work remains, and sample variation rules out a universal 10×.

Opus 5.5 at xhigh confirmed no public GPUI hook exists and recommended retaining
the current dependency (`/tmp/speakeasy-opus-idle-decision.json`). A correct fix
must wake on real work, preserve animation callbacks and immediate first frames,
handle device loss, and avoid lost wakes. A timer slowdown or runtime suspension
would hurt UX. Maintaining an 8 MiB vendor fork for this desktop cost is not
justified by the evidence. No fork, registry patch, thread suspension, or extra
idle timer was added to production. Raw data/probes are in ignored
`artifacts/rust/idle-attribution.json` and `artifacts/rust/profiling/`.

## Cold CUDA initialization

NVIDIA's `cuobjdump` inspected the actual downloaded `ggml-cuda.dll` files.
The v1.8.3 CUDA 12.4 bundle contains 126 native cubin objects, all for `sm_52`.
The v1.9.4 / b5130 bundle contains 143 each for `sm_86` and `sm_89`. NVIDIA's
[Ada compatibility guide](https://docs.nvidia.com/cuda/ada-compatibility-guide/index.html)
explains that compatible native cubins avoid PTX compilation on first use;
`sm_89` matches this RTX 4090. This suggested checking cold initialization rather
than building a custom CUDA toolchain or changing recognition settings.

With four threads, tiny.en, and a fresh process-only `CUDA_CACHE_PATH` for each
engine, the first one-second silence request took **10,088.23 ms on v1.8.3 versus
71.39 ms on b5130 (141.3×)**. Subsequent public JFK requests passed and produced
the same transcript hash. No user cache, driver, or global environment changed.
This corroborates the kernel-compilation explanation, while the timing includes
all work in that first request and does not isolate individual CUDA kernels.

Health readiness took 4,656.66 ms old / 7,279.26 ms new in these fresh-path runs.
Combined health plus warmup was therefore about **14.74 s versus 7.35 s (2.0×)**,
not 141× overall startup. File/DLL caches were not controlled. The larger newer
bundle still has a meaningful first-load cost; background loading remains useful.
Warm fixture timings also varied, so this is not evidence of another 10× warm
inference gain. Raw results are in `artifacts/rust/cuda-cold-start.json`.

The unmodified, SHA-256-verified upstream binary ZIP is staged as the optional
`artifacts/rust/whisper-cuda-b5130.zip` (643 MiB, ignored generated output).
README recommends it for the measured RTX 4090. No provider selection, app code,
driver, or user configuration was changed, and no custom CUDA build was required.

## Vulkan backend evaluation — September 27

Built upstream whisper.cpp v1.9.4 in temporary storage with its supported Vulkan
backend, MinGW x64 Release, AVX2/FMA/F16C, static runtimes, and official Vulkan SDK
1.4.357.1 shaders. The executable imports only Windows system libraries and the
already installed `vulkan-1.dll` (loader 1.4.321.0). No driver or SDK was installed
system-wide, and no app source or user configuration changed.

The worker is **59,577,728 bytes**, about **20× smaller** than the extracted
1,191,220,736-byte CUDA bundle. This excludes the same model from both totals.
That size improvement came with slower dictation on this RTX 4090:

| Real controller, four threads, tiny.en | 11-second clip | 33-second clip |
| --- | ---: | ---: |
| Vulkan, median last four of five | 57.99 ms | 148.69 ms |
| Earlier CUDA measurements | 27.75–33.88 ms | 75.92–105.64 ms |

These are separate runs, not a claim of a precise stable backend ratio. They
measure finish gesture to fake insertion; microphone/audio preparation and OS
paste remain excluded. First Vulkan startup was 5.63 seconds, a subsequent
worker 507 ms, and shutdown 44–47 ms. Caches were not controlled.

A separate interleaved HTTP probe confirmed `using Vulkan0 backend`; normalized
text hashes matched CUDA for corresponding short/long clips, 16/48 kHz input,
and en/auto requests. English warm medians were 52.95/146.35 ms at 16 kHz and
56.01/173.35 ms at 48 kHz (short/long). Vulkan working set was **123 MiB** and
private memory **770 MiB**, versus roughly 438/1,522 MiB in an earlier CUDA
long-fixture sample. These exclude a measured VRAM total and the app process.

Retain CUDA as the recommendation for this machine: latency is the primary
target, and the Vulkan size/memory wins do not establish an overall improvement.
The experiment adds no shipped backend abstraction, dependency, configuration,
test framework, or engine recommendation. Raw data and build/probe recipes are
in ignored `artifacts/rust/vulkan-evaluation.json` and `artifacts/rust/profiling/`.

## Default model and meter follow-up — September 27

The example configuration uses **small.en**, so it received a separate comparison
against optimized v1.9.4 CPU inference. This does not replace the frozen tiny.en
baseline. The model matched upstream SHA1
`db8a495a91d927739e50b3fc1cc4c6b8f6c2d022`.

| Warm stop to fake insertion | 11-second fixture | 33-second fixture |
| --- | ---: | ---: |
| AVX2 CPU, eight threads | 1,231.39 ms | 2,997.21 ms |
| AVX2 CPU, sixteen threads | 1,403.07 ms | 3,425.12 ms |
| CUDA b5130, four threads | 68.43 ms | 178.75 ms |
| Gain over faster measured CPU configuration | 18.0× | 16.8× |

These are medians of the last four of five requests through the actual Rust
controller and provider, with fake capture and insertion and public 16 kHz audio.
Expected phrase and whitespace checks passed. This establishes a greater than
10× latency improvement for these default-model fixtures. It does not establish
an app-wide gain, broad recognition accuracy, microphone preparation time, OS
paste latency, or a memory improvement. Evidence:
`artifacts/rust/small-model-performance.json`.

The recording meter now paints its 24 decorative bars in one GPUI canvas instead
of constructing 24 separate layout elements. The same springs drive the same
bar geometry; reduced motion still paints one wide bar. No dependency or test
framework was added. Native Windows Release CPU draw timings, measured during
seconds one through five of the simulated recording:

| Comparison order | Before median / p95 | Canvas median / p95 |
| --- | ---: | ---: |
| Before, then canvas | 115.8 / 224.5 µs | 75.65 / 146.7 µs |
| Canvas, then before | 99.5 / 187.4 µs | 76.9 / 148.6 µs |

Each run supplied 1,600 timing samples. Median draw cost fell 23–35%; this is a
small CPU optimization, not a 10× gain or a claim about displayed frame rate.
The high draw-call cadence differs from earlier desktop sessions; comparisons
use the same desktop state and reverse order. No idle CPU or memory improvement
is claimed from these short runs. Evidence: `artifacts/rust/meter-performance.json`.

Opus 5.5 at xhigh approved the final delta after inspecting GPUI and Taffy source.
Inherited opacity, clipping, minimum width during expansion, and reduced motion
were verified by source review. Unlike separate layout nodes, canvas bar edges
retain fractional device-pixel positions; this makes meter motion less stepped
and can soften edges at fractional display scales. The shorter GPUI `fill` helper
was adopted from the review. No new test was needed for this decorative change.
Native focus and click-through checks passed. An attempted capture over an owned
opaque underlay failed its ownership check and was skipped, so there is no new
pixel-level screenshot comparison. Existing live-input and macOS limits remain.

## Rejected capture wakeup experiment — September 27

The controller already has no idle timer. Capture still checks its stop flag
between 5 ms sleeps. A candidate retained its worker's `Thread` handle, called
`unpark` after Finish/Cancel, and used `park_timeout(5 ms)` while draining audio.
A native Windows mock measured signal observation falling from 2.63–2.78 ms to
9 µs (240 samples per arm, forward/reverse order). This isolated the scheduling
primitive; it did not exercise CPAL, actual Capture, or microphone teardown.

Opus 5.5 xhigh found no ownership or cancellation defect, but required checking
Windows timed-wait granularity. That check rejected the candidate: an unwoken
`park_timeout(5 ms)` took **15.52/15.98 ms median**, versus **5.51/5.33 ms** for
`sleep(5 ms)`. Slower draining would delay first Ready and reduce meter cadence.
The source was restored exactly. No native wait abstraction, timer-resolution
change, callback notification, or extra test was retained to chase a roughly
2.5 ms stop-path saving. The existing package remains valid. Evidence:
`artifacts/rust/capture-wake-performance.json`; review and standalone probes are
under ignored `artifacts/rust/profiling/`. Existing eleven boundary tests and
Windows Clippy passed during the experiment; no live devices were used.

## Default-model quantization comparison — September 27

Built upstream v1.9.4 `whisper-quantize` in the isolated existing toolchain and
created Q8_0 from the verified small.en fp16 model. No app dependency, user
configuration, model selection, or system installation changed. Tested official
b5130 CUDA with four threads, model-local Windows files, raw HTTP, five cycles of
public 11/33-second audio at 16 kHz and synthetic 48 kHz, English/auto. Repeated in
reverse model order; the first cycle of each run is excluded from warm medians.

| Measurement | fp16 | Q8_0 |
| --- | ---: | ---: |
| Model bytes | 487,614,201 | 264,477,561 |
| Process private memory | 1,991 MiB | 1,771–1,772 MiB |
| Process working set | 441–442 MiB | 438–439 MiB |
| 16 kHz English short, warm median | 66.11–66.44 ms | 67.40–68.72 ms |
| 16 kHz English long, warm median | 182.56–183.26 ms | 187.78–188.03 ms |
| 48 kHz English short, warm median | 70.62–70.99 ms | 72.64–74.03 ms |
| 48 kHz English long, warm median | 201.10–203.25 ms | 206.45–213.56 ms |

Q8_0 reduced the model file by **46%** and private memory by **11%**, with slightly
slower English inference in both orders. Working set barely changed. Keep fp16
for the current latency-first recommendation on this RTX 4090. Q8_0 remains only
an evaluation artifact, not an automatic selection or a new product setting.
All normalized fixture text hashes matched across models, rates and language
parameters for corresponding clips. This is not broad accuracy evidence, VRAM
measurement, or full microphone-to-editor timing; startup file caches were
uncontrolled. Evidence: `artifacts/rust/small-quantization-performance.json`.
The generated model and probe remain under ignored `artifacts/rust/profiling/`.

## Long hands-free workload — September 27

Measured the actual Runtime/Whisper path at 90 seconds and the five-minute
recording limit using small.en fp16 and repeated public JFK audio at synthetic
48 kHz. CPU is the v1.9.4 AVX2 build with GPU explicitly disabled and eight
threads; GPU is official b5130 CUDA with four threads. Five requests per worker,
median of the last four:

| Audio duration | Optimized CPU | CUDA | Ratio |
| --- | ---: | ---: | ---: |
| 90 seconds | 7,335.31 ms | 616.90 ms | 11.9× |
| 300 seconds | 24,298.95 ms | 1,921.64 ms | 12.6× |

Capture and insertion were faked. These times include the real controller,
HTTP, resampling, inference and response handling, but exclude device teardown,
trimming/WAV preparation and OS editor insertion. Previously measured five-minute
48 kHz trimming is about 2.4 ms, a small fraction of this stop-time path.
A separate verbose-JSON probe reported the full 90/300-second decoded inputs,
identical CPU/GPU text hashes, and matching 211/678-word outputs across two
cycles. Segment end timestamps are absent because this was measured while
production still disabled timestamp decoding, so no timestamp-based coverage
claim is made. Repeated public audio
checks workload scaling; it does not establish natural long-form accuracy.
Evidence: `artifacts/rust/longform-performance.json`.

The remaining long-session delay is dominated by inference after Stop. A native
streaming engine is a larger potential improvement than changing UI scheduling
or trimming again. Moonshine offers a native C API and explicit stream finalization;
its Windows runtime is being evaluated outside the app. Its vendor benchmarks
compare CPU engines and do not establish gains over this app's measured CUDA
path. See the official [streaming API](https://moonshine-voice.readthedocs.io/en/latest/api/c-api/),
[benchmark method](https://moonshine-voice.readthedocs.io/en/latest/using/benchmarks/)
and [accuracy qualifications](https://moonshine-voice.readthedocs.io/en/latest/models/accuracy/).
No provider selection, app dependency or default changed for this evaluation.

The Windows probe was blocked at process creation by Application Control. The
policy was left unchanged. The successful screening used the published Linux
x86_64 library, so it cannot establish native Windows gains. With the bundled
Medium Streaming model, the 11-second public clip took about 5 seconds of feeding
compute and 130–162 ms to finalize in four of five update phases (0.7 ms in the
other phase), using about 700 MiB private memory. All five outputs matched the
normalized 22-word reference. This is one clip, not a corpus accuracy result.
An initial run put Stop exactly on an update boundary and reported almost zero
drain time; that result was excluded from representative latency claims.

Disabling intermediate text decoding reduced feeding work to about 2–2.5 seconds,
but finalization increased to 474–499 ms in four phases. Retain the existing
provider. This experiment does not show a win over the measured Windows CUDA
path, and fully paced input with concurrent Stop/cancellation remains unmeasured.
Evidence: `artifacts/rust/moonshine-evaluation.json`. The prototype and upstream
assets are confined to ignored profiling output; no new runtime dependency,
model default or product option was introduced.

## Shorter encoding window: corpus check — September 27

The audit's suggested short-clip `audio_ctx` optimization was tested on 120
LibriSpeech test-clean clips from 40 speakers: one per speaker in each 2–5, 5–10,
and 10–20 second band. Publisher archive checksum verified. These are public
read-English recordings with reference text, separate from all user audio.
Source: [OpenSLR LibriSpeech](https://www.openslr.org/12).

Used the same native Windows b5130 CUDA worker, small.en fp16, four threads,
unchanged decoding/fallback, two paired passes with request order reversed.
Compared full context against `ceil((duration + 3) * 50 / 64) * 64`, explicitly
sending the context on every request. The initial warmup pair was excluded.
Basic case/punctuation-normalized word edit distance was computed locally; neither
reference nor recognized text was printed in diagnostics. Numbers and contractions
were not expanded, so this is a paired screening result rather than a leaderboard
word-error-rate claim.

| Across both passes | Full context | Shortened context |
| --- | ---: | ---: |
| Median request latency | 60.52 ms | 55.90 ms |
| p95 request latency | 125.26 ms | 130.91 ms |
| Word errors / reference words | 240 / 5,230 | 301 / 5,230 |
| Word error rate | 4.59% | 5.76% |
| Clips whose text hash differed between passes | 7 / 120 | 26 / 120 |

In this experiment, the modest median gain came with higher tail latency and 25%
more word errors. The follow-up below identifies persistent attention-cache data
when context sizes change, so these are not isolated measurements of either
context size's accuracy.
Of 38 paired requests with worse recognition, 35 had output word counts within
two words of the full-context output. A simple repetition/length fallback would
miss most of these regressions. Full-context WER was 4.59% in each pass; shortened
context varied from 6.58% to 4.93%. Keep the existing full-context behavior.
No source, model, configuration or dependency change was retained.
Evidence: `artifacts/rust/context-corpus-performance.json`; corpus attribution,
selection manifest and recipe are under ignored `artifacts/rust/profiling/`.

## CPU/GPU corpus comparison — September 27

The same 120 clips were then run twice through the optimized eight-thread AVX2
CPU engine, with full context and small.en fp16. Compared with the full-context
GPU arm above:

| Across both passes | CPU | CUDA |
| --- | ---: | ---: |
| Median request latency | 1,186.56 ms | 60.52 ms |
| p95 request latency | 1,833.84 ms | 125.26 ms |
| Word errors / reference words | 206 / 5,230 | 240 / 5,230 |
| Word error rate | 3.94% | 4.59% |
| Clips whose text hash differed between passes | 0 / 120 | 7 / 120 |

The median paired speedup was 19.55×; two of 240 pairs were below 10×. Normalized
output hashes matched in 205 pairs. The accuracy difference prevents a claim of
quality parity. This is clean read English, not representative microphone-to-editor
dictation; capture, trimming and OS insertion are excluded.

Both upstream tags resolve to commit `927cfce34f31707e17f2bff35c349632fb9e2c3a`:
[v1.9.4 annotated tag](https://api.github.com/repos/ggml-org/whisper.cpp/git/tags/7d75b14994ae7f59623e2471445e2355fe506ed2),
[b5130 tag](https://api.github.com/repos/ggml-org/whisper.cpp/git/ref/tags/b5130).
The local `whisper.cpp` source matches that commit. It resets decoder seeds and
audio context on each request and clears prompt history with the server's default
`no_context` setting. RNG and prompt carryover are ruled out, but attention-cache
carryover is not. Different compilers/backends also remain.

Opus 5.5 at xhigh found a concrete source-level confound, independently checked
against the encoder and decoder: flash attention writes only the active context
rows at a layer stride padded to 256, then reads all padded rows without a mask.
The cross-attention cache is zeroed at state creation, not between requests.
Changing context size changes the layer stride, allowing reduced requests to
overwrite padding later read by full-context requests. Full context uses 1,500
active rows and 36 padding rows per layer. Repeating full context alone leaves
those padding rows zero, as production does. Relevant source:
[encoder and decoder](https://github.com/ggml-org/whisper.cpp/blob/927cfce34f31707e17f2bff35c349632fb9e2c3a/src/whisper.cpp#L2354).

This invalidates attributing the observed WER gap to CUDA and prevents treating
the reduced-context WER as intrinsic to a shorter window. How much of the observed
gap it explains remains a hypothesis until a fresh full-context-only run. Keep
the optimization rejected: its modest measured benefit does not justify this
state-dependent behavior or an engine patch. No production change is warranted;
Speakeasy never changes `audio_ctx`, including during warmup.

A fresh full-context-only CUDA comparison was prepared, but Windows execution
policy rejected its script before worker startup. No policy was changed and no
workaround was attempted. That comparison remains unrun. Keep production decoding
unchanged; do not describe the measured GPU gain as accuracy-neutral.
Evidence: `artifacts/rust/corpus-performance.json`; the prepared recipe and official
source snapshot are under ignored `artifacts/rust/profiling/`. The focused review
is `profiling/opus-corpus-review.json`; returned metadata confirms Opus 5.5.

## Fresh full-context corpus follow-up — September 27

The user authorized permanently resolving the PowerShell script restriction.
Set Windows `CurrentUser` execution policy to `RemoteSigned`; a new process
confirmed it. Windows classifies WSL UNC paths as remote, so the reviewed script
was copied to a unique local Windows temporary file and executed under that policy,
without an execution-policy override. The script and owned worker were cleaned up.

A fresh b5130 CUDA worker then ran only full-context requests, omitting `audio_ctx`
as production does. Same small.en fp16, four threads, 120 public clips, two passes,
one excluded full-context warmup. **All 240 normalized output hashes matched the
earlier CPU run**, with 206 errors / 5,230 reference words (3.94%) and no clip
varying between passes. This supports the attention-cache confound identified by
Opus and supersedes the earlier unresolved accuracy comparison for this corpus.
It does not establish accuracy on noisy microphones, other languages or arbitrary
dictation.

GPU median latency was 69.33 ms; nearest-rank p95 was 137.03 ms. Median paired
CPU/GPU speedup was 17.86×, with three of 240 pairs below 10×. Engines were measured
in separate runs, so this is not an interleaved hardware comparison. Startup was
7.32 seconds and worker private memory was 1,992 MiB. Neither those metrics nor
the full app improved 10×. No production code, model or decoding change was made.
Evidence: `artifacts/rust/corpus-gpu-clean-performance.json`; recipe
`artifacts/rust/profiling/speakeasy-corpus-gpu-only.ps1`.

## Retaining a GPU worker after cancellation — September 27

Implemented bounded cooperative cancellation for GPU preference. A request cancel
signal drops the HTTP future and returns the worker without text. A background
silent inference request then confirms that the server has released its model
lock and remains usable. `/health` cannot establish that. Recovery has a two-second
cap; failure terminates and waits for the worker before loading a replacement.
CPU cancellation still kills and reloads. Quit, Pause and model changes abort
owned work. Recording can start while recovery runs; inference waits for recovery.
No new dependency or provider selection logic was added.

Before changing the controller, an isolated native Windows experiment confirmed
six server-side abort messages after HTTP disconnects. The real Rust provider then
cancelled six five-minute public fixture requests at 250/500/1,000 ms; following
short requests completed in 70–127 ms, with exact short and long text preserved.
Fresh process startup took 6.4 seconds, but that is **not** the reload baseline.

The actual Runtime/Whisper comparison used fake capture and insertion, b5130
CUDA, small.en fp16, four threads, and the same repeated public JFK recording.
Cancel a five-minute request after 250 ms, immediately start/finish an eleven-second
fixture, and time cancellation to its simulated insertion:

| Three runs, milliseconds | Previous reload | Retained worker |
| --- | ---: | ---: |
| Individual runs | 637 / 629 / 640 | 159 / 87 / 87 |
| Median | 637.02 | 87.39 |
| Cancellation feedback | 0.88–1.17 | 0.32–0.33 |

This is a **7.29× median gain for GPU cancel-then-redo**, not app-wide performance.
Runs were sequential on one machine, not interleaved. Capture onset and OS paste
are excluded. Recovery failure is tested with a fake; a real GPU timeout/failure
was not forced. Slower or CPU-only engines with GPU preference enabled may hit the
two-second cap; README already explains disabling that preference for CPU engines.

Opus 5.5 xhigh reviewed the implementation and returned **Approve**. Twelve default
tests pass on Linux and native Windows, both workspace Clippy checks pass, and the
GNU Release package passes the owned native demo check. One cancellation test was
adapted and one failure-boundary test added; temporary profiling tests were removed.
Evidence: `artifacts/rust/cancel-retention-performance.json`; recipes and reviews
are under `artifacts/rust/profiling/`. The app-wide 10× goal remains unmet.

## Long-form decoding and chunking — September 27

An offline streaming feasibility check assembled six consecutive LibriSpeech
chapters, 129–294 seconds each, without inserting silence. Two passes, b5130 CUDA,
small.en fp16, four threads, fixed full audio context. Known source-clip boundaries
gave an optimistic chunking check; a second check used only 20 ms RMS windows,
500 ms of quiet, at least 20 seconds buffered and 250 ms padding.

The experiment exposed a simpler accuracy problem: `--no-timestamps` blocks
timestamp tokens and forces a full 30-second advance even if decoding stops early.
Default decoding normally advances to the last decoded timestamp; a single
trailing timestamp can still advance the full window. The measured improvement
is consistent with this mechanism, but the flag also changes prompt/token
constraints, so window advancement was not isolated as the sole cause. Plain JSON
joins segment text without timecodes in either mode. Source:
[Whisper decoding](https://github.com/ggml-org/whisper.cpp/blob/927cfce34f31707e17f2bff35c349632fb9e2c3a/src/whisper.cpp#L7482),
[JSON text construction](https://github.com/ggml-org/whisper.cpp/blob/927cfce34f31707e17f2bff35c349632fb9e2c3a/examples/server/server.cpp#L454).

| Paired full-context screening | Previous flag | Engine default |
| --- | ---: | ---: |
| Long-form errors / 7,488 reference words | 508 (6.78%) | 130 (1.74%) |
| Long-form median request | 1,937.90 ms | 2,439.87 ms |
| Short-clip errors / 5,230 reference words | 206 (3.94%) | 212 (4.05%) |
| Short-clip median request | 67.50 ms | 74.63 ms |
| Short-clip nearest-rank p95 | 137.01 ms | 153.79 ms |

Long-form errors improved or stayed equal in every chapter. Three previous
outputs omitted dozens of words. In the separate 120-clip / 40-speaker short set,
one clip improved and three worsened; none gained excess output words relative to
its reference beyond the previous mode. These are normalized word-edit counts,
not official benchmark scores or general dictation accuracy guarantees.

Removed the flag after source review and the paired comparison. The short-set
equal-or-better WER criterion was missed by six errors across both passes; Opus
5.5 xhigh reviewed the final results and approved accepting that tradeoff to
reduce long-form omissions. This also accepts a roughly 7 ms median short-clip
cost and a 26% long-form cost. A duration-only switch would retain the early-stop
mechanism for short recordings with pauses; that risk is inferred from source,
not demonstrated by this short read-speech set. No WAV parser, duration plumbing,
configuration option or dependency was
added. Insertion still normalizes segment whitespace and contains no timestamps.

No streaming code was retained. Source-boundary chunks had 106 word errors;
audio-derived pause chunks had 140, compared with 130 for corrected whole-input
decoding. The pause rule found no split in one 176-second sample and slightly
worsened another sample. These offline timings exclude paced capture, backlog
and UI contention; they do not establish a streaming latency gain.
Evidence and raw rows: `artifacts/rust/long-decoding-performance.json`. Prepared
audio, attribution, recipes and the Opus review are under ignored `profiling/`.
Final review: `profiling/opus-timestamps-final-review.json`. Twelve default tests
pass on Linux and Windows, both Clippy checks pass, and the rebuilt Windows package
passes the owned demo check. The native 90-second provider fixture also passed
recognition, normalized output and process shutdown. The corrected CPU comparison
below supersedes the earlier inference baseline for this decoding mode.

## Corrected decoding: CPU versus CUDA — September 27

One new CPU pass covers the same 120 short clips and six assembled chapters,
using default timestamp decoding and full audio context. Compare it with round
zero of the earlier CUDA runs. Both engines share upstream commit
`927cfce34f31707e17f2bff35c349632fb9e2c3a` and small.en fp16; CPU uses
AVX2/FMA/F16C with eight threads, CUDA uses four. Warmup requests are excluded.

| Workload | CPU median / p95 | CUDA median / p95 | Median paired speedup |
| --- | ---: | ---: | ---: |
| 120 clips, 2–20 seconds | 1273.34 / 1957.63 ms | 74.21 / 151.30 ms | 16.84× |
| Six chapters, 129–294 seconds | 28242.97 / 31911.33 ms | 2437.71 / 2732.18 ms | 11.67× |

Short duration-band paired medians are 22.75× (up to five seconds), 16.84×
(five to ten), and 13.59× (ten to twenty), with 40 clips in each band. One short
pair is below 10×; all six long pairs exceed 11.46×. p95 uses nearest rank;
with only six long cases it is the maximum, not a stable tail estimate.

CPU/CUDA short errors are 105/106 per 2,615 reference words (4.02%/4.05%);
long errors are 64/65 per 3,744 words (1.71%/1.74%). Normalized hashes match
118/120 short and 5/6 long outputs. This is close measured accuracy, not exact
backend parity. Runs were separate, not interleaved, and cover public clean
read speech on one desktop. Request timing includes HTTP upload and response;
capture, audio preparation and OS insertion are excluded. These results describe
the delivered decoding configuration but do not prove the app-wide goal.

No runtime or package change was needed. Evidence with per-case metrics, hashes
and raw measurement rows: `artifacts/rust/corrected-decoding-performance.json`.
Recipe: `profiling/speakeasy-corrected-cpu.ps1`. The owned worker exited normally;
the harness reported no stderr errors.

## Provider startup at a stable path — September 27

The earlier fixture harness copied the engine, DLLs and model into a fresh
Windows-local directory each run. A follow-up copies them once, then runs the
actual Rust provider fixture in three separate test processes at those same paths.
Current decoding, b5130 CUDA small.en fp16, four threads, public 90-second audio:

| Launch | Provider ready, including silent warmup | Inference | Shutdown |
| --- | ---: | ---: | ---: |
| First at the new path | 6493.28 ms | 713.76 ms | 43.19 ms |
| Second | 572.38 ms | 704.33 ms | 42.88 ms |
| Third | 573.95 ms | 693.80 ms | 43.12 ms |

All recognition, whitespace and owned-worker shutdown assertions pass. Timings
exclude copying assets, starting the test process and reading the fixture. This
separates first use from immediate repeat startup; it does not isolate file,
driver or security caching, establish cold-boot behavior, or measure the full
app launch. No caches or system settings were changed. The difference is not a
new code optimization and does not justify a persistent background service.
Evidence: `artifacts/rust/provider-restart-performance.json`; recipe under
`profiling/speakeasy-provider-restart.ps1`. The temporary directory was removed.

## Submission intervals under GPU load — September 27

An isolated copy of the current app and GPUI sampled the app's own swap chain
after `Present`, buffering diagnostic rows until the animation ended. No production
source, registry crate, lockfile or package changed. Both DXGI frame-statistics
interfaces returned counts but zero refresh counters and zero QPC timestamps.
The media interface mostly reported `NONE`. Thus these data do **not** establish
displayed frame pacing. Microsoft documents the APIs and their limitations:
[GetFrameStatistics](https://learn.microsoft.com/en-us/windows/win32/api/dxgi/nf-dxgi-idxgiswapchain-getframestatistics),
[media statistics](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_3/ns-dxgi1_3-dxgi_frame_statistics_media).
The input desktop was accessible; physical monitor state was not verified.

The same probe does measure submission intervals, including delays in `Present`.
Four owned previews ran in idle/loaded/loaded/idle order with the same warm b5130
CUDA small.en worker. Loaded runs repeated five-minute public audio using corrected
timestamp decoding. Each row below covers 239 intervals in seconds 1–5 of the
recording animation, excluding initial startup and later state transitions:

| Run | Median / p95 / maximum interval, ms |
| --- | ---: |
| Idle | 16.64 / 17.28 / 17.63 |
| Loaded | 16.65 / 17.39 / 18.04 |
| Loaded | 16.66 / 17.55 / 18.01 |
| Idle | 16.64 / 17.28 / 17.62 |

No interval exceeded 25 ms in those samples. Loaded runs completed five inference
requests each, with 2.21/2.22-second medians. API queries cost about 1–2 µs median;
formatting/buffering overhead is additional and was present in both conditions.
This gives no evidence of a rendering-path stall under that load, and does not
justify changing rendering priorities or adding a GPUI fork. Actual displayed
frame pacing remains unverified. Normal root build output was restored and its
SHA256 matches the existing delivery executable. Evidence:
`artifacts/rust/submission-cadence-performance.json`; isolated sources and recipes
remain under ignored `profiling/`.

## Duplicate snapshot notifications — September 27

The controller published after every loop turn, including unchanged modifier
retries while waiting to insert text. It now compares the complete Snapshot using
PartialEq and publishes through `send_if_modified`. Meter ticks still change the
snapshot; animation and insertion policy are unchanged. Explicit input-channel
failure retains its unconditional notification.

A temporary probe through the actual Runtime and existing fake ports held
modifiers until the 800 ms deadline. Windows publications fell 53→1 and Linux
73→1; insertion completed once in every run. Windows elapsed time was
811.88→815.77 ms and Linux 802.85→800.17 ms. One sample per condition, with no
live devices or native insertion. This measures notification reduction, not UI
CPU time or an end-to-end speedup. The receivers already avoided repainting on
these unchanged updates; the saving is redundant scheduling and snapshot handling.

Opus 5.5 xhigh approved with no blocker. The probe was removed; no permanent test
or dependency added. Twelve tests pass on Linux and Windows, both Clippy checks
and formatting pass, and the rebuilt package passes the native demo check.
Evidence: `artifacts/rust/notification-performance.json`; original source, probe,
logs and `opus-notification-review.json` are under ignored `profiling/`.

## Pause chunks with corrected decoding — September 27

The earlier pause-chunk accuracy screen used `--no-timestamps`. Repeating the
same audio-derived cuts with current default decoding changes the result:
**112 errors / 7,488 words (1.50%)**, versus **130 / 7,488 (1.74%)** for whole
chapters. Six chapters, two alternating paired passes, b5130 CUDA small.en fp16,
four threads, full audio context. Every chapter improves or stays equal, and each
variant's normalized hashes match across passes. This supersedes the earlier
140-error chunk result as evidence for current decoding.

The cuts still require at least 20 seconds buffered and 500 ms below the 0.003
RMS threshold, with 250 ms padding on each side. One 176.6-second chapter never
splits; another ends with a 58-second chunk. Accuracy now supports investigating
this policy, but it cannot by itself guarantee a small final backlog.

Request timing in this run is unsuitable for a clean speed comparison. During
a slow request the GPU reported 98% utilization and 20,878 MiB occupied. After
the owned worker exited, other GPU activity reached 100% and 20,900 MiB. Some
whole requests took 6.4–6.8× their earlier times with matching text hashes.
Other processes were not interrupted. The worker confirmed CUDA and exited
normally; no harness errors occurred. This offline run does not measure paced
capture, backlog, final insertion or UI responsiveness. No streaming code was
integrated. Evidence: `artifacts/rust/pause-corrected-performance.json`; recipe
and raw rows under ignored `profiling/`.

Opus 5.5 xhigh withdrew the earlier accuracy objection and approved a paced
prototype through the real Runtime, not shipping yet. The next gates are final
backlog on CPU and an otherwise idle GPU, chunk-boundary punctuation/casing
(normalized WER hides these), and capture-consumer scheduling under CPU inference.
Use one sequential inference job, a session-owned chunk queue and accumulated
text, and insert only after recording and all chunks finish. Cancel or a chunk
failure must discard the entire uncommitted session. Do not add forced cuts or
retain a second full recording for fallback. Review:
`profiling/opus-pause-corrected-review.json`. Its statement that capture already
computes 20 ms windows live is inaccurate: those windows currently belong to the
post-recording trim scan; the live meter aggregates roughly 32 ms updates.

## First paced controller prototype — September 27

An isolated copy of Runtime now accepts a sequential queue of chunks and holds
session text until recording and every inference complete. Cancel or failure
discards uncommitted text. No streaming changes are in production. Eight default
prototype checks pass on Windows, including focused chunk cancellation, stale
event, single final insertion and partial-result failure checks.

One native Release replay used the 276.21-second chapter `1089-134686`, small.en
fp16, v1.9.4 AVX2 CPU with eight threads, default timestamps and full context.
Whole audio at release took **30942.25 ms** to fake insertion. Replaying the
existing pause chunks at their actual availability times reduced that to
**2068.63 ms (14.96×)**, with one final insertion and correctly ordered text.
The twelve chunk requests totalled 30051.49 ms; the latency gain comes from doing
work during recording. Normalized errors changed 13→12 per 721 reference words.
All eleven chunk boundaries aligned with whole-output words, with one punctuation
disagreement and zero casing disagreements. Differences are not correctness labels.

This is one pair, not a general speedup. The fixture preloads prepared chunks and
bypasses CPAL, the live consumer, GPUI and OS insertion. Its maximum 14.93 ms
emission lag measures Tokio replay scheduling, not microphone overruns. Startup
and input loading are excluded. No GPU was used. The later replay batch was
interrupted by a verified WSL restart. One more chapter (`1188-133604`) completed
before interruption: 32337.53→3231.31 ms, errors 11→12/786 words, six punctuation
and four casing disagreements across eight aligned boundaries. These metrics
were directly observed and preserved with their recovery provenance; unobserved
results were not reconstructed. The unsplit chapter still has no proposed latency
improvement. Evidence:
`artifacts/rust/paced-streaming-performance.json`; isolated source and recipes in
`profiling/stream-probe/` and `profiling/speakeasy-paced-cpu*.ps1`.

### Recognition context at chunk boundaries

The recovered native CPU prototype passed its eight default checks, followed by
a plain/context screen on `1188-133604`. Supplying preceding same-session text
through Whisper's prompt field reduced chunk errors 12→10 (whole: 11/786), but
left the same six punctuation and four casing disagreements at eight boundaries.
These disagreements do not establish correctness; the corpus lacks punctuation
labels. Context has not resolved the premium-UX concern.

Both whole outputs had identical hashes. Chunk totals were 37.28/36.01 seconds;
these unpaced requests measure offline work, not release latency. No production
code or package changed. Evidence: `artifacts/rust/stream-context-performance.json`.
Opus recommended judging the disputed boundaries against restored reference
punctuation/casing, then rejecting chunking if clearly worse. Its "never slower"
claim is too strong: chunk overhead and backlog can increase latency. Its claim
that every short word exceeds 100 ms is also not a safe audio invariant. Neither
claim was adopted.

### Decision: reject independent pause chunks

Used [LibriSpeech-PC](https://www.openslr.org/145/) (CC BY 4.0), whose test-clean
labels cover all 24 input clips in the troublesome `1188-133604` chapter. Native
CPU whole/chunked outputs matched the earlier hashes. Both variants were scored
at the same aligned adjacent reference words:

| Correct labels | Whole | Chunks | Shared positions |
| --- | ---: | ---: | ---: |
| Boundary punctuation class | 5 | 2 | 8 |
| Boundary initial-letter casing | 7 | 3 | 8 |
| Punctuation across output | 725 | 721 | 760 |
| Initial-letter casing across output | 752 | 744 | 760 |

Punctuation classes are none, comma, period/exclamation, and question mark.
First-word casing and final punctuation are excluded. Unaligned words are excluded
from formatting counts but remain in WER: 11/12 errors per 786 words. This is one
quality counterexample, sufficient to fail the proposed integration guardrail;
it is not a general punctuation benchmark. Native eight default prototype checks
and the replay passed. Timings are unpaced offline work, not release latency.
Evidence: `artifacts/rust/stream-format-performance.json`.

The earlier 10–15× stop-latency gains do not justify this formatting regression.
No further paced tests or capture integration for this approach. Production keeps
whole-recording decoding; no editing heuristics, dependency, option or test was
added. The isolated draft also has unresolved background-failure and tail-gating
review concerns, recorded in its STATUS.md. It remains ignored experimental
material and is not part of the Windows package.

## Native Parakeet screening — September 27

Evaluated official Sherpa-ONNX v1.13.8 Windows CPU binaries with Parakeet TDT
0.6b v3 int8, eight threads, public WAVs only. Model and runtime were kept under
ignored profiling assets. [Upstream model documentation](https://k2-fsa.github.io/sherpa/onnx/pretrained_models/offline-transducer/nemo-transducer-models.html)
and pinned API/source findings are in `profiling/parakeet/RESEARCH.md`.

Three initial clips (3.275/6.625/10.435 seconds) took 82/134/172 ms for native
WAV reading, feature preparation and first decode, excluding ~990 ms model
creation. There was one word error across 54 reference words. This suggested
further screening, not a production speedup or warm-controller result.

Six existing long chapters produced **113/3,744 word errors (3.02%)**, versus
**64/3,744 (1.71%)** in the earlier corrected Whisper CPU run. Chapter `1284-1180`
was particularly poor: 64 errors versus 19, with 704 output words for 744
reference words. Long first-decode median was 9080.5 ms; the median ratio against
earlier warm Whisper CPU requests was 3.06×. These are separate runs and different
timing boundaries, not a controlled speedup. Peak private memory sampled every
100 ms reached 3,025,080,320 bytes; model creation took roughly one second per
process. Current CUDA Whisper remains substantially faster than these CPU runs.

**Do not integrate this v3 int8 configuration.** Its quality regression fails the
same guardrail used for chunking. No need to extend it to 120 short clips or build
a persistent helper. This does not establish results for different precision,
models or runtimes. Evidence: `artifacts/rust/parakeet-screen-performance.json`;
source/download hashes, scripts and metrics-only logs in `profiling/parakeet/`.
All owned native processes completed and their Windows temporary directories
were removed. No microphone, hook, clipboard, settings or production change.

The stock Sherpa WebSocket server was not run: pinned source binds all IPv4
interfaces and offers no bind-address option. A future qualifying engine would
need an owned private-pipe helper and explicit cancellation handling; the offline
C API has no cancellation entry point. Avoid adding that work before evidence
supports the model/runtime choice.

## Native whole-recording and continuous-stream candidates — September 27

An official NeMo-Speech.cpp v0.1.0 Windows CUDA runtime with Parakeet v3 q8
improved word accuracy over Whisper small.en CUDA: short errors 124/5,230 versus 212,
long 100/7,488 versus 130. Warm HTTP medians were 18.12 versus 68.91 ms short and
650.41 versus 2,206.31 ms long. External GPU activity was observed during/after the run,
so these are provisional component timings. Long punctuation matches decreased
by 30/4,494 shared aligned positions and casing by 10; short formatting improved
slightly. No production integration occurred at this screening checkpoint. Evidence:
`artifacts/rust/nemo-corpus-performance.json`.

Opus 5.5 xhigh recommended an isolated Runtime/cancellation check if GPU memory was
acceptable. Windows private commit was 9.995 GB with a 545 MB peak working set;
per-process GPU counters subsequently showed 1.208 GB dedicated after readiness and
3.773 GB after 293.7 s audio, still allocated 60 s later (shared 69 MB). This is materially
more than initial use, but far below the review's 8–10 GB inference from commit.
Evidence: `artifacts/rust/nemo-memory-performance.json`. Exact release source
confirms our explicit single HTTP worker, correcting the review's assumption of
four concurrent workers. The isolated Runtime check below subsequently verified
cancellation and reuse with fake capture and insertion.

Nemotron 3.5 offers continuous native streaming with punctuation and case. Three
paced short fixtures finished ~10–11 ms after simulated stop on CUDA (~75–77 ms CPU),
with 2/54 word errors. GPU dedicated memory was 1.407 GB and shared 69 MB, despite 6.022 GB
private commit. These are provider checks, excluding real capture and insertion.
Its broader quality screen failed: long word errors 192/7,488 versus Whisper 130,
with substantially worse punctuation. Increasing right context from 1 to 13 reduced
errors to 78/3,744 but still trailed Whisper 65 and retained a large formatting gap.
Both configurations were rejected for integration. Evidence:
`artifacts/rust/nemotron-stream-performance.json` and
`artifacts/rust/nemotron-context13-performance.json`. Unpaced corpus runs measure
quality and total work; they do not measure stop latency.

The English-only Nemotron sibling at right context 13 improved word errors to 58/3,744
versus Whisper 65, but retained 51/77 reference sentence boundaries versus 77 and 52/161
commas versus 145 at shared aligned words. It is also rejected for integration;
word accuracy alone does not preserve the desired dictation quality. Evidence:
`artifacts/rust/nemotron-english-performance.json`.

An isolated Rust prototype then ran Parakeet through the unchanged session owner
with fake capture/insertion. Paired median stop-to-fake-insert: 3.275 s audio took
11.11 ms versus Whisper 42.43 ms; 10.435 s took 19.99 versus 95.89 ms; 293.7 s took
928.69 versus 2,488.78 ms.
Five requests per case/engine, alternating engine order by case. Initial GPU load
was 2%; interference was not continuously monitored. Cancellation feedback was
~0.19 ms for both; immediately dictating another long fixture took 1,708.75 versus
2,566.97 ms. A confirmation run observed zero reloads after cancellation for both
engines; each test left no owned worker running. This supports bounded recovery
with the single HTTP worker. Native capture, preparation and OS insertion remain
outside these measurements. Evidence: `artifacts/rust/nemo-runtime-performance.json`.

### Parakeet integration checkpoint

Opus 5.5 xhigh recommended the explicit GPU option and then approved the finished
integration. `local_speech.rs` now owns either engine, with no new dependencies
or session-owner abstraction. Old settings default to Whisper; Settings selects
Parakeet explicitly and shows automatic language/GPU-required status. Sample-rate
errors explain the 8–96 kHz input requirement. The existing controller check now
covers an engine change; the default suite remains 12 tests. Native backend source
throws when explicit GPU selection finds no matching device, rather than silently
falling back to CPU (`v0.1.0 src/runtime/ggml/backend.cpp`).

Formatting, Linux/Windows Clippy, 12 Linux and 12 actual-Windows tests, Windows GNU
Release build, prerecorded JFK recognition through the production controller on
both engines, and the extracted-package demo all passed. Public-audio checks used
fake capture/insertion. No real microphone, clipboard, or editor acceptance was
performed; native Mac remains unverified. The app-only package at this checkpoint
was about 4.1 MB, excluding engine/model; current bundle details are in the
[README](../README.md#optional-parakeet-engine). Actual overall 10× remains
unachieved. Review evidence:
`artifacts/rust/profiling/nemo/opus-integration-review.json`.

### Provider computation and memory follow-up

Four isolated CUDA screens kept the same Parakeet q8 recording boundaries. Each
used three short and six long public inputs, two passes, alternating engine order
and reversing case order. They changed only the selected child process. None was
adopted:

| Candidate | Median paired speed ratio, short / long | Decision |
| --- | --- | --- |
| Official CUDA 13 cuBLAS instead of the compact shim | 0.974 / 0.991 | No gain; adds about 500 MB of DLLs. |
| Cached F16 weights plus official cuBLAS | 0.991 / 1.008 | No gain; adds about 968 MiB of cached weights. |
| Stock Q8 kernels instead of specialized kernels | 0.966 / 1.037 | Small long gain, short regression and slightly more word errors. |
| Optional CUDA graph optimization | 1.015 / 1.008 | No useful gain; output hashes matched. |

These are HTTP component screens on this machine, not app-wide results. Ratios
above one favor the candidate. Evidence is in
`artifacts/rust/nemo-{cublas,cached-f16,stock-q8,graph-opt}-performance.json`.
Official vendor libraries were downloaded from NVIDIA's CUDA 13 redistribution
manifest and verified against its checksum; no toolkit or OS settings changed.

A separate instrumented run attributed a repeated 293.7-second recording's
841.47 ms inclusive decode to 27.88 ms feature extraction, 559.99 ms encoding and
projection, and the remaining approximately 254 ms largely to token decoding.
Instrumentation adds overhead, so these locate costs rather than replace the
uninstrumented latency figures. The encoder scheduler retained a 1851.88 MiB
intermediate buffer, plus 114.95 MiB for the frontend. Dedicated memory remained
3.773 GB after a 12-second pause and another short request. Source inspection
confirms buffers retain their largest allocation; graph expiry cannot trim them.
Evidence: `artifacts/rust/nemo-stage-performance.json` and source research in
`artifacts/rust/profiling/nemo/OPTIMIZATION.md`. No engine fork was introduced.

### Whole-recording speculation: rejected

Opus 5.5 at xhigh reviewed recognizing the whole recording during a natural
pause, then reusing the result only when final trimmed audio is identical.
It recommended against implementation: Parakeet's short requests already take
about 11–20 ms through the mocked controller, while stale speculative work can
delay a final request on the single worker. Stop noise, resumed speech, and the
last partial energy window can invalidate a candidate. Repeated recognition also
adds GPU work and copies without a measured benefit.

An independent scan of the six existing public long fixtures found 11 internal
pauses of at least one second, but no qualifying pause at their original ends
(trailing quiet was 0–415 ms). That gives no reuse opportunity at those stops;
it does not estimate real dictation behavior. No artificial trailing silence was
added to create a favorable result, and no prototype or production change was
made. Evidence: `artifacts/rust/whole-recording-pause-opportunities.json` and
`artifacts/rust/profiling/nemo/opus-speculation-review.json`.

### Parakeet at a common microphone sample rate

A single owned native Windows worker compared the original 16 kHz inputs with
synthetic 48 kHz versions of three short and six long public recordings. Two
passes alternated request order and reversed case order. Offline conversion used
FFmpeg/soxr at precision 28 to PCM16; conversion was outside the request timer.
This measures HTTP upload, decoding/resampling and recognition, excluding capture,
the controller and OS insertion. It adds no real microphone information.

| Input group | 16 kHz median HTTP | Synthetic 48 kHz median HTTP | Median paired added time |
| --- | ---: | ---: | ---: |
| Short, 3.3–10.4 seconds | 17.03 ms | 23.58 ms | 4.23 ms |
| Long, 129–294 seconds | 635.42 ms | 771.91 ms | 130.62 ms |

Normalized word hashes matched in all 18 pairs: both rates had 2/108 short and
102/7,488 long word errors. Long punctuation scores matched; casing scores were
4,484 versus 4,480 across 4,558 shared positions. The median paired time ratios
were 1.33 short and 1.22 long. This bounds an input-rate cost without attributing
it wholly to resampling or establishing broader quality equivalence. It does not
support another 10× or justify adding an app-side resampler from this screen.
No production changes were made. Owned worker shutdown and temporary-directory
cleanup succeeded. Evidence: `artifacts/rust/nemo-rate-performance.json` and
`artifacts/rust/profiling/nemo/rate-screen.ps1`.

### Upstream refresh — September 27

Checked current releases and source before considering another dependency build.
[GPUI's registry](https://crates.io/crates/gpui) still lists 0.2.2 as latest.
Zed revision `bda9c0bd43a8d235d82adb01ea5bc875b861ecfc` still has the Windows
VSync loop wait on every refresh and invalidate its windows; updating to this
source would not remove that mechanism.
[Windows platform source](https://github.com/zed-industries/zed/blob/bda9c0bd43a8d235d82adb01ea5bc875b861ecfc/crates/gpui_windows/src/platform.rs#L366).

[NeMo's releases](https://github.com/NVIDIA/NeMo-Speech.cpp/releases) still offer
v0.1.0. Current revision `97a15afa5caa9bce5baaa86c1184103877af4101` is nine commits
ahead. The reviewed changes include dynamic CUDA graph updates, Q8/GELU fusion,
cached streaming attention, and RNNT punctuation handling. These are not evidence
of a faster offline Parakeet TDT path: the TDT decoder files are unchanged,
FastConformer uses SiLU rather than GELU, and the new specialized attention
dispatches require cached streaming shapes. Generic attention and shared runtime
changes still require benchmarks before claiming any speed or output equivalence.
[Source comparison](https://github.com/NVIDIA/NeMo-Speech.cpp/compare/4f9676226f667d14608487df744f375db87127f8...97a15afa5caa9bce5baaa86c1184103877af4101).

No dependency, lockfile, engine or production change was made. API responses and
pinned source are saved under `artifacts/rust/profiling/upstream-refresh/`.
This closes the packaged-upgrade check; it does not prove that further native
kernel optimization is impossible or establish another performance gain.

## Verification at the first delivery checkpoint

Opus 5.5 at xhigh reviewed the implementation changes and returned **Approve**,
with no introduced bug or regression blocking delivery. Its focused review was
read-only. Ten default checks passed on Linux and actual Windows, alongside
workspace Clippy with warnings denied. A separate 33-second public-audio fixture
passed Windows recognition, whitespace normalization, and owned-worker shutdown.
That long fixture addresses the review's note that the original 11-second fixture
alone could not exercise multiple Whisper windows.

The review left two operational caveats: an engine that cannot start can fail one
bounded replacement attempt, and cancellation-triggered reload can briefly overlap
GPU memory with the terminating worker. The later GPU fixture checks above do not
measure that cancellation overlap. The trim pass remains on the capture worker and no
new live microphone/clipboard test was performed.

The sections below preserve Opus's audit at its original source checkpoint;
line references and "fix before delivery" wording describe that checkpoint.
**Read the evidence addendum first:** it corrects the original report's mistaken
attribution of historical .NET measurements to this session. Native Mac execution,
microphone onset, frame pacing, and idle power remain unverified; GPU inference
was subsequently measured as described above.

# Addendum: independent Windows engine evidence

All source findings in my audit still stand, except where this addendum corrects them. I read the evidence note, the measurements JSON, the raw benchmark JSONs and the benchmark script. I couldn't inspect the contents of `artifacts/rust/whisper-cpu-avx2.zip`; no shell was available in this pass.

## What the evidence shows

**The harness matches the app.** The script (`/tmp/speakeasy-whisper-benchmark.ps1:19`, `:37-42`) uses the same launch arguments as `whisper.rs:31-58`: request path, public directory, threads, language, no timestamps, no GPU. It sends the same form fields as `whisper.rs:111-119`: file, `response_format=json`, `temperature=0.0`. The timings include upload and response, so they are app-equivalent on the engine side.

| Engine build, 4 threads unless noted | Warm median (tiny.en, 11 s jfk.wav) | vs scalar |
|---|---|---|
| Scalar (the original fixture) | 3824.78 ms | 1x |
| AVX2 | 496.49 ms | 7.70x |
| AVX-512 | 468.80 ms | 8.16x (only ~6% over AVX2) |
| AVX2, 8 threads | 290.44 ms | 13.17x (1.71x from threads) |

- **Recognized text:** all 16 requests hash identically.
- **First request vs warm:** no first-request penalty on CPU (495 vs 496 ms; 298 vs 290 ms). This matches my Linux result.
- **It explains the earlier Windows vs Linux gap.** I had declined to attribute the 5.79 s Windows fixture test against 0.39 s on Linux. It is now largely accounted for: about 3.8–3.9 s of scalar inference plus 1.3–2.0 s of startup. That was an engine-build problem, not an app hot-path problem.

## Corrections to my audit

`docs/THIS_PC.md` and `docs/VERIFIED.md` are historical notes about the .NET app from another checkout, user and machine context. They are not Rust evidence and not established as this hardware. I withdraw every place I used them that way:

- **The verdict's "user's PC, CUDA + medium.en" framing.** There is no fresh evidence of any GPU or medium.en setup. GPU vs CPU is unmeasured for this hardware.
- **The "on-machine baseline" line** (warm-up ~1.55 s; transcription plus cleanup 305–466 ms). At most it is a historical .NET baseline for its own context.
- **C1's magnitude** ("~1.55 s + 0.2–0.4 s, 5–10x on this PC"). It is replaced with the fresh numbers below.
- **P1's "CUDA ≤0.47 s on this PC".**
- **The 1.55 s figure** in the note rejecting idle model unloading.

The current hardware is the Ryzen 9 9950X3D. My Linux fixture runs were on the same CPU, under WSL2. The "desktop, not a laptop" remark in P5 is an inference from that CPU model only.

## Where the evidence is weak

**About the evidence itself:**

- **13.17x is relative to an artificially crippled fixture,** not to an optimized engine and not end-to-end. It combines two independent factors: 7.7x from CPU instructions and 1.71x from threads.
- **Everything rests on one clip, one model (tiny.en) and one machine.** Identical hashes on one clip don't prove quality. However, only CPU instruction set and thread count changed, so no quality trade is expected.
- **Thread scaling may differ for larger models.** tiny.en is small; small and medium are more compute-bound.
- **The AVX2 Windows build may still leave ~1.9x on the table.** At 4 threads it took 496 ms, against about 260 ms for my Linux native build on the same CPU. Both builds have OpenMP off in the Windows case and MinGW with native off; OS scheduling and my noisy WSL runs could also explain part of it. It's unverified.

  One A/B would settle whether to change the packaging: an OpenMP or clang/MSVC build, or upstream's runtime-dispatch build (`GGML_BACKEND_DL=ON GGML_CPU_ALL_VARIANTS=ON`). The server already calls `ggml_backend_load_all()` at startup (`server.cpp:613-614`). A runtime-dispatch build would also remove the AVX2-only hardware restriction of a single binary.
- **Startup is too noisy to call, but too large to dismiss.** Server start to ready took 1280–2077 ms on Windows, against 83 ms on Linux for the same 77 MB model. Two confounds apply:
  - Each run executes a freshly copied exe, which antivirus may scan on first run.
  - The harness polls `/health` the same way the app does, against a port that isn't open yet (`ps1:30-31`). On Windows a refused connection can take a second or more to fail (C4).

  So this is circumstantial support for C4, not proof. Neither I nor the evidence can attribute it yet.

**About my own conclusions:**

- **"The normal path is inference-dominated" holds, and is stronger.** For tiny.en on an optimized CPU build, inference is 290–500 ms for 11 s of audio. The app's own overhead, which I estimated at 20–60 ms, is still unmeasured. Larger models make inference even more dominant.
- **"No 10x over an already optimized engine" still holds.** The measured 10x-class win is a correction to a bad engine build.

## How priorities change

- **P1 (engine visibility) goes up.** A silently scalar build now has a measured ~7.7x cost at equal threads, and the app discards the server output (`whisper.rs:52-54`), so the user can't tell.
  - Delivery minimum: documentation. State that the engine must be a SIMD (AVX2+FMA+F16C) or GPU build, point to the separately packaged AVX2 engine with its CPU requirement, and make no 10x claim.
  - Follow-up (~30–40 lines): drain stderr and parse only the backend/CPU-feature line, which the server prints with each request (`server.cpp:~863-866`). Show "CPU (no SIMD)" in Settings.
- **C4 goes up** from "plausible" to "plausible, with circumstantial support". The fix is two lines; include it.
- **C1 stays a blocker.** On fresh numbers (tiny.en, this Windows machine), the next dictation after Escape costs 1.3–2.1 s of startup plus 0.29–0.50 s of inference, against inference alone once fixed. That's about 4–8x on that path, with medium confidence because startup is noisy.
- **M2 (threads): measured 1.71x from 4→8 on this CPU, with tight tails on Windows.** My WSL outliers at high thread counts did not reproduce there. Keep the default of 4 (`config.rs:26-28`) until a second machine is measured. A later heuristic could be `(available_parallelism / 2).clamp(1, 8)`, but not yet.
- **P2 (first-GPU-inference warm-up):** there is still no CPU penalty; GPU remains unmeasured.
- **M1, the incremental long-dictation idea, P4, P5 and P7–P10:** unchanged.

## Delivery blockers

These are all small and local, with no new modules or dependencies.

1. **C1: reload the model after a cancel or a failed session** (`runtime.rs`).
   - Add a helper:
     ```rust
     fn warm<P: Ports>(ports: &P, config: &Config) -> (Option<u64>, Job<P::Speech>) {
         let load = ports.load(config.clone());
         (None, tokio::spawn(async move { Ok((load.await?, None)) }))
     }
     ```
   - Use it at `:133-134` and `:185-186`.
   - After the abort in Cancel (`:286-291`), when `server` and `job` are both empty: `job = Some(warm(&ports, &config));`.
   - In the failure branch (`:224-229`), the same, but only if `session.is_some()`. A failed warm-up then can't loop.
   - Test: in `cancelled_inference_and_late_audio_cannot_affect_the_next_session`, drain the startup load and assert a second `loads` receive after Cancel. The existing "no reload on preference changes" assertion is unaffected, because Cancel while idle produces no Cancel action.
2. **C2: remove line breaks between 30-second segments** (`whisper.rs:155`). Replace `.trim().to_owned()` with `text.split_whitespace().collect::<Vec<_>>().join(" ")`.
3. **C3: retry opening the Windows clipboard** (`windows.rs:324`, `:332`). This corrects my earlier suggestion: `new_attempts` only yields with `Sleep(0)` between tries (clipboard-win `lib.rs:148-160`), which won't outlast a listener that holds the clipboard for milliseconds. Use a short bounded retry instead:
   ```rust
   fn open_clipboard() -> anyhow::Result<clipboard_win::Clipboard> {
       for _ in 0..25 {
           if let Ok(clipboard) = clipboard_win::Clipboard::new() { return Ok(clipboard); }
           std::thread::sleep(std::time::Duration::from_millis(2));
       }
       Ok(clipboard_win::Clipboard::new()?)
   }
   ```
   This blocks for at most about 50 ms. Escape still wins through the commit gate.
4. **C4: faster readiness polling** (`whisper.rs:96`, `:102`). Use `.timeout(Duration::from_millis(250))` and a 25 ms sleep. It's harmless on every OS.
5. **Q1 (quality, not performance): restore the .NET speech gate.** Require about 100 ms of audible 20 ms RMS windows instead of 10 ms of samples above −50 dBFS (`audio.rs:159`, `:185`). Porting the edge trimming (`PcmRecording.cs:56-82`) should follow.
6. **Docs:** as described under P1. Optionally extend `whisper.rs:90` to say "check that the engine build matches this CPU", so an illegal-instruction exit from the AVX2 engine is explained.

## Updated 10x table

| Metric | Fresh baseline | Target or result | Confidence | Effort | Prerequisites |
|---|---|---|---|---|---|
| Warm inference by engine build (tiny.en, 11 s, Windows, 9950X3D) | 3824.78 ms (scalar, 4 threads, fixture) | **290.44 ms measured, 13.17x** | High for this setup; low beyond it | No app code; docs now, detection later | SIMD-capable CPU; per-machine thread measurement |
| Next dictation after Escape or an error | 1.3–2.1 s startup + 0.29–0.50 s inference (tiny.en, noisy single runs) | Inference only | ~4–8x; high on mechanism, medium on size | ~15 lines + test | None |
| Windows startup to ready | 1280–2077 ms, against 83 ms on Linux, same model | ~0.1–0.2 s if readiness lag dominates | ≥10x possible but unproven; confounded | 2 lines, plus a split probe | Probe: start→port bound, then bound→health, with a stable, already-scanned exe |
| GPU vs optimized CPU | **Unmeasured** (no fresh GPU evidence) | Unknown | Unknown | Build + measurement | GPU build on the actual hardware |
| Short-clip `audio_ctx` (CPU) | 0.23–0.37 s (Linux, measured) | 0.03–0.16 s; degenerate output at tight margins | Low for shipping | Small + guard + evaluation | Word-error corpus |
| Long hands-free, time after stop | Every 30 s window processed after stop | Last chunk only | Medium; quality risk | High | Long-form fixtures |
| Press to ready (software part) | Unmeasured | `play()` only | Low | Medium | Instrumentation, privacy-indicator check |
| Idle wakeups (Windows) | 60–240/s from GPUI | ~0 | Mechanism high; value unknown | GPUI patch | Power measurement |
| Remaining Windows engine build headroom | 496 ms (AVX2, 4 threads) vs ~260 ms Linux native | Possibly ~1.9x | Low (different OS, noisy WSL) | Build A/B only | OpenMP / clang / runtime-dispatch build |

## Updated verdict

The app's hot path is sound. The only measured 10x-class gain comes from fixing the engine build, not from the app, and the app currently can't tell the user when their engine is slow. Deliver after blockers 1–5 and the documentation change. Report 13.17x strictly as "optimized CPU build and 8 threads vs the scalar fixture, one clip, one model, one machine". Frame pacing, idle power, GPU performance and the Mac remain unmeasured.

---

## Original audit, before delivery fixes

# Speakeasy performance audit

## Verdict

The architecture is sound for performance. Capture, the UI and inference each run on their own thread or process. No PCM reaches the UI. The pill only asks for frames while something is changing. On the user's PC, with a CUDA build and medium.en, the time from end of speech to inserted text is almost all Whisper inference. Everything else on that path adds roughly 20–60 ms.

**A further 10x on that normal GPU path is not evidenced.** Large wins do exist in specific places:

- **Cold model reloads on the critical path.** This is a confirmed defect: after Escape during processing, the next dictation waits for a full model reload.
- **Silent CPU fallback.** If the chosen whisper-server build has no GPU backend, inference quietly runs on CPU, and the app can't tell.
- **Long hands-free dictations.** Everything is transcribed only after the user stops.
- **Short clips on CPU, by shrinking Whisper's fixed 30-second window.** I measured 2–9x, but tight settings produced garbage output. It is unsafe to ship as a default without a guard.

Before delivery, fix the reload defect and the four small issues in the "Fix before delivery" list below. Smoothness of animation, idle power and everything on the Mac are unmeasured and should be described that way.

## What I ran

I did not edit any source, settings or fixture configs. No microphone, global hook, clipboard, input injection or non-demo app was used. Transcripts were never printed; I only compared hashes, word counts and whether a known public phrase appeared.

I ran the Linux fixture `whisper-server` v1.8.3 with tiny.en on CPU, 4 threads unless noted, sending the public `samples/jfk.wav` over loopback. Each server was stopped by its own PID. I created and deleted one temporary stderr file in `/tmp`. The machine is a Ryzen 9 9950X3D under WSL2, so timings are noisy.

| Measurement | Result |
|---|---|
| Server start to `/health` OK (10 ms polling) | 83–84 ms on warm runs. The first run in each batch took about 1.04 s, which I can't explain. Whisper's own load time was 44.8 ms. |
| 11 s clip, full 30 s window | 0.268 / 0.258 / 0.261 s |
| First vs second inference (CPU) | No warm-up penalty (0.268 vs 0.258 s) |
| 11 s clip with `audio_ctx` 768 / 640 | 0.118 / 0.110 s, same text. At 512 (a window shorter than the audio) the text changed. |
| 2 / 3 / 4 / 6 s clips, full window | 0.24 / 0.23 / 0.23 / 0.34 s |
| Same clips, window sized to clip + 3 s | 0.031 / 0.044 / 0.115 / 0.156 s, same word count and phrase checks |
| Same clips, window sized to clip + ~1 s | 3 s clip: 20 words instead of 6 (repetition), 0.31 s. 4 s clip: 1.1 s (fallback retries). |
| Shrunk window with fallback disabled | Repetition loops: 184 words, one word repeated 37 times; 2.35 s on the 4 s clip |
| Fallback disabled, full window, clean audio | Same time and text as the default |
| 48 kHz vs 16 kHz upload | No measurable difference (the server's resampling cost is negligible) |
| Threads 2 / 4 / 8 / 12 | About 0.68 / 0.27–0.44 / 0.16–0.20 / 0.13 s. At 8 and 12 threads there were 0.66–0.89 s outliers (tail-latency risk). |
| Do server logs contain transcript text? | No: 55 log lines, no transcript word. The logs do include backend lines such as "backends = 1" and "no GPU found". |

Existing on-machine baseline: `docs/VERIFIED.md:81-85` (the .NET app, same PC, CUDA medium.en) shows a model warm-up of about **1.55 s**. Transcription plus LLM cleanup of 15–22 s of audio took **305–466 ms**.

## Path traces

- **Press to capture ready.**
  - The hook callback `platform/src/windows.rs:152-230` does only cheap work.
  - Then `deliver` puts the event on a channel to the session-owner thread (`app/src/runtime.rs:158-171`). Capture starts on press, not release (`core/src/gesture.rs:37-60`).
  - `Capture::start` spawns a new thread for every recording (`app/src/audio.rs:31-36`). That thread:
    - creates the audio host and, if a specific microphone is selected, enumerates all devices (`audio.rs:60-70`);
    - queries the default input format, which activates a Windows audio client (`audio.rs:71-73`; cpal `wasapi/device.rs:807-816`);
    - builds the stream, which activates a second audio client, initializes it and spawns cpal's own thread (`wasapi/device.rs:830-900`);
    - reserves a 28.8 MB buffer at 48 kHz (`audio.rs:118`), then calls `play` (`audio.rs:123`).
  - "Ready" is sent when the first sample is popped (`audio.rs:146-150`), up to 5 ms after the first callback.
  - None of this is instrumented.
- **Level meter.**
  - RMS is computed on the capture thread every 32 ms and mapped to −60…−6 dBFS (`audio.rs:165-172`). It is sent with `try_send`.
  - The owner then republishes the whole snapshot (`runtime.rs:196-199`, `:314`).
  - The pill's update task rotates its history and requests a redraw (`pill.rs:53-81`).
- **Release to text.**
  - Release triggers Finish, which sets an atomic flag. The capture thread notices within 5 ms, drops the stream and drains the ring buffer (`audio.rs:139-164`).
  - The WAV header is written in place (`audio.rs:241-258`), and the buffer moves to the owner without an extra copy.
  - A job is spawned (`runtime.rs:296-311`). It sends a multipart POST (`whisper.rs:110-139`). The server decodes and resamples to 16 kHz with miniaudio's linear resampler (`examples/common-whisper.cpp:42-50`) and runs inference under a single global lock (`examples/server/server.cpp:797-799`).
  - The reply is parsed as JSON (`whisper.rs:150-156`).
  - Insertion waits at least 10 ms, then polls modifier keys every 10 ms for up to 800 ms (`runtime.rs:138-142`, `:234-235`). It then inserts synchronously on the owner thread (`runtime.rs:238`).
- **Cancel.**
  - The native callback flips the commit gate immediately (`platform/src/lib.rs:21-28`).
  - The owner aborts the inference job. The worker lives inside that job, so it is dropped: the Windows job object closes and `kill_on_drop` fires, killing the process (`runtime.rs:280-293`).
  - Capture is dropped, and the capture thread zero-fills and exits (`audio.rs:131-134`).
- **Startup and warm-up.**
  - The model starts warming as soon as the runtime starts (`runtime.rs:132-134`).
  - The server loads the model before it binds its port (`server.cpp:698` then `:1194`).
  - The app polls `/health` every 100 ms, with a 1 s per-request timeout (`whisper.rs:85-106`).
- **Settings.**
  - Save runs validation, `fsync` and `apply` on the UI thread (`shell.rs:288-293`, `config.rs:93`).
  - A speech-related change (engine, model, language, threads, GPU; `config.rs:48-54`) kills and reloads the server (`runtime.rs:176-187`).
  - Microphone enumeration runs in the background (`shell.rs:183-201`).
- **Idle, tray and shutdown.**
  - The owner thread parks with no timers. The hook thread wakes only on key events.
  - The server blocks in `accept`, but polls at 100 Hz for 5 s after each request (`httplib.h:18,22`).
  - Upstream GPUI on Windows wakes a thread on every display refresh, forever (details under "Plausible, unmeasured").
  - Pause and Quit join the owner thread, which waits up to 3 s for the worker to exit, on the UI thread (`shell.rs:63-70`, `runtime.rs:110-117`, `whisper.rs:159-163`).

## Confirmed defects

**C1. Escape during processing, or a failed transcription, leaves no warm model. The reload then happens after the user's next utterance.**

- **Mechanism.** The Cancel branch aborts the job, and the worker inside it is killed (`runtime.rs:286-291`). The failure branch doesn't reload either (`runtime.rs:224-229`). A transcription error also drops a healthy worker, because of the `?` at `runtime.rs:308`. The next dictation then loads the model inside its own job, after the speech has ended (`runtime.rs:299-307`). By contrast, the settings-change path does re-warm (`runtime.rs:184-187`).
- **Impact.** Next-dictation latency is load plus inference instead of inference alone. On this PC that is about 1.55 s + 0.2–0.4 s, plus any Windows poll delay (see C4). That's roughly **5–10x** on this path. The factor is larger for big models, smaller for tiny ones.
- **Smallest fix.** Add a small `warm(&ports, &config)` helper that returns the job tuple, and reuse it at `:132-134` and `:185-186`. Call it:
  - after the abort in Cancel, when there is no server and no job;
  - in the failure branch, only when `session.is_some()` and there is no server or job. Warm-up failures must not retrigger, so this can't loop.

  Keep the "kill on cancel" contract.
- **Tradeoff.** One extra model load per cancel, in the background.
- **Verify.** In `cancelled_inference_and_late_audio_cannot_affect_the_next_session`, drain the startup load, cancel, then assert `receive(&h.loads)` fires before the next start. The fake ports already have a `loads` channel (`runtime.rs:337,384`).

**C2. Transcripts longer than 30 s get line breaks at arbitrary 30-second boundaries.** This is correctness rather than performance, but it matters for hands-free use.

- **Mechanism.** The server's JSON `text` joins segments with `"\n"` (`server.cpp:450-466`, `:1105-1108`). With `--no-timestamps` there is one segment per 30 s window. The app only trims the ends (`whisper.rs:155`).
- **Impact.** On the clipboard path this becomes CRLF. On the direct Unicode path it sends a line feed, which chat apps may treat as Enter and send the message early.
- **Fix.** Normalize whitespace, e.g. `text.split_whitespace().collect::<Vec<_>>().join(" ")`.
- **Verify.** Feed a 35 s fixture (for example, two jfk.wav copies) through the ignored provider test and assert there is no `\n`.

**C3. The Windows clipboard is opened with a single attempt, twice, and the second open races clipboard listeners.**

- **Mechanism.** `clipboard_win::Clipboard::new()` makes exactly one `OpenClipboard` call (clipboard-win `lib.rs:130-132`). The app opens it at `windows.rs:324`, writes, then reopens it at `:332` to read the text back. Clipboard History, rdpclip under remote sessions, and clipboard managers open the clipboard right after each change.
- **Impact.** Sporadic "Could not access the clipboard" errors and an error pill instead of a paste. How often this happens is unmeasured.
- **Fix.** Use `Clipboard::new_attempts(10)` for both opens.
- **Verify.** In a scratch session only, with explicit opt-in, run a listener that holds the clipboard briefly. Otherwise treat this as a code-reading finding.

**C4. Model-readiness polling may lag on Windows.** The mechanism is plausible and the fix is trivial.

- **Mechanism.** Until the server binds its port, each `/health` poll hits a closed loopback port (`whisper.rs:92-97`, 1 s timeout, 100 ms interval). Windows commonly retries a TCP connection after it is refused, so a refused loopback connect takes about 0.5–2 s.
- **Impact.** Detection could lag the bind by up to about 1 s on every load: at startup, after a settings change, and after C1 today. On Linux the lag is at most 100 ms, which is about the same as tiny.en's whole load time (83 ms measured).
- **Fix.** Health request timeout of 150–250 ms, poll interval of 25 ms. Once the port is bound, `/health` doesn't wait for inference.
- **Verify.** On Windows, time a PowerShell TCP connect to a closed loopback port: `Measure-Command { try { [Net.Sockets.TcpClient]::new('127.0.0.1', 1) } catch {} }`.
- **The 5.79 s Windows vs 0.39 s Linux fixture result.** I can't attribute the gap. The candidates are:
  - the fixture config loads both the exe and the model from `\\wsl.localhost` paths (`/tmp/speakeasy-fixture-windows.json`);
  - the connect delay above;
  - antivirus scanning;
  - build flags of the MinGW CPU build.

  To separate them: time spawn-to-health apart from inference, and repeat with local NTFS copies of the exe and model.

## Measured levers

**M1. Shrinking the 30-second window (`audio_ctx`) for short clips on CPU.**

- **Mechanism.** The encoder always processes 1500 frames (30 s) unless `audio_ctx` is set. The server accepts `audio_ctx` per request. Upstream resets per-request parameters only on success (`server.cpp:1113`), so always send every field you rely on.
- **Measured.** 2–9x on 2–6 s clips with a +3 s margin. Tight margins produced repetition and fallback slowdowns; with fallback disabled the output was catastrophic.
- **Tradeoff.** Quality risk; Whisper was trained on 30 s windows.
- **Recommendation.** Experimental only. Use `audio_ctx = ceil((secs + 3) * 50 / 64) * 64`, and send full context above about 20 s. Detect degenerate output (repetition, or far too many words per second) and retry once with the full window. Keep fallback on. Ship only after measuring word error rate on a small public corpus with tiny, small and medium models, and after measuring the GPU gain. The CUDA gain in absolute ms is probably small.

**M2. Thread count.** Going from 4 to 8 threads gives about 1.8x on CPU inference only. Higher counts added tail outliers. `config.rs:26-28` defaults to 4. Tune per machine; this isn't a 10x lever.

**M3. Upload sample rate.** Sending native 48 kHz costs nothing measurable, so keep it. The comment at `audio.rs:116-117` is right that there's no app-side resampling. Note, though, that the server's miniaudio linear resampler is what does the conversion. Its effect on quality is unmeasured and low priority.

## Plausible, unmeasured

**P1. Whether the GPU backend is actually live is invisible.**

- `use_gpu` defaults to true (`config.rs:16-17`), but the server's output is discarded (`whisper.rs:52-54`). The fixture build reports "no GPU found".
- Evidence for the gap: CUDA medium.en measured ≤0.47 s including cleanup on this PC. CPU medium.en is likely several times to 10x+ slower (not measured here).
- **Change.** Pipe stderr, drain it in a small task, keep only a backend flag parsed from startup lines, discard everything else, and show "CPU" in Settings status. I verified the logs contain no transcript text. They do contain sample counts, so never store them.
- Worth it even if only documented.

**P2. One-time cost of the first GPU inference.** Metal pipeline compilation or CUDA/cuBLAS/Vulkan initialization may land on the first dictation. There is no warm-up penalty on CPU (measured). To test, time the first and second transcription on the same worker, using CUDA and Metal builds. If the first is materially slower, POST 1 s of silence after `/health` in `Whisper::start`.

**P3. Automatic language detection doubles encoder work on multilingual models.** "auto" runs an extra encoder pass (`src/whisper.cpp:6812-6815`, `:4021-4047`). `.en` models force English (`server.cpp:875-881`). The Settings toggle (`shell.rs:262-270`) also triggers a full reload, because language is part of `speech_changed` (`config.rs:51`). Instead, send `language` per request and drop it from the launch arguments and from `speech_changed`. Changing the language then needs no reload. Document that "auto" costs about 1.5–2x on short clips.

**P4. Press-to-ready software setup.**

- Every press repeats the device enumeration (when a specific microphone is set), the format query and the stream build.
- Option: one long-lived capture thread that keeps a built stream paused between sessions and calls `play()` per session, with the device cached and cleared on error.
- Only the software part improves. Bluetooth or USB wake-up time doesn't change.
- **Privacy check required.** A paused stream must not keep the mic indicator on (Windows mic-in-use icon, macOS orange dot).
- Always-on pre-roll capture is rejected; it contradicts "capture starts only when you trigger dictation".
- Instrument first, with an env-gated trace that records only timestamps: press, Start, built, play, first callback, Ready.

**P5. Upstream GPUI wakes a thread on every display refresh on Windows, forever.**

- The VSync thread calls `DwmFlush` or sleeps, then invalidates every GPUI window (`gpui-0.2.2/src/platform/windows/platform.rs:240-266`, `vsync.rs:40-55`). That's 60–240 wakeups per second even with only the tray icon active.
- The "no CPU-time increment over 5 s" sample can't rule this out; Windows CPU time is tick-granular.
- macOS stops its display link for hidden windows (`mac/window.rs:476-494`, `:1907-1920`).
- The fix would be a GPUI patch; defer unless Process Explorer context-switch deltas or `powercfg` show it matters. This desktop is not a laptop.
- There is no inactive-window throttle in 0.2.2. Only active windows get the extra keep-alive presents (`window.rs:~1041`).

**P6. Frame timing.**

- **Timing jitter.** The pill measures elapsed time with `Instant::now()` when it renders (`pill.rs:112-118`), not with the presentation time. Messages are dispatched after `DwmFlush` on another thread, so timing jitter could make fast-moving edges wobble by a fraction of a pixel.
- **Processing animation.** The pill animates at the full display rate for the whole of processing (`pill.rs:171`).
- **Mixed refresh rates.** The pill is paced by the primary compositor clock, which may judder on a mixed-refresh second monitor.
- **Measure with PresentMon** (Windows) or Instruments' Animation Hitches (Mac) using `--demo`. Also compare fixture inference time with and without the demo animating. Don't lower the rate automatically.

**P7. macOS: a disabled event tap tears down the runtime.**

- On `TapDisabledByTimeout` or `TapDisabledByUserInput` the code calls `tx.close()` (`macos.rs:30-31`). The runtime ends, the warm model is killed, and the user must re-enable dictation.
- A timeout can happen under load, for example during CPU inference.
- Suggestion: re-enable the tap, send Cancel, and tear down only on repeated failure.

**P8. Windows low-level hook thread has normal priority.**

- The hook thread (`windows.rs:57-62`) could be starved if inference threads saturate every core.
- Windows silently removes low-level hooks that time out.
- Low risk with 4 of 16 threads. An optional one-line priority boost helps.

**P9. Whisper worker orphaned after a crash on Unix.**

- On Unix, the process group is killed only when the app shuts down normally (`process.rs:48-76`). After a crash or force-quit on macOS, `whisper-server` keeps 1–2 GB of model memory until reboot.
- Windows is covered by the kill-on-close job object.
- The project rules forbid terminating processes by name, so a fix would be a PID-plus-executable-path check at next launch. That is a product decision.

**P10. Smaller items.**

- The pending-text path always waits at least 10 ms, then polls at 100 Hz (`runtime.rs:138-142`). Try inserting immediately when modifiers are already up. Saves about 10 ms.
- A snapshot is published on every loop turn, even when nothing changed (`runtime.rs:314`). That wakes the pill and settings tasks up to 100 times per second during the modifier wait. `send_if_modified` would avoid it.
- Pause joins the worker shutdown on the UI thread (`shell.rs:63-70`). A CUDA context teardown could freeze the UI for hundreds of ms.
- The hands-free timer wake is not aligned to second boundaries (`pill.rs:177-178`), so the displayed seconds can lag by up to 1 s during silence.
- The Settings window opens on every launch (`main.rs:116`).

## Quality and behavior items found on the path

**Q1. Silence-edge trimming is missing compared with the .NET app.**

- .NET trims quiet edges of 1 s or more and keeps 0.5 s of padding, and it requires about 100 ms of audible audio (`src/Speakeasy.App/Platform/PcmRecording.cs:33`, `:56-82`). That was added to fix trailing-silence hallucinations (`VERIFIED.md:70-71`).
- Rust has no trimming. Its speech gate needs only 10 ms of samples above −50 dBFS (`audio.rs:159`, `:185`), which room noise passes. Accidental taps then get transcribed, and Whisper may insert hallucinated text such as "Thank you.".
- Port the trimming in about 25 lines, applied in place after stop. It also shortens recordings over 30 s.

**Q2. The end of the last word is cut at release.**

- The callback stops pushing samples as soon as the flag is set (`audio.rs:214`). The last device period, about 10–30 ms, is dropped.
- A post-roll (keep capturing briefly after release) trades an equal amount of added latency for tail fidelity. Treat it as an explicit product choice.
- Test with fixture tail cuts at 0, 30 and 60 ms.

**Q3. Mac-specific risks.**

- If microphone permission is denied, macOS may deliver silent buffers rather than an error. The pill would then show "recording" and the result would be empty. Verify on the Mac.
- Ad-hoc signing (`package-rust-macos.sh`) resets the privacy permissions on every rebuild.

## Rejected or deferred

- **Replacing HTTP with in-process whisper.** Measured request time is essentially inference time. The change would add C++/GPU build complexity and lose process isolation.
- **Downsampling to 16 kHz in the app.** Measured no gain, and it would add a resampler.
- **Batching ring-buffer pushes.** Per-sample atomics at 48 kHz cost essentially nothing.
- **Removing extra buffer copies.** For 10 s the payload is about 1 MB. Reducing the 5 ms capture poll is fine at 10 ms but it's a micro change.
- **Changing the spring model.** The springs are analytic and don't depend on frame rate.
- **Keeping a worker pool or disconnect-and-reuse on cancel.** Reuse via the server's abort-on-disconnect (`server.cpp:959-964`) is a possible follow-up only if C1's background reload proves too slow.
- **Disabling temperature fallback.** It measured as dangerous with a shrunk window and gave no gain with the full window.
- **Always-on microphone.** Privacy.
- **Automatically lowering animation to 60 Hz.** The user rejected this.
- **`strip=symbols`, fat LTO, `panic=abort`.** The size win doesn't matter, and `panic=abort` would break the `catch_unwind` in the hooks.
- **Replacing reqwest.** No runtime gain; it adds first-party code.
- **Unloading the model when idle.** It holds 1–2 GB on this PC, but unloading reintroduces the 1.55 s cold start. Only offer it as an explicit option.

## Implementation order

1. **Fix before delivery.** Each is small, local and ordinary Rust; no new modules or dependencies.
   - C1: re-warm after cancel and after a failed session job, plus one test.
   - C2: normalize newlines.
   - C3: clipboard `new_attempts`.
   - C4: health poll timeout and interval.
   - Q1: port the edge trimming and the 100 ms speech gate.
2. **Measure next,** all with fixtures or the demo:
   - P1 backend detection, which also tells you whether P2 matters;
   - the P2 first-vs-second inference timing;
   - the C4 Windows connect probe;
   - P6 PresentMon on `--demo`;
   - P5 context-switch rate;
   - M2 thread count on the target machine.
3. **High-return work, after evidence:**
   - M1, guarded and validated on a corpus;
   - P3 per-request language;
   - P4, after instrumentation and the privacy check;
   - incremental chunked transcription for hands-free recordings over 30 s: pause-aligned chunks transcribed during recording, then only the tail after stop. Build this only if long-dictation latency shows up in use.
4. **Defer:** P5 (GPUI patch), P9, disconnect-and-reuse on cancel.

## Profiling recipe (fixtures and demo only)

1. **Provider.** Start the fixture server as `Whisper::start` does. Poll `/health` every 10 ms and time spawn to healthy. POST jfk.wav cuts of 2, 3, 4, 6 and 11 s with `curl -w %{time_total}`, five runs each. Vary `audio_ctx`, `--threads` and the GPU build. Record hashes, word counts and phrase checks, never text.
   - On Windows, use `curl.exe` and repeat with local NTFS copies to separate the UNC-path effect.
   - For P2, compare the first and second request on a freshly started worker.
2. **Controller overhead.** Add a temporary or ignored tokio test with `FakePorts` that replies instantly, and time Finish to `pasted`. Expect about 10 ms, from the pending-text delay.
3. **Frame pacing.** Loop the `--demo` "Replay preview" under `PresentMon --process_name speakeasy.exe`. Record time between presents, dropped frames and composition mode at 60 and 120/144 Hz. Repeat while a fixture inference loop runs, and compare inference time with and without the demo open.
4. **Idle.** With the tray only and Settings closed, sample per-thread context-switch deltas with Process Explorer or WPR/WPA. On the Mac, use `top -stats pid,idlew` or `powermetrics --samplers tasks`.
5. **Later, with the user's opt-in:** env-gated timestamps (no audio) for press, Start, stream built, play, first callback and Ready, plus the time for the capture stream to stop.

## Strongest potential 10x improvements

| Metric | Baseline | Target | Confidence | Effort | Prerequisites |
|---|---|---|---|---|---|
| Next dictation after Escape or an error (end of speech to text) | Reload (~1.55 s on this PC) + inference 0.2–0.4 s, after speech | Inference only | 5–10x; high on mechanism, medium on size | ~15 lines + test | None |
| End of speech to text if the build silently runs on CPU | Unknown; fixture build reports "no GPU found"; CUDA ≤0.47 s for 16–22 s of audio | Confirmed GPU, shown in UI | ≥10x if currently CPU; medium | ~30 lines or docs | GPU server build per OS |
| Short-clip inference on CPU | 0.23–0.37 s (tiny.en, measured) | 0.03–0.16 s (measured) | 2–9x measured; low confidence for shipping | Small + guard + evaluation | Word-error corpus, degenerate-output fallback |
| Wait after stop for 1–5 min hands-free dictation | Every 30 s window processed after stop | Last chunk only | 5–50x; medium, quality risk | High | Long-form fixtures, chunk-boundary word-error checks |
| Windows model-readiness detection | Possibly ≤1 s per load (hypothesis) | ≤25–50 ms | 10–40x on a 0.1–1 s component; medium | 2 lines | Windows probe |
| Press to ready, software part | Unmeasured | `play()` only | Up to ~10x of that part; low | Medium | Instrumentation, privacy-indicator check |
| Idle wakeups on Windows | 60–240/s from GPUI VSync | ~0 | High on mechanism, unknown power value | GPUI patch | Power measurement |
| First dictation after load, GPU | Unmeasured (no penalty on CPU) | Warm-up request | 1–10x; low | ~10 lines | First-vs-second timing on GPU |


---

## Focused review of the implemented fixes

## Approve

I found no bugs or regressions in these changes that should block delivery. I read the current `runtime.rs`, `audio.rs`, `whisper.rs` and `windows.rs:311-353` only.

### Checked and correct

- **Reload after cancel or failure (`runtime.rs:119-122`, `:233`, `:297-299`).**
  - The failure branch reloads only when `session.is_some()`. It runs after `job.take()`, and a failed session always owned the only worker, so there is never a double job or a loop from a failed warm-up.
  - Cancel reloads only when there is no server and no job, so a warm-up already in flight is kept.
  - Saving settings sends Cancel before the config change. If the speech settings changed, the change handler aborts the warm-up that Cancel just started (still on the old config) and reloads with the new one. If they didn't, the old warm-up is still valid and is kept.
  - The tests drain the startup load and assert a second load after Cancel and after a failed transcription. The "no reload for UI preferences" assertion still holds, because Cancel while idle produces no action.
- **Speech gate and edge trimming (`audio.rs:237-280`).**
  - The check compares summed energy against threshold² × samples, which is exactly "RMS ≥ 0.003". That matches the .NET default (`MicRecorder.cs:22`).
  - The ≥100 ms audible gate, the 1 s edge threshold and the 0.5 s padding match `PcmRecording.cs:78-80`.
  - Both the window size and the padding are whole-sample byte counts, including at 44.1 kHz and 11.025 kHz. The padded end never exceeds the buffer, and the slice is dropped before `copy_within`.
  - I checked the unit test's byte arithmetic (the expected slice is 1.5–7.5 s) and its click case (60 ms, below 100 ms).
  - Trimmed audio is always at least 0.6 s, above whisper.cpp v1.8.3's 100 ms minimum input (`whisper.cpp:6844-6848`).
- **Whisper changes (`whisper.rs`).**
  - The 200 ms health timeout overrides the 2 s connect timeout, and polling every 25 ms is bounded by the 120 s startup limit.
  - Whitespace-only output normalizes to `""`, which still leads to Done.
  - The ignored fixture test prints only timings, never transcript text.
- **Clipboard retry (`windows.rs:313-322`).** It uses a wall-clock budget and a real sleep, and returns the last error once the budget is spent. It runs only on the dictation owner thread, and Escape still wins through the commit gate. `thread` is already imported.

### Non-blocking notes

1. **Reloads when the engine is broken.** Warm-up failures never retry on their own. But when the engine can't start at all (bad model, or the AVX2 engine on a CPU without AVX2):
   - each dictation Escape starts one new load, and a doomed load makes the pill switch from "Cancelled" to the engine error;
   - each failed dictation launches the server twice: once inside the session, once for the reload.

   Both are bounded and the messages are truthful. Just avoid describing this as "no retry".
2. **Possible GPU memory overlap on cancel.** The cancel reload starts right after the aborted worker is dropped. Dropping it only requests termination (job close / `kill_on_drop`); it doesn't wait for the process to exit. On a GPU with less than about twice the model's memory, the new load could overlap the dying process's GPU memory and fail. Recovery is automatic on the next dictation. Worth one check during native GPU acceptance: cancel during processing, then confirm the reload succeeds.
3. **The fixture test can't catch the bug it guards.** jfk.wav produces a single segment, and the old `trim()` would also pass `no-newline`. A pure helper with a unit test like `"a\n b\n" → "a b"`, or a fixture longer than 30 s, would actually exercise the fix.
4. **`split_whitespace` also rewrites other Unicode spaces.** It turns NBSP and ideographic spaces into ASCII spaces. That's harmless for dictation; joining on line breaks only would preserve internal spacing if that ever matters.
5. **Trimming adds a pass after stop.** The trim scans the audio once after capture stops, on the end-of-speech path. That's well under 1 ms for 10 s of 48 kHz audio, and roughly tens of ms for a full 5-minute recording. Negligible next to inference.

I didn't run anything for this review; it was reading only.

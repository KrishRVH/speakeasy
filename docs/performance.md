# Performance

Optimize the time from finishing speech to usable text while preserving recognition quality,
cancellation, smooth animation, idle cost, and memory bounds. A faster model request is useful only
when the full interaction remains correct.

## Implementation constraints

- Parakeet dictation recognizes pieces during recording, on CPU and GPU. A piece contains at least
  twenty seconds and ends in a pause of at least a second, keeping half a second after speech.
  Pieces abut without overlapping or losing PCM; the full recording remains available for fallback.
  Whisper and Show narration retain full audio context.
- Splitting a recording adds fixed request costs. A short recording on a fast GPU can spend a few
  tens of milliseconds more in total inference in exchange for less work after release. Changing
  request boundaries needs at least three paired real-time runs checking accuracy, punctuation,
  release latency, and total inference. Median total inference must stay within 10% of whole-clip
  work or 25 ms per added request, whichever allows more. Accelerated replays check quality and
  capture pressure, not latency.
- The session owner receives completion messages and owns capture, inference, insertion,
  cancellation, and replacement. Native cleanup acknowledges before replacements open; Pause and
  application-owned Quit keep the UI responsive.
- Cancelling or promoting dictation to a Show discards piece results while their worker finishes; no
  partial text reaches insertion. The next session can wait behind that one piece. Cancelling final
  inference on GPU reuses a worker only after it transcribes a second of silence within two seconds.
  Failed checks and cancelled final CPU inference retire the worker. A retained worker may keep its
  largest model buffers until Pause releases it.
- The audio callback allocates nothing and writes to a bounded single-producer, single-consumer
  ring. Consumer-side PCM starts with a ten-second reservation and cannot grow past five minutes.
  Callback sample count, owner timer, and consumer limit defend the bound independently.
- Meter updates coalesce into a pending native frame. Session changes redraw immediately. Grille
  motion has a 200-FPS ceiling; settled and hidden views request no animation frames. Display
  refresh can impose a lower rate.
- Speech classification runs incrementally on the capture consumer, leaving only a partial window to
  classify at stop. [audio/speech.rs](../crates/app/src/audio/speech.rs) decides which audio a
  recording keeps, [audio/pcm.rs](../crates/app/src/audio/pcm.rs) trims to it, and
  [audio.rs](../crates/app/src/audio.rs) seals audio before driver teardown. WAV preparation reuses
  the PCM allocation, checks lengths, and rejects unsupported input.
- A Show's recorder keeps per-frame work to a few milliseconds and its journal under a fixed cap.
  Checkpoint writes use a bounded queue on an owned thread; storage pressure stops checkpointing at
  a complete prefix while in-memory capture continues. Its overlay holds full-window path targets
  only while ink shows; GPUI frees them after a frame without paths, and the window closes when the
  Show ends. Publishing rebuilds, resamples, and encodes one contact-sheet page at a time on up to
  one thread per frame in the page, bounded by the machine's cores. Only encoded output accumulates
  across pages.
- Do not add an always-on microphone, worker pool, cache, resampler, or background service without a
  measured user benefit and explicit resource ownership.

[Architecture](architecture.md) maps owners and cross-module invariants. This page keeps no results:
a measurement speaks only for the revision it ran on.

## Measuring

Record hardware, model and runtime, sample rate, request boundaries, warmup, and competing load.
Compare paired runs and check word accuracy and formatting as well as latency. The opt-in tests
`profile_fixture_dictation` and `profile_ready_text_latency` exercise the real session owner with
public audio or fake devices. `profile_recording_dictation` replays public PCM through the actual
callback, ring, drain, and pause cutter while the local worker recognizes it, with fake insertion.
It reports request hashes and durations, word and formatting errors against a public reference,
total inference work, release latency, and drain and meter cadence. Pair segmented and whole runs at
real time for latency and capture load; accelerated playback is only a quality check.
[Verification](architecture.md#verification) names the fixture variables. The synthetic
`profile_audio_pipeline` probe times callback/downmix/consumer work; `profile_audio_seal` times
sealing a five-minute recording, classified for speech while recording. Sealing includes the
remaining window, range selection, trimming, capacity reclamation, and WAV encoding; it excludes
microphone teardown, inference, and insertion. The synthetic `profile_show_pipeline` probe measures
the recorder's per-frame fingerprint and journal costs on 2560 × 1440 desktops that stay still,
type, scroll, open a dialog, or play video, with journal megabytes per frame. It also measures
checkpoint queuing, final storage acknowledgement, log size, and replay. Publication of a one-minute
Show measures selection, page composition (including keyframe rebuilding and PNG encoding), file
assembly, and the private write including retained source frames. `profile_real_screen_recording`
records the real screen for ten seconds on Windows or Mac, keeping no pixels; on Windows, run the
test binary directly and sample its process, not Cargo's, with `profile-windows.ps1`. Use ordinary
native profilers rather than a permanent benchmark suite.

## Public corpus replay

The same `profile_recording_dictation` test accepts `SPEAKEASY_FIXTURE_CORPUS`, a local JSON file
with 1 to 256 public clips. Paths resolve beside the manifest; each audio and reference hash must
match before that case starts. IDs and tags use up to 80 ASCII letters, digits, underscores or
hyphens. IDs must be unique. For example, replace the two hash placeholders with the files' SHA-256:

```json
{
  "clips": [
    {
      "id": "speaker-01-short",
      "tags": ["english", "short"],
      "wav": "speaker-01.wav",
      "reference": "speaker-01.txt",
      "wav_sha256": "SHA256_OF_WAV_BYTES",
      "reference_sha256": "SHA256_OF_UTF8_REFERENCE_BYTES"
    }
  ]
}
```

Keep recordings, manifests, references and results in ignored `.scratch/` or `artifacts/`. Record
the source, license, selection rule, any audio transformations, model/runtime identities and
hardware alongside the results. Do not put private recordings in this fixture.

```sh
SPEAKEASY_FIXTURE_CONFIG=.scratch/public-speech/config.json \
  SPEAKEASY_FIXTURE_CORPUS=.scratch/public-speech/corpus.json \
  SPEAKEASY_FIXTURE_SPEED=1 SPEAKEASY_FIXTURE_SEGMENT=1 \
  cargo test -p speakeasy --release --locked runtime::fixture::profile_recording_dictation \
  -- --exact --ignored --nocapture --test-threads=1
```

Each case starts and warms a fresh worker, reports startup separately, replays audio through the
capture pipeline and waits for worker teardown. It uses fake insertion and no microphone or hooks.
Output contains hashes and counts, never reference or recognized text. A failed case stops the run;
there is no aggregate success report for an incomplete corpus. Empty recognition counts as word
deletions and has no insertion latency. The final JSON gives overall and per-tag results: total word
errors divided by total reference words, insertion counts, and nearest-rank p50/p95 release latency
for real-time insertions only. Accelerated replays retain quality results but have no aggregate
latency percentiles. Report sample counts; a small corpus cannot establish a stable tail latency.

Word scoring is an alphanumeric-token proxy, identified in output as
`lowercase-alphanumeric-runs-v1`. It lowercases tokens and splits at punctuation and combining
marks; it does not normalize Unicode or numbers, expand contractions or segment languages without
word boundaries. Do not compare its rates across languages. The separate formatting score uses
whitespace tokens with case and punctuation intact. Neither measures human correction effort. Use
references with intended formatting for that score; uppercase audiobook transcripts cannot establish
punctuation quality. Preserve silence cases' insertion counts and raw error totals even when their
zero reference words make an error rate undefined. Compare the same pinned corpus and normalization
across products, and evaluate conversational speech, accents, names, numbers and languages
separately before drawing product-wide conclusions.

## Native process sampling

`profile_linux.py` reads numeric `/proc` metadata, including CPU time, RSS, PSS, private resident
pages, threads and descriptors. It detects process exit and PID reuse. Linux context-switch counters
describe the main thread, not all threads. Fixture tests cover numeric parsing, unknown optional
data, identity changes, counter regression, CPU denominators, and nearest-rank percentiles.

`profile-windows.ps1` samples process CPU, context switches summed over the threads alive at each
read, working set, private committed memory, dedicated and shared GPU memory where the driver
publishes them, threads and handles. Windows private committed memory and Linux private resident
pages are different metrics; compare each on the same OS. The Windows script requires a native
Windows host; Linux fixture tests do not verify its runtime behavior.

Pass explicit app and worker PIDs, reported separately. For example:

```sh
mise exec -- python scripts/profile_linux.py --pid 1234 --pid 1235 --label idle-hidden \
  --seconds 30 --output artifacts/profiles/linux-idle.json
cargo test -p speakeasy --release --locked profile_audio -- --ignored --nocapture --test-threads=1
cargo test -p speakeasy --release --locked profile_ready_text_latency -- --ignored --nocapture
```

```powershell
./scripts/profile-windows.ps1 -ProcessIds 1234,1235 -Label idle-hidden `
  -Seconds 30 -OutputPath artifacts/profiles/windows-idle.json
```

100% CPU means one logical core; machine-normalized samples divide by the logical CPU count. Use at
least 30 seconds for low idle CPU, where OS tick quantization matters. The scripts read no command
lines, audio, transcripts, keystrokes or engine output. Do not add app and worker RSS as unique
physical memory: shared pages can be counted twice. Linux PSS can be apportioned; report Windows
process working sets individually.

For each candidate, collect paired runs after warmup with the same hardware, model, backend, display
refresh, competing load and settings:

| Phase                            | Footprint                                                    | UX checks                                                      |
| -------------------------------- | ------------------------------------------------------------ | -------------------------------------------------------------- |
| Launch and warmup                | CPU time, elapsed readiness, peak memory, worker lifetime    | Settings and Cancel remain responsive                          |
| Hidden idle and visible Settings | CPU, resident/private memory, threads, descriptors/handles   | Reopen and save latency                                        |
| Recording and pill motion        | App CPU, memory growth, GPU activity, displayed frame pacing | Audio onset, 60/144/200/high-refresh pacing and no lost speech |
| Short and long inference         | Worker CPU/GPU memory, peak app PCM memory                   | Stop-to-insertion p50/p95 and recognition quality              |
| Cancel, Pause and Quit           | Cleanup time, returning memory and resource counts           | No stale paste, stuck keys or surviving owned work             |

Process sampling cannot measure displayed frames, driver work, power, or energy; use the native
platform's frame/CPU/GPU profiler for those. Microphone onset and editor acceptance need the
[native acceptance](#native-acceptance) checks.

## Native acceptance

Before making a native performance claim, opt in explicitly and compare live microphone onset, lost
speech, device teardown, stop-to-editor latency, cancellation, and resource return on the target OS.
Keep Settings and Cancel responsive under competing CPU/GPU load, and report native verification
limits with the exact revision and environment.

Exercise short dictation, long recordings, the five-minute boundary, denied and revoked permissions,
device removal, clipboard-owner changes, and focus changes. Include fullscreen apps such as games,
terminals, protected fields, mixed-DPI and mixed-refresh displays, and display removal. Report
editor acceptance separately from an OS accepting injected input. Before changing GPUI termination
handling, check that forced termination, which disposes windows synchronously, keeps the UI
responsive.

[Verification](architecture.md#verification) states what default checks leave unproven, and the
[Linux matrix](linux-implementation.md#native-acceptance-still-required) tracks desktop acceptance.
Hands-on macOS runtime performance, accessibility, idle power, and representative mixed-display
pacing remain measurement gaps.

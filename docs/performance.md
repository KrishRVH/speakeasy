# Performance

Optimize the time from finishing speech to usable text while preserving recognition quality,
cancellation, smooth animation, idle cost, and memory bounds. A faster model request is useful only
when the full interaction remains correct. The [fork PRD](apple-silicon-prd.md) owns targets,
decisions, and the perceptibility experiment; this page owns constraints and methods.

## Implementation constraints

- Recognize the whole recording. Whisper retains full audio context and default timestamp decoding;
  returned text contains no timecodes. Chunking or changing decoding needs paired accuracy and
  punctuation checks before adoption.
- The session owner receives completion messages and owns capture, inference, insertion,
  cancellation, and replacement. Native cleanup acknowledges before replacements open; Pause and
  application-owned Quit keep the UI responsive.
- GPU cancellation reuses a worker only after a bounded health request succeeds. Failed checks and
  cancelled CPU inference retire the worker. A retained worker may keep its largest model buffers
  until Pause releases it.
- The audio callback allocates nothing and writes to a bounded single-producer, single-consumer
  ring. Consumer-side PCM starts with a ten-second reservation and cannot grow past five minutes.
  Callback sample count, owner timer, and consumer limit defend the bound independently.
- Meter updates coalesce into a pending native frame. Session changes redraw immediately. Grille
  motion has a 200-FPS ceiling; settled and hidden views request no animation frames. Display
  refresh can impose a lower rate.
- Edge trimming uses integer energy and keeps speech padding. WAV preparation reuses the PCM
  allocation, checks lengths, and rejects unsupported input.
- Do not add an always-on microphone, worker pool, cache, resampler, or background service without a
  measured user benefit and explicit resource ownership.

The [architecture](architecture.md) owns the detailed state and privacy contracts. Historical
benchmark results belong to Git and published release history; they are not a performance claim
about a newly changed revision.

## Measuring

Record hardware, model and runtime, sample rate, request boundaries, warmup, and competing load.
Compare paired runs and check word accuracy and formatting as well as latency. Keep transcripts out
of logs. The opt-in tests `profile_fixture_dictation` and `profile_ready_text_latency` exercise the
real session owner with public audio or fake devices; the README lists the fixture variables.

Launching the app with `SPEAKEASY_TIMING=1` prints one line per session to standard error, on the
session owner's clock: press to first audio, the device's own open time, release to sealed audio,
any wait for the worker, the engine round trip, text to the insertion outcome, release to outcome,
the recorded length, and how the session ended. It carries durations only, never audio or text:

```text
speakeasy timing: press→audio 41.3 ms · release→sealed 1.2 ms · sealed→engine 0.0 ms · engine 152.4 ms · text→done 7.9 ms · release→done 161.5 ms · device 33.0 ms · audio 4.20 s · done
```

`engine_profile` times warm recognition of public WAV fixtures with the same engine and model as the
app, either in process through the engine's C library or through the `nemo-speech serve` HTTP route
that the 0.3.2 app uses. Each fixture prints its timings, word count, and an FNV-1a hash of its text
with whitespace collapsed as the app inserts it, never the text, so the two routes can be checked
for identical output:

```sh
engine=~/Library/Application\ Support/speakeasy/engines/nemo-speech-0.1.0-macos-aarch64-metal/nemo-speech
model=~/Library/Application\ Support/speakeasy/models/parakeet-tdt-0.6b-v3.q8_0.gguf
cargo run --release -p speakeasy-platform --example engine_profile -- \
  --library "$engine/lib/libnemo_speech_asr_c.1.dylib" --model "$model" --repetitions 20 jfk.wav
NEMO_SPEECH_HTTP_API_KEY=profile "$engine/bin/nemo-speech" serve --asr-model "$model" \
  --device auto --port 8178 --no-ui --threads 1 --asr.batching.enabled=false &
cargo run --release -p speakeasy-platform --example engine_profile -- \
  --http 127.0.0.1:8178 --key profile --repetitions 20 jfk.wav
```

Setting `NEMO_SPEECH_TIMING=1` in the engine's environment adds its own feature, encoder, and decode
stage timings on standard error; those lines carry numbers only.

`--warmup SECONDS` varies the silent warmup's length, to show whether the app's one-second warmup
leaves Metal pipelines for longer audio to compile during the first dictation. `--idle SECONDS`
waits before each request, to expose GPU residency and power-state costs: ggml releases its Metal
residency sets after `GGML_METAL_RESIDENCY_KEEP_ALIVE_S`, three minutes by default, and the helper
inherits the app's environment. `--prime MILLISECONDS` sends a tenth of a second of silence and
waits before each timed request, to evaluate priming the worker when the shortcut is pressed.

`capture_onset` opens the real microphone, so it is opt-in native acceptance that macOS gates behind
the terminal's Microphone permission. It keeps no audio and prints each stage of a cold open, host
through first callback and teardown, beside starting a stream built in advance:

```sh
cargo run --release -p speakeasy-dictation --example capture_onset -- --runs 20
```

## Process sampling

`profile_macos.py` samples explicit app and engine PIDs through `ps`: cumulative CPU time, resident
memory, and threads, with each process's physical footprint from `vmmap -summary` at the end. It
detects process exit and PID reuse, reads no command lines, audio, transcripts, keystrokes, or
engine output, and reports app and engine separately:

```sh
mise exec -- python scripts/profile_macos.py --pid 1234 --pid 1235 --label idle-hidden \
  --seconds 30 --output artifacts/profiles/idle.json
```

100% CPU means one logical core. Use at least 30 seconds for low idle CPU. Keep raw reports under
ignored `artifacts/`. Physical footprint counts compressed and GPU-wired pages that resident memory
misses; do not add app and engine figures as unique physical memory, because shared pages can be
counted twice.

For each candidate, collect paired runs after warmup with the same hardware, model, backend, display
refresh, competing load and settings:

| Phase                            | Footprint                                                    | UX checks                                          |
| -------------------------------- | ------------------------------------------------------------ | -------------------------------------------------- |
| Launch and warmup                | CPU time, elapsed readiness, peak memory, worker lifetime    | Settings and Cancel remain responsive              |
| Hidden idle and visible Settings | CPU, resident memory, physical footprint, threads            | Reopen and save latency                            |
| Recording and pill motion        | App CPU, memory growth, GPU activity, displayed frame pacing | Audio onset, 60/120 Hz pacing and no lost speech   |
| Short and long inference         | Worker CPU/GPU memory, peak app PCM memory                   | Stop-to-insertion p50/p95 and recognition quality  |
| Cancel, Pause and Quit           | Cleanup time, returning memory and resource counts           | No stale paste, stuck keys or surviving owned work |

Process sampling cannot measure GPU energy, displayed frames, microphone onset, idle wakeups, or
editor acceptance. Use Instruments (Time Profiler, System Trace, Metal System Trace, Allocations)
and `powermetrics` for those, and report live microphone/input/clipboard acceptance separately. Keep
the frame ceiling, native display pacing, speech quality and interaction policy as constraints.

## Native acceptance

Default tests and `--demo` do not establish microphone quality or real editor insertion. Before
making a native performance claim, opt in explicitly and compare live microphone onset, lost speech,
device teardown, stop-to-editor latency, cancellation, and resource return. Keep Settings and Cancel
responsive under competing CPU/GPU load.

Exercise short dictation, long recordings, the five-minute boundary, denied and revoked permissions,
device removal, clipboard-owner changes, and focus changes. Include fullscreen apps, terminals,
protected fields, mixed-scale and mixed-refresh displays, and display removal. Report editor
acceptance separately from the OS accepting injected input. Never capture or log private transcript
data for a benchmark. Hands-on runtime performance, accessibility, idle power, and representative
mixed-display pacing remain measurement gaps until measured on a Mac.

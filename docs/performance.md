# Performance

Optimize the time from finishing speech to usable text while preserving recognition quality,
cancellation, smooth animation, idle cost, and memory bounds. A faster model request is useful only
when the full interaction remains correct.

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
real controller with public audio or fake devices; the README lists the fixture variables. Use
ordinary native profilers rather than a permanent benchmark suite.

## Native process sampling

`profile_linux.py` reads numeric `/proc` metadata, including CPU time, RSS, PSS, private resident
pages, threads and descriptors. It detects process exit and PID reuse. Linux context-switch counters
describe the main thread, not all threads. Fixture tests cover numeric parsing, unknown optional
data, identity changes, counter regression, CPU denominators, and nearest-rank percentiles.

`profile-windows.ps1` samples process CPU, working set, private committed memory, threads and
handles. Windows private committed memory and Linux private resident pages are different metrics;
compare each on the same OS. The Windows script requires a native Windows host; Linux fixture tests
do not verify its runtime behavior.

Pass explicit app and worker PIDs, reported separately. For example:

```sh
mise exec -- python scripts/profile_linux.py --pid 1234 --pid 1235 --label idle-hidden \
  --seconds 30 --output artifacts/profiles/linux-idle.json
cargo test -p speakeasy --release --locked profile_audio_pipeline -- --ignored --nocapture
cargo test -p speakeasy --release --locked profile_ready_text_latency -- --ignored --nocapture
```

```powershell
./scripts/profile-windows.ps1 -ProcessIds 1234,1235 -Label idle-hidden `
  -Seconds 30 -OutputPath artifacts/profiles/windows-idle.json
```

100% CPU means one logical core; machine-normalized samples divide by the logical CPU count. Use at
least 30 seconds for low idle CPU, where OS tick quantization matters. Keep raw reports under
ignored `artifacts/`. The scripts read no command lines, audio, transcripts, keystrokes or engine
output. Do not add app and worker RSS as unique physical memory: shared pages can be counted twice.
Linux PSS can be apportioned; report Windows process working sets individually.

For each candidate, collect paired runs after warmup with the same hardware, model, backend, display
refresh, competing load and settings:

| Phase                            | Footprint                                                    | UX checks                                                      |
| -------------------------------- | ------------------------------------------------------------ | -------------------------------------------------------------- |
| Launch and warmup                | CPU time, elapsed readiness, peak memory, worker lifetime    | Settings and Cancel remain responsive                          |
| Hidden idle and visible Settings | CPU, resident/private memory, threads, descriptors/handles   | Reopen and save latency                                        |
| Recording and pill motion        | App CPU, memory growth, GPU activity, displayed frame pacing | Audio onset, 60/144/200/high-refresh pacing and no lost speech |
| Short and long inference         | Worker CPU/GPU memory, peak app PCM memory                   | Stop-to-insertion p50/p95 and recognition quality              |
| Cancel, Pause and Quit           | Cleanup time, returning memory and resource counts           | No stale paste, stuck keys or surviving owned work             |

Process sampling cannot measure GPU energy, displayed frames, microphone onset or editor acceptance.
Use the native platform's frame/CPU/GPU profiler for those checks, and report live
microphone/input/clipboard acceptance separately. Keep the 200-FPS ceiling, native display pacing,
speech quality and interaction policy as constraints. No cache, worker pool or background microphone
is introduced.

## Native acceptance

Default tests and `--demo` do not establish microphone quality or real editor insertion. Before
making a native performance claim, opt in explicitly and compare live microphone onset, lost speech,
device teardown, stop-to-editor latency, cancellation, and resource return on the target OS. Keep
Settings and Cancel responsive under competing CPU/GPU load.

Exercise short dictation, long recordings, the five-minute boundary, denied and revoked permissions,
device removal, clipboard-owner changes, and focus changes. Include fullscreen apps such as games,
terminals, protected fields, mixed-DPI and mixed-refresh displays, and display removal. Report
editor acceptance separately from an OS accepting injected input. Never capture or log private
transcript data for a benchmark.

Use native frame/CPU/GPU profilers for displayed pacing, driver work, power, and energy. Process CPU
counters alone do not establish those outcomes. In particular,
[QueryProcessCycleTime](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-queryprocesscycletime)
counts CPU cycles, not elapsed time; frequency and core placement affect their relationship. Measure
sustained idle periods after warmup and report native verification limits with the exact revision
and environment.

Linux GNOME/KDE/X11 editor, portal, and audio acceptance remains separate from the private Mesa
rendering fixture. Windows/macOS hooks and insertion also need native acceptance. Hands-on macOS
runtime performance, accessibility, idle power, and representative mixed-display pacing remain
measurement gaps.

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
  moves. Settled and hidden views request none.

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

Each of these was measured and rejected:

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
- A GPUI fork, automatic frame-rate reduction, an always-on microphone, a worker
  pool, and a background service are not justified by these measurements.

## Not yet measured

Microphone onset and teardown, real editor insertion, displayed frame pacing,
mixed-DPI and mixed-refresh displays, accessibility, idle power, and macOS
runtime performance. Default tests and `--demo` do not cover these boundaries.

## Measuring

Record hardware, model and runtime, sample rate, request boundaries, warmup, and
competing load. Compare paired runs and check word accuracy and formatting as
well as latency. Keep transcripts out of logs. The opt-in tests
`profile_fixture_dictation` and `profile_ready_text_latency` exercise the real
controller with public audio or fake devices; the README lists the fixture
variables. Use ordinary native profilers rather than a permanent benchmark suite.

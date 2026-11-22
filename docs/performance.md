# Performance

Measure from release to usable text as well as total inference time. Compare speech quality, onset,
cancellation, display pacing, and resource return under the same model, platform, and load.

## Implementation constraints

- Parakeet recognizes pause-aligned pieces during longer dictations. Nemotron can stream during
  capture. Both keep whole audio for fallback. Whisper and Show narration use the whole recording.
  Pair real-time runs with and without early recognition before changing request boundaries.
- The audio callback writes to a bounded ring without allocating. The consumer gates speech
  incrementally and seals audio before teardown. Measure sealing separately from device shutdown.
- The pill coalesces meter updates into display frames. Settled or hidden views request no animation
  frames. Use a native frame profiler for displayed pacing.
- The Show journal thins samples toward its compressed budget, preserving the first and last held
  frames. Checkpoint I/O runs off the recorder thread. Publication encodes one contact-sheet page at
  a time. Keep the recorder responsive under storage pressure.

## Measuring

`mise run profile:audio` measures sealing and the synthetic capture pipeline.
`mise run profile:latency` measures owner reply-to-insertion latency with fake devices.
`mise run profile:dictation` uses public WAV input and a real local speech worker with fake
insertion. Its report includes request times, recognition scores, capture statistics and
stop-to-fake-insertion latency. Continuous Nemotron inference has no per-request timing.

## Public corpus replay

Set `SPEAKEASY_FIXTURE_CONFIG` to a settings file with a usable engine and model. For one clip, set
`SPEAKEASY_FIXTURE_WAV` and `SPEAKEASY_FIXTURE_REFERENCE`. Use canonical mono PCM16 audio. For a
corpus, set `SPEAKEASY_FIXTURE_CORPUS` to a JSON file with paths relative to that file.

```json
{
  "clips": [
    {
      "id": "speaker-01-short",
      "tags": ["english", "short"],
      "wav": "speaker-01.wav",
      "reference": "speaker-01.txt"
    }
  ]
}
```

```sh
SPEAKEASY_FIXTURE_CONFIG="$PWD/.scratch/public-speech/config.json" \
  SPEAKEASY_FIXTURE_CORPUS="$PWD/.scratch/public-speech/corpus.json" \
  SPEAKEASY_FIXTURE_SEGMENT=1 mise run profile:dictation
```

Use `SPEAKEASY_FIXTURE_SEGMENT=1` to admit the model's early recognition mode. Default speed is one
(real time). Larger integer `SPEAKEASY_FIXTURE_SPEED` values are useful for quality replays. Use
real-time runs for release latency. Report hardware, warmup, runtime and model, and what each corpus
represents. Keep fixtures and results in ignored `.scratch/` or `artifacts/`.

Word scores count lowercase alphanumeric runs. Formatting scores retain case and punctuation.
Compare paired results on the same corpus. Results for names, numbers, and languages without
space-separated words need separate interpretation. `SPEAKEASY_FIXTURE_CLEANUP=raw` replays text
cleanup on one recognition result. In raw mode `SPEAKEASY_FIXTURE_CANDIDATE` optionally points to a
second settings file differing only in corrections and filler cleanup. Use production replay for
latency.

## Native process sampling

`scripts/profile_linux.py` and `scripts/profile-windows.ps1` sample explicit app and worker PIDs.
Compare each process separately across the same OS, phase, and duration. CPU at 100% means one
logical core. Linux private resident pages and Windows private committed memory measure different
things. Use the native profiler for display pacing or GPU work.

```sh
python scripts/profile_linux.py --pid 1234 --pid 1235 --label idle-hidden \
  --seconds 30 --output artifacts/profiles/linux-idle.json
```

```powershell
./scripts/profile-windows.ps1 -ProcessIds 1234,1235 -Label idle-hidden `
  -Seconds 30 -OutputPath artifacts/profiles/windows-idle.json
```

## Native acceptance

With explicit opt-in, measure live onset, release-to-editor latency, device loss, focus changes, and
resource return after Pause and Quit. Exercise representative terminals, editors, protected fields,
fullscreen apps, and mixed-DPI or mixed-refresh displays. Report what the editor accepted separately
from native input submission. Record the revision and host. See the
[native tasks](../README.md#build-and-verify) and
[Linux desktop matrix](linux-implementation.md#native-acceptance-still-required).

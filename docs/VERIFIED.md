# Verification on this PC

Verified on September 9, 2026: Windows 11 x64, Ryzen 9 9950X3D,
NVIDIA RTX 4090, and 64 GB RAM.

## Working installation

- Self-contained app installed at `C:\Users\Krish\Apps\speakeasy` with a desktop shortcut.
- Configuration and both verified local models live at `C:\Users\Krish\.speakeasy`.
- The installed process runs independently of the development host, initializes
  its global keyboard hook, and warms both native workers successfully.
- Reopening the app retains one instance and brings its dashboard into view.
- Graceful quit closes the app and both owned workers. The final app was then
  restarted in the background, ready for dictation.
- Dashboard, Preferences, and pill rendered and inspected at 200% DPI.
- Windows microphone access is allowed; four input devices were enumerated.

## Performance and text quality

Known synthetic speech was rendered locally with Windows SAPI and processed
through Whisper medium.en on CUDA and Qwen2.5 3B Instruct Q4_K_M.
Each fixture was run three times with the revised cleanup prompt.

| Fixture | Audio duration | Model warmup | Transcription and cleanup |
| --- | --- | --- | --- |
| Casual email | 15.795 s | 1.553 s | 348–463 ms |
| Technical request | 15.560 s | 1.561 s | 353–466 ms |
| Formatting and correction | 22.210 s | 1.546 s | 305–408 ms |

The resulting text retained names, filenames, technical casing, numbers, and
signoffs. Explicit formatting became paragraphs and bullets; a spoken
Tuesday-to-Wednesday correction was applied. Dictated technical requests
remained text, rather than causing the cleanup model to generate code.

The tray process alone occupied about 59 MB and consumed no measurable CPU
time during a three-second idle observation. Native model workers use
additional RAM/GPU memory. These are observations for this PC and these
fixtures, not latency guarantees for arbitrary speech or hardware.

## Checks

- Locked NuGet restore passed, including after publishing.
- Release build: zero warnings and errors.
- 125 .NET checks passed: 72 Core/provider checks and 53 Windows adapter/controller checks.
- Four simulated setup-download checks passed under Windows PowerShell 5.1.
- Repository whitespace verification passed.
- Both installed model files matched their official Hugging Face SHA256 values.

Checks cover gesture boundaries, independent recording limits, cancellation,
stale callbacks, device failures, clipboard formats and ownership, provider
requests, and actual termination of owned subprocess trees.

No live microphone audio was recorded during development, and no test pasted
into the user's applications. Microphone speech quality and physical shortcut
use therefore await the user's first dictation; API requests were checked with
test handlers rather than paid credentials. The pipeline timing fixture does
not include physical microphone startup or the target application's paste time.

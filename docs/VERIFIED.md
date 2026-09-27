# .NET verification record

This record covers the existing .NET app. Rust delivery and verification are
documented separately in [handoff.md](../handoff.md).

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
- Dashboard, Preferences, practice window, microphone check, and pill rendered
  and inspected on this desktop. Documentation previews use 200% DPI.
- Windows microphone access is allowed; four input devices were enumerated.
- Final saved preferences use **Ctrl+Alt+Space**, the Windows default microphone,
  a five-minute limit, local Whisper and local LLM cleanup, natural casing, and
  clipboard restoration off.
- Starting the installed app with dictation disabled displayed **Paused** and
  started no model workers. Enabling dictation displayed **Ready** and started
  exactly one owned Whisper server and one owned llama server.
- Turning **Launch at login** on registered the installed executable and its
  custom `--config-dir` argument in the current user's Windows Run key. Turning
  it off removed the entry; the original off preference was restored. An actual
  Windows sign-in or reboot was not exercised.

## Interactive acceptance

The installed app was exercised through Windows UI automation. A known
synthetic speech fixture was played into **CABLE Input**, captured by the real
Windows **CABLE Output** recording device, transcribed by local Whisper, cleaned
up by local Qwen, and sent through the app's normal clipboard and Ctrl+V path.

- Automatic insertion succeeded in the practice text box and a separate
  Notepad scratch tab. Paragraphs render correctly with Windows CRLF endings.
- A shortened 25-second recording limit stopped capture and inserted the text
  automatically. The normal five-minute setting was restored afterward.
- A global shortcut double tap from Notepad selected hands-free mode; one more
  tap finished capture and entered processing. A short single tap started
  capture and then correctly reported no speech.
- Escape sent while Notepad had focus cancelled hands-free capture. A practice
  recording was also cancelled without any text being inserted.
- With a temporary single-thread CPU CLI configuration, the owned Whisper
  process was observed running during transcription. Global Escape terminated
  that process, returned the app to Cancelled, and left the target empty. The
  normal CUDA server configuration was then restored.
- The microphone check opened the selected device, displayed input activity,
  stopped after ten seconds, and supported another check and Escape to close.
- Preferences rejected Escape as the activation shortcut, saved a selected
  recording device, and reopened correctly. The original shortcut was retained.
- With clipboard restoration enabled, Notepad's unavailable metadata triggered
  the native Edit/RichEdit insertion fallback. It replaced a selected 173-character
  joined passage with the expected five-line fixture, captured as 177 characters.
  Notepad's status bar reported **Windows (CRLF)** and paragraphs rendered correctly.
- One **Ctrl+Z** restored the old selected passage; **Ctrl+Y** restored the correct
  paragraphs. Moving to the end, adding a line, and pressing **Ctrl+V** appended
  the exact previous clipboard text, `SPEAKEASY clipboard restore check 4792`.
  This confirms undoable selection replacement and preservation of the prior
  clipboard text through the direct insertion path.

Local development evidence is saved in `artifacts/acceptance/native-edit-result.txt`
and `artifacts/acceptance/native-edit-clipboard-preserved.txt` in the source workspace.
These ignored artifacts are excluded from release packages. The exported text files
normalize line breaks to LF; Notepad's displayed line-ending mode and visible
paragraphs were checked separately.

These trials found and corrected native clipboard sequencing, Windows newline
formatting, opaque clipboard snapshot failures, and trailing-silence hallucination issues. Audio capture now trims
long quiet edges conservatively while retaining half a second of padding.
Only synthetic fixture audio was retained as a development artifact; ordinary
recordings and microphone checks do not create an audio archive.

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
- All 168 .NET tests passed: 83 Core/provider and 85 Windows adapter/controller tests.
- Six simulated setup checks passed under Windows PowerShell 5.1.
- Repository whitespace verification passed.
- Both installed model files matched their official Hugging Face SHA256 values.

Checks cover gesture boundaries, independent recording limits, cancellation,
stale callbacks, device failures, clipboard formats and ownership, provider
requests, and actual termination of owned subprocess trees.

The keyboard trials use injected Windows input, not a person physically holding
the shortcut. The available automation does not expose a timed key hold;
hold/release timing boundaries are covered by deterministic tests. API requests
were checked with test handlers rather than paid credentials. The pipeline
timing fixture excludes microphone startup and the target application's paste
time.

An actual Moonlight client microphone sender was not connected. The host has
Voicemeeter and VB-CABLE, and the cable capture/insertion path passed, but client
speech quality and the complete network audio path require the sender described
in [remote dictation](remote-dictation.md). Sunshine's streamed desktop can remain
logged in after Moonlight disconnects, so the recording cap remains important.

# speakeasy

System-wide AI dictation for Windows. Hold a shortcut, speak, and release to put polished text at your cursor. Speakeasy runs in the system tray, with a small floating pill while the microphone is live.

The app uses native Windows input and clipboard APIs, .NET 10 WinForms, local whisper.cpp speech recognition, and optional local or cloud text cleanup. The fully local setup needs no account, API key, or network connection after installation. Speakeasy has no telemetry or transcript history.

![speakeasy dashboard](docs/images/dashboard.png)

## Install and start

If you have the packaged ZIP, extract it and open `speakeasy.exe`. The local
model setup script is included in `scripts/`. For source builds, use the steps
below. Your installed copy can be opened again to bring its dashboard forward.

Use Windows 11 x64. Building from source requires the .NET SDK pinned in [global.json](global.json); the published app includes its .NET runtime. Run these commands in PowerShell from the repository directory:

```powershell
dotnet restore Speakeasy.sln --locked-mode
./scripts/setup-local.ps1 -Backend cuda -Model medium.en -IncludeCleanup
./scripts/install.ps1 -Launch
```

This installs the app under `%LOCALAPPDATA%\Programs\speakeasy`, creates a Start Menu shortcut, and opens Speakeasy. The CUDA setup downloads whisper.cpp, the English medium Whisper model, llama.cpp, and the Qwen2.5 3B Instruct model for local cleanup. It sets the actual executable and model paths in `%APPDATA%\speakeasy\settings.json`. Downloads require internet access and several gigabytes of disk space; normal local dictation does not.

For a machine without a compatible NVIDIA GPU, use CPU builds and the smaller model:

```powershell
./scripts/setup-local.ps1 -Backend cpu -Model small.en -IncludeCleanup
```

The setup script supports `small.en`, `small`, `medium.en`, and `medium`. Choose a model without `.en` for multilingual recognition and set `transcription.language` to the appropriate language code or `auto`. Running setup again selects the local provider and updates its model/backend settings. Choose **Reload settings** in the tray menu afterward.

To run directly from source:

```powershell
dotnet run --project src/Speakeasy.App -- --settings
```

Source runs and the installed app use the same configuration directory. Only one tray instance runs at a time. Closing the main window leaves dictation available in the tray; choose **Quit** to close it completely.

## Everyday use

| Action | Result |
| --- | --- |
| Hold **Ctrl+Alt+Space**, speak, then release | Records while held, transcribes, cleans up if configured, and pastes. Release the modifier keys so they do not interfere with paste. |
| Double-tap the shortcut | Starts hands-free recording. Nothing needs to remain held. |
| Tap once during hands-free recording | Stops recording and inserts the result. |
| Press **Esc** | Discards active audio or cancels transcription/cleanup. Escape also reaches the app you are typing in. |
| Disable dictation in the tray | Cancels the current session and pauses the shortcut. |

The first tap of a double-tap must last at most **220 ms**; press the shortcut again within **300 ms** after releasing it. A short single tap waits through this window before it finishes, so the opening words of a hands-free session are retained. These timings are adjustable.

Every recording stops automatically after **five minutes**, including hold-to-talk. A shorter limit is configurable; a longer limit is rejected. Locking Windows, logging off, disconnecting the session, or suspending the PC cancels active work. Silence and very short accidental recordings are skipped.

The pill shows the recording mode, elapsed time, and microphone level, then the processing stage. It does not take keyboard focus. The result goes to the editable field that has focus **when paste happens**; keep your intended destination focused while transcription finishes. Escape cannot retract text after Windows has already received the paste shortcut.

Double-click the tray icon to open Speakeasy. **Preferences** provides the common microphone, shortcut, provider, cleanup, and clipboard controls. The tray also offers **Dictation enabled**, **Launch at login**, **Edit settings**, **Edit API keys (.env)**, **Reload settings**, and **Quit**. Launch at login applies only to your Windows user and does not require administrator access.

## Windows permissions and keyboard behavior

In **Settings → Privacy & security → Microphone**, enable **Microphone access**, **Let apps access your microphone**, and **Let desktop apps access your microphone**. Speakeasy is a desktop app. Select the intended recording device in Preferences or use the Windows default. See [Microsoft's microphone permission instructions](https://support.microsoft.com/en-us/windows/privacy/turn-on-app-permissions-for-your-microphone-in-windows).

**Accessibility** and **Input Monitoring** permissions are macOS settings; Windows has no corresponding permission panels to grant for this app. Speakeasy observes its shortcut through a Windows keyboard hook and pastes with **Ctrl+V**.

The **Fn** key is often handled inside keyboard firmware and never produces an observable Windows key event. Speakeasy therefore rejects `Fn` as a shortcut. If your keyboard's own software or firmware can remap Fn to **F13**, configure `F13` in Speakeasy. Otherwise choose an observable key such as `RightAlt`, `F8`, or `Ctrl+Alt+Space`. A remapping utility cannot recover a key event the keyboard never sends. Side-specific modifiers and F1–F24 are supported; Escape is reserved for cancellation.

Windows can block automatic input into applications running as administrator. In that case Speakeasy leaves the transcript on the clipboard and asks you to press **Ctrl+V** in the target app. Secure desktops and applications that reject paste are outside normal automatic insertion support. This follows Windows' [SendInput integrity-level restrictions](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-sendinput).

## Clipboard behavior

By default, the last result remains on the clipboard, so you can paste it again. Set `restoreClipboard` to `true` to restore the previous contents after automatic paste. `clipboardRestoreDelayMs` defaults to **800 ms**.

Restoration captures the available clipboard formats before replacement and only restores if the clipboard still contains Speakeasy's result. A newer copy from another app wins. Some custom or delayed clipboard formats cannot be copied reliably; restoration is best effort. Applications that read the clipboard unusually late may need a longer delay. Windows can confirm that Ctrl+V was sent, but cannot prove that every target application consumed the text. If automatic paste is blocked, the result stays available for manual paste even when restoration is enabled.

## Settings and providers

Configuration lives in `%APPDATA%\speakeasy`:

| File or folder | Purpose |
| --- | --- |
| `settings.json` | Shortcut, audio, provider, cleanup, and clipboard preferences. |
| `.env` | Optional provider keys. Never put keys in `settings.json`. |
| `models/` | Downloaded Whisper and local cleanup models. |
| `tools/` | Local inference executables and their dependencies. |
| `downloads/` | Downloaded installation archives. |

[settings.example.json](settings.example.json) contains every supported setting. JSON comments and trailing commas are accepted; unknown setting names are rejected to catch typos. Relative executable/model paths resolve against the configuration directory. Changes to JSON or `.env` take effect after **Reload settings**. Process environment variables override `.env` values; the app does not alter the global environment.

Useful settings include `hotkey`, `doubleTapMs`, `tapMaxMs`, `maxRecordingSeconds`, `microphoneDevice` (`-1` means Windows default), `silenceThreshold`, and `restoreClipboard`. For a separate configuration directory, start with `--config-dir "C:\path\to\configuration"`.

### Local speech and cleanup

`transcription.provider` defaults to `local`. Adding an API key never switches local speech recognition to the cloud.

`cleanup.provider` accepts `auto`, `none`, `local`, `groq`, or `openai`:

| Cleanup choice | Behavior |
| --- | --- |
| `auto` with local transcription | Keeps the raw Whisper text; makes no cloud request. |
| `auto` with Groq/OpenAI transcription | Uses the same cloud provider for cleanup. |
| `local` with `autoStartLocal: true` | Speakeasy starts and owns the installed llama.cpp model. Configured by `setup-local.ps1 -IncludeCleanup`. |
| `local` with `autoStartLocal: false` | Uses your already-running OpenAI-compatible server at `cleanup.endpoint`; specify its `cleanup.model`. Only loopback URLs are accepted. |
| `none` | Inserts the raw transcript. |

For an existing local server such as Ollama, set `cleanup.provider` to `local`, `cleanup.autoStartLocal` to `false`, `cleanup.endpoint` to its OpenAI-compatible base URL, such as `http://localhost:11434/v1`, and `cleanup.model` to a model you have installed. An optional `LOCAL_LLM_API_KEY` is sent only to that local endpoint.

The cleanup prompt removes filler words and accidental repetitions, fixes punctuation, and preserves meaning and language. It treats dictated commands and questions as text to edit. `cleanup.style` supports `natural`, `sentence`, and `lowercase`. If cleanup fails or times out, Speakeasy inserts the original transcript with a notice. Cancellation discards the result instead. Use `none` when you want the original recognition output without an LLM edit.

### Optional cloud providers

Put the applicable key in `%APPDATA%\speakeasy\.env`, using [.env.example](.env.example) as a reference:

```dotenv
GROQ_API_KEY=your-key
OPENAI_API_KEY=your-key
```

Set `transcription.provider` explicitly to `groq` or `openai`. Leave `transcription.model` empty to select the provider's default here: `whisper-large-v3-turbo` for Groq, or `whisper-1` for OpenAI. The implementation uses their documented [Groq speech-to-text](https://console.groq.com/docs/speech-to-text) and [OpenAI file transcription](https://developers.openai.com/api/docs/guides/speech-to-text) endpoints.

With cloud cleanup, an empty `cleanup.model` selects `llama-3.3-70b-versatile` for Groq or `gpt-4.1-mini` for OpenAI. Models are configurable; consult the [Groq model catalog](https://console.groq.com/docs/models) and [OpenAI model documentation](https://developers.openai.com/api/docs/models/gpt-4.1-mini) for availability. These providers use your own API credentials and may charge for requests. There is no automatic cloud fallback if a local model fails.

Local speech plus local cleanup keeps audio and text on the machine. Cloud transcription sends the recording to the selected provider; cloud cleanup sends the transcript to its selected provider. Speakeasy does not log audio, keys, provider response bodies, or ordinary dictated text. Windows clipboard history/sync and the destination app follow their own settings.

## Performance and local model lifetime

The default local mode is `server`: a hidden, owned `whisper-server.exe` keeps the model loaded between recordings. The optional owned `llama-server.exe` does the same for cleanup. Both bind only to loopback addresses and warm up asynchronously. They consume RAM and, with CUDA, GPU memory while resident. The microphone is opened only during recording.

Escape aborts an active local inference by terminating that owned worker. The next session starts a fresh worker, so its first result may take longer. Quitting or reloading settings closes the old workers. Pausing dictation stops input but keeps already-loaded models available.

Set `transcription.localMode` to `cli` to run whisper.cpp separately for each recording. This releases the model after every invocation but adds model-loading time. If server mode is selected and its executable is absent, Speakeasy uses the configured CLI; a present but failing server reports an error. `transcription.useGpu`, `transcription.threads`, `transcription.timeoutSeconds`, and `transcription.startupTimeoutSeconds` control execution. CPU use is available for both local models.

Actual latency depends on recording length, model, backend, hardware, and whether models are already warm. The file diagnostic below measures warmup separately from three successive transcription/cleanup runs; no fixed latency is promised.

## Build, package, and verify

```powershell
dotnet restore Speakeasy.sln --locked-mode
dotnet build Speakeasy.sln -c Release --no-restore
dotnet test Speakeasy.sln -c Release --no-build --no-restore
dotnet format whitespace Speakeasy.sln --verify-no-changes --no-restore
./scripts/publish.ps1
```

Publishing creates a self-contained Windows x64 app in `artifacts/publish` and `artifacts/speakeasy-win-x64.zip`. `publish.ps1 -OutputDirectory` and `install.ps1 -InstallDirectory -ConfigDirectory` allow custom locations. Installation does not change your launch-at-login preference.

Use `install.ps1 -DesktopShortcut` to add a desktop launcher. An installed
`config-location.txt` remembers a custom configuration folder even when opening
the executable directly. `--config-dir` overrides it. `speakeasy.exe --quit`
gracefully closes the running instance; the installer uses this when updating.

Tests cover gesture boundaries, cancellation, audio limits, clipboard ownership, provider requests, and owned process termination. Automated tests use fakes or explicit fixtures and do not record the live microphone or type into your applications. The project adopts a small, scoped set of [repository standards](docs/standards.md); see [architecture](docs/architecture.md) for component and lifetime details.

For an explicit diagnostic with a known **16 kHz mono PCM16 WAV** fixture:

```powershell
dotnet run --project src/Speakeasy.App -- --transcribe-file "C:\fixtures\speech.wav" --result-file "C:\fixtures\speakeasy-result.json"
```

This opens no microphone and does not use the clipboard. It uses the configured providers, so a cloud configuration sends the fixture to that provider. The requested result JSON includes timings and transcript text; use a non-sensitive test fixture.

Before relying on a new microphone or shortcut, check the following in an editable test document:

- Hold, speak, release: the pill appears and text arrives at the cursor.
- Double-tap, speak with both hands free, tap once: recording stops and text arrives.
- Press Escape during recording and processing: the target app still receives Escape and no cancelled result appears later.
- Try the apps you normally type in; confirm manual Ctrl+V fallback in an elevated app.
- Copy a test value, enable clipboard restoration, and confirm it returns after paste; a newer copy should be preserved.
- Temporarily lower `maxRecordingSeconds` to check automatic stopping, then restore it; lock or suspend during a test recording and confirm cancellation.

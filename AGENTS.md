# Speakeasy agent guide

Speakeasy is a personal Windows dictation app. Keep it fast, small, and usable
without an account or network connection when local providers are selected.
Read `README.md` for setup and behavior, and `docs/standards.md` for the scoped
adoption of KrishRVH/standards.

## Layout and ownership

- `src/Speakeasy.Core`: configuration, pure gesture decisions, transcription,
  and cleanup providers. No Windows UI dependencies.
- `src/Speakeasy.App`: WinForms startup, tray/dashboard/pill, and the dictation
  controller. `Platform/` owns microphone, keyboard, clipboard, and login APIs.
- `tests/Speakeasy.Tests`: platform-independent decisions and provider contracts.
- `tests/Speakeasy.Platform.Tests`: Windows adapters and controller lifecycle
  tests with fakes. Default repeatable tests must not record live microphone audio,
  install a real global hook, or modify the user's clipboard or focused app.
  Explicitly user-authorized, opt-in interactive acceptance may use the real
  microphone, clipboard, and a target scratch app; report that evidence separately.
- `scripts/`: explicit setup and packaging helpers. Keep ordinary host commands
  direct rather than creating wrappers for every utility.

## Workflow

Use the pinned .NET SDK and native commands from the repository root:

```powershell
dotnet restore Speakeasy.sln --locked-mode
dotnet build Speakeasy.sln -c Release --no-restore
dotnet test Speakeasy.sln -c Release --no-build --no-restore
dotnet format whitespace Speakeasy.sln --verify-no-changes --no-restore
powershell.exe -NoProfile -File tests/SetupDownload.Tests.ps1
```

Use focused project/test filters while editing. Before handoff, run the checks
relevant to the final change and report any checks that could not run. Windows
tests and the app require Windows; pure Core tests can run independently.
Use `dotnet format whitespace Speakeasy.sln --no-restore` to apply formatting.

Package versions live in `Directory.Packages.props`. For an intentional package
change, update that file, run `dotnet restore Speakeasy.sln --force-evaluate`,
and include the resulting project `packages.lock.json` files. Do not hand-edit
lockfiles. Update `global.json` deliberately when changing SDK versions.

## Design and code

- Prefer obvious code, explicit inputs, constructors, and direct calls. Add an
  abstraction only when it removes actual duplication or isolates a tested
  side effect. No service locator, plugin framework, or extra hosted service is
  needed for this desktop utility.
- Keep Core decisions independent of the UI. Keep Win32 interop and native
  resource ownership in the platform adapters, with narrow contracts.
- Keep the UI thread responsive. Own asynchronous work, propagate cancellation,
  and dispose resources. An event boundary may start asynchronous work only
  when its owner handles completion, cancellation, and failure.
- Give recordings and asynchronous requests explicit ownership. Old callbacks
  must never stop a newer recording, overwrite its state, or paste stale text.
- Preserve hold-to-talk, double-tap hands-free, passive Escape, and the hard
  five-minute recording limit. Observe Escape without swallowing it.
- Avoid unnecessary copies of audio buffers and work in keyboard hook callbacks.
  Measure a performance concern before adding caching or background services.
- Respect nullable analysis, warnings-as-errors, and existing formatting. Fix
  causes of diagnostics. A focused interop exception should explain the native
  requirement; do not suppress a whole project to hide an individual issue.
- Keep user errors actionable and local. Never add transcript history, accounts,
  telemetry, automatic audio upload, or silent provider switching.

## Verification and changes

- Reproduce behavior bugs with a regression test when practical. Favor boundary
  tests for cancellation, timers, device failure, process termination, HTTP
  parsing, and clipboard ownership over tests that mirror implementation.
- Use injected fakes or local test fixtures for microphone, network, clipboard,
  and native input behavior. Report the distinction between simulated tests
  and a manual test in an actual target app.
- Make the smallest coherent change. Preserve user edits and avoid speculative
  compatibility layers, unrelated restructuring, and unused configuration.
- Keep secrets in ignored local `.env` files. Do not print keys, recorded audio,
  or transcript text in diagnostic output. Keep model files and generated
  outputs out of source control; regenerate them from their owning tools.
- Read files with targeted `rg` searches. Avoid opening generated `bin/`,
  `obj/`, large models, or package lockfiles wholesale for orientation.
- Use plain, direct prose. When committing, use concise Conventional Commit
  subjects; add a body only to explain a material reason or tradeoff.

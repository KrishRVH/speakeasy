# Speakeasy: handoff to WSL Codex

## Start here

The user wants to pivot Speakeasy to **one Rust codebase for Windows and
macOS**, including the interface and application core. The priorities are
measured performance, responsiveness, a clean and elegant experience, and
smooth, high-frame-rate animation. Separate Swift and C# applications are
not the desired architecture.

The immediate request is to write this handoff so the user can resume in WSL
Codex, discuss the next steps, and involve **Fable** in planning. Continue that
discussion before starting the migration. No Rust implementation has begun.
No Fable or Claude session has been contacted. Discover the tools available in
the WSL session and agree on how to involve Fable; do not assume a particular
CLI, model identifier, or integration is installed.

To resume from a fresh WSL checkout:

```sh
git clone https://github.com/KrishRVH/speakeasy.git
cd speakeasy
```

Then ask the agent: "Read handoff.md and AGENTS.md, inspect the current code and
my standards repo, and continue planning the Rust rewrite with me. Help involve
Fable in reviewing the architecture, performance goals, and implementation plan."

## Workspace and Git state

| Item | Value |
| --- | --- |
| Repository | <https://github.com/KrishRVH/speakeasy.git> |
| Windows checkout | `C:\Users\rvhsp\Documents\Codex\speakeasy` |
| Expected WSL path | `/mnt/c/Users/rvhsp/Documents/Codex/speakeasy` |
| Handoff delivery branch | `main` (the remote default) |
| Base commit | `9f8f3cb216dc865bf7dbfd85afdb71ee5a7acd6a` |
| Standards repository | <https://github.com/KrishRVH/standards.git> |
| Standards Windows checkout | `C:\Users\rvhsp\Documents\Codex\standards` |
| Expected standards WSL path | `/mnt/c/Users/rvhsp/Documents/Codex/standards` |
| Standards revision inspected | `f1909fdd2c55bd23604d0c5042ade09013495847` |

Confirm WSL mount paths and Git status on arrival. Both repositories were
cloned during this session. GitHub authentication initially blocked cloning;
the user resolved it. WSL may use a separate credential configuration.

The local `perf/responsive-status` branch was created for an initial Windows
optimization pass, before the Rust pivot, and contains no new commits. No
application source changes or optimization patches were made. The working
tree was clean before this handoff was added. At the user's request, this
handoff is being delivered as a documentation-only commit on remote `main`,
so a normal fresh clone includes it. No application implementation is being
committed or pushed as part of this handoff.

If moving development to the WSL Linux filesystem for toolchain performance,
preserve this handoff and check for subsequent user changes first. The
Windows checkout remains the reference implementation.

## Decisions and open choices

Agreed direction:

- Windows and macOS first. iPhone and iPad are outside the initial scope.
- One Rust application codebase, with shared UI and application behavior.
- Small platform-specific modules are expected for OS integration. They can
  live in the same Rust workspace; separate product implementations are not
  required.
- Follow the user's standards repository, adapting its actual Rust and
  shared profiles to this desktop application.
- Treat responsiveness, animation, resource use, and reliable dictation as
  design requirements from the beginning.

Discussed recommendations, not final architecture decisions:

- **GPUI is the leading UI prototype candidate**, because the user wants a
  custom, GPU-rendered desktop experience. It has Windows and macOS backends.
  Its official documentation warns that it is pre-1.0 with breaking changes.
  Framework choice remains open pending a technical proof and Fable's review.
- Iced is another Rust UI option worth comparing where useful. Its official
  README also describes it as experimental. Do not label either option
  production-ready for Speakeasy without testing the required behavior.
- Keep existing native inference technology such as whisper.cpp initially.
  A Rust application can own and call a C/C++ inference engine. There is no
  agreed requirement to rewrite speech recognition or model kernels in Rust.
- Decide separately whether inference stays in owned worker processes or
  moves in-process. Measure startup, cancellation, memory, failure isolation,
  and latency before replacing the existing worker design.

Native compilation and a Rust UI do not automatically provide platform-native
widgets, accessibility, or good frame pacing. Test keyboard navigation, IME,
screen readers, focus behavior, scaling, and OS conventions explicitly.
Rust removes GC from the Rust application layer; it does not guarantee faster
inference or lower total memory once models and graphics are included.

## Existing implementation and behavior to preserve

Read [AGENTS.md](AGENTS.md), [README.md](README.md),
[docs/architecture.md](docs/architecture.md), and the relevant tests.

- `src/Speakeasy.Core`: settings, pure gesture decisions, transcription,
  cleanup, and owned inference-worker lifetimes.
- `src/Speakeasy.App`: .NET 10 WinForms app, tray/dashboard/preferences,
  recording pill, controller, and Windows adapters.
- `tests/Speakeasy.Tests`: pure behavior and provider contracts.
- `tests/Speakeasy.Platform.Tests`: Windows adapter/controller tests using
  fakes for microphone, clipboard, and keyboard side effects.

The current app already has important behavior beyond the visible UI:

- Hold-to-talk, double-tap hands-free, and passive Escape cancellation.
- A hard five-minute capture ceiling, enforced independently of UI updates.
- Session ownership and generation checks: late callbacks must never cancel
  newer work, overwrite its status, or insert stale text.
- Microphone failure handling; cancellation on lock, disconnect, and suspend.
- Clipboard restoration that respects a newer copy from another app, plus
  direct insertion and manual-paste fallback with accurate user notices.
- A recording pill that does not steal the target application's focus.
- Explicit local/cloud provider selection, local-only operation when chosen,
  no silent cloud fallback, no transcript history, and no telemetry.
- Warm local inference workers, conservative silence trimming, and cleanup
  failure fallback to the original transcript.

Use these behaviors and their tests as migration acceptance criteria. Port
the contracts into Rust tests rather than blindly translating every C# class.
Retain the old implementation until equivalent behavior has been verified.

## Evidence from this Windows session

The machine initially had no `dotnet` on PATH. Microsoft's installation script
installed the repository-pinned SDK **10.0.100** at
`C:\Users\rvhsp\.dotnet\dotnet.exe`. No repository SDK or package versions
were changed. This is a Windows SDK installation, not a WSL Linux SDK.

Checks completed on the unchanged source:

| Check | Result |
| --- | --- |
| `dotnet restore Speakeasy.sln --locked-mode` | Passed |
| `dotnet build Speakeasy.sln -c Release --no-restore` | Passed; zero warnings and errors |
| `dotnet test Speakeasy.sln -c Release --no-build --no-restore` | Passed; 83 Core tests and 85 Windows tests, 168 total |

The commands used the absolute Windows SDK path and opted out of CLI telemetry.
Whitespace verification, PowerShell setup tests, packaging, and interactive
acceptance were **not run**. No live microphone capture, clipboard insertion,
model download, inference benchmark, or macOS test was performed here.

`docs/THIS_PC.md` and `docs/VERIFIED.md` contain historical evidence from a
different Windows user/machine (`C:\Users\Krish\...`). Do not report those
paths, model installations, hardware, or latency results as this session's
environment or newly verified results.

One source-level performance finding: `DictationController` has a 40 ms UI
timer. Its callback calls `Apply(...)`, which invokes `Changed`, and then
invokes `Changed` again. Typical ticks therefore trigger two status refreshes,
including static dashboard updates. This is approximately 25 timer ticks per
second, not a measured rendering FPS result. No optimization was applied.
An earlier suspicion about model-file checks on every tick was incorrect;
those checks occur when settings are installed.

## Applying the user's standards

The standards catalog was read locally at the revision above, including its
README, `shared/AGENTS.md`, `Rust/AGENTS.md`, `Rust/README.md`, and Rust manifest.
Speakeasy's existing `docs/standards.md` describes an older, scoped C# adoption;
it is not the completed standards baseline for the proposed Rust rewrite.

Relevant Rust principles:

- One owner for application state; pure state/event transitions where useful.
- Explicit inputs, side effects, ownership, cancellation, and failures.
- Message passing across work boundaries; avoid unnecessary shared mutable
  state or abstractions.
- Pinned tools, committed generated lockfiles, narrow dependencies, meaningful
  boundary/property tests, and executable formatting/lint/test gates.
- Follow the catalog's adoption instructions: select and adapt applicable
  files instead of copying the entire catalog or its root maintenance setup.

Account for two concrete adoption issues during planning:

1. The Rust profile forbids first-party unsafe code. OS/FFI work may need a
   narrowly scoped adaptation. Prefer suitable safe interfaces; document any
   required interop boundary and follow the profile's approval rules before
   weakening enforcement. Do not silently loosen the whole workspace.
2. The mutation runner currently uses Linux process-group behavior. It may run
   in WSL for portable code, but native Windows/macOS verification needs an
   explicit, tested workflow. Linux success does not establish OS integration.

Read the actual profiles again when implementing their adoption; the summary
here is not a replacement for their rules.

## Planning work for WSL Codex and Fable

Produce a compact architecture decision and implementation sequence together.
Fable should challenge the proposal with concrete failure modes and evidence.
The useful questions are:

1. Which Rust UI framework best supports a non-activating transparent pill,
   high-DPI/multiple displays, tray/menu bar, keyboard interaction, accessibility,
   and efficient animation on both target OSes?
2. Where do session state, audio capture, inference, rendering, and text
   insertion live, and how do cancellation and ownership cross those boundaries?
3. Which platform adapters and dependencies are necessary? What maintenance,
   packaging, licensing, and standards-adaptation costs do they introduce?
4. What minimum OS versions and CPU architectures are required? Which Mac or
   macOS runner is available for builds and real acceptance testing?
5. What measurements and thresholds establish success on named hardware?
   Include input-to-feedback latency, frame pacing, microphone startup,
   warm/cold inference, insertion latency, idle CPU, RAM/VRAM, and power use.
6. How can the rewrite preserve proven behavior while keeping the repository
   and agent workflows small and understandable?

Proposed performance intent, not measured results or finalized budgets:
display-paced animation targeting 120 Hz where supported (about 8.3 ms per
frame), immediate recording feedback, and no continuous animation when hidden
or idle. Respect reduced-motion preferences. Separate UI performance from model
inference, and evaluate contention when both use the GPU.

The suggested first implementation milestone is one complete path:
**shortcut -> non-activating animated pill -> record -> transcribe -> insert**,
with reliable Escape cancellation. Prove it on Windows and macOS before
committing to the full UI migration. WSL can host planning and portable Rust
work, but is not a substitute for native acceptance on either target.

## Primary references already consulted

- [GPUI](https://gpui.rs/) and its
  [official README](https://github.com/zed-industries/zed/blob/main/crates/gpui/README.md)
  for architecture, platform support, and pre-1.0 status.
- [Zed's Windows implementation](https://zed.dev/blog/zed-for-windows-is-here)
  for DirectX and native Windows integration.
- [GPUI frame-pacing investigation](https://zed.dev/blog/120fps) for why GPU
  rendering alone does not guarantee smooth presentation.
- [Iced](https://github.com/iced-rs/iced) for the alternative shared Rust UI.
- [whisper.cpp](https://github.com/ggml-org/whisper.cpp) for Windows/macOS
  support and Apple Silicon acceleration.

Recheck current versions and APIs before adopting a framework or dependency.

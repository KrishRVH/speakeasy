# Rust rewrite

The implementation lives in `crates/`: core dictation, premium feel, small
modules, and focused checks without ADR, fuzzing, or mutation gates.

Fable 5.1 was consulted through Claude Code with `--effort xhigh` on architecture,
interaction design, and implementation. Its returned metadata confirmed the
model. See [interaction-design.md](interaction-design.md) for the visual direction
and [handoff.md](../handoff.md) for actual verification evidence.

## Scope

One Rust app for Windows 11 x64 and macOS 14+ Apple Silicon. Local whisper.cpp or
explicitly selected NeMo-Speech.cpp/Parakeet provides recognition. No automatic
editing, accounts, cloud fallback, telemetry, or transcript history. Existing C#
remains available during native acceptance.

## Ownership

| Module | Responsibility |
| --- | --- |
| `core` | Gesture deadlines and motion springs; no UI or OS dependencies. |
| `platform` | Native shortcut, lifecycle, windows, insertion, permissions, and owned process containment. |
| `app/runtime.rs` | One session owner: capture, inference, cancellation, settings, and final insertion ordering. |
| `app/audio.rs` | CPAL capture; bounded ring, real amplitude, independent five-minute cap. |
| `app/local_speech.rs` | Warm loopback-only Whisper/Parakeet worker; memory audio upload and bounded response; bounded GPU cancellation recovery with process termination on failure. |
| `app/ports.rs` | Narrow capture, speech, and insertion seams for unattended controller checks. |
| `app/pill.rs` | GPUI Tally pill; frame-driven springs and actual audio levels; no idle animation. |
| `app/shell.rs` | Settings, native file selection, microphone choice, and runtime recovery. |

Every recording and request belongs to a session. Old audio or inference cannot
complete a newer session. Native Escape invalidates the insertion gate immediately
and passes to the focused app. Clipboard mode leaves text available for manual
paste; preserve-clipboard mode uses direct Unicode input. OS input submission
cannot prove an editor accepted it.

Core and app forbid unsafe. Platform owns narrowly documented FFI. Follow the
scoped standards in [standards.md](standards.md), not the catalog's unrelated
business-critical testing machinery.

## Finishing evidence

Use ordinary Cargo formatting, Clippy, and workspace tests. Default checks mock
the microphone, hook events, and clipboard. Keep real public-audio inference
and native demo evidence separate. The demo shares the actual pill and settings
views, with scripted levels and no microphone, global hook, or insertion.

Native packages require Windows SDK/MSVC or macOS/Xcode. The workflow builds both
and uploads packages; local scripts are also available. No CI result should be
claimed until that workflow actually runs. Linux supports UI preview only.

The available Windows delivery is independent of remaining native acceptance:
Mac execution, microphone onset, target app compatibility, sleep/lock and
mixed-DPI behavior, accessibility, and Release frame
pacing/resource measurements. Those are evidence gaps, not reasons to introduce
additional test frameworks or fake performance claims.

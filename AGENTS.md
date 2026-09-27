# Speakeasy agent guide

Speakeasy is a native Rust dictation app for Windows and macOS. Keep it fast,
small, and usable offline with local speech engines. Read `README.md` for setup
and behavior, `docs/architecture.md` for module boundaries, and
`docs/standards.md` for the scoped adoption of `~/devr/standards`.

## Ownership

- `crates/core` owns pure gestures and motion; `crates/app` owns GPUI, session
  state, capture, and the local worker; `crates/platform` owns native adapters.
- Keep unsafe FFI confined to platform with a `SAFETY` explanation at each site.
  Shared atomics are limited to callback control and cancellation during OS
  calls; the session owner receives other changes as messages.
- Own asynchronous work, cancellation, and native resources. Old callbacks must
  never stop a newer recording, overwrite its state, or paste stale text.
- Preserve hold-to-talk, double-tap hands-free, passive Escape, and the hard
  five-minute recording limit. Observe Escape without swallowing it.
- Keep the UI thread responsive and keyboard/audio callbacks small. Measure
  performance before adding caching or background services.
- Prefer explicit inputs and direct calls. Add abstractions for actual duplication
  or a useful side-effect boundary. Maintain the current implementation without
  obsolete configuration aliases or migration scaffolding.
- Keep user errors actionable and local. Never add transcript history, accounts,
  telemetry, automatic audio upload, or silent provider switching.

## Verification

Run native Cargo commands from the root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
```

Regenerate lockfiles through Cargo. Use `#[expect(..., reason = "...")]` for a
necessary lint exception. Keep tests focused on real failure boundaries such as
cancellation, timers, device failure, process termination, and clipboard ownership.
Fuzzing, mutation, coverage targets, and ADR gates are outside the requested scope.

Default tests use fakes or public fixtures. They must not record live microphone
audio, install a global hook, or modify the user's clipboard or focused app.
`--demo` is simulated. Native microphone/input/clipboard acceptance requires
explicit opt-in; report it separately from mocked checks. Before handoff, run
checks relevant to the change and report any unavailable native verification.

## Changes

Preserve user edits and settings. Keep secrets in ignored local `.env` files.
Never print keys, recorded audio, or transcript text in diagnostics. Keep models
and generated artifacts out of Git. Read with targeted `rg` searches; avoid
opening build output, large models, or lockfiles wholesale for orientation.
Use plain prose and concise Conventional Commit subjects.

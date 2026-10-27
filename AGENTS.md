# Speakeasy agent guide

Speakeasy is a native Rust dictation app for Windows, macOS, and Linux. Keep it fast, small, and
usable offline with local speech engines. Before changing behavior or a module interface, find its
entry in the [change and test map](docs/architecture.md#change-and-test-map); README's
[Build and verify](README.md#build-and-verify) section lists packaging and native checks.

## Workflows

Use `mise run` for project workflows; run `mise tasks` and read the task you use. `Cargo.toml`,
`clippy.toml`, and `rustfmt.toml` own Rust policy, and `rust-toolchain.toml` pins the compiler.

- `mise install` installs the pinned tools and may use the network.
- `mise run rust:lint`, `mise run rust:test`, and `mise run rust:doc` verify Rust changes.
- `mise run rust:lock` refreshes Cargo.lock through Cargo after dependency changes and checks that
  packaging can cite every dependency's license.
- `mise run standards` applies safe formatters and autofixes.
- `mise run standards:check` is the final local gate. CI builds and publishes release packages.

Run `git`, `rg`, `tokei`, and focused native commands directly. Extend an existing task before
adding one. Measure the routine gate cost before adding checks. Mutation testing, dependency
advisory gates, secret-scanning gates, fuzzing, coverage targets, and ADR gates are outside this
project's profile; dependency licenses are checked only where packaging must cite them.

## Ownership and interfaces

- `crates/core` (`speakeasy-core`) owns pure gestures and motion; `crates/show` (`speakeasy-show`)
  pure Show composition, no I/O; `crates/platform` (`speakeasy-platform`) native adapters, including
  screen capture and the overlay window; `crates/app` (`speakeasy`) GPUI, the session owner, audio
  capture, the local worker, and Show recording and publishing. Only platform may use unsafe code.
- Explain every unsafe FFI site in a `SAFETY` comment; vendored code follows its upstream policy.
- Give session state one owner. Move cross-thread work over channels; callbacks cannot stop a newer
  recording, overwrite its state, or paste stale text.
- Locks, cells, atomics, and shared ownership need a per-site reason explaining why the value cannot
  belong to the owner or arrive as an event. Immutable Arc snapshots are permitted. Native callback
  control, handle lifetime, and cancellation during blocked OS calls justify narrowly scoped
  synchronization. Thread-local storage is confined to Windows native callbacks without context
  arguments, including owned-window tests; its mutable value needs a site reason.
- Own asynchronous work, cancellation, and native resources. Retain resources until cleanup
  acknowledges before opening replacements. Keep UI and callbacks responsive; measure before
  introducing caches or background services.
- Build deep modules with small interfaces and explicit inputs. Put behavior, state, and tests
  together; a test module over about 250 lines lives in a child file. Add abstractions for actual
  duplication or a useful side-effect seam; remove forwarding wrappers and obsolete re-exports.
- Carry invariants in types, use enums for meaningful alternatives, and make always-present values
  non-optional. Keep internals private and document public contracts, failure modes, and required
  native-thread ownership.
- Handle external failures with explicit errors or events. Use iterators or checked access and
  choose checked, saturating, or wrapping arithmetic according to the domain. Never let a panic
  cross FFI or include private content in errors.
- Production code returns errors instead of asserting; it keeps only `const` assertions and
  `debug_assert!`s on internal contracts that release builds may skip. It writes output through
  fallible I/O, such as `writeln!` on a locked handle, and discards a `#[must_use]` value only with
  `let _ =` under a reasoned `clippy::let_underscore_must_use` expectation: `_ =`, `drop(...)`, and
  empty error arms hide discards from the gate.
- Never use `#[allow]`. A necessary exception uses one lint in a reasoned
  `#[expect(lint, reason = "...")]` on the smallest relevant item, never on a whole module, `impl`,
  or `trait` for a panic lint outside tests. Stale expectations fail. Tests may print, and may
  assert and unwrap invariants they establish.

## App contracts

Preserve hold-to-talk, double-tap hands-free, S, W, or A during a hold for Shows, and the hard
five-minute recording limit. Windows and macOS observe passive Escape without swallowing it; Linux
uses an explicit reserved cancel chord or desktop command binding.

Keep errors actionable and local. Never add dictation history, accounts, telemetry, automatic
upload, or silent provider switching. A Show is the one record Speakeasy keeps: the screen is
captured only after the user asks, frames leave out Speakeasy's windows, and bundles stay private to
the user and expire after seven days unless pinned. Preserve user settings and edits. Keep secrets
in ignored local `.env` files; never print keys, recorded audio, screen pixels, transcript text, or
engine output, including through `Debug`. Models and generated artifacts stay out of Git.

## Tests and review

A test defends behavior, an invariant, a failure, or a trust seam that types and the gate do not
enforce. Do not test trivial getters, constructors, copied config values, or framework behavior.
Show that a new assertion fails for a concrete fault; mechanical moves need no new tests. Use
property tests for generated boundary inputs and commit proptest regression seeds.

Default tests use fakes or public fixtures. They must not record live microphone audio, capture the
screen, install a global hook, or modify the user's clipboard or focused app. `--demo` is simulated.
Native microphone/screen/input/clipboard acceptance requires explicit opt-in; report it separately
from mocked checks. Screen checks compare only owned-window pixels and never write others.
Owned-window rendering checks run on private or native displays without microphone or input access.

Get an independent read-only agent review for nontrivial behavior or architecture changes. Verify
findings with source evidence or a regression test. One writer owns one worktree. Serialize
lockfiles, shared config, formatters, and integration. Each worktree has its own build directory.

Before handoff, run `mise run standards:check` and relevant native checks. Report the checked
revision, behavior proved, results, unavailable native verification, and unresolved findings. Push
verified changes directly to `main`; between releases it keeps the released version and its notes.
Release only when the maintainer asks: choose the bump by [Versions](README.md#versions), set
`[workspace.package].version`, run `mise run rust:lock`, and rewrite `docs/release-notes.md` in the
same push. A new version on `main` publishes with those notes; no other push releases.

## Current repository

Keep current contracts in code, tests, and docs; Git holds history. Replace rather than accumulate:
remove obsolete code, settings, tests, and docs with the change. Comments earn their place like
tests: they explain invariants, non-obvious platform behavior, and trust seams that names, types,
and tests cannot carry, never narration or history. Update the owning page when a contract changes
and state each policy once.

Keep task notes in ignored `.scratch/` and run output in `artifacts/`. Do not commit plans,
handoffs, research, reports, or dated audits. Prefer native toolchain guarantees over duplicate
evidence scaffolding. Read with targeted `rg` searches; `.ignore` hides `vendor/`, so search the
GPUI patch with `rg PATTERN vendor`. Avoid opening build output, large models, or lockfiles
wholesale for orientation. Use plain present-tense prose and concise Conventional Commit subjects.

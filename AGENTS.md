# Speakeasy agent guide

Speakeasy is a native Rust dictation app for Windows, macOS, and Linux. Keep it fast, small, and
usable offline with local speech engines. Read `README.md` for setup and behavior. Before changing
session, shutdown, keyboard, insertion, or module interfaces, read the
[architecture change and test map](docs/architecture.md#change-and-test-map).

## Workflows

Use `mise run` for project workflows; run `mise tasks` and read the task you use. `Cargo.toml`,
`clippy.toml`, and `rustfmt.toml` own Rust policy. The pinned `rust-toolchain.toml` selects the
compiler for mise, rustup, and editors.

- `mise install` installs the pinned tools and may use the network.
- `mise run rust:lint` and `mise run rust:test` verify Rust changes.
- `mise run rust:doc` checks changed public interfaces.
- `mise run rust:lock` refreshes Cargo.lock through Cargo after dependency changes.
- `mise run standards` applies safe formatters and autofixes.
- `mise run standards:check` is the final local gate. CI builds and publishes release packages.

Run `git`, `rg`, `tokei`, and focused native commands directly. Extend an existing task before
adding one. Measure the routine gate cost before adding checks. Mutation testing, dependency
advisory and license enforcement, secret-scanning gates, fuzzing, coverage targets, and ADR gates
are outside this project's profile.

## Ownership and interfaces

- `crates/core` owns pure gestures and motion; `crates/app` owns GPUI, session state, capture, and
  the local worker; `crates/platform` owns native adapters.
- Keep unsafe FFI confined to platform with a `SAFETY` explanation at every site. Core and the app
  forbid unsafe code. Vendored code follows its upstream policy.
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
  together. Add abstractions for actual duplication or a useful side-effect seam; remove forwarding
  wrappers and obsolete re-exports.
- Carry invariants in types, use enums for meaningful alternatives, and make always-present values
  non-optional. Keep internals private and document public contracts, failure modes, and required
  native-thread ownership.
- Handle external failures with explicit errors or events. Use iterators or checked access and
  choose checked, saturating, or wrapping arithmetic according to the domain. Never let a panic
  cross FFI or include private content in errors.
- Never use `#[allow]`. A necessary exception uses one lint in a reasoned
  `#[expect(lint, reason = "...")]` on the smallest relevant item. Stale expectations fail. Tests
  may assert and unwrap invariants they establish.

## App contracts

Preserve hold-to-talk, double-tap hands-free, and the hard five-minute recording limit. Windows and
macOS observe passive Escape without swallowing it; Linux uses an explicit reserved cancel chord or
desktop command binding.

Keep errors actionable and local. Never add transcript history, accounts, telemetry, automatic audio
upload, or silent provider switching. Preserve user settings and edits. Keep secrets in ignored
local `.env` files; never print keys, recorded audio, transcript text, or engine output. Models and
generated artifacts stay out of Git.

## Tests and review

A test defends behavior, an invariant, a failure, or a trust seam that types and the gate do not
enforce. Do not test trivial getters, constructors, copied config values, or framework behavior.
Show that a new assertion fails for a concrete fault; mechanical moves need no new tests. Use
property tests for generated boundary inputs and commit proptest regression seeds.

Default tests use fakes or public fixtures. They must not record live microphone audio, install a
global hook, or modify the user's clipboard or focused app. `--demo` is simulated. Native
microphone/input/clipboard acceptance requires explicit opt-in; report it separately from mocked
checks. Owned-window rendering checks run on private or native displays without microphone or input
access.

Get an independent read-only agent review for nontrivial behavior or architecture changes. Verify
findings with source evidence or a regression test. One writer owns one worktree; reviewers stay
read-only. Serialize lockfiles, shared config, formatters, and integration. Each worktree has its
own build directory.

Before handoff, run `mise run standards:check` and relevant native checks. Report the checked
revision, behavior proved, results, unavailable native verification, and unresolved findings. Gates
block and humans merge.

## Current repository

Keep current contracts in code, tests, and docs; Git holds history. Replace rather than accumulate:
remove obsolete code, settings, tests, and docs with the change. Comments earn their place like
tests: they explain invariants, non-obvious platform behavior, and trust seams that names, types,
and tests cannot carry, never narration or history. Update the owning page when a contract changes
and state each policy once.

Keep task notes in ignored `.scratch/` and run output in `artifacts/`. Do not commit plans,
handoffs, research, reports, or dated audits. Prefer native toolchain guarantees over duplicate
evidence scaffolding. Read with targeted `rg` searches; avoid opening build output, large models, or
lockfiles wholesale for orientation. Use plain present-tense prose and concise Conventional Commit
subjects.

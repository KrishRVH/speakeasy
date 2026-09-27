# Standards adopted for Speakeasy

## Scope

Reviewed `~/devr/standards` at `f1909fdd2c55bd23604d0c5042ade09013495847`,
including its adoption guide and shared/Rust profiles. The user explicitly
requested a lean adoption for this personal app.

The Rust workspace keeps explicit ownership, cancellation, small interfaces,
pinned Rust/GPUI versions, and a generated Cargo lockfile. `core` and `app` forbid
unsafe code. Native FFI is confined to `platform`, with documented unsafe blocks
and `unsafe_op_in_unsafe_fn` denied. This is the necessary adaptation for native
keyboard, window, clipboard, and process ownership. Callback control atomics
avoid blocking the audio thread and let Escape invalidate a pending OS paste;
session state otherwise belongs to the dictation owner. Clippy rejects unwraps,
panic placeholders, debug macros, and bare allow attributes in first-party code.

Use `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked --
-D warnings`, and `cargo test --workspace --locked`.
The focused tests cover gestures, interrupted motion, cancellation at insertion,
the session owner with mocked capture/inference/insertion, and conservative
PCM edge trimming. A controller boundary also checks that stopping input does
not wait for native cleanup; another checks that failed GPU recovery stops its
worker before replacement while allowing a new recording. A separate opt-in
provider test uses public fixture audio and checks worker shutdown.
OS acceptance remains separate from these simulated checks. The user's requested
scope does not add ADR gates, fuzzing, mutation tooling, coverage floors, or a
framework around ordinary Cargo commands.

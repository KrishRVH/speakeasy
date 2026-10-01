# Speakeasy GPUI patch

This directory contains the published [GPUI 0.2.2 crate](https://crates.io/crates/gpui/0.2.2)
under its original Apache-2.0 license. The published archive SHA-256 is
`979b45cfa6ec723b6f42330915a1b3769b930d02b2d505f9697f8ca602bee707`.
`Cargo.toml.orig`, `.cargo_vcs_info.json`, `README.md`, and `LICENSE-APACHE`
preserve upstream provenance. Registry cache and checksum markers are omitted.

The source patches cover native windows, frame scheduling, and path resources:

- `src/platform/linux/x11/window.rs` backports
  [zed commit 608185be4e0cc32ad925074185b5821af8aa2da9](https://github.com/zed-industries/zed/commit/608185be4e0cc32ad925074185b5821af8aa2da9)
  ([PR #50768](https://github.com/zed-industries/zed/pull/50768)) to implement
  `HasWindowHandle` and `HasDisplayHandle` for `X11Window`, with local SAFETY
  comments. The published implementations panic when Speakeasy configures its
  Linux pill. The handles borrow the existing owned window ID, XCB connection,
  and screen index.
- `src/window.rs` honors `WindowOptions::show` before mapping. This lets the
  hidden Linux pill receive its nonactivating hints and empty input shape
  before its first map.
- `src/platform/linux/x11/client.rs` drains queued events before startup and
  after foreground dispatch. Checked XCB requests can buffer MapNotify and
  Expose while reading replies; those events must start the refresh timer
  even when the socket has no unread bytes. Connection errors exit the event
  loop and refresh timers, including when every window is hidden. Retrying a
  dead, continuously readable socket creates an unbounded CPU loop.
- `src/platform/windows/vsync.rs` and `vsync/interval.rs` convert timing
  fractions without truncating the frequency, and reject invalid intervals
  through the existing 60 Hz fallback. Valid reduced refresh ratios must not
  divide by zero or terminate the redraw thread. The portable integration
  test in `crates/app/tests/gpui_timing.rs` exercises this production helper
  on every host.
- `src/platform/mac/display_link.rs` and `mac/window.rs` backport
  [zed commit 96ce8f2a05f8912851e5d20d808fe21f4134bd45](https://github.com/zed-industries/zed/commit/96ce8f2a05f8912851e5d20d808fe21f4134bd45)
  ([PR #60696](https://github.com/zed-industries/zed/pull/60696)). One registry
  entry retains a CoreVideo link per encountered display ID. Each window's
  dispatch source is retained across hide/show and layer callbacks.
  Unsubscribe precedes source cancellation and release; CoreVideo creation,
  start, and stop happen outside the registry lock. Retaining the links for
  the process lifetime avoids a CoreVideo release race. `Cargo.toml` adds the
  pinned dispatch2 RAII dependency with only its `std` feature. Portable
  production-registry tests use `display_link_test_support.rs` and
  `crates/app/tests/gpui_macos_lifecycle.rs`; they call no native API.
- `src/platform/mac/metal_renderer.rs`, `windows/directx_renderer.rs`, and
  `blade/blade_renderer.rs` realize path resolve/MSAA textures only for a
  nonempty path scene. Resize and format changes invalidate those resources;
  existing GPU ownership, synchronization, and sample counts are preserved.
  DirectX also clears bound shader-resource references during resize.
  Speakeasy's static Settings diamonds use an embedded SVG atlas tile, so
  Settings does not require full-window path scratch textures. The pill
  retains its existing path rendering.

- `src/taffy.rs` gives grid fractions explicit `f32` literals for the pinned
  nightly's stricter float fallback diagnostics. `platform/linux/platform.rs`
  compiles its Wayland-only descriptor reader and imports only with Wayland.
  The X11 build excludes that Wayland-only code.
- `Cargo.toml` updates the macOS Core Foundation requirement to `0.10.1`.
  The matching media requirement is documented in the
  [media compatibility patch](../gpui_media/README.speakeasy.md).

The Linux native pill tests and `scripts/check_demo_linux.py` exercise real
windows in a private X server without microphone, shortcut, or clipboard access.
They verify initial visibility, delayed rendering, and hidden display disconnect.
The opt-in `check_native_rendering` example runs on the process main thread and
exercises SVG and path scenes, resizing, repeated visibility, and disposal.
Linux verifies actual SVG and path pixels; Windows and macOS verify native
render callbacks without pixel assertions.

The root `[patch.crates-io]` selects this directory, which is excluded from the
workspace. Remove the override and this directory when a compatible published
GPUI release covers these native-handle, visibility, event-draining, timing,
frame-source lifetime, and path-resource contracts and passes the regression
checks. Remove the portable dependency integration tests along with the override.

The app's forced-exit fallback retains its instance lock in the future returned
from `on_app_quit`. Re-check `App::shutdown` ordering on GPUI updates: observers
must run before window disposal/Settings saves, and returned futures must be
polled after window disposal. Settings must remain owned by its window only.
The standalone GPUI manifest carries the same media override as the root so
its Cargo-regenerated lockfile remains resolvable.

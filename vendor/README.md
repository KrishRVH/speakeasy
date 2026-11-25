# Vendored crates

Each directory holds a crates.io release archive, unpacked as published apart from the patches
below. The root `Cargo.toml` selects them through `[patch.crates-io]` and excludes them from the
workspace. They keep their upstream licenses, style, and comment policy. A `Speakeasy patch:`
comment marks each patched item, and new source files say so in their first line.

| Directory           | Release                                                                       | Archive SHA-256                                                    |
| ------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| `accesskit_windows` | [accesskit_windows 0.34.0](https://crates.io/crates/accesskit_windows/0.34.0) | `106c2b961215864d1c2e703ee63269c25c4e80a577ffb2c1017b9c17dcdf83a1` |
| `gpui-pre`          | [gpui-pre 0.3.13](https://crates.io/crates/gpui-pre/0.3.13)                   | `e17ece47c15edeca5889096947ddc73a86fb29318f811a8c7a64a8ddd6364247` |
| `gpui-pre-apple`    | [gpui-pre-apple 0.3.13](https://crates.io/crates/gpui-pre-apple/0.3.13)       | `7b3a742d990b38f1b3c2d3a117df8cddf7e9b2e0757017cdb2dc9897a6fd3f07` |
| `gpui-pre-linux`    | [gpui-pre-linux 0.3.13](https://crates.io/crates/gpui-pre-linux/0.3.13)       | `61235d40538557c44d3dff166b9984cd1d94824f71e6695f19bcc55b5dee7fa0` |
| `gpui-pre-macos`    | [gpui-pre-macos 0.3.13](https://crates.io/crates/gpui-pre-macos/0.3.13)       | `5e48bf4ea6371e1c735c46163e57fc950f1c23999cb16191ead567cc82517da4` |
| `gpui-pre-wgpu`     | [gpui-pre-wgpu 0.3.13](https://crates.io/crates/gpui-pre-wgpu/0.3.13)         | `7d1505797c4019f72e65ad5a23c585a3acbe6a574531794e5690d083ee19d3c8` |
| `gpui-pre-windows`  | [gpui-pre-windows 0.3.13](https://crates.io/crates/gpui-pre-windows/0.3.13)   | `1831c6841cd0928c5a7e6ac8c7610cb3eed6e0a2257f62bffab9a12184b8f4f3` |

The hashes match the `cksum` crates.io publishes for each release. To see every change, unpack
the archive (`curl -sL https://static.crates.io/crates/NAME/NAME-VERSION.crate | tar xz`) and
`diff -r` it against the directory.

## Updating

Replace a directory with the new archive, then reapply each patch below only if the new release
still has the problem it fixes, and rerun the check that patch names. Update the table, the
workspace requirements in the root `Cargo.toml`, and this list together. On every GPUI update,
recheck the GPUI contracts in [native contracts](../docs/native-contracts.md#gpui-and-vendored-adapters):
app liveness for `AsyncApp`, and `App::shutdown` ordering. Run `mise run native:pill` on Windows and
Linux: it fails when GPUI's frame pacing throttles the pill.

## gpui-pre

- `src/window.rs` maps a window only when `WindowOptions::show` asks for it and returns a failed
  map as an error. Upstream maps every window and panics on failure. The hidden X11 pill must
  receive its nonactivating hints and empty input shape before its first map;
  `idle_native_pill_starts_unmapped_and_can_show_and_hide` fails without this.
- `src/app.rs` and `src/platform.rs` add the macOS-only `Application::on_quit_requested` and its
  platform hook. The handler runs once, outside any application borrow, and termination waits for,
  or defers to, its `App::quit`. The Mac quit acceptance in CI (`mac_updates`) covers it.
- `src/window.rs` and `src/platform.rs` add `Window::set_window_visible` and its platform hook,
  which hides a window without closing it. X11 and Wayland implement it, and Speakeasy shows and
  hides every Linux window through it. Upstream has no way to hide a Wayland window, so without it
  closing Settings to the tray drops its view and stops its downloads, and the idle layer-shell pill
  stays mapped. `idle_native_pill_starts_unmapped_and_can_show_and_hide` covers X11; the Settings
  close, idle pill, and reopening rows of
  [Linux native acceptance](../docs/linux-implementation.md#native-acceptance-still-required) cover
  Wayland.
- `Cargo.toml` builds `image` with only the formats GPUI's `ImageFormat` names, dropping the dds,
  exr, ff, hdr, qoi, and tga decoders and image's rayon feature. Cargo unifies features, so the
  `gpui-pre-apple`, `gpui-pre-macos`, `gpui-pre-wgpu`, and `gpui-pre-windows` manifests carry the
  same patch; `cargo tree -e features -i image --target all` lists only those formats. This saves
  size; it is not a contract.

## gpui-pre-apple

- `src/dispatcher.rs` marks foreground tasks running on the main queue, so macOS termination can
  tell when a nested run loop could not drain that serial queue.

## gpui-pre-macos

- `src/platform.rs` answers `applicationShouldTerminate:` by handing the first native request to
  the app's `on_quit_requested` handler and replying `NSTerminateLater`. `App::quit` then replies
  YES. A request arriving inside a main-queue task or an application borrow is canceled, because
  AppKit's nested run loop could not drain the queue the app's shutdown needs; the drain still
  runs and quits afterward. `terminate:` is ignored while a request is pending. Contract:
  [native contracts](../docs/native-contracts.md#gpui-and-vendored-adapters).

## gpui-pre-linux

- `src/linux/platform.rs`, `src/linux.rs`, and `Cargo.toml` stub the credential methods (writes
  fail, reads find nothing, deletes succeed) and drop system notifications, which removes the oo7
  keyring and notify-rust clients and about 30 crates. Speakeasy does not use GPUI's credential
  store or notifications. `src/linux/system_notifications.rs` is deleted.
- `src/linux/x11/window.rs` and `Cargo.toml` implement `set_input_region` with an XShape input
  shape, as Wayland's input region already does; upstream X11 ignores it. The X11 pill passes
  through every click; `idle_native_pill_starts_unmapped_and_can_show_and_hide` fails without this.
- `src/linux/x11/window.rs` implements `PlatformWindow::set_visible` with MapWindow and UnmapWindow
  on GPUI's own connection; a managed window also sends the ICCCM withdrawal event to the root,
  so a window manager forgets it even when it was already unmapped.
  `idle_native_pill_starts_unmapped_and_can_show_and_hide` fails without the map and unmap, and
  `native_pill_app_withdraws_a_managed_window_that_is_already_unmapped`, which listens on the root
  as a window manager and a taskbar do, fails without the withdrawal event or its
  SubstructureNotify mask.
- `src/linux/wayland/window.rs` and `src/linux/wayland/client.rs` implement
  `PlatformWindow::set_visible`. A toplevel or layer
  surface opened without `show` has no role objects until it is shown. Hiding one does as GTK and
  Qt do: it destroys its role objects, the decoration, toplevel, and xdg_surface or the layer
  surface, and commits a null buffer, so the compositor keeps nothing of it and the client
  discards a configure sent to those objects instead of acknowledging it. Showing gives the same
  surface new role objects with the saved title, app ID, size limits, maximized or fullscreen
  state, and decoration request, or the layer surface's options, which runtime exclusive-zone and
  edge changes keep current, on the output the compositor chooses; the window draws on that new
  role's first configure. Each configure or close, from the xdg_surface, toplevel, decoration, or
  layer surface, carries the role object that sent it and is handled only while that role is
  current, checked again after every observer callback, since a callback can hide and show the
  window. A withdrawn window never draws, reports itself hidden, and takes decoration requests
  without a decoration object to send them to; a popup cannot open on one. Floating windows,
  dialogs, and popups open shown and ignore it. Check it on sway with two outputs: the
  `--demo-tray` pill shows its tray hint on the output focused when Settings closes, and under
  `WAYLAND_DEBUG=client` hiding and showing Settings while it resizes logs configures for the old
  role as discarded and never ends the connection. Keeping the layer surface leaves the hint on the
  output where the pill first showed, and acknowledging such a configure ends the connection with
  `wrong configure serial`.

## gpui-pre-windows

- `src/platform.rs` stops the `VSyncProvider` thread while no window is shown. Upstream wakes it
  every vblank for the app's whole life: about 950 times a second at 400 Hz with Speakeasy hidden
  in the tray. The thread waits for WinEvent hooks on this process's windows and wakes when one is
  shown or restored, when GPUI tracks a window created visible, when vsync stops, and once a
  second to notice a lost device. `mise run native:pill` fails if a shown
  pill stays without frames.
- `src/vsync.rs` and the new `src/vsync/interval.rs` convert DWM timing without truncating the
  QPC frequency and reject zero or implausible intervals through the 60 Hz fallback. Upstream
  divides by zero when it falls back to a refresh ratio such as 60/1, which ends the redraw
  thread. [gpui_timing.rs](../crates/platform/tests/gpui_timing.rs) runs the helper on every host.
- The new `src/accessibility.rs`, with hunks in `window.rs` and `events.rs`, ties UI Automation to
  committed visibility. Hide publishes an empty root-only tree before the window disappears and
  minimize as it minimizes; exposure resumes only after `WM_WINDOWPOSCHANGED` commits visibility,
  through a forced frame. Windows start redacted until presented. `WM_DESTROY` publishes the empty
  tree while the HWND is still valid and then disconnects the window from UI Automation. Events are
  raised outside the window-state borrow because they can reenter through `WM_GETOBJECT`.
  `scripts/check-demo-windows.ps1` checks these natively.

## accesskit_windows

- `Cargo.toml` declares `(MIT OR Apache-2.0) AND BSD-3-Clause`, so package notices cite the
  Chromium-derived code. The license texts come from
  [the AccessKit repository](https://github.com/AccessKit/accesskit/tree/1bbcf100942bac96c2c3a4a91cb67b0b20201a24).
- `src/text.rs`: range actions return a `Result`, so a missing node reports
  `UIA_E_ELEMENTNOTAVAILABLE` instead of panicking across COM. `GetBoundingRectangles` uses
  clipped geometry. Each range keeps the provider that created it and reports
  `UIA_E_ELEMENTNOTAVAILABLE` once that provider retires, so a range retained across a hide does
  not resolve against the restored tree. `ScrollIntoView` sends the range as a `SetTextSelection`
  payload whose focus is the aligned end, targeted at the run holding that end; upstream sends only
  a `ScrollHint`, which cannot say which part of the run to reveal.
- The new `src/text/geometry.rs` clips range rectangles and visible character spans to every
  clipping ancestor. Each line yields one continuous span even with fractional advances, and
  inverted or empty clips expose nothing.
- `src/node.rs`: `GetVisibleRanges` returns the clipped visible spans instead of null, and text
  ranges are created with their provider. A provider whose node was removed returns
  `UIA_E_ELEMENTNOTAVAILABLE` even when a later tree reuses the node id; `src/context.rs` adds the
  identity check. GPUI derives node ids from element ids, so without it a stale provider revives.
- `src/adapter.rs` adds `Adapter::disconnect`, which publishes a final tree, raises its events, and
  returns a null provider for the window so UI Automation drops its entries for the HWND.
- `src/adapter/tests.rs` (test-only) and the tests beside `geometry.rs` run on Windows through
  `mise run rust:test`.

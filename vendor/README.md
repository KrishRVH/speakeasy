# Vendored crates

Each directory holds a crates.io release archive, unpacked as published apart from the patches
below. The root `Cargo.toml` selects them through `[patch.crates-io]` and excludes them from the
workspace. They keep their upstream licenses, style, and comment policy. A `Speakeasy patch:`
comment marks each patched item, and new source files say so in their first line.

| Directory           | Release                                                                       | Archive SHA-256                                                    |
| ------------------- | ----------------------------------------------------------------------------- | ------------------------------------------------------------------ |
| `accesskit_windows` | [accesskit_windows 0.34.0](https://crates.io/crates/accesskit_windows/0.34.0) | `106c2b961215864d1c2e703ee63269c25c4e80a577ffb2c1017b9c17dcdf83a1` |
| `gpui-pre`          | [gpui-pre 0.3.11](https://crates.io/crates/gpui-pre/0.3.11)                   | `9303c771abf2acff2a96d706f69d9a82088c4770173a07cce88f0002431d887d` |
| `gpui-pre-apple`    | [gpui-pre-apple 0.3.11](https://crates.io/crates/gpui-pre-apple/0.3.11)       | `8c0bea8ffa563d3a1ada533e148bb55be7fb873c178979e889a3d1204f8e1b91` |
| `gpui-pre-linux`    | [gpui-pre-linux 0.3.11](https://crates.io/crates/gpui-pre-linux/0.3.11)       | `79cac5cbd3485c1e3db8eafd5dc69019188f5cb0a39846946af772efa386f624` |
| `gpui-pre-macos`    | [gpui-pre-macos 0.3.11](https://crates.io/crates/gpui-pre-macos/0.3.11)       | `d7410cfdaa1fa55a414346cc492d18ce340e3a8b6d0ac3213b984ed690c82b92` |
| `gpui-pre-wgpu`     | [gpui-pre-wgpu 0.3.11](https://crates.io/crates/gpui-pre-wgpu/0.3.11)         | `211cf352de1a0b0c9bea3f105df5572ce1d2c899389b924a888e5af36b50f93d` |
| `gpui-pre-windows`  | [gpui-pre-windows 0.3.11](https://crates.io/crates/gpui-pre-windows/0.3.11)   | `5141a3db59bab761a16ab19529fe4117a86e91922e8fc00af6dfc4829cb1744d` |

The hashes match the `cksum` crates.io publishes for each release. To see every change, unpack
the archive (`curl -sL https://static.crates.io/crates/NAME/NAME-VERSION.crate | tar xz`) and
`diff -r` it against the directory.

## Updating

Replace a directory with the new archive, then reapply each patch below only if the new release
still has the problem it fixes, and rerun the check that patch names. Update the table, the
workspace requirements in the root `Cargo.toml`, and this list together. On every GPUI update,
recheck the GPUI contracts in [native contracts](../docs/native-contracts.md#gpui-and-vendored-adapters):
app liveness for `AsyncApp`, and `App::shutdown` ordering.

## gpui-pre

- `src/window.rs` maps a window only when `WindowOptions::show` asks for it and returns a failed
  map as an error. Upstream maps every window and panics on failure. The hidden Linux pill must
  receive its nonactivating hints and empty input shape before its first map;
  `idle_native_pill_starts_unmapped_and_can_show_and_hide` fails without this.
- `src/app.rs` and `src/platform.rs` add the macOS-only `Application::on_quit_requested` and its
  platform hook. The handler runs once, outside any application borrow, and termination waits for,
  or defers to, its `App::quit`. The Mac quit acceptance in CI (`mac_updates`) covers it.
- `Cargo.toml` builds `image` with only the formats GPUI's `ImageFormat` names, dropping the dds,
  exr, ff, hdr, qoi, and tga decoders and rayon. This saves size; it is not a contract.

## gpui-pre-apple

- `src/dispatcher.rs` marks foreground tasks running on the main queue, so macOS termination can
  tell when a nested run loop could not drain that serial queue.
- `src/metal_renderer.rs` allocates the path resolve and MSAA textures only for frames whose scene
  has paths, sized to that frame, and releases them on the first frame without paths. A resize no
  longer allocates them. Metal command buffers retain their attachments, so frames in flight keep
  what they use. CI builds it; only Linux samples path pixels.

## gpui-pre-macos

- `src/platform.rs` answers `applicationShouldTerminate:` by handing the first native request to
  the app's `on_quit_requested` handler and replying `NSTerminateLater`. `App::quit` then replies
  YES. A request arriving inside a main-queue task or an application borrow is canceled, because
  AppKit's nested run loop could not drain the queue the app's shutdown needs; the drain still
  runs and quits afterward. `terminate:` is ignored while a request is pending. Contract:
  [architecture](../docs/architecture.md#change-and-test-map), "Pause, reconfigure, quit, and
  saves".

## gpui-pre-linux

- `src/linux/platform.rs`, `src/linux.rs`, and `Cargo.toml` stub the credential methods (writes
  fail, reads find nothing, deletes succeed) and drop system notifications, which removes the oo7
  keyring and notify-rust clients and about 30 crates. Speakeasy stores no credentials and posts
  no notifications. `src/linux/system_notifications.rs` is deleted.
- `src/linux/platform.rs` compiles its pipe reader only with Wayland, so the X11-only build has no
  dead code.

## gpui-pre-wgpu

- `src/wgpu_renderer.rs` ensures the path textures only for frames that draw paths and releases
  them otherwise; upstream ensured them every frame and already released them on resize. wgpu
  keeps resources alive for pending submissions. `check_native_rendering` alternates path and
  pathless frames and samples their pixels on Linux.

## gpui-pre-windows

- `src/vsync.rs` and the new `src/vsync/interval.rs` convert DWM timing without truncating the
  QPC frequency and reject zero or implausible intervals through the 60 Hz fallback. Upstream
  divides by zero when it falls back to a refresh ratio such as 60/1, which ends the redraw
  thread. [gpui_timing.rs](../crates/platform/tests/gpui_timing.rs) runs the helper on every host.
- `src/directx_renderer.rs` keeps the path textures and views in one optional value that the first
  path frame allocates and a pathless frame or resize releases. Release unbinds shader-resource
  slot 0 so the device context cannot keep the textures alive. D3D11 defers destruction until the
  GPU finishes with them.
- The new `src/accessibility.rs`, with hunks in `window.rs` and `events.rs`, ties UI Automation to
  committed visibility. Hide publishes an empty root-only tree before the window disappears and minimize as
  it minimizes; exposure resumes only
  after `WM_WINDOWPOSCHANGED` commits visibility, through a forced frame. Windows start redacted
  until presented. `WM_DESTROY` publishes the empty tree while the HWND is still valid. Events
  are raised outside the window-state borrow because they can reenter through `WM_GETOBJECT`.
  `scripts/check-demo-windows.ps1` checks these natively.

## accesskit_windows

- `Cargo.toml` declares `(MIT OR Apache-2.0) AND BSD-3-Clause`, so package notices cite the
  Chromium-derived code. The license texts come from
  [the AccessKit repository](https://github.com/AccessKit/accesskit/tree/1bbcf100942bac96c2c3a4a91cb67b0b20201a24).
- `src/text.rs`: range actions return a `Result`, so a missing node reports
  `UIA_E_ELEMENTNOTAVAILABLE` instead of panicking across COM. `GetBoundingRectangles` uses
  clipped geometry. `ScrollIntoView` sends `SetScrollOffset` to the range's clipping container;
  upstream sends a `ScrollHint` to a text run, which GPUI does not handle.
- The new `src/text/geometry.rs` clips range rectangles and visible character spans to every
  clipping ancestor. Each line yields one continuous span even with fractional advances, and
  inverted or empty clips expose nothing.
- The new `src/text/scroll.rs` finds the nearest ancestor that clips and supports
  `SetScrollOffset`, then computes the smallest horizontal offset that reveals the range's start
  or end. Empty, inverted, unreachable, or non-finite viewports are unsupported.
- `src/node.rs`: `GetVisibleRanges` returns the clipped visible spans instead of null. A provider
  whose node was removed returns `UIA_E_ELEMENTNOTAVAILABLE` even when a later tree reuses the
  node id; `src/context.rs` adds the identity check. GPUI derives node ids from element ids, so
  without it a stale provider revives.
- `src/adapter/tests.rs` (test-only) and the tests beside `geometry.rs` and `scroll.rs` run on
  Windows through `mise run rust:test`.

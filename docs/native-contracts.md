# Native contracts

Use the [change and test map](architecture.md#change-and-test-map) to find the adapter. This page
records native thread, callback, and resource ownership that spans files. Rules one function, type,
or unsafe block must keep live in comments at that site.

## Owned threads and insertion authority

An [OwnedThread](../crates/platform/src/owned_thread.rs)'s full join, including thread-local
destructors, is the replacement boundary: owners open a replacement only after it.

On Windows and macOS the insertion thread owns the clipboard snapshot; see
[commit](../crates/platform/src/insertion/commit.rs). Adapters keep synthetic input buffers alive
through submission and split direct text at valid encoding boundaries.

## Child process containment

Terminate a [process group](../crates/platform/src/process.rs) before reaping its leader;
[local speech](../crates/app/src/local_speech.rs) orders every stop that way.

## Linux desktop ownership

Keep libei pointers on the desktop thread; see the
[libei adapter](../crates/platform/src/linux/ei.rs). Shortcut events come from a separate stream.

## Mac AppKit ownership

Use AppKit windows and status items on the main thread. Adopt Create-rule Core Foundation references
once and keep Get-rule borrows tied to their owner. See the
[macOS adapter](../crates/platform/src/macos.rs).

## Movie presentation and checkpoint lease

[Recording](../crates/platform/src/movie/recording.rs) and
[encode](../crates/platform/src/movie/encode.rs) own a fixed canvas, a 30 Hz presentation clock, the
native codec, and the checkpoint lease, which outlives the codec.

## GPUI and vendored adapters

Foreground tasks use `AsyncApp` from `cx.spawn` within the application loop. During forced shutdown,
GPUI disposes windows before polling quit futures for up to 200 ms;
[shutdown](../crates/app/src/shell/shutdown.rs) drains and releases app state around it. See the
[vendor index](../vendor/README.md).

On macOS AppKit's deferred termination loop cannot drain GPUI's serial main queue. Defer only
outside a foreground task and an application borrow. Release the termination lock before calling the
app handler or AppKit.

Windows UI Automation can reenter via `WM_GETOBJECT`; raise events and return providers outside
window-state borrows. Hide and minimize withdraw the tree, committed visibility restores it, and
destruction withdraws it before the HWND becomes invalid. A text range retires with its provider.
Queue actions outside the tree lock. Use physical client coordinates and pass the requested text
range to the field, which resolves it against its painted layout. See
[GPUI Windows accessibility](../vendor/gpui-pre-windows/src/accessibility.rs) and
[AccessKit text](../vendor/accesskit_windows/src/text.rs).

# Native contracts

Use the [change and test map](architecture.md#change-and-test-map) to find the adapter. This page
records thread, callback, and resource lifetimes that cross Rust and native APIs.

## Owned threads and insertion authority

[OwnedThread](../crates/platform/src/owned_thread.rs) closes an exit lane when its body returns or
unwinds. Async join moves the owner to Tokio's blocking pool for the OS join, including thread-local
destructors. Drop joins synchronously. `StoppableThread` closes its stop lane first. Keep the full
join as the replacement boundary. The Show recorder sends its terminal outcome before thread exit,
with an unwind fallback. The instance listener creates and drops its Tokio executor on the server
thread. See [Show recorder](../crates/app/src/show/recorder.rs) and
[instance](../crates/app/src/instance.rs).

[MonitorControl](../crates/platform/src/monitor.rs) synchronizes readiness, a live native wake
handle, and stop. Stop before publication must still take effect. Retirement removes the wake handle
before later callers can use it.

[Input](../crates/platform/src/input.rs) shares one generation-tagged commit gate with native
cancellation. Check it after blocking clipboard acquisition and at the irreversible effect. Keep
synthetic buffers alive through submission and mark own input so the hook ignores it. Report partial
submission without retrying it. Direct text splits at valid encoding boundaries.

On Windows and macOS the insertion thread owns the clipboard snapshot. It writes transient or
history-exclusion marks with the dictation and restores after paste only while its sequence or
change count still matches. The reply can precede restore, but the next request waits for restore.
Cancellation before submission attempts an immediate restore or clears its own copy if the clipboard
was empty. A failed write skips restoration. Return-to-send spends the permit on the text and skips
delayed restore. The later Return requires the same foreground target and a released shortcut. Only
the main thread may query macOS secure input, including for continuation spacing. See
[commit](../crates/platform/src/insertion/commit.rs) and
[continuation](../crates/platform/src/insertion/continuation.rs).

## Windows handles, hooks, and dialogs

[Windows](../crates/platform/src/windows.rs) adopts fresh kernel handles requiring `CloseHandle`
through `take_handle`. HWNDs are borrowed. Do presentation and subclass work on the thread that owns
the HWND. Remove subclasses on native destruction and reapply pill and overlay bounds after a DPI
transition that can move them synchronously. See
[presentation](../crates/platform/src/windows/presentation.rs).

The context-free low-level keyboard hook uses thread-local policy on its message-loop thread. Only a
nonnegative hook code provides a valid `KBDLLHOOKSTRUCT`; its borrow lasts through the callback.
Catch unwinds at the ABI. Remove the hook and window registrations before releasing callback state.
The monitor and thread share the stop event until both finish. Swallowed F13 transitions establish
its hold because Windows updates async key state after the hook. Ctrl and Win reconcile against
physical state. See [shared key policy](../crates/platform/src/keyboard.rs).

The [file dialog](../crates/platform/src/windows/dialog.rs) owns its STA and buffers until
`GetOpenFileNameW` returns. Dropping the receiver leaves the modal dialog to finish and discards its
answer. The [WSL lookup](../crates/app/src/show/wsl.rs) is also intentionally detached. Windows
redirector reads can block while WSL starts, and callers bound their own wait.

## Child process containment

[Process group](../crates/platform/src/process.rs) attaches a freshly spawned Windows child to a
private kill-on-close job. Spawn Unix children with `process_group(0)`. Observe exit via
`waitid(WNOWAIT)`, terminate the group before reaping its leader, and consume the stored leader
before signaling so another drop cannot signal a reused PID.

## Linux desktop and transfer ownership

Keep libei pointers on the desktop thread. `RTLD_NODELETE` preserves copied C function pointers.
Terminate variadic capabilities with integer zero. Drop event, device, and ping references before
the context. libei owns the transferred EIS descriptor, while the readiness registration and keymap
own duplicates. Prepared input borrows the sender exclusively through submission. The resume ping
orders the compositor's modifier state before paste on versions that support it. Shortcut events
come from a separate stream. See [libei adapter](../crates/platform/src/linux/ei.rs).

Wayland's clipboard loop consumes ownership and transfer events in bus order. A selection change
needs the ownership event after the SetSelection reply because the backend can lag the frontend.
Transfers already started retain their text. Close an empty transfer descriptor when there is
nothing to serve so Mutter can finish. Registered descriptors stay owned and nonblocking. Pause
closes pending consent and sessions. On X11, one arboard read orders the clipboard claim before
XTEST uses its separate connection. Submit the entire chord before synchronizing. See
[portal insertion](../crates/platform/src/linux/portal/insertion.rs),
[transfers](../crates/platform/src/linux/portal/transfer.rs), and
[X11 paste](../crates/platform/src/linux/x11/paste.rs).

## Linux realtime opt-out

[Realtime opt-out](../crates/platform/src/linux/realtime.rs) sets `DISABLE_RTKIT` before threads
start, and writes only if `/proc/self/task` shows that the caller is alone. PipeWire loaded through
ALSA can otherwise set a zero realtime budget and cause the process to be killed when capture
starts. Child processes inherit the setting.

## Mac event tap and AppKit ownership

Use AppKit windows and status items on the main thread. A retained NSWindow is non-Send. Adopt
Create-rule Core Foundation references once and keep Get-rule borrows tied to their owner. The
[status item](../crates/platform/src/macos/status_item.rs) keeps its menu alive while AppKit tracks
it.

The event tap, borrowed context, source, and entry observer live through their run loop. Publish
entry before treating a stop as delivered. Remove the observer before its stack context goes away.
Lifecycle callbacks touch synchronized policy and the stop request. Closing the input lane wakes the
loop through a callback that takes no lock, since the lane can close under the policy lock. Callback
panics close the lane and stop the loop. Remove notification tokens before dropping their context.
Use `MainThreadMarker` for Carbon's argumentless secure-input query. See
[macOS adapter](../crates/platform/src/macos.rs).

## Capture buffers, geometry, and exclusion

[Windows capture](../crates/platform/src/windows/screen.rs) owns its free-threaded frame pool and
capture resources on one recorder thread. Its MTA usage reference lasts for the process because
cached WinRT factories can outlive a capture. Close the session and pool before releasing their
resources. D3D textures and context share a device. Check crop, pitch, and height before reading a
mapped texture, and end the borrow before unmapping. Admit frames only after app-window exclusion is
in effect. The pending image carries its sampled target rectangle. See
[readback](../crates/platform/src/windows/screen/readback.rs) and
[exclusion](../crates/platform/src/windows/screen/exclusion.rs).

A [Mac capture source](../crates/platform/src/macos/screen.rs) can cross threads because the
retained filter is immutable and carries no AppKit object. It excludes the whole app. Window stills
check target geometry after the request. Fixed-region
[streams](../crates/platform/src/macos/screen/stream.rs) keep their native output and serial
callback queue through acknowledged stop and drainage. Successful stop, the native already-stopped
error, or the did-stop signal confirms cessation. An owner stop during startup still requires
confirmation before releasing the output and queue. Report a stop failure before cleanup blocks.
Drop tries once more and retains the native objects for the lifetime of the process if stop is still
unconfirmed. The closed frame lane then discards samples. Copy callback pixels while the native
buffer is locked and within the callback's borrow.

## Movie presentation and checkpoint lease

[Recording](../crates/platform/src/movie/recording.rs) and
[encode](../crates/platform/src/movie/encode.rs) own a fixed canvas, a 30 Hz presentation clock, the
native codec, and the checkpoint lease. Submit geometry before the first sample using it. For ticks
earlier than a new image, repeat only already captured pixels. Observe the original session deadline
in both capture and quiet advancement. Release file handles and native callbacks before the movie
lease. A failed finalize can leave a playable prefix and cannot revoke already committed coverage.

On Windows, [Foundation](../crates/platform/src/windows/movie/foundation.rs) starts COM MTA and
Media Foundation on the codec or reader thread, and outlives all native objects. The
[converter](../crates/platform/src/windows/movie/convert.rs) uses one output per input with
frame-rate conversion disabled. Release each MFT sample and events exactly once. The
[COM stream](../crates/platform/src/windows/movie/file.rs) serializes its cursor, cap checks, and
writes. Caller-owned buffers are borrowed only for synchronous access. A Closed receipt waits for
the last file owner. The [reader](../crates/platform/src/windows/movie/read.rs) decodes the
validated open file and checks signed pitch and offsets before copying locked pixels.

On macOS, the [image generator](../crates/platform/src/macos/movie.rs) requests the validated
sample's exact rational time and awaits completion even on cancellation. The
[resource loader](../crates/platform/src/macos/movie/file.rs) strongly owns the delegate behind the
weak native registration. Copy data under the file lock and release it before responding. Taking the
File under the same lock ends file access. The
[writer](../crates/platform/src/macos/movie/encode.rs) keeps its segment delegate and output file
alive through native completion. Latch write failures before releasing the output lock. Each sample
owns an independent pixel buffer, and mutable slices last only while it is locked. Calling finalize
after an asynchronous transition out of the writing state can raise a fatal Objective-C exception.
Keep the writer status check and let the checkpoint lease outlive the codec.

[Publication and ZIP export](../crates/app/src/show/store/movie.rs) normalize Media Foundation
fragment headers so the Mac reader can use the saved bytes directly. Preserve picture bytes, sample
timing, and file offsets.

## GPUI and vendored adapters

Foreground tasks use `AsyncApp` from `cx.spawn` within the application loop. During forced shutdown,
GPUI disposes windows before polling quit futures for up to 200 ms. The app removes Services before
disposal and retains the instance lock through it. Settings disposal waits for the in-flight write
and drops any queued draft. Normal app-owned Quit drains both before asking GPUI to exit. See
[shutdown](../crates/app/src/shell/shutdown.rs) and the [vendor index](../vendor/README.md).

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

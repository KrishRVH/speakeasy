# Native contracts

These contracts govern the platform adapters and the locally maintained GPUI adapters. Consult the
[change and test map](architecture.md#change-and-test-map) before changing their interfaces. The
Rust ownership types and checked boundaries enforce the local obligations described here; foreign
APIs also require these thread, buffer, and callback lifetime rules.

## Owned threads and insertion authority

Owners: `crates/platform/src/owned_thread.rs`, `input.rs`, `insertion.rs`, `insertion/commit.rs`,
`insertion/continuation.rs`, `clipboard.rs`, `monitor.rs`; `crates/app/src/show/recorder.rs`,
`show/wsl.rs`, and `instance.rs` hold the app's recorder, WSL lookup, and instance-listener threads.

`OwnedThread` owns exactly one join handle. Its exit lane closes after the body returns or unwinds;
thread-local destructors can still run afterward. Async join therefore performs the final OS join on
Tokio's blocking pool, while Drop always joins synchronously. The blocking closure owns the whole
thread owner, so cancelling a join cannot detach native cleanup. `OwnedThread::join` reports a
worker panic, a missing Tokio runtime, or a failed join task without exposing panic content. A
missing runtime still triggers the synchronous fallback. `StoppableThread` closes its stop lane
before joining. Stop requests do not acknowledge cleanup. Owners that depend on thread-local
destruction await the full join before opening replacements.

The Show recorder explicitly delivers its terminal outcome on its dedicated native thread. Its guard
delivers an unwind failure only if normal delivery has not started. Both paths hide the canvas and
respect session cancellation. The fallback may wait for the owner to drain the bounded event lane;
dropping that final event could leave the session waiting forever.

Insertion authority is a generation-tagged atomic commit gate shared with native cancellation
callbacks. An old permit cannot revoke or commit a newer recording. Final authorization checks occur
after blocking clipboard acquisition and immediately before an irreversible write/send; successful
commitment is not permission to repeat input after an error. Synthetic key arrays remain live for
their synchronous submission and carry the app's own-input marker so hooks ignore them. Direct text
is split only at valid encoding boundaries. Windows and macOS type each line break, whether CRLF, CR
or LF, as Shift+Enter, which chat and form fields take as a new line rather than a send; Linux sends
line breaks inside EI text, and the compositor decides how to insert them. Windows input cut short
releases every key its sent part left down, and reports the paste or Return as sent once the key
that acts, V with Ctrl or Return, went down; typed text cut short reports itself blocked. Clipboard
data and decoded images never enter Debug/error output.

`KeyCount` is a relaxed atomic counter. The Windows keyboard hook adds each key press it passes
through that is not a modifier or lock key. The Mac event tap adds each key-down it passes through,
since modifiers arrive as FlagsChanged, and adds one whenever the tap was disabled and may have
missed typing. Both callbacks skip keys with the own-input marker first. The insertion worker
compares the reading it takes after one paste with the one it takes before the next: the count and
the pointer position, and on macOS the HID system's mouse-down counters, which Speakeasy never posts
to. No other memory needs ordering against the count. A pointer that cannot be read means no
reading, and no space. On macOS the worker first asks the main thread for `secure_input` through the
main dispatch queue and waits 50 ms; secure input, or no answer, also means no reading.

A dictation sent with Return spends its permit on the text; the Return 100 ms later is a second send
that requires the foreground window (macOS: the frontmost app) captured right after the text,
Speakeasy not holding focus, and a released shortcut, so a newer press or a focus change withholds
it. A failure after the text landed reports that Return was not pressed, never a lost paste. A sent
dictation keeps its clipboard snapshot unused, so a slow target cannot paste the restored copy and
then receive Return. A paste snapshots the clipboard under the same lease that writes the dictation,
and the insertion thread answers the request before it waits 500 ms and puts the snapshot back. The
write records the sequence number or change count once its copy is complete, on Windows after the
clipboard closes, because closing advances the number. Another copy published in that moment would
lend the write its number, so the paste trusts the number only once the clipboard is shown to hold
the dictated text. The restore reopens the clipboard, prepares the whole snapshot, and only then
checks that the number is unchanged before it writes. Cancellation before submission restores the
same snapshot immediately or clears the dictated copy when no snapshot was available, as does a
failure once the dictated text is confirmed; a cancellation that finds another copy, or a check that
fails, restores nothing. These restores are best effort, so the dictation still reports its
cancellation or the original error. A revocation that meets a failing clipboard call reports the
cancellation, and a paste left to the user reports the text copied only while the clipboard number
is unchanged and the permit outlived that last check; a revocation during it restores the snapshot.
A write that fails partway puts the snapshot back, or clears its partial copy when there is none,
before it releases its lease; macOS rebuilds the snapshot first and puts it back only while the
change count its clearing produced still stands. One that fails before touching the clipboard leaves
it alone. The paste restores nothing after a failed write, so it never writes under a number another
copy may own. Windows and macOS inspect every clipboard format or item type for concealed content
before reading any payload; Windows saves nothing when its format listing ends in an error, and
macOS reads the items again when its change count moved while it read them, and saves nothing after
three tries that never held still. The dictated-text check reads text only from a clipboard that
bears no concealment mark and carries the dictation's own marks, so another app's copy of the same
text never passes for it: macOS reads the one item whose types it has just inspected and requires
its transient mark, and Windows requires both private marks to exclude the copy and lists no format
beyond them and the text formats. The thread serves the next request only after the restore, which
therefore snapshots the restored copy. The snapshot lives only on the insertion thread and is
dropped after the restore.

`MonitorControl` serializes publishing a live run-loop handle, readiness and stop. Stop-before-start
remains effective; retirement removes the handle before a later stop can wake it. This lock cannot
become owner-only state because a blocked native run loop does not receive ordinary owner messages.

Nothing joins the WSL lookup thread: its single lookup can block in the Windows redirector for as
long as WSL takes to start, or indefinitely on a wedged share. Handles ask it over a channel, each
question carrying its own one-slot reply lane, and a caller that stops waiting drops that lane, so a
late answer is discarded. The thread ends once every handle is gone and the lookup has returned;
process exit ends one still blocked.

The instance listener owns its current-thread Tokio executor and bounded set of control tasks for
receipt/disconnect waits. Shutdown aborts and drains those tasks before the server thread joins;
application state changes remain on the Services or session owner. The server thread builds its
executor and reports startup before `Instance::acquire` returns, so no executor is created or
dropped on the acquiring thread.

## Windows handles, hooks, windows and dialogs

Owners: `crates/platform/src/windows.rs`, `windows/input.rs`, `windows/insertion.rs`,
`windows/dialog.rs`, `windows/presentation.rs`, `windows/login.rs`, `windows/installation.rs`,
`windows/registry.rs`, `windows/awake.rs`, `awake.rs`, `wsl.rs`; the test-only `windows/fixture.rs`.

`take_handle` accepts only a fresh unowned kernel-object handle from an API that reports failure
with null and requires `CloseHandle`; it is not an adopter for pseudo-handles, window handles, or
APIs using `INVALID_HANDLE_VALUE`. The OwnedHandle closes it exactly once. Native byte/UTF-16
buffers have checked ABI lengths and remain alive through the synchronous call. Registry strings are
bounded and terminated, then validated before becoming an installation path or a WSL share name: the
default distribution's key holds only hex digits, braces, and hyphens, and its name only letters,
digits, `.`, `_`, and `-`. Reading `/etc/wsl.conf` through `\\wsl.localhost` or `\\wsl$` can start
the WSL VM and block, so the app reads it only on the WSL lookup thread; the CLI reads it
synchronously. The file is bounded to 64 KiB; an unreadable or oversized file gives no WSL path.

Windows presentation/subclass operations run on the HWND's owning thread. `Window::new` verifies
that ownership. The pill and overlay reapply their physical bounds after a DPI transition because
GPUI can reposition them synchronously during the initial move. A subclass is removed during native
destruction. Low-level hook callbacks without a context argument use thread-local state only on
their owning hook/message-loop thread, and callback failures cannot unwind across the native ABI.
The input owner removes hooks/window registrations before releasing their reachable state. A raw
integer HWND transports identity, not ownership. The test-only `native_window` requires a live HWND
that outlives its borrowed handle. `keyboard_hook` casts `lparam` only for a nonnegative hook code,
when Windows supplies a `KBDLLHOOKSTRUCT` valid through that invocation; other codes are forwarded
unchanged. `Monitor` shares its `StopEvent` with the native thread so owner cancellation can wake a
blocked message wait; the final `Arc` releases the kernel event only after both users finish.

The file dialog owns a dedicated STA/message-loop thread and all title/filter/output buffers until
`GetOpenFileNameW` returns. Successful COM initialization is balanced on that same thread. Dropping
the selection future does not cancel or join this modal dialog; its result is discarded if the
receiver left. This intentional detached lifetime must remain explicit rather than being mistaken
for the joined worker contract.

`Awake` holds one power request from `PowerCreateRequest`, which copies its reason string, and sets
`PowerRequestSystemRequired` on it, plus `PowerRequestDisplayRequired` for a Show when the session
has a display; a service session without one refuses that part and keeps the system request. The
handle is a kernel object with no thread affinity, so the session owner may hold and drop it on any
runtime thread; dropping it clears each type it set and closes the handle. A refused request records
without it. On the Mac it is an `NSProcessInfo` activity that disables idle system sleep, and idle
display sleep for a Show; `NSProcessInfo` is thread-safe, so its token moves with the owner and ends
on drop. Linux holds nothing.

## Child process containment

Owner: `crates/platform/src/process.rs`.

Termination authority originates only from a freshly spawned child. Windows attaches its process
handle to a private kill-on-close job immediately after spawn; listener PID lookups only test
membership and never grant authority. Unix children must start with `process_group(0)`, and their
owner polls for exit with `waitid(WNOWAIT)` and terminates the group before reaping the leader. The
same check verifies that the leader is still this process's unreaped child; consuming the stored
leader before signaling prevents a second signal after PID reuse. Listener table parsing and `/proc`
inspection remain read-only membership evidence.

## Show directory handles

Owner: `crates/platform/src/directory.rs`.

A Show directory handle keeps every relative open anchored to the selected directory. Unix uses
`openat` with `O_NOFOLLOW` and `O_NONBLOCK` for child entries and validates the resulting descriptor
before reading. Linux opens directories with `O_PATH`, macOS with `O_SEARCH`, and Windows with
traverse and attribute access, so a directory its owner may search but not list still opens; only
listing reopens it with list access. Windows opens a single child name with `NtOpenFile` relative to
its parent handle, refuses alternate data streams, and transfers the handle to `File` only after a
successful open. The opened entry's normalized long name must equal the requested name apart from
ASCII case, so an 8.3 short name cannot reach a hidden entry. Name-surrogate reparse points
(symbolic links, junctions and mount points) are neither files nor directories, so both platforms
refuse them by type; other reparse points, such as cloud placeholders, open as their file or
directory. Unix uses `fdopendir` on an independent directory open, and Windows uses an empty
relative name with `NtOpenFile` and `NtQueryDirectoryFile`, so listing owns its cursor. Readers
validate size, type, and bytes through the same handle. A presence check looks a name up without
opening or following it: Unix uses `fstatat` with `AT_SYMLINK_NOFOLLOW`, which needs only search
permission, and Windows opens the entry for its attributes as a reparse point. Both match names as
the file system's own lookup does, so a case-insensitive volume finds any case. An entry that is not
a plain directory, including a link, fails a directory open as not a directory. Identity compares
device and inode on Unix, and on Windows the volume's 128-bit file ID. Only a file system that
refuses that query as unsupported falls back to the 64-bit file index; any other failure is
returned, since the index need not be unique on ReFS.

Windows handles share read and delete, so they never refuse a rename or delete of their own entry;
directory handles and ordinary reads also share write, so editing continues while a Show is read.
Movie reads exclude writers while a validated prefix is in use. NTFS refuses to rename or move any
directory above an open directory handle, so a root an MCP server holds for its session pins its
ancestors until the server exits.

`open_lock` gives Unix lease files mode 0600, refuses links and multiply linked files, and narrows
an older file's mode. Narrowing cannot revoke a descriptor another user opened before it, and macOS
ACL entries the file inherited from its folder still apply.

## Linux desktop and transfer ownership

Owners: `crates/platform/src/linux.rs`, `linux/ei.rs`, `linux/portal.rs`,
`linux/portal/insertion.rs`, `linux/portal/transfer.rs`, `linux/token.rs`, `linux/x11.rs`,
`linux/x11/clipboard.rs`, `linux/x11/paste.rs`.

`linux::watch_readable` gives readiness registration its own duplicated descriptor, owned for the
entire `AsyncFd` lifetime. Both the libei and X11 adapters use this registration.

The libei function pointers use the declared C ABI, including integer-zero termination of variadic
capability lists. `RTLD_NODELETE` keeps copied pointers valid after the library handle drops.
Context/device/event/ping references are adopted and released once; device and ping references drop
before their context. The EIS descriptor transfers to libei, while readiness and keymap access own
duplicates. The sender and native pointers remain on their desktop thread/LocalSet. Prepared input
borrows the sender exclusively from capability/modifier validation through submission, so queued
dispatch cannot replace the selected device during that interval. EIS modifier events govern paste
readiness. A keyboard starts neutral and returns to neutral when paused; resuming preserves the last
reported state. With libei 1.4 or newer, each resume sends one ping, and the keyboard pastes only
after that ping's answer, which follows the nonzero modifier state the EIS must report after a
resume. Older libei has no sender ping, so a keyboard there can paste right after it resumes, before
a modifier event that arrives in a later dispatch. Global shortcut signals can arrive out of order
with those events and never reset the modifier state. A disconnect ends the service even though
later dispatch calls may succeed.

Portal transfer descriptors are owned, set nonblocking before AsyncFd registration and retained
until the transfer future drops. Speakeasy subscribes to portal restarts before it reads the
portal's unique bus name, then sends every portal call to that name, so a restart during startup
ends the service instead of splitting calls and signals between two portals. Portal signal match
rules name that unique name rather than the well-known one, so zbus also drops a signal another
client addresses straight to Speakeasy's connection; a malformed shortcut or transfer signal is
skipped, and a malformed ownership change counts as lost ownership. Session-scoped signal streams
keep draining while transfers are full. A replacement selection counts as owned only after an
ownership change received after its SetSelection reply, because the portal frontend replies before
its backend sets the selection; ownership changes do not name the selection they belong to, so a
late change for the previous copy can still confirm a replacement. A transfer Speakeasy declines,
for a full transfer set, a paste replacing the selection, another MIME type, or no current committed
copy, still gets SelectionWrite: its descriptor closes empty and SelectionWriteDone reports failure,
since Mutter ends a transfer only when its descriptor closes. Refusals run beside the insertion
loop, at most `SIGNAL_BACKLOG` at once, and up to `SIGNAL_BACKLOG` more wait their turn in arrival
order; a request past those ends the service, whose session close retires the transfers still open.
A transfer whose selection was invalidated before it started still asks for its descriptor and
closes it empty. The insertion loop polls method replies and new insertions ahead of transfer
requests, and on the desktop thread's single-threaded runtime a signal stream refills only between
polls, at most `SIGNAL_BACKLOG` messages at a time, so transfer requests cannot hold off an
insertion. The portal clipboard serves bytes only after the permit commits to paste or manual copy.
Selection loss invalidates its transfer payloads. Writes and reporting have deadlines. An X11 copy
that waits for released keys watches XFixes selection-owner notifications during the wait and
refuses to overwrite any copy made meanwhile, even one from an app that keeps its owner window. An X
server without XFixes degrades only that wait, which then compares the owner window before and after
it and so misses a new copy from an app that keeps its owner window. X11 clipboard serving and
insertion have separate owners; before committing paste, the X11 paste confirms that the clipboard
still holds the dictated text from the same owner and only then samples focus, layout, and held
modifiers, because a clipboard manager can take seconds to answer that read. It then reads the owner
window once more, one server round trip, immediately before committing: Speakeasy's own window owns
the dictated text, so a copy made during those queries changes the owner and refuses the paste. A
content check that fails or errors never leaves the clipboard for manual paste, and Escape during a
check that errors reports cancellation rather than the error. The last check and XTEST submission
are not one atomic server transaction.

## Linux realtime opt-out

Owners: `crates/platform/src/linux/realtime.rs`, called first in `crates/app/src/main.rs`.

cpal's ALSA host opens the default device through PipeWire's ALSA plugin, which loads PipeWire's
[realtime module](https://docs.pipewire.org/page_module_rt.html) into Speakeasy's process. Without
realtime privileges, that module asks the desktop's Realtime portal or RTKit for a budget and lowers
`RLIMIT_RTTIME` to it, so a portal that cached a zero budget gets Speakeasy killed by the kernel
seconds after the microphone opens. Capture is buffered and needs no realtime scheduling, so
`disable_rtkit` sets
[`DISABLE_RTKIT`](https://docs.pipewire.org/page_man_pipewire_1.html#pipewire-env__disable_rtkit)
before any other thread starts. The module then never asks the portal or RTKit and fails to load,
which PipeWire's client configuration tolerates, and the stream opens without it. A user with
realtime privileges still gets them under the module's own limits.

Writing the environment races any thread that reads it, so `disable_rtkit` writes only while
`/proc/self/task` lists the calling thread alone, and otherwise leaves the environment unchanged.
Processes Speakeasy starts inherit the variable. Its engine, encoder, and archive helpers play no
audio, but an application Speakeasy opens through `xdg-open`, such as the player for a Show's movie,
also streams through PipeWire without realtime scheduling unless it holds realtime privileges.
PipeWire releases through 1.6 read the variable and its development branch ignores it, so a later
PipeWire release needs another opt-out.

## Mac event tap and AppKit ownership

Owners: `crates/platform/src/macos.rs`, `macos/clipboard.rs`, `macos/status_item.rs`, `monitor.rs`,
`permissions.rs`; `keyboard.rs` holds the hold-key and F13 policy this tap shares with the Windows
hook, and `keyboard/typematic.rs` reads the Windows repeat settings.

AppKit window/view operations require the main thread. The platform wrapper retains the NSWindow and
is not Send. Core Foundation Create-rule results are adopted once; Get-rule references remain
borrowed or are explicitly retained. Toll-free bridging casts are limited to APIs representing the
same underlying CF/Objective-C object, with the original owner retained through the borrow.

The event tap, its borrowed policy/context and run-loop source live on the same monitor thread. The
run-loop entry observer's stack context outlives the loop and is removed before returning. Entry
publication is necessary because stopping a not-yet-entered loop is not a reliable future stop. The
tap port is published once before events can run, allowing disabled-tap recovery. Lifecycle
callbacks can arrive on the posting thread; they touch only the synchronized policy, the input lane
and the monitor's stop request. Policy decisions and delivery occur under one lock to preserve event
order. Callback panics close the lane and stop the loop. The monitor thread registers a waker on the
input lane's closure, so a lane closed from any thread, including by its receiver, stops the run
loop without waiting for another event. The waker only stops the loop and takes no lock, so a lane
closed under the policy or monitor lock cannot deadlock. The event tap passes the current key
through and disables itself when the input lane closes. Notification tokens remain retained until
removal. A hold key acts only when pressed afresh during a hold, never on a repeat, so a key already
down when the hold began does nothing. A swallowed hold key stays suppressed until its own release
or a fresh press of it, which shows that the release went unseen, so a key held through a tap
outage, lifecycle interrupt or missed chord release never repeats into the app. The Mac tap tells a
fresh press from a repeat by the event's autorepeat field. Only a fresh press can be a quick
application combination with a right modifier, so a swallowed key's repeat stays swallowed after
that modifier's release.

Mac F13 uses key code 105 and the event's autorepeat field. Its initial press and release establish
the pre-event hold explicitly; unrelated events reconcile against HID-system physical key state. The
secondary Fn flag can accompany function keys, so it does not make F13 another modifier chord. The
insertion worker retains the configured shortcut and polls physical F13 state for both its release
wait and its final preflight. Its per-request autorelease pool encloses native delivery.

Windows F13 is VK 0x7C, and the hook swallows its every event. Windows updates async key state only
after the hooks pass an event, so a swallowed F13 never reads as down, and its own events establish
the hold. The low-level hook reports no autorepeat, so the Windows policy remembers each key whose
press it saw, with the time of its last down, and takes another down of that key before its release
as a repeat while it arrives within the repeat limit of the last one. Windows autorepeat repeats
only the most recent key and does not resume an earlier one, so a down after a longer silence is a
fresh press whose release went unseen. The limit is twice the longer of the repeat delay and
interval, plus 250 ms. The hook reads those settings with `SystemParametersInfoW` whenever it judges
such a silence: the keyboard delay and speed, or FilterKeys' acceptance wait plus repeat delay, and
its repeat interval, when FilterKeys is on without BounceKeys. Without repeat every further down is
fresh, and an unreadable FilterKeys setting counts as its 20 s maximum. The read fills caller-owned
values and sends no messages, so it cannot re-enter the hook. The policy keeps those keys through a
lifecycle interrupt and a missed chord release, so a swallowed key's repeats stay swallowed until
its own release, and a press after the lock screen or a stall is fresh again; a key released unseen
and pressed again within the limit loses that press. Nothing reports a swallowed F13's physical
state, so an unseen F13 release keeps the hold until the next fresh F13 down, which ends the hold
and starts another, or until the next F13 release. While F13 is the last key to go down, its repeats
keep arriving, so a hold key pressed after a silence longer than the limit presumes the release went
unseen: the hold ends and the key reaches the app. Any other key-down, a Show key or a Shift tap
included, makes F13's silence expected until F13 is heard again, and so does an F13 down the hook
reports as injected, since Windows never repeats injected input. FilterKeys without repeat never
presumes a release. A hold key pressed within the limit after an unseen release, or after another
key or an injected F13 went down, still acts. F13 modifies no other key, so the insertion worker's
release wait and preflight do not wait for it.

`StatusItem` is created, updated and removed on the main thread. It takes the item's button at
creation, removing the item again if AppKit offers none, and holds that main-thread-only button, so
it is not Send. It owns the muda menu and retains muda's borrowed NSMenu pointer once, while that
menu is held, as the status item's own menu. AppKit therefore tracks the menu from the user's own
click, as for any status item with a menu, rather than from a synthesized `performClick`; muda still
dispatches item actions as `MenuEvent`s and updates the same NSMenu. Dropping the item removes it
from the status bar.

`secure_input` reads Carbon's `IsSecureEventInputEnabled`, which reports whether any process holds
Secure Event Input. The `unsafe extern` block that links Carbon vouches for its argumentless
`Boolean` signature. Its header marks it not thread safe, so the main thread is the precondition
each call discharges: `secure_input` calls it only once `MainThreadMarker` proves that thread, and
reports false elsewhere. While another process holds Secure Event Input, the session tap still
receives FlagsChanged but no KeyDown or KeyUp.

Permission queries report consent only; they do not open a microphone, prove device readiness or
install a hook. The Mac clipboard's private/transient marks precede user content, and change-count
plus content checks bound clipboard replacement races.

## Capture buffers, geometry and privacy

Owners: `crates/platform/src/screen.rs`, `screen/geometry.rs`, `windows/screen.rs`,
`windows/screen/exclusion.rs`, `windows/screen/layout.rs`, `windows/screen/readback.rs`,
`macos/screen.rs`, `macos/screen/stream.rs`, `macos/screen/stream/output.rs`; owned-window checks in
`windows/screen/acceptance.rs`, `windows/screen/acceptance/geometry.rs`, and
`windows/screen/acceptance/movie.rs`.

Windows capture runs on one owner thread with a free-threaded frame pool. Each capture takes an MTA
usage reference it never releases, so the apartment lasts for the process, because cached WinRT
activation factories outlive individual captures. Session/pool closure precedes resource release,
and the display-closed callback only closes its lane. Texture/context resources belong to the same
D3D device; crop bounds lie inside the source; CPU mappings are read only until Unmap. Checked
row/pitch/height calculations precede raw slices. App-owned windows, including any whose display
affinity cannot be read, are excluded before admissible frames, and new exclusions discard pending
images under the existing composition margin.

Pending Windows frames retain their observed target rectangle and are rejected when observations
before or after GPU readback differ. While DWM cannot report the frame bounds of a window that still
exists on screen, Windows discards its frames instead of ending the Show; when the bounds return, it
crops only frames composed after a fresh composition margin, so a frame from the unmeasured interval
never takes the new rectangle. A readback whose before or after observation finds the bounds
unreadable or changed counts as the same loss, so the next measured bounds also start a fresh
margin. Mac window stills are admitted only when the target rectangle observed after the request
equals that attached to the image. A still rejected because the measurable target rectangle changed
refreshes the response clock; unknown geometry does not. Neither platform's sampled observations
establish exact composition-time window geometry or detect every move-away-and-back.

Mac `Source` may cross threads because its retained ScreenCaptureKit filter is immutable and lacks
thread affinity; no AppKit object is moved with it. The filter excludes the whole application,
including subsequently opened app windows. Fixed-region streams retain the native stream, output and
private serial queue until stop acknowledgement and queue drainage. Start waits for its completion,
the did-stop signal, or a stop request. A failed start or a did-stop signal detaches the output and
drains the queue directly; a start interrupted by a stop request counts as running until a stop
confirms. For a started stream, only successful stop, the exact documented already-stopped
domain/code, or the actual did-stop signal establishes cessation; other stop errors do not. A
confirming stop completion closes the did-stop signal's lane, so a confirmation that arrives after
its attempt stopped waiting still confirms every later attempt. Each stop waits at most five seconds
for its completion or the did-stop signal, whichever comes first. An unconfirmed stop is reported
and retried after 0.1, 0.4, 1 and 2 seconds. If none confirms, the stream, output and queue stay
retained for the life of the process, since releasing them could leave capture calling freed
objects; the closed frame lane makes the output discard every sample. Retaining them latches a
process-wide flag before the capture thread exits, whether the recorder awaited the stop or dropped
the capture after a failed start. From then on `ScreenCapture::available` returns `Unstopped` and
`ScreenCapture::start` refuses, and every session owner, including one created by Pause and Resume
or a shortcut change, reads the flag when it presents status: it adds the quit warning to every
status message and refuses a Show before opening the picker until Speakeasy restarts. A stream
dropped before confirming cessation requests stop once more first. Failure notification precedes
potentially blocking cleanup. Callback samples are borrowed only during the call; copied bounded
BGRA leaves the callback. Native image buffers stay locked through reads. The stream's bounded
host-clock calibration and display-geometry checks precede frame admission.

A screen Show's window watch lists windows on its own stoppable thread every frame interval and
sends only changed layouts over a bounded lane, dropping a change the lane cannot take until the
next interval. It reads bounds, a layer class, and the owning process; never titles, app names, or
pixels. Windows marks topmost windows other than taskbars, menus, and popups floating; Mac marks
levels above normal and up to utility floating, the pop-up menu level a menu, and the main-menu and
Dock levels bars. Window and process identifiers become small per-watch numbers before they leave
the thread. Windows walks `EnumWindows` front to back and reads DWM extended frame bounds for
visible, uncloaked, not minimized windows that accept clicks; Mac reads on-screen
`CGWindowListCopyWindowInfo` entries. Both leave out Speakeasy's process, as frames do.

Owned-window acceptance creates and destroys its window classes and HWNDs on their owning thread.
Monitor enumeration writes only to the local vector that remains live throughout the synchronous
`EnumDisplayMonitors` call. The rescaling test window procedure casts `lparam` only for the
corresponding `WM_NCCALCSIZE` or `WM_DPICHANGED` message and uses its buffer only during that
callback.

## Shared movie presentation and checkpoint lease

Owners: `crates/platform/src/movie.rs`, `movie/canvas.rs`, `movie/encode.rs`, `movie/recording.rs`.

One recorder owns presentation scheduling, canvas, native codec and checkpoint lease. Geometry is
durably acknowledged before the first submitted sample using it. All grid points earlier than a
newly captured image use the previous canvas; quiet periods repeat already captured pixels. Windows
quantizes to 100 ns and Mac keeps nanoseconds while native samples retain their rational 30 Hz
timing. Per-advance catch-up, total sample, canvas and disk bounds remain independent. Coverage
records an accepted sample before a later checkpoint error can occur. A held image never claims a
new capture time.

Begin/geometry acknowledgements observe both checkpoint cancellation and capture stop. The
non-cloneable target drops after the native encoder and its actual file handles. A recording that
failed drops its codec without finalizing it. Finalization failure reports partial video; it cannot
erase committed coverage or unlock/delete checkpoint storage while callbacks retain file access.

## Windows codec and verified-file decoder

Owners: `crates/platform/src/windows/movie/foundation.rs`, `encoder.rs`, `convert.rs`, `file.rs`,
`read.rs`, `read/pixels.rs`.

The Foundation guard starts COM MTA and Media Foundation and is deliberately not Send. It outlives
every native codec/reader object on its initiating thread, including partial-open failures. Encoding
releases writer, sink, converter and stream before Foundation teardown. Conversion is synchronous,
disables frame-rate conversion and requires one output with unchanged time/duration for each input.
The generated MFT output's ManuallyDrop sample/events are owned by a guard and released exactly once
on every result path.

The COM file adapter serializes cursor operations and caps actual writes, seeks and SetSize. Foreign
pointers and optional result pointers obey IStream's caller-owned buffer contract. It maps failures
to content-free HRESULTs. Reads, reported size and end-relative seeks stay within the validated
prefix even if the underlying file grows. Explicit IMFByteStream Close cancels I/O, but the Closed
receipt waits for the last underlying IStream/file owner. A writer's extra File handle exists only
to sync after Finalize closes the native byte stream, and drops before that receipt.

Decoding uses the same validated open File, never reopens a path. Media-type-change notifications
revalidate dimensions/color format and associate stride with the returned sample. Flat and 2D native
locks remain owned until unlock; signed pitch, offsets and allocation bounds are checked before
copying top-down opaque BGRA. Cancellation is observed between native calls, and native I/O is
released before return.

## Mac codec and verified-file decoder

Owners: `crates/platform/src/macos/movie.rs`, `macos/movie/file.rs`, `macos/movie/encode.rs`,
`encode/output.rs`, `encode/buffers.rs`, `encode/settings.rs`.

AVAssetImageGenerator requests one exact rational timestamp with no preferred transform or aperture
crop. The request retains the generator and waits for the promised completion even after
cancellation or unwind. Completion borrows native objects only during its call and copies checked
pixels/timing into Rust-owned values. Numeric native error codes may be reported; native
descriptions and userInfo are not exposed.

The asset's private resource URL resolves only to the validated file. Its strongly held delegate
backs a weak native registration; external references are forbidden. Each bounded read holds the
file mutex only through file access, releasing it before responding with copied NSData. Asset
teardown detaches/cancels and takes the optional File under the same mutex, so no concurrent read
retains the file afterward. The reader passes the published movie bytes directly to AVFoundation;
Media Foundation fragment headers are normalized before publication.

The AVAssetWriter has no output URL: only the strongly held segment delegate owns its create-new
private file. Segment order, actual byte cap and sync are enforced under one lock. Failure/panic is
latched before releasing that lock. Each appended sample owns an independent CVPixelBuffer; the pool
allocation threshold is based on actual buffer size. Writable pixel slices are exclusive and bounded
by the native allocation while locked. CMSampleBuffer/format results follow Create ownership and
retain their dependencies. Finalization requires a writing writer and waits for its native
completion. A writer that fails asynchronously between that status check and the finalization call
still makes AVFoundation raise an Objective-C exception, which aborts the process; catching it needs
objc2's `exception` feature, whose helper crate the build does not include. Drop cancels a writer
still writing before closing the sink. The checkpoint lease outlives those resources.

## GPUI and vendored adapters

Owners: the [vendor index](../vendor/README.md) lists each patched vendor file;
`crates/app/src/application.rs`, `main.rs`, `shell/shutdown.rs`, and
`shell/settings/text_field/accessible.rs` hold the app side.

GPUI's `AsyncApp::update` panics if the app is gone or already borrowed. Every `AsyncApp` Speakeasy
uses comes from `cx.spawn`, and foreground tasks run only from the platform loop while
`Application::run` holds the app. `App::shutdown` blocks on its quit futures alone, so no foreground
task runs under its borrow. Speakeasy never calls `to_async` or keeps an `AsyncApp` beyond its task.

`App::shutdown` runs the quit observers, then disposes windows and flushes effects, then polls the
observers' futures for up to `SHUTDOWN_TIMEOUT` (200 ms). Speakeasy's observer removes the services
before any window is disposed, and Settings finishes its queued saves as its window is disposed. The
forced-exit fallback keeps the instance lock inside the returned future until window disposal has
completed. Settings stays owned by its window alone.

On macOS a deferred termination (`NSTerminateLater`) runs AppKit's nested run loop, which cannot
drain the serial main queue that GPUI's foreground tasks need. The Apple dispatcher marks main-queue
tasks with a flag only the main thread writes, and `applicationShouldTerminate:` defers only outside
such a task and outside an application borrow. The platform takes its state lock only around reads
and writes of the termination state, never across the app's handler or AppKit calls.

Vendor patches follow upstream documentation policy. Windows UIA native event raising and provider
return occur outside window-state borrows because either can synchronously reenter through
`WM_GETOBJECT`. Hide publishes an empty root-only tree before the window disappears and minimize
publishes it as the window minimizes; the tree returns only after `WM_WINDOWPOSCHANGED` commits
visibility. `WM_DESTROY` publishes the empty tree while the HWND is valid, then returns a null
provider for the HWND so UI Automation drops its entries for the window. A provider retires when its
node leaves the tree, even when a later tree reuses the id, because GPUI derives node ids from
element ids, and a text range retires with the provider that created it. Range actions are requested
after the tree lock is released and report errors instead of panicking across COM. Bounds are
physical client coordinates, and clipping must not turn disjoint or inverted rectangles into visible
content. A range scroll request carries the range rather than coordinates, so the field resolves it
against the layout it paints next. GPUI queues action requests and handles them on the foreground
after the adapter returns, so a request made just before a hide reaches the restored tree when its
element survives; rejecting it needs a tree generation in GPUI's action path. The macOS and Linux
AccessKit bridges carry no Speakeasy patches and do not withdraw a hidden window's tree.

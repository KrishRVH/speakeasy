# Tray and motion behavior contract

Speakeasy stays available without occupying the desktop. Feedback follows
real capture and insertion state; animation never delays either operation.

1. The tray/menu bar is the app's home. Configured native launches stay quiet;
   first setup and invalid configuration open Settings. Closing Settings hides
   the existing window and preserves unsaved edits. Windows minimize also hides
   to the tray. Reopening reveals that window. The first hide shows a brief hint
   when the pill is idle.
2. The waveform tray mark uses static state badges for model loading, readiness,
   recording, processing, paused, and attention states. The menu provides
   Pause/Resume, context-sensitive Start/Finish, Cancel, Settings, and Quit.
   Errors remain reachable after pill dismissal.
3. The pill follows capture and insertion: immediate startup feedback, microphone
   readiness, quick audio attack and gentle release, a clear hands-free symbol,
   timer, final 30-second countdown, processing, a brief submission check, quiet
   cancellation, and readable errors. Silence must not claim text was submitted.
4. Transitions remain continuous when interrupted. Reduced motion uses immediate
   geometry changes and static status symbols. Hidden and settled idle views do
   not request animation frames. Presentation timers belong to the view and
   cannot mutate a recording or dismiss a later session.
5. The simulated demo covers completion, cancellation, silence, errors, countdown,
   and interrupted dismissal. Fake capture/insertion and timing tests cover
   session failure boundaries. Native tray checks use `--demo-tray` with owned
   windows and temporary settings. Live microphone, hook, clipboard, and input
   acceptance requires explicit opt-in and is reported separately.

Visual direction: graphite capsule, fine edge, restrained shadow, orange-red
only while capture is active, amber for attention. Motion follows the analytic
springs described in [interaction design](interaction-design.md), with 350 ms of
submission feedback. A completion mark means input was submitted to the OS; it
cannot prove that the target editor accepted it.

See [handoff.md](../handoff.md) for completed checks, measured idle behavior,
and the remaining native macOS and live-dictation acceptance limits.

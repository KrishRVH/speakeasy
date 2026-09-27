# Interaction direction: Tally

Proposed direction from Fable 5.1, consulted through Claude Code using
`--model claude-fable-5-1 --effort xhigh`. Returned metadata confirmed the model.
The brief reflects the user's clarified scope: dictate into any app, with
premium feel, seamless interaction, and high-fidelity animation. Automatic
editing and broad AI features are outside the initial focus.

The Rust pill implements the foundation of this direction in `crates/app/src/pill.rs`.
Run `--demo` to inspect it. The detailed behaviors below remain design targets;
native acceptance, accessibility, and performance proof are still pending.
Dimensions, colors, and timings are initial tuning values. See the [rewrite plan](rust-rewrite-plan.md) for ownership and proof
requirements.

## Fable's take

Use the broadcast tally light as the organizing idea: a distinct signal means
the microphone is live. Make the experience calm, precise, and unambiguous.
Invest in timing, continuity, and accurate feedback. The pill should feel like
a small instrument that appears exactly when needed.

Fable recommends a graphite capsule, crisp edge, subtle shadow, restrained
typography, one live-capture color, and an honest audio envelope. No backdrop
blur, glowing orb, fabricated meter movement, or idle breathing. Treat those
as aesthetic and complexity choices; their performance cost still needs
measurement on the actual renderer.

## Form and placement

- Graphite near `#17181B`, initially 94% opacity; 1 DIP inner edge at 9% white.
  Try a tight shadow: 4 DIP vertical offset, 16 DIP blur, 28% black.
- Red-orange near `#FF4F2E` means live capture only. Processing is neutral;
  actionable errors use amber near `#F5A524` plus text. Shape and accessible
  state labels must also distinguish states; color alone is insufficient.
- System typography: Segoe UI Variable / SF Pro where available, medium weight,
  around 12.5 DIP, tabular timer digits. Verify font availability and rendering.
- Bottom center of the foreground window's monitor work area, initially
  28 DIP above its lower edge. Keep the anchor fixed through a session. Rehome
  visibly and safely if that display disappears; never leave the pill offscreen.
- Do not track the caret. Avoid distracting position changes between words.

| Visual state | Initial size | Content |
| --- | --- | --- |
| Starting microphone | 56 × 28 DIP | Dim neutral dot; accessible starting status. |
| Recording | 148 × 36 DIP | Live dot and audio envelope. |
| Hands-free | 188 × 36 DIP | Ringed live dot, envelope, timer. |
| Transcribing | 72 × 20 DIP | Small neutral processing line. |
| Error | Up to 360 × 44 DIP | Amber indicator, short message, one action. |

Keep the bottom-center anchor stable as dimensions change. Validate legibility,
hit targets, localized text, high contrast, and display scaling; adapt sizes
when needed rather than clipping useful information.

## Motion and choreography

Use damped springs for position and size, preserving position and velocity when
the target changes. Fable suggests roughly 240–260 ms response, damping ratios
around 0.88–0.90, and less than 2% overshoot. These are perceptual targets until
the chosen spring implementation defines its parameter units. Use time curves
for opacity/color and a monotonic exit. All motion derives from elapsed time.

| Event | Proposed visual response | Behavioral requirement |
| --- | --- | --- |
| Press | Appear from 0.9 scale; fade in over about 90 ms. | Request capture immediately; show startup honestly. |
| Capture ready | Dot takes live color over about 50 ms; expand into recording; reveal meter over about 120 ms. | Trigger from confirmed capture activity, such as the first real buffer. Never claim readiness from a timer. |
| Speech | Only the envelope moves. | Drive it from measured audio; retain clear live indication in silence. |
| Stop requested | Retarget toward the compact processing shape. | Live-capture indication remains truthful until capture stops. Animation does not control device lifetime. |
| Processing | Reveal a subtle indeterminate line only if work lasts beyond about 250 ms; try a 1.2-second cycle. | Do not delay transcription or insertion to display it. |
| Input submitted | Fade over about 160 ms, with a 4 DIP downward drift and a small scale reduction. | No checkmark or success claim beyond the actual insertion outcome. Clipboard cleanup can finish separately. |
| Escape | Flatten the meter briefly, collapse toward center, and fade; aim for about 120–180 ms total. | Invalidate work immediately, independently of animation; Escape reaches the target app. |
| Error | Expand to a concise message and action; no shake. | Keep recovery available in tray/settings after any temporary pill notice dismisses. |
| Hands-free | Draw a ring over about 180 ms; reveal timer as width changes. | Use the established gesture transitions and recording limit. |

A new session retargets the current visual shape instead of restarting from
invisible. Old session animations cannot hide or overwrite a new session.
If processing ends before its indicator appears, transition directly to exit.
Do not impose minimum dwell times on capture or insertion to make motion look
complete. Reduced motion replaces morphs with brief crossfades and removes
travel, scale, and perpetual processing motion.

Stop requesting frames when nothing changes. A static live indicator can remain
visible without continuous drawing. The timer still redraws when its displayed
value changes. Measure actual presentation at 60/120 Hz and mixed refresh rates;
GPUI 0.2.2 has no inactive-window throttle; source inspection and frame requests
are not proof of smooth presentation.

## Audio envelope

Fable proposes a mirrored strip of 24 narrow columns showing about 1.2 seconds
of recent amplitude. Start with 2 DIP columns and gaps, mapping roughly
−60 to −6 dBFS onto available height. Use a fast attack and about 150 ms release.
Silence settles to a thin line. Do not synthesize activity when the microphone
is quiet or unavailable.

Compute fixed-size summaries off the UI thread; aggregate from callback buffers
without introducing a high-frequency timer or allocating per update. The UI
interpolates display values at presentation time and never reads PCM. Timestamp
summaries so a slow frame does not cause the meter to lag indefinitely. Coalesce
visual updates under load.

Prototype the history strip against a simpler instantaneous envelope. Retain
whichever communicates syllables clearly with less visual noise. Reduced motion
uses a single smoothed level bar; a disabled meter still needs a visible and
accessible live-recording signal.

## Interaction and setup

The hold-to-talk pill is click-through. Explore non-activating Stop and Cancel
targets for hands-free, with at least 32 DIP hit areas. Verify that revealing
or clicking them does not steal editor focus or change the insertion target.
Keyboard controls and tray actions remain available; essential controls cannot
depend on hover. Opening settings is an explicit focus-changing action.

Use one small settings window with native title-bar behavior and clear sections:
microphone, shortcut, provider, appearance/accessibility, and login. A bounded,
explicit microphone check uses the same visual meter. Practice uses the actual
dictation path in a temporary scratch field. Keep permission recovery actionable
and test whether changes require rechecking or restarting on the selected OS.
Do not promise universal live permission recovery without native evidence.

Model setup shows genuine readiness/download state; do not fake percentage
progress. Provider setup must precede any practice requiring inference. The
initial run should quickly lead to a successful dictation, with troubleshooting
close to the failed step. Keep sounds off by default.

## Corrections to the consultation

Preserve these contracts over Fable's suggested changes:

- **Tap behavior:** retain the configured 220 ms tap maximum and 300 ms
  double-tap window by default. A short single tap finalizes after the window;
  it is not silently cancelled. Capture stays continuous through the window.
- **Microphone truth:** a stop request is not proof the driver stopped. Keep
  stopping/resource state distinct from visual exit. Startup feedback cannot
  recover speech spoken before the device supplies samples; test onset loss
  and device latency explicitly rather than treating an honest dot as a fix.
- **Fallback:** initially keep existing clipboard/manual-paste outcomes. Fable's
  suggested temporary transcript plus Copy action is a separate product choice,
  not an implied addition to scope or authorization to overwrite the clipboard.
- **After commit:** Escape cannot undo OS input already submitted. It must not
  produce a misleading cancelled result during clipboard restoration.
- **Performance:** do not lower visible animation from 120 to 60 Hz under load
  as an automatic design decision. Measure contention, simplify the animation,
  and review any cadence compromise against the user's priorities.

## First prototype and review

Build a small native pill prototype with scripted states and tunable design
constants. Use the same view in the OS shell proof; keep scripted visual evidence
distinct from actual microphone/insertion evidence. Avoid inventing a general
hot-reload or animation framework for this experiment.

Exercise 20/400 ms startup, 80 ms/4 s processing, release mid-appearance, Escape
mid-processing, a new press mid-exit, device failure, and the hard recording cap.
Include complete hold, single-tap, and hands-free paths. Default scenarios use
fake audio levels and input. Live microphone and target-app tests remain opt-in.

Review normal-speed motion first; use frame captures and, where helpful,
high-speed camera footage to inspect discontinuities. Compare two parameter
sets with only one variable changed. Test over light documents, dark editors,
and video, at multiple display scales, with reduced motion and screen readers.
Repeat while local inference is active and on battery. Combine the user's taste
judgment with frame-presentation measurements; a screen recording alone does
not prove real display cadence.

Exit criteria: the user prefers the direction, transitions have no visible
pops or distracting bounce, state remains understandable, focus never moves,
and native measurements support the responsiveness and frame-pacing goals.

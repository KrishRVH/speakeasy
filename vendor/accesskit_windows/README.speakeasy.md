# Speakeasy AccessKit Windows patch

This directory contains the published
[AccessKit Windows 0.32.1 crate](https://crates.io/crates/accesskit_windows/0.32.1),
under its original MIT or Apache-2.0 license. The published archive SHA-256 is
`eff7009f1a532e917d66970a1e80c965140c6cfbbabbdde3d64e5431e6c78e21`.
Upstream source, manifest provenance, and version are preserved. License texts come from
[the recorded upstream commit](https://github.com/AccessKit/accesskit/tree/1bbcf100942bac96c2c3a4a91cb67b0b20201a24).
The manifest includes BSD-3-Clause for the Chromium-derived Windows code so package notices
include its license too. Registry cache markers are omitted.

`src/text/geometry.rs` intersects text geometry with transformed clipping ancestors.
`ITextRangeProvider::GetBoundingRectangles` omits degenerate and invisible rectangles;
`ITextProvider::GetVisibleRanges` returns visible character spans, including a degenerate
range when no text is visible. Full document text and positions remain available while
the viewport scrolls. The app's Windows acceptance checks these methods on owned fields.
`src/text/scroll.rs` implements reachable horizontal `ScrollIntoView` through `SetScrollOffset` on
an explicit clipping container. It uses exact character positions, preserves absolute request
ordering, and reports unsupported for unreachable ranges. Action callbacks run after releasing the
tree lock. `mise run rust:test` runs the pure geometry and scrolling regressions on Windows.

//! Pure dictation gestures and frame-independent motion.

#![forbid(unsafe_code)]

/// Shortcut transitions with explicit timestamps and bounded recording deadlines.
pub mod gesture;
/// Analytic motion that retains velocity across target changes.
pub mod motion;

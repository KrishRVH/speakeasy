//! Turning a recorded Show into a bundle a coding agent can read.
//!
//! A Show is a screen recording with narration, pointer events, and ink and numbered pins over
//! the screen. During capture, [`Tracker`] turns pointer samples into ink, pins, and actions, and
//! [`Journal`] keeps sampled frames losslessly with their signatures, thinned evenly to fit a
//! memory budget.
//!
//! After capture, [`select()`] picks the keyframes worth showing from those signatures, and
//! [`compose`] annotates them into a [`Bundle`]: keyframes, contact sheets, summary, and manifest.
//! Nothing here performs I/O: callers supply frames and receive encoded files.
//!
//! Selection, layout, and drawing follow Scrawl's, under the MIT License; see `NOTICE.md`.

#![forbid(unsafe_code)]

mod align;
mod bundle;
mod convert;
mod demo;
mod describe;
mod draw;
mod error;
mod geometry;
mod image;
pub mod ink;
mod journal;
mod marks;
mod moment;
mod parallel;
mod recording;
mod select;
mod serde_range;
mod sheet;
mod signature;
mod tracker;
mod typeface;

pub use crate::{
    bundle::{Bundle, File, Manifest, Unreadable, compose},
    demo::Demo,
    error::Error,
    geometry::{Point, Rect},
    image::Image,
    journal::{Admission, Journal},
    marks::{Mark, Shape},
    moment::{Moment, Span},
    recording::{
        Action, EmptyStroke, Ending, InkPoint, Narration, NarrationLoss, Pin, Recording, Size,
        Stroke, Word,
    },
    select::{Keyframe, Reason, SAMPLE_INTERVAL, Selection, select},
    signature::{Sample, Signature},
    tracker::Tracker,
    typeface::Typeface,
};

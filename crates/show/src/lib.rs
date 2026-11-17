#![forbid(unsafe_code)]

mod align;
mod bundle;
mod convert;
mod demo;
mod describe;
mod draw;
mod error;
mod framing;
mod geometry;
mod image;
pub mod ink;
mod journal;
mod layout;
pub mod locate;
mod marks;
mod moment;
mod movie;
mod parallel;
mod recording;
mod select;
mod serde_range;
mod sheet;
mod signature;
mod tracker;
mod typeface;
mod wsl;

pub use crate::{
    align::{TimedClause, timed_clauses},
    bundle::{Bundle, File, Location, Manifest, Unreadable, compose},
    demo::Demo,
    draw::video::{AnnotationReport, Annotations, VideoViewport},
    error::Error,
    framing::Framing,
    geometry::{PixelRect, Point, Rect},
    image::Image,
    journal::{
        Admission, Entry, Journal, MAX_FRAME_EDGE, Source, SourceError, SourceFrame, Take,
        TakeEvent, log,
    },
    layout::{Layer, Layout, MOST_WINDOWS, Window},
    marks::{Mark, Shape},
    moment::{Moment, Span},
    movie::{MovieError, MovieNormalization, MoviePrefix, MovieSample},
    recording::{
        Action, Ending, InkPoint, Narration, NarrationLoss, Pin, Recording, Size, Spaces, Stroke,
        Word,
    },
    select::{Keyframe, Reason, SAMPLE_INTERVAL, Selection, select},
    signature::{Sample, Signature},
    tracker::Tracker,
    typeface::Typeface,
    wsl::Wsl,
};

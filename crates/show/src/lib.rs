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

pub use crate::{
    align::{TimedClause, timed_clauses},
    bundle::{Bundle, File, Manifest, Retrieval, Unreadable, compose},
    demo::Demo,
    draw::video::{AnnotationReport, Annotations, VideoViewport},
    error::Error,
    framing::Framing,
    geometry::{Point, Rect},
    image::Image,
    journal::{
        Admission, Entry, Journal, Log, MAX_FRAME_EDGE, Source, SourceError, SourceFrame, Take,
        TakeEvent,
    },
    marks::{Mark, Shape},
    moment::{Moment, Span},
    movie::{MovieError, MovieNormalization, MoviePrefix, MovieSample},
    recording::{
        Action, EmptyStroke, Ending, InkPoint, Narration, NarrationLoss, Pin, Recording, Size,
        Spaces, Stroke, Word,
    },
    select::{Keyframe, Reason, SAMPLE_INTERVAL, Selection, select},
    signature::{Sample, Signature},
    tracker::Tracker,
    typeface::Typeface,
};

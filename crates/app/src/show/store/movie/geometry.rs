//! Durable sample-indexed content rectangles and crops within the actual presented surface.

use std::{fs::File, io::Write as _, path::Path};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use speakeasy_show::{
    Image, Rect, VideoViewport,
    locate::{self, PixelRect},
};

use super::super::recall;

pub(in crate::show::store) const FILE: &str = "recording.geometry";
const MAGIC: &[u8; 8] = b"SPKGEO01";
const HEADER_BYTES: usize = 36;
const RECORD_BYTES: usize = 40;
const CHANGE_LIMIT: u32 = 9_000;
const FILE_LIMIT: u64 = HEADER_BYTES as u64 + RECORD_BYTES as u64 * CHANGE_LIMIT as u64;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Change {
    pub sample: u32,
    pub content: [u32; 4],
    pub part: [f32; 4],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Geometry {
    /// Exclusive sample index through which the content rectangles are established.
    pub through: u32,
    pub changes: Vec<Change>,
}

impl Geometry {
    pub(super) fn validate(&self, size: [u32; 2], frames: u32) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.through <= frames && self.changes.len() <= usize::try_from(CHANGE_LIMIT)?,
            "Video crop geometry exceeds its recording"
        );
        let mut previous = None;
        for &change in &self.changes {
            validate(change, previous, size)?;
            anyhow::ensure!(
                change.sample < self.through,
                "Video crop geometry exceeds its coverage"
            );
            previous = Some(change);
        }
        anyhow::ensure!(
            self.changes.is_empty() == (self.through == 0),
            "Video crop geometry has no initial rectangle"
        );
        Ok(())
    }

    pub(super) fn at(&self, sample: u32) -> Option<Change> {
        if sample >= self.through {
            return None;
        }
        self.changes
            .get(
                self.changes
                    .partition_point(|change| change.sample <= sample)
                    .checked_sub(1)?,
            )
            .copied()
    }
}

pub(super) fn viewport(movie: &super::Inspected, at: std::time::Duration) -> Option<VideoViewport> {
    let sample = movie
        .prefix
        .samples()
        .binary_search_by_key(&at, |at| super::decoder_time(*at))
        .ok()?;
    let layout = movie
        .metadata
        .geometry
        .as_ref()?
        .at(u32::try_from(sample).ok()?)?;
    let [left, top, width, height] = layout.content;
    Some(VideoViewport {
        content: PixelRect {
            left,
            top,
            width,
            height,
        },
        source: Rect::from(layout.part),
    })
}

pub(super) fn crop(
    image: Image,
    viewport: Option<VideoViewport>,
    crop: Option<super::Crop>,
) -> anyhow::Result<(Image, Option<PixelRect>)> {
    let Some(crop) = crop else {
        return Ok((image, None));
    };
    let layout = viewport
        .context("This video frame has no saved crop geometry; export it without --crop")?;
    anyhow::ensure!(
        crop.part == layout.source,
        "The mark and video frame have different or unknown geometry; export without --crop"
    );
    let PixelRect {
        left,
        top,
        width,
        height,
    } = layout.content;
    let content = image.cropped(left, top, width, height)?;
    let (image, crop) = locate::crop_image(content, Some(crop.bounds))?;
    let mut crop = crop.context("The selected mark has no crop")?;
    crop.left = crop
        .left
        .checked_add(left)
        .context("Video crop exceeds its canvas")?;
    crop.top = crop
        .top
        .checked_add(top)
        .context("Video crop exceeds its canvas")?;
    Ok((image, Some(crop)))
}

pub(in crate::show::store) struct Writing {
    file: File,
    size: [u32; 2],
    last: Option<Change>,
}

impl Writing {
    pub(super) fn create(directory: &Path, id: [u8; 16], size: [u32; 2]) -> anyhow::Result<Self> {
        let mut file = File::create_new(directory.join(FILE))?;
        file.write_all(&header(id, size))?;
        file.sync_all()?;
        Ok(Self {
            file,
            size,
            last: None,
        })
    }

    pub(in crate::show::store) fn record(
        &mut self,
        sample: u32,
        content: [u32; 4],
        part: [f32; 4],
    ) -> anyhow::Result<()> {
        let change = Change {
            sample,
            content,
            part,
        };
        validate(change, self.last, self.size)?;
        let [left, top, width, height] = content;
        let mut bytes: Vec<_> = [sample, left, top, width, height]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        bytes.extend(part.into_iter().flat_map(f32::to_le_bytes));
        bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
        self.file.write_all(&bytes)?;
        self.file.sync_data()?;
        self.last = Some(change);
        Ok(())
    }
}

pub(super) fn recover(
    directory: &Path,
    id: [u8; 16],
    size: [u32; 2],
    frames: u32,
) -> anyhow::Result<Geometry> {
    if std::fs::symlink_metadata(directory.join(FILE))
        .is_ok_and(|metadata| metadata.is_file() && metadata.len() == 0)
    {
        return Ok(Geometry::default());
    }
    let bytes = match recall::read_file(directory, FILE, FILE_LIMIT) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Geometry::default());
        },
        Err(error) => return Err(error.into()),
    };
    let expected = header(id, size);
    let Some(records) = bytes.strip_prefix(expected.as_slice()) else {
        return Ok(Geometry::default());
    };
    let (records, tail) = records.as_chunks::<RECORD_BYTES>();
    let mut complete = tail.is_empty();
    let mut changes = Vec::new();
    for record in records {
        let Some(change) = decode(record)
            .filter(|change| validate(*change, changes.last().copied(), size).is_ok())
        else {
            complete = false;
            break;
        };
        changes.push(change);
    }
    let through = if changes.is_empty() {
        0
    } else if complete {
        frames
    } else {
        changes
            .last()
            .map_or(0, |change| change.sample.saturating_add(1))
            .min(frames)
    };
    changes.retain(|change| change.sample < through);
    let geometry = Geometry { through, changes };
    geometry.validate(size, frames)?;
    Ok(geometry)
}

fn header(id: [u8; 16], size: [u32; 2]) -> Vec<u8> {
    let mut bytes = Vec::from(*MAGIC);
    bytes.extend_from_slice(&id);
    bytes.extend(size.into_iter().flat_map(u32::to_le_bytes));
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
    bytes
}

fn decode(record: &[u8; RECORD_BYTES]) -> Option<Change> {
    let fields: [[u8; 4]; 10] = record.as_chunks::<4>().0.try_into().ok()?;
    let [
        sample,
        left,
        top,
        width,
        height,
        x,
        y,
        right,
        bottom,
        checksum,
    ] = fields.map(u32::from_le_bytes);
    (crc32fast::hash(record.get(..36)?) == checksum).then_some(Change {
        sample,
        content: [left, top, width, height],
        part: [x, y, right, bottom].map(f32::from_bits),
    })
}

#[expect(
    clippy::float_cmp,
    reason = "Parts use a shared integer pixel grid; a tolerance could merge distinct captured rectangles"
)]
fn validate(
    change: Change,
    previous: Option<Change>,
    [width, height]: [u32; 2],
) -> anyhow::Result<()> {
    anyhow::ensure!(
        change.sample < CHANGE_LIMIT
            && previous.map_or_else(
                || change.sample == 0,
                |previous| {
                    change.sample > previous.sample
                        && (change.content != previous.content || change.part != previous.part)
                }
            ),
        "Video crop geometry is not in sample order"
    );
    let [x, y, right, bottom] = change.part;
    anyhow::ensure!(
        change
            .part
            .into_iter()
            .all(|edge| edge.is_finite() && (0.0..=1.0).contains(&edge))
            && x < right
            && y < bottom,
        "Video source rectangle is outside its display"
    );
    let [left, top, w, h] = change.content;
    anyhow::ensure!(
        w > 0
            && h > 0
            && left.checked_add(w).is_some_and(|right| right <= width)
            && top.checked_add(h).is_some_and(|bottom| bottom <= height),
        "Video content rectangle is outside its canvas"
    );
    Ok(())
}

#[cfg(test)]
mod tests;

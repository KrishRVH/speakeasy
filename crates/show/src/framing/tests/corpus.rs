use serde::Deserialize;

use super::*;
use crate::{InkPoint, Layer, Reason, Stroke, Window, marks};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    screen: [f32; 2],
    px_per_pt: f32,
    duration: u64,
    keyframes: Vec<Expected>,
    strokes: Vec<Vec<(f32, f32, u64)>>,
    clicks: Vec<(f32, f32, u64)>,
    drags: Vec<(f32, f32, u64, f32, f32, u64)>,
    changes: Vec<Vec<Rect>>,
    layouts: Vec<Arranged>,
    spans: Vec<Truth>,
}

#[derive(Deserialize)]
struct Arranged {
    at: u64,
    windows: Vec<Placed>,
}

#[derive(Deserialize)]
struct Placed {
    id: u32,
    app: u32,
    layer: String,
    bounds: Rect,
}

#[derive(Deserialize)]
struct Expected {
    at: u64,
    must: Vec<Rect>,
}

#[derive(Deserialize)]
struct Truth {
    start: u64,
    end: u64,
    subject: String,
    ideal: Rect,
    targets: Vec<Rect>,
}

fn corpus() -> Vec<Case> {
    serde_json::from_str(include_str!("scrawl-corpus.json")).unwrap()
}

impl Case {
    fn recording(&self) -> Recording {
        let [width, height] = self.screen;
        let mut actions: Vec<Action> = self
            .clicks
            .iter()
            .map(|&(x, y, at)| Action::Click {
                at: Moment::from_millis(at),
                point: Point::new(x, y),
            })
            .chain(
                self.drags
                    .iter()
                    .map(|&(x0, y0, t0, x1, y1, t1)| Action::Drag {
                        span: crate::Span::new(Moment::from_millis(t0), Moment::from_millis(t1)),
                        from: Point::new(x0, y0),
                        to: Point::new(x1, y1),
                    }),
            )
            .collect();
        actions.sort_by_key(|action| action.span().start());
        Recording {
            duration: Moment::from_millis(self.duration),
            surface: Size { width, height },
            strokes: self
                .strokes
                .iter()
                .map(|points| {
                    Stroke::new(
                        points
                            .iter()
                            .map(|&point| {
                                InkPoint::from((point.0, point.1, Moment::from_millis(point.2)))
                            })
                            .collect(),
                    )
                    .unwrap()
                })
                .collect(),
            actions,
            ..Recording::default()
        }
    }

    fn layouts(&self) -> Vec<Layout> {
        self.layouts
            .iter()
            .map(|arranged| Layout {
                at: Moment::from_millis(arranged.at),
                windows: arranged
                    .windows
                    .iter()
                    .map(|placed| Window {
                        id: placed.id,
                        app: placed.app,
                        layer: match placed.layer.as_str() {
                            "Normal" => Layer::Normal,
                            "Floating" => Layer::Floating,
                            "Menu" => Layer::Menu,
                            "Bar" => Layer::Bar,
                            other => panic!("{other} is not a window layer"),
                        },
                        bounds: placed.bounds,
                    })
                    .collect(),
            })
            .collect()
    }

    fn plan(&self, layouts: &[Layout]) -> Vec<Rect> {
        let recording = self.recording();
        let marks = marks::group(&recording.strokes, &recording.pins, recording.surface);
        let owner = |mark: &Mark| {
            let fit = |keyframe: &Expected| {
                keyframe
                    .must
                    .iter()
                    .map(|must| must.overlap(mark.bounds))
                    .fold(0.0, f32::max)
            };
            self.keyframes
                .iter()
                .enumerate()
                .max_by(|(_, one), (_, other)| fit(one).total_cmp(&fit(other)))
                .filter(|(_, keyframe)| fit(keyframe) > 0.5)
                .map(|(position, _)| position)
        };
        let keyframes: Vec<Keyframe> = self
            .keyframes
            .iter()
            .enumerate()
            .zip(1..)
            .map(|((position, expected), index)| {
                let held: Vec<usize> = (0..marks.len())
                    .filter(|&mark| owner(&marks[mark]) == Some(position))
                    .collect();
                let range = match (held.first(), held.last()) {
                    (Some(&first), Some(&last)) => first..last.saturating_add(1),
                    _ => 0..0,
                };
                assert_eq!(range.len(), held.len(), "{} marks interleave", self.name);
                Keyframe {
                    index,
                    at: Moment::from_millis(expected.at),
                    reason: if held.is_empty() {
                        Reason::Middle
                    } else {
                        Reason::Mark
                    },
                    event: String::new(),
                    said: None,
                    marks: range,
                    action: None,
                    crop: None,
                }
            })
            .collect();
        plan(&recording, &keyframes, &marks, &self.changes, layouts)
            .into_iter()
            .map(|plan| plan.map_or(Rect::UNIT, |plan| plan.crop))
            .collect()
    }

    fn truth(&self, at: u64) -> &Truth {
        self.spans
            .iter()
            .find(|span| span.start <= at && at < span.end)
            .or_else(|| self.spans.last())
            .unwrap()
    }

    fn legibility(&self, crop: Rect) -> f32 {
        let [width, height] = self.screen;
        let (wide, tall) = (
            crop.width() * width * self.px_per_pt,
            crop.height() * height * self.px_per_pt,
        );
        let cell = if tall > wide { 440.0 } else { 640.0 };
        let scale = (cell / wide).min(960.0 / tall);
        let shown = scale.min(1.0) * self.px_per_pt;
        ((shown / 0.8).min(1.0) - 0.5 * (scale - 1.0).max(0.0)).clamp(0.0, 1.0)
    }

    fn score(&self, crops: &[Rect]) -> Score {
        let [width, height] = self.screen;
        let mut subjects = Vec::new();
        let mut keyframes = Vec::new();
        for (expected, &crop) in self.keyframes.iter().zip(crops) {
            let truth = self.truth(expected.at);
            let slack =
                crop.expanded_within(2.0 / width.max(height), Rect::from([-1.0, -1.0, 2.0, 2.0]));
            let covered = expected
                .must
                .iter()
                .chain(&truth.targets)
                .all(|&need| slack.encloses(need));
            let context = if truth.targets.is_empty() {
                1.0
            } else {
                truth
                    .targets
                    .iter()
                    .map(|&target| {
                        let margin = (0.2 * (target.width() * width).max(target.height() * height))
                            .max(24.0);
                        let grown = Rect {
                            left: target.left - margin / width,
                            top: target.top - margin / height,
                            right: target.right + margin / width,
                            bottom: target.bottom + margin / height,
                        }
                        .intersection(Rect::UNIT);
                        grown.intersection(crop).area() / grown.area().max(1e-9)
                    })
                    .sum::<f32>()
                    / truth.targets.len() as f32
            };
            let precision = crop.intersection(truth.ideal).area() / crop.area().max(1e-9);
            let value = if covered {
                0.45 * crop.overlap(truth.ideal)
                    + 0.2 * context
                    + 0.15 * self.legibility(crop)
                    + 0.2 * precision
            } else {
                0.0
            };
            subjects.push(truth.subject.as_str());
            keyframes.push((covered, value, crop));
        }
        let steady: Vec<f32> = subjects
            .array_windows::<2>()
            .zip(keyframes.array_windows::<2>())
            .filter(|([before, after], _)| before == after)
            .map(|(_, [(.., before), (.., after)])| after.overlap(*before))
            .collect();
        let stability = if steady.is_empty() {
            1.0
        } else {
            steady.iter().sum::<f32>() / steady.len() as f32
        };
        let mean =
            keyframes.iter().map(|&(_, value, _)| value).sum::<f32>() / keyframes.len() as f32;
        Score {
            value: 0.85 * mean + 0.15 * stability,
            uncovered: keyframes
                .iter()
                .zip(1..)
                .filter(|((covered, ..), _)| !covered)
                .map(|(&(.., crop), index)| (index, crop))
                .collect(),
        }
    }
}

struct Score {
    value: f32,
    uncovered: Vec<(usize, Rect)>,
}

fn corpus_mean(window_aware: bool) -> f32 {
    let cases = corpus();
    assert_eq!(cases.len(), 13);
    let mut total = 0.0;
    for case in &cases {
        let layouts = if window_aware {
            case.layouts()
        } else {
            Vec::new()
        };
        let crops = case.plan(&layouts);
        let score = case.score(&crops);
        let whole = case.score(&vec![Rect::UNIT; crops.len()]);
        assert!(
            score.uncovered.is_empty(),
            "{} cuts off {:?}",
            case.name,
            score.uncovered
        );
        assert!(
            score.value >= whole.value,
            "{}: {:.3} vs the whole screen's {:.3}",
            case.name,
            score.value,
            whole.value
        );
        total += score.value;
    }
    total / cases.len() as f32
}

#[test]
fn crops_keep_each_keyframes_subject_and_never_lose_to_the_whole_screen() {
    let mean = corpus_mean(false);
    assert!(mean >= 0.64, "corpus mean without windows {mean:.3}");
}

#[test]
fn window_positions_frame_the_windows_the_user_worked_in() {
    let mean = corpus_mean(true);
    assert!(mean >= 0.8, "corpus mean with windows {mean:.3}");
}

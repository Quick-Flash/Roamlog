//! Map labels drawn into tile pixmaps: place names at points, road names along straightest
//! stretch. Labels crossing tile edge are dropped, since neighbor tile cannot finish them.
use super::{
    basemap::Frame,
    style::{PLACE_LABEL, ROAD_LABEL, rgba},
};
use crate::mvt::{Layer, Step};
use ab_glyph::{Font, FontVec, OutlineCurve, ScaleFont};
use std::sync::OnceLock;
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};

/// System sans font: Android Roboto, else common desktop fonts. None disables labels.
pub fn system_font() -> Option<&'static FontVec> {
    static FONT: OnceLock<Option<FontVec>> = OnceLock::new();
    FONT.get_or_init(|| {
        [
            "/system/fonts/RobotoStatic-Regular.ttf",
            "/system/fonts/Roboto-Regular.ttf",
            "/usr/share/fonts/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf",
            "/usr/share/fonts/TTF/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        ]
        .iter()
        .find_map(|p| FontVec::try_from_vec(std::fs::read(p).ok()?).ok())
    })
    .as_ref()
}

pub struct Candidate {
    pub priority: u8,
    pub text: String,
    /// CSS px.
    pub size: f64,
    /// Shown from this display zoom.
    pub min_zoom: f64,
    /// Place label, else road label.
    pub place: bool,
    /// Center and angle (radians, y down) in units of `map`.
    pub at: (f64, f64, f64),
    /// Straight run length available for text, `map` units; None for point labels.
    pub room: Option<f64>,
}

impl Candidate {
    /// (text, halo) colors.
    pub fn colors(&self, dark: bool) -> (u32, u32) {
        let theme = usize::from(dark);
        if self.place {
            PLACE_LABEL[theme]
        } else {
            ROAD_LABEL[theme]
        }
    }
}

/// Label candidates of `layers`, positions mapped by `map` (layer extent -> map fn),
/// most important first.
pub fn candidates<M: Fn(f64, f64) -> (f64, f64)>(
    layers: &[Layer],
    map: impl Fn(f64) -> M,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    for layer in layers {
        let map = map(f64::from(layer.extent.max(1)));
        for f in &layer.features {
            let name = layer.str(f, "name");
            if name.is_empty() {
                continue;
            }
            match layer.name.as_str() {
                "places" if f.kind == 1 => {
                    let min_zoom = match layer.get(f, "min_zoom") {
                        Some(crate::mvt::Value::Num(n)) => *n,
                        _ => 0.0,
                    };
                    let kind = layer.str(f, "kind");
                    let (priority, size) = match kind {
                        "country" | "region" => (0, 13.0),
                        "locality" => (1, 14.0),
                        _ => (5, 12.0),
                    };
                    let mut at = None;
                    f.steps(|s| {
                        if let (None, Step::Move(x, y)) = (at, s) {
                            let (x, y) = map(x, y);
                            at = Some((x, y, 0.0));
                        }
                    });
                    if let Some(at) = at {
                        candidates.push(Candidate {
                            priority,
                            text: name.into(),
                            size,
                            min_zoom,
                            place: true,
                            at,
                            room: None,
                        });
                    }
                }
                "roads" if f.kind == 2 => {
                    let (priority, from) = match layer.str(f, "kind") {
                        "highway" => (2, 12),
                        "major_road" => (3, 13),
                        "medium_road" => (4, 14),
                        "minor_road" => (6, 15),
                        _ => continue,
                    };
                    if let Some((at, room)) = straightest(f, &map) {
                        candidates.push(Candidate {
                            priority,
                            text: name.into(),
                            size: 11.0,
                            min_zoom: f64::from(from),
                            place: false,
                            at,
                            room: Some(room),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    candidates.sort_by(|a, b| {
        a.priority
            .cmp(&b.priority)
            .then(b.room.unwrap_or(0.0).total_cmp(&a.room.unwrap_or(0.0)))
    });
    candidates
}

/// Draws labels for `layers` into `pixmap` placed by `frame`; `unit` maps CSS px to px.
pub fn draw(
    pixmap: &mut Pixmap,
    font: &FontVec,
    layers: &[Layer],
    zoom: u8,
    frame: &Frame,
    unit: f64,
    dark: bool,
) {
    let candidates = candidates(layers, |extent| frame.map(extent));
    let edge = f64::from(pixmap.width());
    let mut placed: Vec<Rect> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for c in candidates {
        if f64::from(zoom) < c.min_zoom {
            continue;
        }
        // One label per road name per tile.
        if c.room.is_some() && names.contains(&c.text) {
            continue;
        }
        let Some((path, width)) = text_path(font, &c.text, (c.size * unit) as f32) else {
            continue;
        };
        if c.room
            .is_some_and(|room| room < f64::from(width) + 8.0 * unit)
        {
            continue;
        }
        let (x, y, angle) = c.at;
        let height = c.size * unit;
        let transform = Transform::from_translate(x as f32, y as f32)
            .pre_concat(Transform::from_rotate(angle.to_degrees() as f32))
            .pre_translate(-width / 2.0, (height * 0.35) as f32);
        let Some(bounds) = path
            .clone()
            .transform(transform)
            .map(|p| p.bounds())
            .and_then(|b| b.outset(3.0 * unit as f32, 3.0 * unit as f32))
        else {
            continue;
        };
        let inside = bounds.left() >= 0.0
            && bounds.top() >= 0.0
            && f64::from(bounds.right()) <= edge
            && f64::from(bounds.bottom()) <= edge;
        if !inside || placed.iter().any(|p| p.intersect(&bounds).is_some()) {
            continue;
        }
        placed.push(bounds);
        if c.room.is_some() {
            names.push(c.text.clone());
        }
        let mut paint = Paint {
            anti_alias: true,
            ..Paint::default()
        };
        let colors = c.colors(dark);
        let [r, g, b, a] = rgba(colors.1);
        paint.set_color_rgba8(r, g, b, a);
        let halo = Stroke {
            width: (2.5 * unit) as f32,
            line_join: tiny_skia::LineJoin::Round,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &halo, transform, None);
        let [r, g, b, a] = rgba(colors.0);
        paint.set_color_rgba8(r, g, b, a);
        pixmap.fill_path(&path, &paint, FillRule::Winding, transform, None);
    }
}

/// Longest nearly straight run of polyline: center, readable angle, and length.
fn straightest(
    f: &crate::mvt::Feature,
    map: &impl Fn(f64, f64) -> (f64, f64),
) -> Option<((f64, f64, f64), f64)> {
    let mut lines: Vec<Vec<(f64, f64)>> = Vec::new();
    f.steps(|s| match s {
        Step::Move(x, y) => lines.push(vec![map(x, y)]),
        Step::Line(x, y) => {
            if let Some(l) = lines.last_mut() {
                l.push(map(x, y));
            }
        }
        Step::Close => {}
    });
    type Run = ((f64, f64), (f64, f64), f64);
    let mut best: Option<Run> = None;
    for line in &lines {
        let mut start = 0;
        while start + 1 < line.len() {
            let heading = angle(line[start], line[start + 1]);
            let mut end = start + 1;
            while end + 1 < line.len() {
                let turn = (angle(line[end], line[end + 1]) - heading + std::f64::consts::PI)
                    .rem_euclid(std::f64::consts::TAU)
                    - std::f64::consts::PI;
                if turn.abs() > 0.2 {
                    break;
                }
                end += 1;
            }
            let (a, b) = (line[start], line[end]);
            let length = (b.0 - a.0).hypot(b.1 - a.1);
            if best.is_none_or(|(_, _, l)| length > l) {
                best = Some((a, b, length));
            }
            start = end;
        }
    }
    let (a, b, length) = best?;
    let mut theta = angle(a, b);
    // Keep text upright.
    if theta.abs() > std::f64::consts::FRAC_PI_2 {
        theta -= std::f64::consts::PI.copysign(theta);
    }
    Some((((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, theta), length))
}

fn angle(a: (f64, f64), b: (f64, f64)) -> f64 {
    (b.1 - a.1).atan2(b.0 - a.0)
}

/// Glyph outlines for `text` at `size` px, baseline at y=0; None if any glyph is missing.
fn text_path(font: &FontVec, text: &str, size: f32) -> Option<(tiny_skia::Path, f32)> {
    let scaled = font.as_scaled(size);
    let (sx, sy) = (scaled.h_scale_factor(), scaled.v_scale_factor());
    let mut builder = PathBuilder::new();
    let mut pen = 0.0;
    let mut previous = None;
    for ch in text.chars() {
        let id = font.glyph_id(ch);
        if id.0 == 0 && !ch.is_whitespace() {
            return None;
        }
        if let Some(p) = previous {
            pen += scaled.kern(p, id);
        }
        if let Some(outline) = font.outline(id) {
            let mut last = None;
            for curve in &outline.curves {
                let map = |p: ab_glyph::Point| (pen + p.x * sx, -p.y * sy);
                let (first, end) = match curve {
                    OutlineCurve::Line(a, b) => (*a, *b),
                    OutlineCurve::Quad(a, _, c) => (*a, *c),
                    OutlineCurve::Cubic(a, _, _, d) => (*a, *d),
                };
                if last != Some(first) {
                    if last.is_some() {
                        builder.close();
                    }
                    let (x, y) = map(first);
                    builder.move_to(x, y);
                }
                match curve {
                    OutlineCurve::Line(_, b) => {
                        let (x, y) = map(*b);
                        builder.line_to(x, y);
                    }
                    OutlineCurve::Quad(_, b, c) => {
                        let ((x1, y1), (x, y)) = (map(*b), map(*c));
                        builder.quad_to(x1, y1, x, y);
                    }
                    OutlineCurve::Cubic(_, b, c, d) => {
                        let ((x1, y1), (x2, y2), (x, y)) = (map(*b), map(*c), map(*d));
                        builder.cubic_to(x1, y1, x2, y2, x, y);
                    }
                }
                last = Some(end);
            }
            if last.is_some() {
                builder.close();
            }
        }
        pen += scaled.h_advance(id);
        previous = Some(id);
    }
    Some((builder.finish()?, pen))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn test_font() -> FontVec {
        FontVec::try_from_vec(
            include_bytes!("../../tests/fixtures/fonts/NotoSans-Regular.ttf").to_vec(),
        )
        .unwrap()
    }

    #[test]
    fn text_has_width_and_missing_glyphs_skip() {
        let font = test_font();
        let (_, width) = text_path(&font, "Main Street", 20.0).unwrap();
        assert!(width > 60.0 && width < 140.0, "{width}");
        assert!(text_path(&font, "東京", 20.0).is_none());
    }
}

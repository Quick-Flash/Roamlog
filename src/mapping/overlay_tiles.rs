//! Dense overlay (untraveled roads inside area) drawn on CPU into image tiles.
//!
//! Canvas strokes cost area x segments per shape on Mali every frame; a whole area of
//! roads drops panning well below display rate. Tiles render once per zoom level on worker
//! threads with tiny-skia, so panning only moves images, like the basemap.
use super::{
    basemap::{encode, tiles_at},
    camera::{Camera, TileId, WORLD},
};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use tiny_skia::{LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, Transform};
use zgui::prelude::*;
use zgui_image::ImageBytes;

/// Projected polyline with bounding box.
#[derive(Clone, Default, PartialEq)]
pub struct Polyline {
    pub points: Vec<[f64; 2]>,
    pub min: [f64; 2],
    pub max: [f64; 2],
}

/// (lines version, z, canonical x, y).
type Key = (u64, u8, u32, u32);

#[derive(Clone, Copy)]
pub struct OverlayTiles {
    lines: StoredValue<Arc<Vec<Polyline>>, LocalStorage>,
    version: StoredValue<u64, LocalStorage>,
    tiles: StoredValue<HashMap<Key, Option<ImageBytes>>, LocalStorage>,
    pending: StoredValue<HashSet<Key>, LocalStorage>,
    loaded: RwSignal<u64, LocalStorage>,
}

impl OverlayTiles {
    pub fn new() -> Self {
        Self {
            lines: StoredValue::new_local(Arc::new(Vec::new())),
            version: StoredValue::new_local(0),
            tiles: StoredValue::new_local(HashMap::new()),
            pending: StoredValue::new_local(HashSet::new()),
            loaded: RwSignal::new_local(0),
        }
    }

    pub fn set_lines(self, lines: Vec<Polyline>) {
        self.lines.set_value(Arc::new(lines));
        self.version.update_value(|v| *v += 1);
        self.tiles.update_value(HashMap::clear);
        self.loaded.update(|n| *n += 1);
    }

    /// Tiles to show; requests missing ones. Tracks arrivals.
    pub fn visible(self, camera: Camera, size: [f64; 2], dpr: f64) -> Vec<(TileId, String)> {
        self.loaded.track();
        let lines = self.lines.get_value();
        if lines.is_empty() {
            return Vec::new();
        }
        let version = self.version.get_value();
        let z = (camera.zoom.round() - 1.0).clamp(0.0, 20.0) as u8;
        let px = (512.0 * dpr).round().clamp(256.0, 2048.0) as u32;
        let mut show = Vec::new();
        let mut keep = HashSet::new();
        for tile in tiles_at(camera, size, z) {
            let key = (version, tile.z, tile.canonical_x(), tile.y);
            keep.insert(key);
            match self.tiles.with_value(|t| t.get(&key).cloned()) {
                Some(Some(image)) => show.push((tile, image.url())),
                Some(None) => {}
                None => self.request(&lines, key, px),
            }
        }
        self.tiles.update_value(|cache| {
            if cache.len() > 64 {
                cache.retain(|k, _| keep.contains(k));
            }
        });
        show
    }

    fn request(self, lines: &Arc<Vec<Polyline>>, key: Key, px: u32) {
        if self.pending.with_value(|p| p.contains(&key)) {
            return;
        }
        self.pending.update_value(|p| {
            p.insert(key);
        });
        let lines = lines.clone();
        spawn_local(async move {
            let png = blocking(move || render(&lines, key, px)).await;
            self.pending.update_value(|p| {
                p.remove(&key);
            });
            // Lines replaced meanwhile: drop stale tile.
            if key.0 == self.version.get_value() {
                self.tiles.update_value(|t| {
                    t.insert(key, png.ok().flatten().map(ImageBytes::new));
                });
                self.loaded.update(|n| *n += 1);
            }
        });
    }
}

/// PNG of tile, or `None` when no line crosses it. Tile shows at 512 CSS px.
fn render(lines: &[Polyline], (_, z, x, y): Key, px: u32) -> Result<Option<Vec<u8>>, String> {
    let span = WORLD / f64::from(1_u32 << z);
    let (left, top) = (
        -WORLD / 2.0 + f64::from(x) * span,
        WORLD / 2.0 - f64::from(y) * span,
    );
    let k = f64::from(px) / span;
    let unit = f64::from(px) / 512.0;
    let margin = 4.0 * unit / k;
    let mut path = PathBuilder::new();
    let mut any = false;
    for line in lines {
        if line.max[0] < left - margin
            || line.min[0] > left + span + margin
            || line.max[1] < top - span - margin
            || line.min[1] > top + margin
        {
            continue;
        }
        let map = |p: [f64; 2]| (((p[0] - left) * k) as f32, ((top - p[1]) * k) as f32);
        let (sx, sy) = map(line.points[0]);
        path.move_to(sx, sy);
        for &p in &line.points[1..] {
            let (px, py) = map(p);
            path.line_to(px, py);
        }
        any = true;
    }
    let Some(path) = path.finish().filter(|_| any) else {
        return Ok(None);
    };
    let mut pixmap = Pixmap::new(px, px).ok_or("Tile pixmap")?;
    let mut paint = Paint::default();
    paint.set_color_rgba8(235, 64, 64, 255);
    paint.anti_alias = true;
    let stroke = Stroke {
        width: (3.0 * unit) as f32,
        line_cap: LineCap::Round,
        line_join: LineJoin::Round,
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    encode(&pixmap).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::camera::project;

    #[test]
    fn renders_only_tiles_crossed_by_lines() {
        let a = project(-111.90, 40.86).unwrap();
        let b = project(-111.89, 40.86).unwrap();
        let line = Polyline {
            points: vec![a, b],
            min: [a[0].min(b[0]), a[1].min(b[1])],
            max: [a[0].max(b[0]), a[1].max(b[1])],
        };
        // z12 tile containing that point.
        let n = 4096.0;
        let x = ((a[0] + WORLD / 2.0) / WORLD * n) as u32;
        let y = ((WORLD / 2.0 - a[1]) / WORLD * n) as u32;
        let png = render(std::slice::from_ref(&line), (1, 12, x, y), 256).unwrap();
        assert!(png.is_some_and(|p| p.starts_with(b"\x89PNG")));
        assert!(render(&[line], (1, 12, x + 5, y), 256).unwrap().is_none());
    }
}

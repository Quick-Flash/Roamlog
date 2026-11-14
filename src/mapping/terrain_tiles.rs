//! Vector map and trip overlays rasterized once per tile on workers, then draped on DEM.
use super::{
    basemap::{self, Kind},
    camera::{TileId, WORLD},
    overlay_tiles::Polyline,
    tiles::{Payload, Raster},
};
use crate::pmtiles::Archive;
use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use zgui::prelude::*;

#[derive(Clone, PartialEq)]
pub struct Style {
    pub dark: bool,
    pub layers: Vec<Vec<Polyline>>,
}
#[derive(Clone, Copy)]
pub struct Tiles {
    pub ready: StoredValue<BTreeMap<TileId, Arc<Payload>>, LocalStorage>,
    pending: StoredValue<HashSet<(u64, TileId)>, LocalStorage>,
    pub revision: RwSignal<u64, LocalStorage>,
    pub error: RwSignal<String, LocalStorage>,
    generation: StoredValue<Arc<AtomicU64>, LocalStorage>,
    wanted: StoredValue<Arc<Mutex<HashSet<TileId>>>, LocalStorage>,
}
impl Tiles {
    pub fn new() -> Self {
        Self {
            ready: StoredValue::new_local(BTreeMap::new()),
            pending: StoredValue::new_local(HashSet::new()),
            revision: RwSignal::new_local(0),
            error: RwSignal::new_local(String::new()),
            generation: StoredValue::new_local(Arc::new(AtomicU64::new(0))),
            wanted: StoredValue::new_local(Arc::new(Mutex::new(HashSet::new()))),
        }
    }
    pub fn reset(self) {
        self.generation.get_value().fetch_add(1, Ordering::Relaxed);
        self.ready.update_value(BTreeMap::clear);
        self.error.set(String::new());
        self.revision.update(|n| *n += 1);
    }
    pub fn request(
        self,
        ids: &[TileId],
        style: Arc<Style>,
        archive: Option<Arc<Archive>>,
        online: bool,
    ) {
        let generation = self.generation.get_value();
        let version = generation.load(Ordering::Relaxed);
        let wanted: HashSet<_> = ids.iter().copied().collect();
        *self.wanted.get_value().lock().unwrap() = wanted.clone();
        self.ready.update_value(|r| {
            if r.len() > 192 {
                r.retain(|id, _| wanted.contains(id));
            }
        });
        for &id in ids {
            if self.ready.with_value(|r| r.contains_key(&id))
                || self.pending.with_value(|p| p.contains(&(version, id)))
            {
                continue;
            }
            self.pending.update_value(|p| {
                p.insert((version, id));
            });
            let (style, archive, generation) = (style.clone(), archive.clone(), generation.clone());
            let wanted = self.wanted.get_value();
            spawn_local(async move {
                let tile = blocking(move || {
                    if generation.load(Ordering::Relaxed) != version
                        || !wanted.lock().unwrap().contains(&id)
                    {
                        return Err("stale".into());
                    }
                    render(archive.as_deref(), id, &style, online, version)
                })
                .await;
                self.pending.update_value(|p| {
                    p.remove(&(version, id));
                });
                if self.generation.get_value().load(Ordering::Relaxed) == version {
                    match tile {
                        Ok(tile) => self.ready.update_value(|r| {
                            r.insert(id, Arc::new(tile));
                        }),
                        Err(error) if error != "stale" => self.error.set(error),
                        Err(_) => {}
                    }
                    self.revision.update(|n| *n += 1);
                }
            });
        }
    }
}
fn render(
    archive: Option<&Archive>,
    id: TileId,
    style: &Style,
    online: bool,
    version: u64,
) -> Result<Payload, String> {
    use tiny_skia::{
        LineCap, LineJoin, Paint, PathBuilder, PixmapPaint, Stroke, StrokeDash, Transform,
    };
    let mut map = basemap::terrain_tile(archive, id, style.dark, Kind::Map, online)?;
    let span = WORLD / f64::from(1u32 << id.z);
    let left = -WORLD / 2. + id.x as f64 * span;
    let top = WORLD / 2. - f64::from(id.y) * span;
    // Same overlay hierarchy as 2D: wider coverage, thinner GPS, dashed inference.
    let colors = [
        (60, 220, 145, 5.),
        (40, 185, 235, 5.),
        (250, 205, 50, 5.),
        (240, 100, 60, 5.),
        (80, 155, 255, 2.),
        (245, 171, 47, 2.),
        (170, 110, 255, 2.5),
        (225, 60, 225, 7.),
        (245, 80, 80, 3.),
    ];
    // Untraveled roads beneath recorded paths.
    for layer in [8, 0, 1, 2, 3, 4, 5, 6, 7] {
        let (r, g, b, width) = colors[layer];
        let mut paint = Paint::default();
        paint.set_color_rgba8(r, g, b, 255);
        paint.anti_alias = true;
        for line in &style.layers[layer] {
            if line.max[0] < left
                || line.min[0] > left + span
                || line.max[1] < top - span
                || line.min[1] > top
            {
                continue;
            }
            let mut path = PathBuilder::new();
            for (i, p) in line.points.iter().enumerate() {
                let (x, y) = (
                    ((p[0] - left) / span * 512.) as f32,
                    ((top - p[1]) / span * 512.) as f32,
                );
                if i == 0 {
                    path.move_to(x, y);
                } else {
                    path.line_to(x, y);
                }
            }
            if let Some(path) = path.finish() {
                map.stroke_path(
                    &path,
                    &paint,
                    &Stroke {
                        width,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Round,
                        dash: if layer == 5 {
                            StrokeDash::new(vec![5., 4.], 0.)
                        } else {
                            None
                        },
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
        }
    }
    let labels = basemap::terrain_tile(archive, id, style.dark, Kind::Labels, online)?;
    map.draw_pixmap(
        0,
        0,
        labels.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    // tiny-skia stores premultiplied RGBA; opaque map means bytes already straight RGBA.
    Ok(Payload::Raster(Raster {
        version,
        encoded: Vec::new(),
        rgba: map.take(),
        width: 512,
        height: 512,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn vector_tile_preserves_trip_overlay_and_light_dark_styling() {
        let archive = Archive::open(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/downtown.pmtiles")
                .as_path(),
        )
        .unwrap();
        let p = super::super::camera::project(archive.center[0], archive.center[1]).unwrap();
        let z = 15;
        let span = WORLD / f64::from(1u32 << z);
        let id = TileId {
            z,
            x: ((p[0] + WORLD / 2.) / span).floor() as i64,
            y: ((WORLD / 2. - p[1]) / span).floor() as u32,
        };
        let mut style = Style {
            dark: false,
            layers: vec![Vec::new(); 9],
        };
        let Payload::Raster(light) = render(Some(&archive), id, &style, false, 1).unwrap() else {
            panic!("Expected raster");
        };
        style.dark = true;
        let Payload::Raster(dark) = render(Some(&archive), id, &style, false, 2).unwrap() else {
            panic!("Expected raster");
        };
        assert_ne!(light.rgba, dark.rgba);
        let left = -WORLD / 2. + id.x as f64 * span;
        let top = WORLD / 2. - f64::from(id.y) * span;
        let a = [left + span * 0.1, top - span * 0.5];
        let b = [left + span * 0.9, top - span * 0.5];
        style.layers[4].push(Polyline {
            points: vec![a, b],
            min: [a[0], a[1]],
            max: [b[0], b[1]],
        });
        let Payload::Raster(with_path) = render(Some(&archive), id, &style, false, 3).unwrap()
        else {
            panic!("Expected raster");
        };
        let blue = |rgba: &[u8]| {
            rgba.chunks_exact(4)
                .filter(|p| *p == [80, 155, 255, 255])
                .count()
        };
        assert!(blue(&with_path.rgba) > blue(&dark.rgba) + 200);
    }
}

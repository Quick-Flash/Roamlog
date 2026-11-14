//! Vector map and trip overlays rasterized once per tile on workers, then draped on DEM.
use super::{
    basemap::{self, Kind},
    camera::{Camera, TileId, WORLD},
    overlay_tiles::Polyline,
    terrain::Orbit,
    tiles::{Payload, Raster},
};
use crate::pmtiles::Archive;
use std::{
    collections::{BTreeMap, HashSet},
    hash::{Hash, Hasher},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use zgui::prelude::*;

#[derive(Clone, PartialEq)]
pub struct Style {
    pub dark: bool,
    pub satellite: bool,
    pub layers: Vec<Vec<Polyline>>,
}
type SourceKey = Option<(TileId, u64)>;
type RequestKey = (u64, TileId, SourceKey);
type Imagery = BTreeMap<TileId, (TileId, Arc<Payload>)>;
#[derive(Clone, Copy)]
pub struct Tiles {
    pub ready: StoredValue<BTreeMap<TileId, Arc<Payload>>, LocalStorage>,
    pending: StoredValue<HashSet<RequestKey>, LocalStorage>,
    sources: StoredValue<BTreeMap<TileId, SourceKey>, LocalStorage>,
    pub revision: RwSignal<u64, LocalStorage>,
    pub error: RwSignal<String, LocalStorage>,
    generation: StoredValue<Arc<AtomicU64>, LocalStorage>,
    wanted: StoredValue<Arc<Mutex<BTreeMap<TileId, RequestKey>>>, LocalStorage>,
}
impl Tiles {
    pub fn new() -> Self {
        Self {
            ready: StoredValue::new_local(BTreeMap::new()),
            pending: StoredValue::new_local(HashSet::new()),
            sources: StoredValue::new_local(BTreeMap::new()),
            revision: RwSignal::new_local(0),
            error: RwSignal::new_local(String::new()),
            generation: StoredValue::new_local(Arc::new(AtomicU64::new(0))),
            wanted: StoredValue::new_local(Arc::new(Mutex::new(BTreeMap::new()))),
        }
    }
    pub fn reset(self) {
        self.generation.get_value().fetch_add(1, Ordering::Relaxed);
        self.ready.update_value(BTreeMap::clear);
        self.sources.update_value(BTreeMap::clear);
        self.error.set(String::new());
        self.revision.update(|n| *n += 1);
    }
    pub fn request(
        self,
        ids: &[TileId],
        style: Arc<Style>,
        archive: Option<Arc<Archive>>,
        online: bool,
        imagery: &Imagery,
    ) {
        let generation = self.generation.get_value();
        let version = generation.load(Ordering::Relaxed);
        let wanted: BTreeMap<_, _> = ids
            .iter()
            .filter_map(|&id| {
                let incoming = imagery
                    .get(&id)
                    .map(|(id, payload)| (*id, payload.version()));
                let cached = self.sources.with_value(|s| s.get(&id).copied().flatten());
                let source = best_source(cached, incoming);
                (!style.satellite || source.is_some()).then_some((id, (version, id, source)))
            })
            .collect();
        *self.wanted.get_value().lock().unwrap() = wanted.clone();
        self.ready.update_value(|r| {
            if r.len() > 192 {
                r.retain(|id, _| wanted.contains_key(id));
            }
        });
        self.ready.with_value(|ready| {
            self.sources
                .update_value(|sources| sources.retain(|id, _| ready.contains_key(id)))
        });
        for (id, key) in wanted {
            let source = key.2;
            if self.ready.with_value(|r| r.contains_key(&id))
                && self.sources.with_value(|s| s.get(&id) == Some(&source))
                || self.pending.with_value(|p| p.contains(&key))
            {
                continue;
            }
            self.pending.update_value(|p| {
                p.insert(key);
            });
            let photo = imagery.get(&id).cloned();
            let (style, archive, generation) = (style.clone(), archive.clone(), generation.clone());
            let wanted = self.wanted.get_value();
            spawn_local(async move {
                let tile = blocking(move || {
                    if generation.load(Ordering::Relaxed) != version
                        || wanted.lock().unwrap().get(&id) != Some(&key)
                    {
                        return Err("stale".into());
                    }
                    let mut hash = std::collections::hash_map::DefaultHasher::new();
                    key.hash(&mut hash);
                    render(
                        archive.as_deref(),
                        id,
                        &style,
                        online,
                        hash.finish(),
                        photo.as_ref(),
                    )
                })
                .await;
                self.pending.update_value(|p| {
                    p.remove(&key);
                });
                if self.generation.get_value().load(Ordering::Relaxed) == version
                    && self.wanted.get_value().lock().unwrap().get(&id) == Some(&key)
                {
                    match tile {
                        Ok(tile) => {
                            self.ready.update_value(|r| {
                                r.insert(id, Arc::new(tile));
                            });
                            self.sources.update_value(|s| {
                                s.insert(id, source);
                            });
                        }
                        Err(error) if error != "stale" => self.error.set(error),
                        Err(_) => {}
                    }
                    self.revision.update(|n| *n += 1);
                }
            });
        }
    }
}
/// A cached sharper composite remains useful even when raw source was evicted.
fn best_source(cached: Option<(TileId, u64)>, incoming: Option<(TileId, u64)>) -> SourceKey {
    match (cached, incoming) {
        (Some(old), Some(new)) if old.0.z > new.0.z => Some(old),
        (_, Some(new)) => Some(new),
        (old, None) => old,
    }
}
/// Keep cached sharper imagery, but do not let old coarse tiles starve upgrades.
pub(super) fn prioritize_detail(
    requested: Vec<TileId>,
    mut cached: Vec<TileId>,
    fallback: &[TileId],
) -> Vec<TileId> {
    cached.retain(|old| {
        !requested
            .iter()
            .any(|detail| detail.z > old.z && super::terrain::contains(*old, *detail))
    });
    cached.extend(requested);
    cached.extend(fallback.iter().copied());
    let mut seen = std::collections::BTreeSet::new();
    cached
        .into_iter()
        .filter(|id| seen.insert(*id))
        .take(256)
        .collect()
}
/// Separate imagery LOD: retain sharp descendants when terrain mesh coarsens.
pub(super) fn detail_tiles(
    orbit: Orbit,
    camera: Camera,
    size: [f64; 2],
    dpr: f64,
    leaves: &[TileId],
) -> Vec<TileId> {
    // Split largest projected tiles first. Budget bounds selection work as well as downloads.
    let score = |tile| orbit.projected_tile_size(camera, size, tile) * dpr;
    let mut tiles: std::collections::BTreeSet<_> = leaves.iter().copied().collect();
    let mut queue = std::collections::BinaryHeap::new();
    for &id in leaves {
        queue.push((score(id).to_bits(), id));
    }
    while let Some((pixels, id)) = queue.pop() {
        if f64::from_bits(pixels) <= 256. || id.z >= 19 {
            continue;
        }
        if tiles.len() + 3 > 192 {
            break;
        }
        tiles.remove(&id);
        for y in 0..2 {
            for x in 0..2 {
                let child = TileId {
                    z: id.z + 1,
                    x: id.x * 2 + x,
                    y: id.y * 2 + y,
                };
                tiles.insert(child);
                queue.push((score(child).to_bits(), child));
            }
        }
    }
    let mut tiles: Vec<_> = tiles.into_iter().collect();
    orbit.screen_first(camera, size, &mut tiles);
    tiles
}

fn render(
    archive: Option<&Archive>,
    id: TileId,
    style: &Style,
    online: bool,
    version: u64,
    imagery: Option<&(TileId, Arc<Payload>)>,
) -> Result<Payload, String> {
    use tiny_skia::{
        LineCap, LineJoin, Paint, PathBuilder, PixmapPaint, Stroke, StrokeDash, Transform,
    };
    let dark = style.dark || style.satellite;
    let mut map = if style.satellite {
        let (source, payload) = imagery.ok_or("Satellite tile unavailable")?;
        let Payload::Raster(photo) = payload.as_ref() else {
            return Err("Invalid satellite tile".into());
        };
        let rgba = image::RgbaImage::from_raw(photo.width, photo.height, photo.rgba.clone())
            .ok_or("Invalid satellite pixels")?;
        let ratio = 1u32 << (id.z - source.z);
        let width = photo.width / ratio;
        let height = photo.height / ratio;
        if width == 0 || height == 0 {
            return Err("Satellite fallback too coarse".into());
        }
        let crop = image::imageops::crop_imm(
            &rgba,
            id.canonical_x() % ratio * width,
            id.y % ratio * height,
            width,
            height,
        );
        let pixels = image::imageops::resize(
            &crop.to_image(),
            512,
            512,
            image::imageops::FilterType::Triangle,
        )
        .into_raw();
        let mut map =
            tiny_skia::Pixmap::from_vec(pixels, tiny_skia::IntSize::from_wh(512, 512).unwrap())
                .ok_or("Invalid satellite image")?;
        let roads = basemap::terrain_tile(archive, id, dark, Kind::Roads, online)?;
        map.draw_pixmap(
            0,
            0,
            roads.as_ref(),
            &PixmapPaint::default(),
            Transform::identity(),
            None,
        );
        map
    } else {
        basemap::terrain_tile(archive, id, dark, Kind::Map, online)?
    };
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
    let labels = basemap::terrain_tile(archive, id, dark, Kind::Labels, online)?;
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
            satellite: false,
            layers: vec![Vec::new(); 9],
        };
        let Payload::Raster(light) = render(Some(&archive), id, &style, false, 1, None).unwrap()
        else {
            panic!("Expected raster");
        };
        style.dark = true;
        let Payload::Raster(dark) = render(Some(&archive), id, &style, false, 2, None).unwrap()
        else {
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
        let Payload::Raster(with_path) =
            render(Some(&archive), id, &style, false, 3, None).unwrap()
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

#[cfg(test)]
mod satellite_tests {
    use super::*;
    #[test]
    fn satellite_crops_parent_and_keeps_photography_same_in_both_themes() {
        let parent = TileId { z: 2, x: 1, y: 1 };
        let child = TileId { z: 3, x: 3, y: 2 };
        let mut rgba = vec![0; 256 * 256 * 4];
        for y in 0..256 {
            for x in 0..256 {
                let color = if x >= 128 && y < 128 {
                    [180, 40, 20, 255]
                } else {
                    [20, 60, 180, 255]
                };
                rgba[(y * 256 + x) * 4..(y * 256 + x + 1) * 4].copy_from_slice(&color);
            }
        }
        let photo = (
            parent,
            Arc::new(Payload::Raster(Raster {
                version: 99,
                encoded: Vec::new(),
                rgba,
                width: 256,
                height: 256,
            })),
        );
        let mut style = Style {
            dark: false,
            satellite: true,
            layers: vec![Vec::new(); 9],
        };
        let Payload::Raster(light) = render(None, child, &style, false, 1, Some(&photo)).unwrap()
        else {
            panic!("Expected raster");
        };
        style.dark = true;
        let Payload::Raster(dark) = render(None, child, &style, false, 2, Some(&photo)).unwrap()
        else {
            panic!("Expected raster");
        };
        assert_eq!(light.rgba, dark.rgba);
        assert!(light.rgba.chunks_exact(4).all(|p| p == [180, 40, 20, 255]));
        let span = WORLD / f64::from(1u32 << child.z);
        let left = -WORLD / 2. + child.x as f64 * span;
        let top = WORLD / 2. - f64::from(child.y) * span;
        let a = [left + 0.1 * span, top - 0.5 * span];
        let b = [left + 0.9 * span, top - 0.5 * span];
        style.layers[0].push(Polyline {
            points: vec![a, b],
            min: a,
            max: b,
        });
        let Payload::Raster(overlay) = render(None, child, &style, false, 3, Some(&photo)).unwrap()
        else {
            panic!("Expected raster");
        };
        assert!(
            overlay
                .rgba
                .chunks_exact(4)
                .filter(|p| *p == [60, 220, 145, 255])
                .count()
                > 200
        );
    }
}

#[cfg(test)]
mod quality_tests {
    use super::*;
    #[test]
    fn cached_detail_survives_coarser_or_evicted_source_and_accepts_upgrades() {
        let old = (TileId { z: 17, x: 3, y: 4 }, 10);
        let coarse = (TileId { z: 15, x: 0, y: 1 }, 20);
        assert_eq!(best_source(Some(old), Some(coarse)), Some(old));
        assert_eq!(best_source(Some(old), None), Some(old));
        assert_eq!(best_source(Some(old), Some((old.0, 30))), Some((old.0, 30)));
        let finer = (TileId { z: 18, x: 6, y: 8 }, 40);
        assert_eq!(best_source(Some(old), Some(finer)), Some(finer));
    }
    #[test]
    fn imagery_refines_for_device_pixels_with_bounded_work() {
        let leaf = TileId {
            z: 14,
            x: 8192,
            y: 8192,
        };
        let span = WORLD / f64::from(1u32 << leaf.z);
        let camera = Camera {
            center: [span / 2., -span / 2.],
            zoom: 14.,
        };
        let orbit = Orbit::default();
        let size = [400., 700.];
        let normal = detail_tiles(orbit, camera, size, 1., &[leaf]);
        let sharp = detail_tiles(orbit, camera, size, 3., &[leaf]);
        assert!(sharp.len() > normal.len());
        assert!(
            sharp
                .iter()
                .all(|t| super::super::terrain::contains(leaf, *t))
        );
        let leaves = super::super::terrain::Selector::default().select(orbit, camera, size);
        assert!(detail_tiles(orbit, camera, size, 4., &leaves).len() <= 192);
    }
    #[test]
    fn offscreen_cached_composite_keeps_its_source_quality_metadata() {
        let window = zgui_testkit_view::Window::open();
        window.scope.with(|| {
            let tiles = Tiles::new();
            let id = TileId { z: 15, x: 0, y: 0 };
            let source = Some((id, 123));
            tiles.ready.update_value(|r| {
                r.insert(
                    id,
                    Arc::new(Payload::Raster(Raster {
                        version: 1,
                        encoded: vec![],
                        rgba: vec![0; 4],
                        width: 1,
                        height: 1,
                    })),
                );
            });
            tiles.sources.update_value(|s| {
                s.insert(id, source);
            });
            tiles.request(
                &[],
                Arc::new(Style {
                    dark: false,
                    satellite: true,
                    layers: vec![Vec::new(); 9],
                }),
                None,
                false,
                &BTreeMap::new(),
            );
            assert_eq!(
                tiles.sources.with_value(|s| s.get(&id).copied()),
                Some(source)
            );
        });
    }
}

#[cfg(test)]
mod request_priority_tests {
    use super::*;
    #[test]
    fn stale_coarse_cache_cannot_starve_finer_requests_but_sharp_cache_stays_first() {
        let parent = TileId {
            z: 10,
            x: 10,
            y: 10,
        };
        let children: Vec<_> = (0..4)
            .map(|i| TileId {
                z: 11,
                x: 20 + i % 2,
                y: 20 + (i / 2) as u32,
            })
            .collect();
        let fine = TileId {
            z: 12,
            x: 40,
            y: 40,
        };
        let order = prioritize_detail(children.clone(), vec![parent, fine, fine], &[parent]);
        assert_eq!(order[0], fine);
        assert_eq!(&order[1..5], children.as_slice());
        assert_eq!(order[5], parent);
    }
}

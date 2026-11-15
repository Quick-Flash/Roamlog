//! Base map geometry for GPU drawing: vector tiles decoded once on worker threads into
//! fill fans and line segments grouped by style slot. Drawing scales them to any zoom,
//! so map stays sharp between levels.
use super::{
    basemap::tiles_at,
    camera::{Camera, TileId, WORLD},
    download,
    labels::{self, Candidate},
    style::{Paint, STYLES, classify},
};
use crate::{
    mvt::{self, Step},
    pmtiles::Archive,
};
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
};
use zgui::prelude::*;

/// Decoded tiles kept beyond visible set, for panning back and zoom fallback.
const CACHE: usize = 96;

/// One data tile. Positions are metres from tile's north-west corner, y up.
#[derive(Default)]
pub struct Tile {
    /// Triangle-fan triangles, three vertices each, for stencil fill.
    pub fills: Vec<[f32; 2]>,
    /// Segments (a, b).
    pub segments: Vec<[f32; 4]>,
    /// (style slot, range, bounds [min x, min y, max x, max y]) in `fills`, in paint order.
    pub fill_runs: Vec<(usize, Range<u32>, [f32; 4])>,
    /// (style slot, range) in `segments`, in paint order.
    pub line_runs: Vec<(usize, Range<u32>)>,
    /// Label candidates, most important first; positions in metres from north-west
    /// corner, y down.
    pub labels: Vec<Candidate>,
}

impl Tile {
    /// Geometry of MVT tile at zoom `z`.
    pub fn build(bytes: &[u8], z: u8) -> Result<Self, String> {
        let span = WORLD / f64::from(1_u32 << z);
        let mut fills: Vec<Vec<[f32; 2]>> = STYLES.iter().map(|_| Vec::new()).collect();
        let mut lines: Vec<Vec<[f32; 4]>> = STYLES.iter().map(|_| Vec::new()).collect();
        let layers = mvt::decode(bytes)?;
        for layer in &layers {
            let k = span / f64::from(layer.extent.max(1));
            let point = |x: f64, y: f64| [(x * k) as f32, (-y * k) as f32];
            for feature in &layer.features {
                let Some(slot) = classify(layer, feature) else {
                    continue;
                };
                match STYLES[slot].paint {
                    Paint::Fill { .. } => fan(&mut fills[slot], feature, point),
                    Paint::Line(_) => strokes(&mut lines[slot], feature, point),
                }
            }
        }
        let mut tile = Self {
            labels: labels::candidates(&layers, |extent| {
                let k = span / extent;
                move |x, y| (x * k, y * k)
            }),
            ..Self::default()
        };
        for (slot, f) in fills.into_iter().enumerate() {
            if !f.is_empty() {
                let start = tile.fills.len() as u32;
                let bounds = f
                    .iter()
                    .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| {
                        [
                            b[0].min(p[0]),
                            b[1].min(p[1]),
                            b[2].max(p[0]),
                            b[3].max(p[1]),
                        ]
                    });
                tile.fills.extend(f);
                tile.fill_runs
                    .push((slot, start..tile.fills.len() as u32, bounds));
            }
        }
        for (slot, l) in lines.into_iter().enumerate() {
            if !l.is_empty() {
                let start = tile.segments.len() as u32;
                tile.segments.extend(l);
                tile.line_runs
                    .push((slot, start..tile.segments.len() as u32));
            }
        }
        Ok(tile)
    }
}

/// Fan triangles per ring from its first vertex; stencil counting gives non-zero winding,
/// so holes and overlaps need no triangulation.
pub fn fan(out: &mut Vec<[f32; 2]>, feature: &mvt::Feature, point: impl Fn(f64, f64) -> [f32; 2]) {
    let (mut first, mut prev) = ([0.0; 2], None);
    feature.steps(|s| match s {
        Step::Move(x, y) => {
            first = point(x, y);
            prev = None;
        }
        Step::Line(x, y) => {
            let p = point(x, y);
            if let Some(q) = prev {
                out.extend([first, q, p]);
            }
            prev = Some(p);
        }
        Step::Close => prev = None,
    });
}

fn strokes(out: &mut Vec<[f32; 4]>, feature: &mvt::Feature, point: impl Fn(f64, f64) -> [f32; 2]) {
    let (mut start, mut prev) = ([0.0; 2], [0.0; 2]);
    feature.steps(|s| match s {
        Step::Move(x, y) => {
            start = point(x, y);
            prev = start;
        }
        Step::Line(x, y) => {
            let p = point(x, y);
            out.push([prev[0], prev[1], p[0], p[1]]);
            prev = p;
        }
        Step::Close => out.push([prev[0], prev[1], start[0], start[1]]),
    });
}

/// (z, canonical x, y).
type Key = (u8, u32, u32);

#[derive(Clone, Copy)]
pub struct VectorTiles {
    /// None marks tile absent or failed; nothing drawn there.
    tiles: StoredValue<HashMap<Key, Option<Arc<Tile>>>, LocalStorage>,
    pending: StoredValue<HashSet<Key>, LocalStorage>,
    /// Bumped when tile arrives.
    loaded: RwSignal<u64, LocalStorage>,
    /// Online state of last pass; change retries absent tiles.
    online: StoredValue<bool, LocalStorage>,
    /// Data level in view; queued builds far from it are skipped as stale.
    level: StoredValue<Arc<AtomicU8>, LocalStorage>,
}

fn key(t: TileId) -> Key {
    (t.z, t.canonical_x(), t.y)
}

impl VectorTiles {
    pub fn new() -> Self {
        Self {
            tiles: StoredValue::new_local(HashMap::new()),
            pending: StoredValue::new_local(HashSet::new()),
            loaded: RwSignal::new_local(0),
            online: StoredValue::new_local(true),
            level: StoredValue::new_local(Arc::new(AtomicU8::new(0))),
        }
    }

    fn request(self, archive: &Arc<Archive>, key: Key, online: bool) {
        if self.tiles.with_value(|t| t.contains_key(&key))
            || self.pending.with_value(|p| p.contains(&key))
        {
            return;
        }
        self.pending.update_value(|p| {
            p.insert(key);
        });
        let archive = archive.clone();
        let level = self.level.get_value();
        spawn_local(async move {
            let (z, x, y) = key;
            let result = blocking(move || {
                let now = level.load(Ordering::Relaxed);
                if z > now + 1 || z + 5 < now {
                    return Err("stale".to_string());
                }
                let build = |archive: &Archive| {
                    archive
                        .tile(z, x, y)?
                        .map(|bytes| Tile::build(&bytes, z))
                        .transpose()
                };
                // Beyond downloaded area: same data from latest online build.
                match build(&archive)? {
                    Some(tile) => Ok(Some(tile)),
                    None if online => build(&*download::remote()?),
                    None => Ok(None),
                }
            })
            .await;
            self.pending.update_value(|p| {
                p.remove(&key);
            });
            if matches!(&result, Err(e) if e == "stale") {
                return;
            }
            self.tiles.update_value(|t| {
                t.insert(key, result.ok().flatten().map(Arc::new));
            });
            self.loaded.update(|n| *n += 1);
        });
    }

    fn cached(self, t: TileId) -> Option<Option<Arc<Tile>>> {
        self.tiles.with_value(|c| c.get(&key(t)).cloned())
    }

    /// Requests tiles covering view and returns what to draw, coarse fallbacks first.
    /// Tracks arrivals, so callers re-run as tiles load.
    pub fn visible(
        self,
        archive: &Arc<Archive>,
        camera: Camera,
        size: [f64; 2],
        online: bool,
    ) -> Vec<(TileId, Arc<Tile>)> {
        self.loaded.track();
        if self.online.get_value() != online {
            self.online.set_value(online);
            self.tiles.update_value(|t| t.retain(|_, v| v.is_some()));
        }
        // Same data level CPU tiles use, capped at archive max: vectors overzoom freely.
        let level = |zoom: f64| (zoom.round() - 1.0).clamp(0.0, f64::from(archive.max_zoom)) as u8;
        let z = level(camera.zoom);
        self.level.with_value(|l| l.store(z, Ordering::Relaxed));
        let wanted = tiles_at(camera, size, z);
        let mut keep = HashSet::new();
        let mut show: Vec<(TileId, Arc<Tile>)> = Vec::new();
        let mut add = |t: TileId, tile: Arc<Tile>| {
            if !show.iter().any(|(s, _)| *s == t) {
                show.push((t, tile));
            }
        };
        for &tile in &wanted {
            self.request(archive, key(tile), online);
            keep.insert(key(tile));
            match self.cached(tile) {
                Some(Some(g)) => add(tile, g),
                Some(None) => {}
                None => {
                    // Finer tiles after zooming out, else coarser one after zooming in.
                    let (finer, covered) = self.descendants(tile, 2);
                    if !covered && let Some((a, g)) = self.ancestor(tile) {
                        keep.insert(key(a));
                        add(a, g);
                    }
                    for (c, g) in finer {
                        keep.insert(key(c));
                        add(c, g);
                    }
                }
            }
        }
        let sharp = wanted.iter().all(|t| self.cached(*t).is_some());
        // Once sharp: adjacent level near its switch point, and coarse ring for zoom-outs.
        if sharp {
            let next = match camera.zoom.rem_euclid(1.0) {
                f if (0.3..0.5).contains(&f) => Some(level(camera.zoom + 0.5)),
                f if (0.5..0.7).contains(&f) => Some(level(camera.zoom - 0.5)),
                _ => None,
            };
            let mut more = next
                .filter(|&n| n != z)
                .map(|n| tiles_at(camera, size, n))
                .unwrap_or_default();
            more.extend(tiles_at(
                camera,
                [size[0] * 2.0, size[1] * 2.0],
                z.saturating_sub(2),
            ));
            for tile in more {
                keep.insert(key(tile));
                self.request(archive, key(tile), online);
            }
        }
        for t in &wanted {
            keep.insert((t.z.saturating_sub(1), t.canonical_x() / 2, t.y / 2));
        }
        self.tiles.update_value(|cache| {
            if cache.len() > CACHE {
                cache.retain(|k, _| keep.contains(k));
            }
        });
        show.sort_by_key(|(t, _)| t.z);
        show
    }

    fn ancestor(self, tile: TileId) -> Option<(TileId, Arc<Tile>)> {
        let mut t = tile;
        while t.z > 0 && tile.z - t.z < 6 {
            t = TileId {
                z: t.z - 1,
                x: t.x.div_euclid(2),
                y: t.y / 2,
            };
            match self.cached(t) {
                Some(Some(g)) => return Some((t, g)),
                Some(None) => return None,
                None => {}
            }
        }
        None
    }

    fn descendants(self, tile: TileId, depth: u8) -> (Vec<(TileId, Arc<Tile>)>, bool) {
        if depth == 0 {
            return (Vec::new(), false);
        }
        let mut found = Vec::new();
        let mut complete = true;
        for i in 0..4 {
            let c = TileId {
                z: tile.z + 1,
                x: tile.x * 2 + i % 2,
                y: tile.y * 2 + (i / 2) as u32,
            };
            match self.cached(c) {
                Some(Some(g)) => found.push((c, g)),
                Some(None) => {}
                None => {
                    let (more, covered) = self.descendants(c, depth - 1);
                    found.extend(more);
                    complete &= covered;
                }
            }
        }
        (found, complete)
    }
}

/// North-west corner of tile in projected metres.
pub fn corner(t: TileId) -> [f64; 2] {
    let span = WORLD / f64::from(1_u32 << t.z);
    [
        t.x as f64 * span - WORLD / 2.0,
        WORLD / 2.0 - f64::from(t.y) * span,
    ]
}

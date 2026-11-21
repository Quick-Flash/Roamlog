//! Offline vector basemap from local PMTiles, drawn on CPU into image tiles.
//!
//! Zgui draws canvas paths on Mali-G715 (Pixel 9) with coverage rasterizer whose cost is
//! area x segments per shape; full map geometry hangs GPU. Tiles instead render once per
//! theme on worker threads with tiny-skia and display like raster tiles, so pan/zoom costs
//! only image placement.
use super::{
    camera::{Camera, TileId, WORLD},
    download, labels,
    style::{BACKGROUND, STYLES, classify},
};
use crate::{
    mvt::{self, Step},
    pmtiles::Archive,
};
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tiny_skia::{FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};
use zgui::prelude::*;
use zgui_image::ImageBytes;

/// Rendered tiles kept beyond visible set, for panning back and zoom fallback.
const CACHE: usize = 160;

/// Deepest display tile level; vectors overzoom past archive data.
const MAX_LEVEL: u8 = 20;

/// What a tile layer draws. Labels sit in their own layer above trip/road overlays,
/// so street names stay readable over traveled and untraveled lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// Opaque background, areas and roads.
    Map,
    /// Roads only, transparent, over satellite imagery.
    Roads,
    /// Labels only, transparent.
    Labels,
}

/// (z, canonical x, y, dark, kind).
type Key = (u8, u32, u32, bool, Kind);

/// Where display tile sits inside data tile: past archive max zoom, display tiles
/// crop and enlarge their max-zoom ancestor so vectors stay sharp.
pub struct Frame {
    px: f64,
    /// Display zoom minus data zoom.
    depth: u8,
    /// Display tile position within data tile, in display tiles.
    sub: (u32, u32),
}

impl Frame {
    /// Tile units of layer with `extent` to pixmap px.
    pub fn map(&self, extent: f64) -> impl Fn(f64, f64) -> (f64, f64) + use<> {
        let k = self.px * f64::from(1_u32 << self.depth) / extent;
        let (dx, dy) = (
            f64::from(self.sub.0) * self.px,
            f64::from(self.sub.1) * self.px,
        );
        move |x, y| (x * k - dx, y * k - dy)
    }
}

/// PNG of display tile at zoom `z`, `px` square. Tile shows at 512 CSS px at zoom z+1,
/// so style widths use that zoom scaled to render pixels.
fn render(
    bytes: &[u8],
    z: u8,
    frame: &Frame,
    dark: bool,
    kind: Kind,
    font: Option<&ab_glyph::FontVec>,
) -> Result<Vec<u8>, String> {
    let zoom = z + 1;
    let px = frame.px as u32;
    let unit = frame.px / 512.0;
    let layers = mvt::decode(bytes)?;
    let mut slots: Vec<PathBuilder> = STYLES.iter().map(|_| PathBuilder::new()).collect();
    for layer in &layers {
        let map = frame.map(f64::from(layer.extent.max(1)));
        let point = |x: f64, y: f64| {
            let (x, y) = map(x, y);
            (x as f32, y as f32)
        };
        if kind == Kind::Labels || (kind == Kind::Roads && layer.name != "roads") {
            continue;
        }
        for feature in &layer.features {
            let Some(slot) = classify(layer, feature).filter(|&s| STYLES[s].visible(zoom)) else {
                continue;
            };
            let builder = &mut slots[slot];
            feature.steps(|s| match s {
                Step::Move(x, y) => {
                    let (x, y) = point(x, y);
                    builder.move_to(x, y)
                }
                Step::Line(x, y) => {
                    let (x, y) = point(x, y);
                    builder.line_to(x, y)
                }
                Step::Close => builder.close(),
            });
        }
    }
    let mut pixmap = Pixmap::new(px, px).ok_or("Tile size invalid")?;
    if kind == Kind::Map {
        pixmap.fill(color(BACKGROUND.rgba(dark, zoom)));
    }
    for (style, builder) in STYLES.iter().zip(slots) {
        let Some(path) = builder.finish() else {
            continue;
        };
        let mut paint = Paint::default();
        paint.set_color(color(style.rgba(dark, zoom)));
        paint.anti_alias = true;
        match style.width(zoom) {
            Some(width) => {
                let stroke = Stroke {
                    width: (width * unit) as f32,
                    line_cap: tiny_skia::LineCap::Round,
                    line_join: tiny_skia::LineJoin::Round,
                    ..Stroke::default()
                };
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
            None => pixmap.fill_path(
                &path,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                None,
            ),
        }
    }
    if kind == Kind::Labels
        && let Some(font) = font
    {
        labels::draw(&mut pixmap, font, &layers, zoom, frame, unit, dark);
    }
    encode(&pixmap)
}

/// PNG with fast settings: tiles live in memory briefly, so speed beats size.
pub(super) fn encode(pixmap: &Pixmap) -> Result<Vec<u8>, String> {
    let mut rgba = Vec::with_capacity(pixmap.data().len());
    for p in pixmap.pixels() {
        let c = p.demultiply();
        rgba.extend([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_compression(png::Compression::Fast);
    encoder.set_filter(png::FilterType::NoFilter);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(&rgba).map_err(|e| e.to_string())?;
    writer.finish().map_err(|e| e.to_string())?;
    Ok(out)
}

fn color([r, g, b, a]: [u8; 4]) -> tiny_skia::Color {
    tiny_skia::Color::from_rgba8(r, g, b, a)
}

#[derive(Clone, Copy)]
pub struct Basemap {
    path: StoredValue<PathBuf, LocalStorage>,
    pub archive: RwSignal<Option<Arc<Archive>>, LocalStorage>,
    /// None marks tile absent from archive or failed; nothing drawn there.
    tiles: StoredValue<HashMap<Key, Option<ImageBytes>>, LocalStorage>,
    pending: StoredValue<HashSet<Key>, LocalStorage>,
    /// Bumped when tile arrives.
    loaded: RwSignal<u64, LocalStorage>,
    /// Display tiles (z, x, y) drawn from online build, or absent locally while offline.
    outside: StoredValue<HashSet<(u8, u32, u32)>, LocalStorage>,
    /// Online state of last request pass; change retries absent tiles.
    online: StoredValue<bool, LocalStorage>,
    /// Display level in view; queued renders far from it are skipped as stale.
    level: StoredValue<Arc<std::sync::atomic::AtomicU8>, LocalStorage>,
}

impl Basemap {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path: StoredValue::new_local(path),
            archive: RwSignal::new_local(None),
            tiles: StoredValue::new_local(HashMap::new()),
            pending: StoredValue::new_local(HashSet::new()),
            loaded: RwSignal::new_local(0),
            outside: StoredValue::new_local(HashSet::new()),
            online: StoredValue::new_local(true),
            level: StoredValue::new_local(Arc::new(std::sync::atomic::AtomicU8::new(0))),
        }
    }

    /// Opens (or reopens after import) archive. Missing file means no basemap.
    pub fn reload(self, report: impl Fn(String) + 'static) {
        let path = self.path.get_value();
        spawn_local(async move {
            let result = blocking(move || path.exists().then(|| Archive::open(&path))).await;
            self.tiles.update_value(HashMap::clear);
            match result {
                None => self.archive.set(None),
                Some(Ok(archive)) => self.archive.set(Some(Arc::new(archive))),
                Some(Err(error)) => {
                    self.archive.set(None);
                    report(error);
                }
            }
        });
    }

    fn request(self, archive: &Arc<Archive>, key: Key, px: u32, online: bool) {
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
            let (z, x, y, dark, kind) = key;
            let result = blocking(move || {
                // Pinch zoom queues tiles for every level it passes; skip ones view left.
                let now = level.load(std::sync::atomic::Ordering::Relaxed);
                if z > now + 1 || z + 5 < now {
                    return Err("stale".to_string());
                }
                let draw = |archive: &Archive| -> Result<Option<Vec<u8>>, String> {
                    // Past archive max zoom, render from max-zoom ancestor.
                    let depth = z.saturating_sub(archive.max_zoom);
                    let frame = Frame {
                        px: f64::from(px),
                        depth,
                        sub: (x & ((1 << depth) - 1), y & ((1 << depth) - 1)),
                    };
                    archive
                        .tile(z - depth, x >> depth, y >> depth)?
                        .map(|bytes| render(&bytes, z, &frame, dark, kind, labels::system_font()))
                        .transpose()
                };
                // Beyond downloaded area: same vector data and style from latest build.
                match draw(&archive)? {
                    Some(png) => Ok::<_, String>((Some(png), false)),
                    None if online => Ok((draw(&*download::remote()?)?, true)),
                    None => Ok((None, true)),
                }
            })
            .await;
            self.pending.update_value(|p| {
                p.remove(&key);
            });
            if matches!(&result, Err(e) if e == "stale") {
                // Not cached: requested again if view returns to this level.
                return;
            }
            let (png, outside) = result.unwrap_or((None, online));
            if outside {
                self.outside.update_value(|o| {
                    o.insert((z, x, y));
                });
            }
            self.tiles.update_value(|t| {
                t.insert(key, png.map(ImageBytes::new));
            });
            self.loaded.update(|n| *n += 1);
        });
    }

    /// Nearest cached coarser tile covering `tile`.
    fn ancestor(self, tile: TileId, dark: bool, kind: Kind) -> Option<(TileId, ImageBytes)> {
        let mut t = tile;
        while t.z > 0 && tile.z - t.z < 6 {
            t = TileId {
                z: t.z - 1,
                x: t.x.div_euclid(2),
                y: t.y / 2,
            };
            match self.cached(t, dark, kind) {
                Some(Some(image)) => return Some((t, image)),
                Some(None) => return None,
                None => {}
            }
        }
        None
    }

    /// Cached finer tiles inside `tile`, up to `depth` levels; true when they cover it.
    fn descendants(
        self,
        tile: TileId,
        dark: bool,
        kind: Kind,
        depth: u8,
    ) -> (Vec<(TileId, ImageBytes)>, bool) {
        if depth == 0 || tile.z >= MAX_LEVEL {
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
            match self.cached(c, dark, kind) {
                Some(Some(image)) => found.push((c, image)),
                Some(None) => {}
                None => {
                    let (more, covered) = self.descendants(c, dark, kind, depth - 1);
                    found.extend(more);
                    complete &= covered;
                }
            }
        }
        (found, complete)
    }

    fn cached(self, t: TileId, dark: bool, overlay: Kind) -> Option<Option<ImageBytes>> {
        self.tiles
            .with_value(|c| c.get(&key(t, dark, overlay)).cloned())
    }

    /// Requests visible tiles and returns images to show, coarse fallbacks first.
    /// Tracks archive and arrivals, so callers re-run as tiles load.
    pub fn visible(
        self,
        camera: Camera,
        size: [f64; 2],
        dark: bool,
        overlay: Kind,
        dpr: f64,
        online: bool,
    ) -> Vec<(TileId, String)> {
        self.loaded.track();
        let Some(archive) = self.archive.get() else {
            return Vec::new();
        };
        if self.online.get_value() != online {
            self.online.set_value(online);
            // Going online: absent tiles may exist in online build.
            self.tiles.update_value(|t| t.retain(|_, v| v.is_some()));
        }
        let max = f64::from(MAX_LEVEL);
        // 512 CSS px tiles; nearest level keeps display scale within 0.7x-1.4x.
        let level = |zoom: f64| (zoom.round() - 1.0).clamp(0.0, max) as u8;
        let z = level(camera.zoom);
        let px = (512.0 * dpr).round().clamp(256.0, 2048.0) as u32;
        let opaque = overlay == Kind::Map;
        let mut show: Vec<(TileId, String)> = Vec::new();
        let mut keep = HashSet::new();
        let mut add = |t: TileId, image: &ImageBytes, keep: &mut HashSet<Key>| {
            keep.insert(key(t, dark, overlay));
            if !show.iter().any(|(s, _)| *s == t) {
                show.push((t, image.url()));
            }
        };
        let wanted = tiles_at(camera, size, z);
        self.level
            .with_value(|l| l.store(z, std::sync::atomic::Ordering::Relaxed));
        // Labels render after base tiles: worker time goes to what fills gray first.
        let deferred = overlay == Kind::Labels
            && self
                .pending
                .with_value(|p| p.iter().any(|k| k.4 != Kind::Labels));
        // Coarse backdrop first, over 2x view: blurry map is ready before sharp tiles, so
        // pinch zoom out and fast pans show map instead of gray.
        // Overview five levels up (one or two tiles) backs rapid multi-level zoom-outs.
        if opaque {
            for (scale, up) in [(4.0, 5), (2.0, 2)] {
                for tile in tiles_at(
                    camera,
                    [size[0] * scale, size[1] * scale],
                    z.saturating_sub(up),
                ) {
                    keep.insert(key(tile, dark, overlay));
                    self.request(&archive, key(tile, dark, overlay), px, online);
                }
            }
        }
        for &tile in &wanted {
            if !deferred {
                self.request(&archive, key(tile, dark, overlay), px, online);
            }
            keep.insert(key(tile, dark, overlay));
            match self.cached(tile, dark, overlay) {
                Some(Some(image)) => {
                    // Fresh image needs a frame or two to decode; opaque coarser tile
                    // underneath keeps that from flashing gray.
                    if opaque && let Some((a, under)) = self.ancestor(tile, dark, overlay) {
                        add(a, &under, &mut keep);
                    }
                    add(tile, &image, &mut keep);
                }
                Some(None) => {}
                None => {
                    // Finer tiles from zooming out, coarser ones from zooming in. Transparent
                    // layers use one or other, never both, so text and lines never double.
                    let (finer, covered) = self.descendants(tile, dark, overlay, 2);
                    if !covered && let Some((a, under)) = self.ancestor(tile, dark, overlay) {
                        add(a, &under, &mut keep);
                        if !opaque {
                            continue;
                        }
                    }
                    for (c, image) in &finer {
                        add(*c, image, &mut keep);
                    }
                }
            }
        }
        // Prefetch adjacent level as zoom nears its switch point, so crossing it stays sharp.
        let next = match camera.zoom.rem_euclid(1.0) {
            f if (0.3..0.5).contains(&f) => Some(level(camera.zoom + 0.5)),
            f if (0.5..0.7).contains(&f) => Some(level(camera.zoom - 0.5)),
            _ => None,
        };
        if !deferred
            && wanted
                .iter()
                .all(|t| self.cached(*t, dark, overlay).is_some())
            && let Some(next) = next.filter(|&n| n != z)
        {
            for tile in tiles_at(camera, size, next) {
                keep.insert(key(tile, dark, overlay));
                self.request(&archive, key(tile, dark, overlay), px, online);
            }
        }
        // Wider coarse ring once view is sharp, for larger zoom-outs and flings.
        if opaque
            && !deferred
            && wanted
                .iter()
                .all(|t| self.cached(*t, dark, overlay).is_some())
        {
            for tile in tiles_at(camera, [size[0] * 4.0, size[1] * 4.0], z.saturating_sub(2)) {
                keep.insert(key(tile, dark, overlay));
                self.request(&archive, key(tile, dark, overlay), px, online);
            }
        }
        show.sort_by_key(|(t, _)| t.z);
        // Pruning keeps adjacent levels under view: they are next zoom step's fallbacks.
        for t in &wanted {
            let parent = (
                t.z.saturating_sub(1),
                t.canonical_x() / 2,
                t.y / 2,
                dark,
                overlay,
            );
            keep.insert(parent);
            for i in 0..4 {
                keep.insert((
                    t.z + 1,
                    t.canonical_x() * 2 + i % 2,
                    t.y * 2 + i / 2,
                    dark,
                    overlay,
                ));
            }
        }
        self.tiles.update_value(|cache| {
            if cache.len() > CACHE {
                cache.retain(|k, _| keep.contains(k) || k.4 != overlay);
            }
        });
        show
    }

    /// Visible area reaches beyond offline map (drawn online, or blank while offline).
    pub fn outside(self, camera: Camera, size: [f64; 2]) -> bool {
        self.loaded.track();
        if self.archive.with(Option::is_none) {
            return false;
        }
        let z = (camera.zoom.round() - 1.0).clamp(0.0, f64::from(MAX_LEVEL)) as u8;
        self.outside.with_value(|o| {
            tiles_at(camera, size, z)
                .iter()
                .any(|t| o.contains(&(t.z, t.canonical_x(), t.y)))
        })
    }
}

fn key(t: TileId, dark: bool, overlay: Kind) -> Key {
    (t.z, t.canonical_x(), t.y, dark, overlay)
}

pub(crate) fn tiles_at(camera: Camera, size: [f64; 2], z: u8) -> Vec<TileId> {
    tiles_within(camera, size, z, 64)
}

/// Tiles at `z` covering view, nearest first; empty when more than `limit`.
pub(crate) fn tiles_within(camera: Camera, size: [f64; 2], z: u8, limit: i64) -> Vec<TileId> {
    let n = 1_i64 << z;
    let span = WORLD / n as f64;
    let [dx, dy] = size.map(|v| v / camera.scale() / 2.0);
    let x0 = ((camera.center[0] - dx + WORLD / 2.0) / span).floor() as i64;
    let x1 = ((camera.center[0] + dx + WORLD / 2.0) / span).ceil() as i64;
    let y0 = (((WORLD / 2.0 - camera.center[1] - dy) / span).floor() as i64).clamp(0, n);
    let y1 = (((WORLD / 2.0 - camera.center[1] + dy) / span).ceil() as i64).clamp(0, n);
    if (x1 - x0) * (y1 - y0) > limit {
        return Vec::new();
    }
    let mut tiles: Vec<TileId> = (y0..y1)
        .flat_map(|y| (x0..x1).map(move |x| TileId { z, x, y: y as u32 }))
        .collect();
    camera.center_first(&mut tiles);
    tiles
}

/// Same vector styling as 2D, supplied to terrain worker before overlays/labels.
pub(super) fn terrain_tile(
    archive: Option<&Archive>,
    id: TileId,
    dark: bool,
    kind: Kind,
    online: bool,
) -> Result<tiny_skia::Pixmap, String> {
    let draw = |archive: &Archive| {
        let depth = id.z.saturating_sub(archive.max_zoom);
        let frame = Frame {
            px: 512.,
            depth,
            sub: (
                id.canonical_x() & ((1 << depth) - 1),
                id.y & ((1 << depth) - 1),
            ),
        };
        archive
            .tile(id.z - depth, id.canonical_x() >> depth, id.y >> depth)?
            .map(|bytes| render(&bytes, id.z, &frame, dark, kind, labels::system_font()))
            .transpose()
    };
    let mut png = archive.map(draw).transpose()?.flatten();
    if png.is_none() && online {
        png = draw(&*download::remote()?)?;
    }
    if let Some(png) = png {
        return tiny_skia::Pixmap::decode_png(&png).map_err(|e| e.to_string());
    }
    let mut empty = tiny_skia::Pixmap::new(512, 512).ok_or("Invalid tile size")?;
    if kind == Kind::Map {
        empty.fill(color(BACKGROUND.rgba(dark, id.z + 1)));
    }
    Ok(empty)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_fixture_tile_with_labels_quickly() {
        // Protomaps v4 extract, z14-15, Portland OR. © OpenStreetMap contributors (ODbL).
        let archive = Archive::open(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/downtown.pmtiles"),
        )
        .unwrap();
        let bytes = archive.tile(15, 5217, 11720).unwrap().unwrap();
        let layers = mvt::decode(&bytes).unwrap();
        let styled = |slot: usize| {
            layers
                .iter()
                .any(|l| l.features.iter().any(|f| classify(l, f) == Some(slot)))
        };
        assert!(styled(15));
        assert!((21..=25).any(styled));
        let font = labels::tests::test_font();
        let start = std::time::Instant::now();
        let whole = Frame {
            px: 1344.0,
            depth: 0,
            sub: (0, 0),
        };
        let plain = render(&bytes, 15, &whole, false, Kind::Map, None).unwrap();
        let labeled = render(&bytes, 15, &whole, false, Kind::Labels, Some(&font)).unwrap();
        // Overzoom: z17 quarter-of-quarter crop differs from whole tile.
        let crop = Frame {
            px: 1344.0,
            depth: 2,
            sub: (1, 2),
        };
        assert_ne!(
            render(&bytes, 17, &crop, false, Kind::Map, None).unwrap(),
            plain
        );
        eprintln!("two 1344px renders: {:?}", start.elapsed());
        assert_ne!(plain, labeled, "labels drew nothing");
        let image = zgui_image::decode(&plain, Default::default()).unwrap();
        assert_eq!(image.size.width, 1344);
        assert!(image.texels.chunks(4).any(|t| t != &image.texels[..4]));
        let labels = zgui_image::decode(&labeled, Default::default()).unwrap();
        assert!(
            labels.texels.chunks(4).any(|p| p[3] == 0) && labels.texels.chunks(4).any(|p| p[3] > 0),
            "label layer is transparent text"
        );
        let overlay = render(&bytes, 15, &whole, true, Kind::Roads, Some(&font)).unwrap();
        let overlay = zgui_image::decode(&overlay, Default::default()).unwrap();
        assert!(
            overlay.texels.chunks(4).any(|p| p[3] == 0),
            "satellite must show through background"
        );
        assert!(
            overlay.texels.chunks(4).any(|p| p[3] > 0),
            "roads must remain visible"
        );
        assert!(
            image.texels.chunks(4).all(|p| p[3] == 255),
            "normal map stays opaque"
        );
    }

    /// GPU base map draws fixture tile like CPU tile renderer, at tile's nominal zoom.
    #[test]
    fn gpu_tile_matches_cpu_tile() {
        use crate::mapping::{map_gpu, vector_tiles};
        let archive = Archive::open(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/downtown.pmtiles"),
        )
        .unwrap();
        let id = TileId {
            z: 15,
            x: 5217,
            y: 11720,
        };
        let bytes = archive.tile(id.z, 5217, id.y).unwrap().unwrap();
        let whole = Frame {
            px: 512.0,
            depth: 0,
            sub: (0, 0),
        };
        let cpu = render(&bytes, id.z, &whole, false, Kind::Map, None).unwrap();
        let cpu = zgui_image::decode(&cpu, Default::default()).unwrap().texels;
        let span = WORLD / f64::from(1 << id.z);
        let nw = vector_tiles::corner(id);
        let camera = Camera {
            center: [nw[0] + span / 2.0, nw[1] - span / 2.0],
            zoom: f64::from(id.z + 1),
        };
        // Labels are placed differently (across tiles), so compare map without them.
        let bare = vector_tiles::Tile {
            labels: Vec::new(),
            ..vector_tiles::Tile::build(&bytes, id.z).unwrap()
        };
        let scene = map_gpu::Scene {
            tiles: vec![(id, Arc::new(bare))],
            base: Some(false),
            ..Default::default()
        };
        let Some(gpu) = map_gpu::tests::render(&scene, camera, 512, 512) else {
            return;
        };
        // Antialiasing differs at edges; colours and shapes must agree elsewhere.
        let differ = cpu
            .chunks(4)
            .zip(gpu.chunks(4))
            .filter(|(c, g)| (0..3).map(|i| c[i].abs_diff(g[i]) as u32).sum::<u32>() > 48)
            .count();
        let share = differ as f64 / (512.0 * 512.0);
        eprintln!("pixels differing from CPU tile: {:.2}%", share * 100.0);
        assert!(share < 0.03, "{:.2}% pixels differ", share * 100.0);

        // Labels: text and halos absent from bare map.
        if labels::system_font().is_none() {
            return;
        }
        let scene = map_gpu::Scene {
            tiles: vec![(
                id,
                Arc::new(vector_tiles::Tile::build(&bytes, id.z).unwrap()),
            )],
            base: Some(false),
            ..Default::default()
        };
        let labeled = map_gpu::tests::render(&scene, camera, 512, 512).unwrap();
        if let Ok(path) = std::env::var("MAP_PNG") {
            let mut out = Vec::new();
            let mut encoder = png::Encoder::new(&mut out, 512, 512);
            encoder.set_color(png::ColorType::Rgba);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&labeled)
                .unwrap();
            std::fs::write(path, out).unwrap();
        }
        let added = labeled
            .chunks(4)
            .zip(gpu.chunks(4))
            .filter(|(l, g)| (0..3).map(|i| l[i].abs_diff(g[i]) as u32).sum::<u32>() > 96)
            .count();
        assert!(added > 300, "only {added} text pixels");
    }

    #[test]
    fn tiles_cover_viewport() {
        let camera = Camera {
            center: [0.0, 0.0],
            zoom: 3.0,
        };
        // 512 px tiles at z2 → 400x400 viewport around origin touches 4 tiles.
        let tiles = tiles_at(camera, [400.0, 400.0], 2);
        assert_eq!(tiles.len(), 4);
        assert!(tiles.contains(&TileId { z: 2, x: 1, y: 1 }));
    }
}

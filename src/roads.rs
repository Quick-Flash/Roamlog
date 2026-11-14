//! Coverage roads from basemap tiles at archive max zoom (z15 for Protomaps).
//! Only tiles near GPS history load. Lines clip to tile bounds, so tile buffers never
//! double-count; named roads share one id across tiles so matching sees one road.
use crate::{
    coverage::Road,
    geo::Point,
    model::State,
    mvt::{self, Step},
    pmtiles::Archive,
};
use std::collections::BTreeSet;

/// Neighbor tiles load when GPS point is this close to tile edge.
const EDGE_M: f64 = 40.0;

/// Tile position (fractional) of point at zoom `z`.
fn tile_xy(p: Point, z: u8) -> (f64, f64) {
    let n = f64::from(1_u32 << z);
    let lat = p.lat.clamp(-85.05, 85.05).to_radians();
    (
        (p.lon + 180.0) / 360.0 * n,
        (1.0 - lat.tan().asinh() / std::f64::consts::PI) / 2.0 * n,
    )
}

fn point(x: f64, y: f64, z: u8) -> Point {
    let n = f64::from(1_u32 << z);
    Point {
        lon: x / n * 360.0 - 180.0,
        lat: (std::f64::consts::PI * (1.0 - 2.0 * y / n))
            .sinh()
            .atan()
            .to_degrees(),
    }
}

/// Tiles covering recorded points, with neighbors near edges.
pub fn tiles(state: &State, z: u8) -> BTreeSet<(u32, u32)> {
    let n = 1_i64 << z;
    let mut out = BTreeSet::new();
    let mut add = |p: Point, margin_m: f64| {
        let (x, y) = tile_xy(p, z);
        let tile_m = 40_075_016.0 * p.lat.to_radians().cos() / n as f64;
        let margin = margin_m / tile_m.max(1.0);
        for tx in (x - margin).floor() as i64..=(x + margin).floor() as i64 {
            for ty in (y - margin).floor() as i64..=(y + margin).floor() as i64 {
                if (0..n).contains(&ty) {
                    out.insert((tx.rem_euclid(n) as u32, ty as u32));
                }
            }
        }
    };
    for trip in &state.trips {
        for p in &trip.points {
            add(p.fix.point, EDGE_M);
        }
        // A dropout can span tiles with no fixes. Load its corridor, not only endpoints.
        for pair in trip
            .points
            .windows(2)
            .filter(|p| p[1].connected && p[1].estimated)
        {
            let distance = pair[0].fix.point.distance(pair[1].fix.point);
            if distance > 5000.0 {
                continue;
            }
            let count = (distance / 150.0).ceil().max(1.0) as usize;
            for i in 0..=count {
                add(
                    pair[0]
                        .fix
                        .point
                        .interpolate(pair[1].fix.point, i as f64 / count as f64),
                    200.0,
                );
            }
        }
    }
    out
}

/// Roads that count toward coverage: streets and paths, not service ways, sidewalks,
/// crossings, rail, or ferries. Sidewalks parallel to streets would make matches ambiguous.
fn counts(kind: &str, detail: &str) -> bool {
    match kind {
        "highway" | "major_road" | "medium_road" | "minor_road" => detail != "service",
        "path" => !matches!(detail, "sidewalk" | "crossing"),
        _ => false,
    }
}

pub fn roads(archive: &Archive, tiles: &BTreeSet<(u32, u32)>) -> Result<Vec<Road>, String> {
    let z = archive.max_zoom;
    let mut roads = Vec::new();
    for &(x, y) in tiles {
        let Some(bytes) = archive.tile(z, x, y)? else {
            continue;
        };
        for layer in mvt::decode(&bytes)?.iter().filter(|l| l.name == "roads") {
            let extent = f64::from(layer.extent.max(1));
            for (index, f) in layer.features.iter().enumerate() {
                let (kind, detail) = (layer.str(f, "kind"), layer.str(f, "kind_detail"));
                if f.kind != 2 || !counts(kind, detail) {
                    continue;
                }
                let name = layer.str(f, "name");
                // Id prefix names road class, for per-class area totals.
                let class = if detail.is_empty() { kind } else { detail };
                let id = if name.is_empty() {
                    format!("{class}:{z}/{x}/{y}/{index}")
                } else {
                    format!("{class}:{name}")
                };
                let mut lines: Vec<Vec<(f64, f64)>> = Vec::new();
                f.steps(|s| match s {
                    Step::Move(u, v) => lines.push(vec![(u, v)]),
                    Step::Line(u, v) => {
                        if let Some(line) = lines.last_mut() {
                            line.push((u, v));
                        }
                    }
                    Step::Close => {}
                });
                for piece in lines.iter().flat_map(|l| clip(l, extent)) {
                    roads.push(Road {
                        half_width_m: match detail {
                            "motorway" | "trunk" => 9.0,
                            "primary" => 8.0,
                            "secondary" => 7.0,
                            "tertiary" => 5.5,
                            "residential" | "unclassified" | "living_street" => 4.0,
                            _ if kind == "path" => 1.5,
                            _ => 3.0,
                        },
                        id: id.clone(),
                        name: if name.is_empty() {
                            "Unnamed road".into()
                        } else {
                            name.into()
                        },
                        points: piece
                            .iter()
                            .map(|&(u, v)| {
                                point(f64::from(x) + u / extent, f64::from(y) + v / extent, z)
                            })
                            .collect(),
                    });
                }
            }
        }
    }
    Ok(roads)
}

/// Tiles covering bounding box of `points`, grown by `margin_m`; `None` over `limit` tiles.
pub fn bbox_tiles(
    points: &[Point],
    margin_m: f64,
    z: u8,
    limit: usize,
) -> Option<BTreeSet<(u32, u32)>> {
    let lat = points.iter().map(|p| p.lat);
    let lon = points.iter().map(|p| p.lon);
    let (south, north) = (lat.clone().fold(90.0, f64::min), lat.fold(-90.0, f64::max));
    let (west, east) = (
        lon.clone().fold(180.0, f64::min),
        lon.fold(-180.0, f64::max),
    );
    let dlat = margin_m / 111_195.0;
    let dlon = dlat / north.abs().max(south.abs()).min(85.0).to_radians().cos();
    let (x0, y0) = tile_xy(
        Point {
            lat: north + dlat,
            lon: west - dlon,
        },
        z,
    );
    let (x1, y1) = tile_xy(
        Point {
            lat: south - dlat,
            lon: east + dlon,
        },
        z,
    );
    let (x0, x1, y0, y1) = (x0 as u32, x1 as u32, y0 as u32, y1 as u32);
    if (x1 - x0 + 1) as usize * (y1 - y0 + 1) as usize > limit {
        return None;
    }
    Some(
        (x0..=x1)
            .flat_map(|x| (y0..=y1).map(move |y| (x, y)))
            .collect(),
    )
}

/// Splits polyline into pieces inside [0, size]² (Liang-Barsky per segment).
fn clip(line: &[(f64, f64)], size: f64) -> Vec<Vec<(f64, f64)>> {
    let mut pieces: Vec<Vec<(f64, f64)>> = Vec::new();
    let mut open = false;
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
        let mut inside = true;
        for (p, q) in [(-dx, a.0), (dx, size - a.0), (-dy, a.1), (dy, size - a.1)] {
            if p == 0.0 {
                inside &= q >= 0.0;
            } else {
                let t = q / p;
                if p < 0.0 {
                    t0 = t0.max(t);
                } else {
                    t1 = t1.min(t);
                }
            }
        }
        if !inside || t0 >= t1 {
            open = false;
            continue;
        }
        let at = |t: f64| (a.0 + t * dx, a.1 + t * dy);
        if !open || t0 > 0.0 {
            pieces.push(vec![at(t0)]);
        }
        pieces.last_mut().unwrap().push(at(t1));
        open = t1 >= 1.0;
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_splits_lines_at_tile_edges() {
        let pieces = clip(&[(-10.0, 5.0), (5.0, 5.0), (15.0, 5.0), (5.0, 8.0)], 10.0);
        assert_eq!(pieces.len(), 2);
        assert_eq!(pieces[0], vec![(0.0, 5.0), (5.0, 5.0), (10.0, 5.0)]);
        assert_eq!(pieces[1][1], (5.0, 8.0));
    }

    #[test]
    fn tile_math_round_trips() {
        let p = Point {
            lat: 45.516,
            lon: -122.6775,
        };
        let (x, y) = tile_xy(p, 15);
        assert_eq!((x as u32, y as u32), (5217, 11720));
        let back = point(x, y, 15);
        assert!(p.distance(back) < 0.01);
    }

    #[test]
    fn fixture_roads_exclude_sidewalks_and_share_named_ids() {
        let archive = Archive::open(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/downtown.pmtiles"),
        )
        .unwrap();
        let roads = roads(&archive, &BTreeSet::from([(5217, 11720)])).unwrap();
        assert!(roads.len() > 50);
        assert!(roads.iter().all(|r| !r.id.starts_with("sidewalk:")));
        assert!(roads.iter().any(|r| r.id == "secondary:Southwest Broadway"));
        // Clipped to tile: every point inside tile bounds.
        let (lo, hi) = (point(5217.0, 11721.0, 15), point(5218.0, 11720.0, 15));
        for p in roads.iter().flat_map(|r| &r.points) {
            assert!(p.lon >= lo.lon - 1e-9 && p.lon <= hi.lon + 1e-9);
            assert!(p.lat >= lo.lat - 1e-9 && p.lat <= hi.lat + 1e-9);
        }
    }
}

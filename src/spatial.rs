//! Small spherical metre grid for road candidates; works across longitude wrap.
use crate::geo::{EARTH_RADIUS_M, Point};
use std::collections::{BTreeSet, HashMap};

#[derive(Default)]
pub(crate) struct Index {
    cells: HashMap<[i32; 3], Vec<usize>>,
}

fn xyz(p: Point) -> [f64; 3] {
    let (lat, lon) = (p.lat.to_radians(), p.lon.to_radians());
    [
        lat.cos() * lon.cos() * EARTH_RADIUS_M,
        lat.cos() * lon.sin() * EARTH_RADIUS_M,
        lat.sin() * EARTH_RADIUS_M,
    ]
}

fn cell(p: [f64; 3]) -> [i32; 3] {
    p.map(|v| (v / 100.0).floor() as i32)
}

fn samples(a: Point, b: Point) -> impl Iterator<Item = [f64; 3]> {
    let n = (a.distance(b) / 50.0).ceil().max(1.0) as usize;
    let (a, b) = (xyz(a), xyz(b));
    (0..=n).map(move |i| std::array::from_fn(|j| a[j] + (b[j] - a[j]) * i as f64 / n as f64))
}

impl Index {
    pub fn insert(&mut self, id: usize, a: Point, b: Point) {
        for point in samples(a, b) {
            let ids = self.cells.entry(cell(point)).or_default();
            if ids.last() != Some(&id) {
                ids.push(id);
            }
        }
    }

    /// Broad phase only: callers still check actual distance/alignment.
    pub fn near(&self, a: Point, b: Point) -> BTreeSet<usize> {
        let mut ids = BTreeSet::new();
        for point in samples(a, b) {
            let [x, y, z] = cell(point);
            for dx in -1..=1 {
                for dy in -1..=1 {
                    for dz in -1..=1 {
                        if let Some(found) = self.cells.get(&[x + dx, y + dy, z + dz]) {
                            ids.extend(found);
                        }
                    }
                }
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_middle_of_long_segment_across_dateline_without_far_roads() {
        let mut index = Index::default();
        index.insert(
            1,
            Point {
                lat: 0.0,
                lon: 179.999,
            },
            Point {
                lat: 0.0,
                lon: -179.999,
            },
        );
        index.insert(
            2,
            Point { lat: 1.0, lon: 1.0 },
            Point {
                lat: 1.0,
                lon: 1.001,
            },
        );
        assert_eq!(
            index.near(
                Point {
                    lat: 0.0001,
                    lon: 180.0
                },
                Point {
                    lat: 0.0001,
                    lon: 180.0
                }
            ),
            BTreeSet::from([1])
        );
    }
}

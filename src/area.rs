//! Road coverage inside a drawn polygon, by length. Each road section counts once; both
//! sides of a divided road are separate map roads, so one direction covers one side.
use crate::{
    coverage::{Road, Section},
    geo::{Point, local},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct ClassStats {
    pub class: String,
    pub total_m: f64,
    pub traveled_m: f64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Stats {
    pub total_m: f64,
    pub traveled_m: f64,
    /// Longest total first.
    pub classes: Vec<ClassStats>,
    /// Named streets with any length inside, done (>=90% traveled), and started.
    pub streets: usize,
    pub streets_done: usize,
    pub streets_started: usize,
    /// Untraveled sections inside, for highlighting what remains (smaller areas only).
    pub untraveled: Vec<Section>,
    /// Red untraveled roads were too many to draw.
    #[serde(default)]
    pub large: bool,
}

/// Road class from road id prefix (`residential:Main Street`).
pub fn class(road_id: &str) -> &str {
    road_id.split_once(':').map_or("other", |(class, _)| class)
}

/// Street name, or `None` for unnamed pieces (`class:z/x/y/index`).
pub fn name(road_id: &str) -> Option<&str> {
    road_id
        .split_once(':')
        .map(|(_, name)| name)
        .filter(|name| !name.split('/').all(|p| p.parse::<u32>().is_ok()))
}

/// Ray casting in local metres around first corner.
fn inside(polygon: &[[f64; 2]], p: [f64; 2]) -> bool {
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (a, b) = (polygon[i], polygon[j]);
        if (a[1] > p[1]) != (b[1] > p[1])
            && p[0] < (b[0] - a[0]) * (p[1] - a[1]) / (b[1] - a[1]) + a[0]
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Fraction of segment `a`–`b` inside polygon.
fn fraction_inside(polygon: &[[f64; 2]], a: [f64; 2], b: [f64; 2]) -> f64 {
    let d = [b[0] - a[0], b[1] - a[1]];
    let mut cuts = vec![0.0, 1.0];
    for i in 0..polygon.len() {
        let (p, q) = (polygon[i], polygon[(i + 1) % polygon.len()]);
        let e = [q[0] - p[0], q[1] - p[1]];
        let denominator = d[0] * e[1] - d[1] * e[0];
        if denominator.abs() < 1e-12 {
            continue;
        }
        let w = [p[0] - a[0], p[1] - a[1]];
        let t = (w[0] * e[1] - w[1] * e[0]) / denominator;
        let u = (w[0] * d[1] - w[1] * d[0]) / denominator;
        if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
            cuts.push(t);
        }
    }
    cuts.sort_by(f64::total_cmp);
    cuts.windows(2)
        .filter(|c| {
            let t = (c[0] + c[1]) / 2.0;
            c[1] > c[0] && inside(polygon, [a[0] + t * d[0], a[1] + t * d[1]])
        })
        .map(|c| c[1] - c[0])
        .sum()
}

/// Accumulates area totals from map roads and traveled length from coverage sections.
/// Large areas stream roads tile by tile instead of building coverage for all of them.
pub struct Area {
    origin: Point,
    shape: Vec<[f64; 2]>,
    min: [f64; 2],
    max: [f64; 2],
    /// Class -> (total, traveled); street -> (total, traveled).
    classes: BTreeMap<String, (f64, f64)>,
    streets: BTreeMap<String, (f64, f64)>,
    untraveled: Vec<Section>,
}

impl Area {
    pub fn new(polygon: &[Point]) -> Self {
        let origin = polygon[0];
        let shape: Vec<_> = polygon.iter().map(|&p| local(origin, p)).collect();
        let mut min = [f64::INFINITY; 2];
        let mut max = [f64::NEG_INFINITY; 2];
        for p in &shape {
            for i in 0..2 {
                min[i] = min[i].min(p[i]);
                max[i] = max[i].max(p[i]);
            }
        }
        Self {
            origin,
            shape,
            min,
            max,
            classes: BTreeMap::new(),
            streets: BTreeMap::new(),
            untraveled: Vec::new(),
        }
    }

    /// Length of `a`-`b` inside, metres.
    fn inside(&self, a: Point, b: Point) -> f64 {
        let (p, q) = (local(self.origin, a), local(self.origin, b));
        if (0..2).any(|i| p[i].max(q[i]) < self.min[i] || p[i].min(q[i]) > self.max[i]) {
            return 0.0;
        }
        let fraction = fraction_inside(&self.shape, p, q);
        if fraction <= 0.0 {
            0.0
        } else {
            a.distance(b) * fraction
        }
    }

    fn add(&mut self, road_id: &str, length: f64, traveled: bool) {
        let add = |entry: &mut (f64, f64)| {
            if traveled {
                entry.1 += length;
            } else {
                entry.0 += length;
            }
        };
        add(self.classes.entry(class(road_id).into()).or_default());
        if let Some(name) = name(road_id) {
            add(self.streets.entry(name.into()).or_default());
        }
    }

    /// Every road counts toward total.
    pub fn add_roads(&mut self, roads: &[Road]) {
        for road in roads {
            for pair in road.points.windows(2) {
                let length = self.inside(pair[0], pair[1]);
                if length > 0.0 {
                    self.add(&road.id, length, false);
                }
            }
        }
    }

    /// Traveled sections count toward traveled; untraveled ones are kept for map display.
    pub fn add_sections(&mut self, sections: &[Section], keep_untraveled: bool) {
        for section in sections {
            if !section.traveled && !keep_untraveled {
                continue;
            }
            let length = self.inside(section.start, section.end);
            if length <= 0.0 {
                continue;
            }
            if section.traveled {
                self.add(&section.road_id, length, true);
            } else {
                self.untraveled.push(section.clone());
            }
        }
    }

    pub fn finish(self) -> Stats {
        let mut stats = Stats {
            untraveled: self.untraveled,
            ..Stats::default()
        };
        for (class, (total_m, traveled_m)) in self.classes {
            // Coverage and map clip same tiles, but guard rounding.
            let traveled_m = traveled_m.min(total_m);
            stats.total_m += total_m;
            stats.traveled_m += traveled_m;
            stats.classes.push(ClassStats {
                class,
                total_m,
                traveled_m,
            });
        }
        stats
            .classes
            .sort_by(|a, b| b.total_m.total_cmp(&a.total_m));
        stats.streets = self.streets.values().filter(|s| s.0 > 0.0).count();
        stats.streets_done = self
            .streets
            .values()
            .filter(|s| s.0 > 0.0 && s.1 >= s.0 * 0.9)
            .count();
        stats.streets_started = self
            .streets
            .values()
            .filter(|s| s.0 > 0.0 && s.1 > 0.0)
            .count();
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64) -> Point {
        Point {
            lon: x / 111_195.080,
            lat: y / 111_195.080,
        }
    }

    /// Same sections as map roads (totals) and coverage (traveled, untraveled).
    fn stats(sections: &[Section], polygon: &[Point]) -> Stats {
        let roads: Vec<Road> = sections
            .iter()
            .map(|s| Road {
                id: s.road_id.clone(),
                name: String::new(),
                points: vec![s.start, s.end],
                half_width_m: 0.0,
            })
            .collect();
        let mut area = Area::new(polygon);
        area.add_roads(&roads);
        area.add_sections(sections, true);
        area.finish()
    }

    fn section(id: &str, a: (f64, f64), b: (f64, f64), traveled: bool) -> Section {
        Section {
            road_id: id.into(),
            start: p(a.0, a.1),
            end: p(b.0, b.1),
            traveled,
            visits: 0,
            first_trip: None,
        }
    }

    #[test]
    fn clips_sections_to_polygon_and_groups_by_class_and_street() {
        let square = [p(0.0, 0.0), p(100.0, 0.0), p(100.0, 100.0), p(0.0, 100.0)];
        let sections = [
            // Half inside.
            section("residential:Main", (50.0, 50.0), (150.0, 50.0), true),
            section("residential:Main", (10.0, 50.0), (30.0, 50.0), false),
            section("path:15/1/2/3", (20.0, 20.0), (20.0, 40.0), false),
            section("primary:Far", (200.0, 200.0), (300.0, 200.0), true),
        ];
        let stats = stats(&sections, &square);
        // Totals come from map roads: sections stand in for them here.
        assert!((stats.total_m - 90.0).abs() < 0.5, "{}", stats.total_m);
        assert!((stats.traveled_m - 50.0).abs() < 0.5);
        assert_eq!(stats.classes[0].class, "residential");
        assert_eq!(stats.classes.len(), 2);
        assert_eq!(
            (stats.streets, stats.streets_done, stats.streets_started),
            (1, 0, 1)
        );
        assert_eq!(stats.untraveled.len(), 2);
    }

    #[test]
    fn names_skip_unnamed_tile_ids() {
        assert_eq!(name("residential:15/6198/12302/4"), None);
        assert_eq!(name("secondary:300 West"), Some("300 West"));
        assert_eq!(class("secondary:300 West"), "secondary");
    }

    #[test]
    fn concave_polygon_excludes_notch() {
        // U shape: notch between x 40..60 above y 30.
        let u = [
            p(0.0, 0.0),
            p(100.0, 0.0),
            p(100.0, 100.0),
            p(60.0, 100.0),
            p(60.0, 30.0),
            p(40.0, 30.0),
            p(40.0, 100.0),
            p(0.0, 100.0),
        ];
        let stats = stats(
            &[section("residential:A", (0.0, 50.0), (100.0, 50.0), false)],
            &u,
        );
        assert!((stats.total_m - 80.0).abs() < 0.5, "{}", stats.total_m);
    }
}

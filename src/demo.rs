//! Simulated GPS for the demo app: position moves along `demo-route.json` by wall clock,
//! so live map and recorder processes agree without talking to each other.
use crate::geo::Point;
use std::path::Path;

/// Package directory marker; real app never reads a demo route.
pub const PACKAGE: &str = "dev.traveler.demo";
pub const FILE: &str = "demo-route.json";
/// Simulated travel speed, m/s (city riding).
const SPEED: f64 = 9.0;

pub struct Route {
    points: Vec<Point>,
    /// Distance from start to each point, metres.
    along: Vec<f64>,
}

impl Route {
    pub fn new(points: Vec<Point>) -> Option<Self> {
        if points.len() < 2 {
            return None;
        }
        let mut along = vec![0.0];
        for pair in points.windows(2) {
            along.push(along.last().unwrap() + pair[0].distance(pair[1]));
        }
        (*along.last().unwrap() > 1.0).then_some(Self { points, along })
    }

    /// Demo route when `directory` belongs to demo app and holds one.
    pub fn load(directory: &Path) -> Option<Self> {
        if !directory.to_string_lossy().contains(PACKAGE) {
            return None;
        }
        let bytes = std::fs::read(directory.join(FILE)).ok()?;
        Self::new(serde_json::from_slice(&bytes).ok()?)
    }

    /// Position at `ms` since 1970: travels route, then back, repeating.
    pub fn at(&self, ms: u64) -> Point {
        let total = *self.along.last().unwrap();
        let d = (ms as f64 / 1000.0 * SPEED).rem_euclid(2.0 * total);
        let d = if d > total { 2.0 * total - d } else { d };
        let i = self
            .along
            .partition_point(|&a| a <= d)
            .clamp(1, self.points.len() - 1);
        let (a, b) = (self.along[i - 1], self.along[i]);
        let t = if b > a { (d - a) / (b - a) } else { 0.0 };
        self.points[i - 1].interpolate(self.points[i], t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn moves_along_route_and_back() {
        let p = |lon: f64| Point { lat: 0.0, lon };
        // ~1112 m straight east.
        let route = Route::new(vec![p(0.0), p(0.005), p(0.01)]).unwrap();
        let total = p(0.0).distance(p(0.01));
        let ms = |m: f64| (m / SPEED * 1000.0) as u64;
        assert!(route.at(0).distance(p(0.0)) < 0.5);
        assert!(route.at(ms(total / 2.0)).distance(p(0.005)) < 0.5);
        assert!(route.at(ms(total)).distance(p(0.01)) < 0.5);
        // Returning: 1.5 lengths is back at middle, not jumping to start.
        assert!(route.at(ms(total * 1.5)).distance(p(0.005)) < 0.5);
        assert!(Route::load(Path::new("/data/user/0/dev.traveler/files")).is_none());
    }
}

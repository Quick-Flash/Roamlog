use serde::{Deserialize, Serialize};

pub const EARTH_RADIUS_M: f64 = 6_371_008.8;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq)]
pub struct Point {
    pub lat: f64,
    pub lon: f64,
}

impl Point {
    pub fn interpolate(self, other: Self, t: f64) -> Self {
        let dlon = (other.lon - self.lon + 180.0).rem_euclid(360.0) - 180.0;
        Self {
            lat: self.lat + (other.lat - self.lat) * t,
            lon: (self.lon + dlon * t + 180.0).rem_euclid(360.0) - 180.0,
        }
    }

    pub fn valid(self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
    }

    pub fn distance(self, other: Self) -> f64 {
        let a = ((other.lat - self.lat).to_radians() / 2.0).sin().powi(2)
            + self.lat.to_radians().cos()
                * other.lat.to_radians().cos()
                * ((other.lon - self.lon).to_radians() / 2.0).sin().powi(2);
        2.0 * EARTH_RADIUS_M * a.clamp(0.0, 1.0).sqrt().asin()
    }
}

/// Local metre coordinates for short road-matching segments. Wrap longitude at dateline.
pub fn local(origin: Point, point: Point) -> [f64; 2] {
    let longitude = (point.lon - origin.lon + 180.0).rem_euclid(360.0) - 180.0;
    [
        longitude.to_radians() * origin.lat.to_radians().cos() * EARTH_RADIUS_M,
        (point.lat - origin.lat).to_radians() * EARTH_RADIUS_M,
    ]
}

pub fn segment_distance(p: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
    let v = [b[0] - a[0], b[1] - a[1]];
    let length2 = v[0] * v[0] + v[1] * v[1];
    let t = if length2 == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * v[0] + (p[1] - a[1]) * v[1]) / length2).clamp(0.0, 1.0)
    };
    (p[0] - a[0] - t * v[0]).hypot(p[1] - a[1] - t * v[1])
}

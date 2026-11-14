use super::camera::{Camera, MAX_INTERACTIVE_ZOOM};
use std::collections::BTreeMap;

/// Twist needed before two fingers rotate map, so pinch-zoom does not tilt it.
const ROTATE_START: f64 = 19.0 * std::f64::consts::PI / 180.0;

#[derive(Clone, Copy)]
struct Base {
    camera: Camera,
    bearing: f64,
    mid: [f64; 2],
    spread: f64,
    angle: f64,
    /// Viewport centre, window px: rotation and zoom pivot around touch midpoint.
    center: [f64; 2],
}

#[derive(Default)]
pub struct Contacts {
    pub points: BTreeMap<u64, [f64; 2]>,
    baseline: Option<Base>,
    /// Twist past threshold this gesture, and angle already consumed by it.
    rotating: Option<f64>,
}

/// Screen offset (px, y down) to projected-metre offset for map rotated `bearing`
/// clockwise on screen.
fn offset(v: [f64; 2], bearing: f64, scale: f64) -> [f64; 2] {
    let (sin, cos) = bearing.sin_cos();
    let u = [v[0] * cos + v[1] * sin, -v[0] * sin + v[1] * cos];
    [u[0] / scale, -u[1] / scale]
}

impl Contacts {
    fn geometry(&self) -> Option<([f64; 2], f64, f64)> {
        let mut points = self.points.values();
        let a = *points.next()?;
        let Some(b) = points.next() else {
            return Some((a, 0.0, 0.0));
        };
        Some((
            [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5],
            (a[0] - b[0]).hypot(a[1] - b[1]),
            (b[1] - a[1]).atan2(b[0] - a[0]),
        ))
    }

    pub fn rebase(&mut self, camera: Camera, bearing: f64, center: [f64; 2]) {
        if self.points.is_empty() {
            self.rotating = None;
        }
        // Finger count changed: angle restarts from new pair, already rotating stays on.
        self.rotating = self.rotating.map(|_| 0.0);
        self.baseline = self.geometry().map(|(mid, spread, angle)| Base {
            camera,
            bearing,
            mid,
            spread,
            angle,
            center,
        });
    }

    /// Camera and bearing keeping map point under fingers' midpoint under it.
    pub fn camera(&mut self) -> Option<(Camera, f64)> {
        let base = self.baseline?;
        let (mid, spread, angle) = self.geometry()?;
        let pair = spread > 0.0 && base.spread > 0.0;
        let zoom = (base.camera.zoom
            + if pair {
                (spread / base.spread).log2()
            } else {
                0.0
            })
        .clamp(0.0, MAX_INTERACTIVE_ZOOM);
        let twist = if pair {
            (angle - base.angle + std::f64::consts::PI).rem_euclid(std::f64::consts::TAU)
                - std::f64::consts::PI
        } else {
            0.0
        };
        if self.rotating.is_none() && twist.abs() > ROTATE_START {
            self.rotating = Some(ROTATE_START.copysign(twist));
        }
        let bearing = base.bearing + self.rotating.map_or(0.0, |consumed| twist - consumed);
        let next = Camera {
            center: base.camera.center,
            zoom,
        };
        let v0 = [base.mid[0] - base.center[0], base.mid[1] - base.center[1]];
        let v1 = [mid[0] - base.center[0], mid[1] - base.center[1]];
        let p0 = offset(v0, base.bearing, base.camera.scale());
        let p1 = offset(v1, bearing, next.scale());
        Some((
            Camera {
                center: [
                    base.camera.center[0] + p0[0] - p1[0],
                    base.camera.center[1] + p0[1] - p1[1],
                ],
                zoom,
            },
            bearing,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contacts(points: &[(u64, [f64; 2])], bearing: f64) -> Contacts {
        let mut c = Contacts::default();
        for &(id, p) in points {
            c.points.insert(id, p);
        }
        c.rebase(
            Camera {
                center: [0.0, 0.0],
                zoom: 10.0,
            },
            bearing,
            [100.0, 100.0],
        );
        c
    }

    #[test]
    fn drag_on_rotated_map_moves_map_with_finger() {
        // Map rotated 90° clockwise: north points screen-right. Dragging right pulls
        // southern content under centre, so centre moves south.
        let mut c = contacts(&[(1, [100.0, 100.0])], std::f64::consts::FRAC_PI_2);
        c.points.insert(1, [110.0, 100.0]);
        let (cam, bearing) = c.camera().unwrap();
        assert_eq!(bearing, std::f64::consts::FRAC_PI_2);
        let scale = cam.scale();
        assert!(cam.center[0].abs() < 1e-9);
        assert!(
            (cam.center[1] + 10.0 / scale).abs() < 1e-9,
            "{:?}",
            cam.center
        );
    }

    #[test]
    fn small_twist_zooms_without_rotating_large_twist_rotates() {
        let mut c = contacts(&[(1, [50.0, 100.0]), (2, [150.0, 100.0])], 0.0);
        // Pinch out with 15° twist: under dead zone.
        let t = 15f64.to_radians();
        c.points
            .insert(1, [100.0 - 100.0 * t.cos(), 100.0 - 100.0 * t.sin()]);
        c.points
            .insert(2, [100.0 + 100.0 * t.cos(), 100.0 + 100.0 * t.sin()]);
        let (cam, bearing) = c.camera().unwrap();
        assert_eq!(bearing, 0.0);
        assert!((cam.zoom - 11.0).abs() < 1e-9);
        // 40° twist: rotates by 40 - 19.
        let t = 40f64.to_radians();
        c.points
            .insert(1, [100.0 - 50.0 * t.cos(), 100.0 - 50.0 * t.sin()]);
        c.points
            .insert(2, [100.0 + 50.0 * t.cos(), 100.0 + 50.0 * t.sin()]);
        let (cam, bearing) = c.camera().unwrap();
        assert!((bearing.to_degrees() - 21.0).abs() < 1e-6);
        // Midpoint at pivot: centre unchanged.
        assert!(cam.center[0].abs() < 1e-9 && cam.center[1].abs() < 1e-9);
    }
}

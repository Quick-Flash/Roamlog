//! One contact orbits; two contacts pan and pinch, with rebasing on contact changes.
use super::{
    camera::{Camera, MAX_INTERACTIVE_ZOOM},
    terrain::Orbit,
};
use std::collections::BTreeMap;
#[derive(Default)]
pub struct Contacts {
    pub points: BTreeMap<u64, [f64; 2]>,
    base: Option<Base>,
}
#[derive(Clone, Copy)]
struct Base {
    camera: Camera,
    orbit: Orbit,
    mid: [f64; 2],
    spread: f64,
}
impl Contacts {
    fn geometry(&self) -> Option<([f64; 2], f64)> {
        let mut points = self.points.values();
        let a = *points.next()?;
        Some(if let Some(b) = points.next() {
            (
                [(a[0] + b[0]) / 2., (a[1] + b[1]) / 2.],
                (a[0] - b[0]).hypot(a[1] - b[1]),
            )
        } else {
            (a, 0.)
        })
    }
    pub fn rebase(&mut self, camera: Camera, orbit: Orbit) {
        self.base = self.geometry().map(|(mid, spread)| Base {
            camera,
            orbit,
            mid,
            spread,
        });
    }
    pub fn view(&self, size: [f64; 2]) -> Option<(Camera, Orbit)> {
        let base = self.base?;
        let (mid, spread) = self.geometry()?;
        if self.points.len() == 1 {
            return Some((
                base.camera,
                base.orbit
                    .dragged([mid[0] - base.mid[0], mid[1] - base.mid[1]]),
            ));
        }
        let zoom = (base.camera.zoom
            + if spread > 1. && base.spread > 1. {
                (spread / base.spread).log2()
            } else {
                0.
            })
        .clamp(0., MAX_INTERACTIVE_ZOOM);
        let mut next = Camera {
            zoom,
            ..base.camera
        };
        // Keep ground under original midpoint anchored through both pan and pinch.
        if let (Some(a), Some(b)) = (
            base.orbit.ground_at(base.camera, size, base.mid),
            base.orbit.ground_at(next, size, mid),
        ) {
            next.center = [next.center[0] + a[0] - b[0], next.center[1] + a[1] - b[1]];
        } else {
            next = base.orbit.pan(next, size, base.mid, mid).unwrap_or(next);
        }
        next.center[0] = (next.center[0] + super::camera::WORLD / 2.)
            .rem_euclid(super::camera::WORLD)
            - super::camera::WORLD / 2.;
        next.center[1] =
            next.center[1].clamp(-super::camera::WORLD / 2., super::camera::WORLD / 2.);
        Some((next, base.orbit))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn one_finger_orbits_two_pan_and_transition_does_not_jump() {
        let camera = Camera {
            center: [0., 0.],
            zoom: 13.,
        };
        let orbit = Orbit::default();
        let size = [800., 600.];
        let mut c = Contacts::default();
        c.points.insert(1, [400., 300.]);
        c.rebase(camera, orbit);
        c.points.insert(1, [440., 320.]);
        let (same, rotated) = c.view(size).unwrap();
        assert_eq!(same, camera);
        assert_ne!(rotated, orbit);
        c.points.insert(2, [500., 320.]);
        c.rebase(same, rotated);
        let (next, angle) = c.view(size).unwrap();
        assert!((next.center[0] - same.center[0]).abs() < 1e-8);
        assert_eq!(angle, rotated);
        c.points.insert(1, [480., 350.]);
        c.points.insert(2, [540., 350.]);
        let (panned, angle) = c.view(size).unwrap();
        assert_ne!(panned.center, next.center);
        assert_eq!(angle, rotated);
        c.points.remove(&2);
        c.rebase(panned, angle);
        assert_eq!(c.view(size).unwrap(), (panned, angle));
    }
    #[test]
    fn pinch_changes_zoom_without_changing_orbit_and_preserves_ground_anchor() {
        let mut c = Contacts::default();
        let camera = Camera {
            center: [0., 0.],
            zoom: 12.,
        };
        let orbit = Orbit::default();
        let size = [800., 600.];
        c.points.insert(1, [350., 400.]);
        c.points.insert(2, [450., 400.]);
        c.rebase(camera, orbit);
        c.points.insert(1, [300., 400.]);
        c.points.insert(2, [500., 400.]);
        let (next, angle) = c.view(size).unwrap();
        assert_eq!(angle, orbit);
        assert!((next.zoom - 13.).abs() < 1e-8);
        let a = orbit.ground_at(camera, size, [400., 400.]).unwrap();
        let b = orbit.ground_at(next, size, [400., 400.]).unwrap();
        assert!((a[0] - b[0]).hypot(a[1] - b[1]) < 1e-7);
    }
}

//! Terrain surface composed from existing vector maps and Enlumi DEM/LOD machinery.
use super::{
    camera::TileId,
    terrain::{Orbit, Scene, Selector, Tile},
    terrain_gestures::Contacts,
    terrain_surface::Producer,
    terrain_tiles::{Style, Tiles},
    tiles::{Elevation, Key, Payload, Provider, Service},
};
use crate::ui::state::UiState;
use std::{cell::RefCell, collections::BTreeSet, rc::Rc, sync::Arc};
use zgui::{prelude::*, reactive::RenderEffect};
use zgui_ui::prelude::*;

#[component]
pub fn TerrainView(
    service: Service,
    offline: RwSignal<bool, LocalStorage>,
    map_style: Signal<Arc<Style>, LocalStorage>,
) -> impl IntoView {
    let s = UiState::current();
    let camera = s.camera;
    let orbit = RwSignal::new_local(Orbit::default());
    let host = NodeRef::new();
    let bounds = host.observe_border_box();
    let size = Memo::new(move |_| {
        bounds.get().map_or([0., 0.], |b| {
            [
                f64::from(b.size.width.0 / host.scale()),
                f64::from(b.size.height.0 / host.scale()),
            ]
        })
    });
    let selector = StoredValue::new_local(RefCell::new(Selector::default()));
    let ids = Memo::new(move |_| {
        selector.with_value(|v| v.borrow_mut().select(orbit.get(), camera.get(), size.get()))
    });
    let tiles = Tiles::new();
    let previous = StoredValue::new_local(Scene::default());
    let scene_revision = RwSignal::new_local(0u64);
    let has_height = RwSignal::new_local(false);
    let retry = RwSignal::new_local(0u64);
    let reset = RenderEffect::new(move |_| {
        map_style.track();
        offline.track();
        s.basemap.archive.track();
        tiles.reset();
        previous.set_value(Scene::default());
    });
    let demands = RenderEffect::new(move |_| {
        retry.track();
        let ids = ids.get();
        let mut wanted = BTreeSet::new();
        for id in &ids {
            for z in id.z.saturating_sub(3)..=id.z {
                let shift = id.z - z;
                wanted.insert(TileId {
                    z,
                    x: id.x.div_euclid(1i64 << shift),
                    y: id.y >> shift,
                });
            }
        }
        let mut wanted: Vec<_> = wanted.into_iter().collect();
        orbit
            .get()
            .screen_first(camera.get(), size.get(), &mut wanted);
        // Coarse tiles first: show terrain promptly rather than wait for all leaves.
        wanted.sort_by_key(|t| t.z);
        wanted.truncate(192);
        service.demand_layers(2, &[], &wanted, Provider::OpenStreetMap, !offline.get());
        tiles.request(
            &wanted,
            map_style.get(),
            s.basemap.archive.get(),
            !offline.get(),
        );
    });
    let producer = Producer::new();
    let surface = zgui::elements::surface()
        .class("terrain-surface")
        .source(&producer.handle)
        .into_view();
    let draw = RenderEffect::new(move |_| {
        service.revision.track();
        tiles.revision.track();
        let camera = camera.get();
        let orbit = orbit.get();
        let size = size.get();
        let ids = ids.get();
        let mut ready = std::collections::BTreeMap::new();
        tiles.ready.with_value(|rasters| {
            for (&id, raster) in rasters {
                let elevation = (0..=id.z).rev().find_map(|z| {
                    let shift = id.z - z;
                    let parent = TileId {
                        z,
                        x: id.x.div_euclid(1i64 << shift),
                        y: id.y >> shift,
                    };
                    service
                        .payload_untracked(Key::elevation(parent))
                        .map(|e| (parent, e))
                });
                // Flat preview while DEM downloads; replaced automatically when it arrives.
                let (dem_id, elevation) = elevation.unwrap_or_else(|| {
                    (
                        id,
                        Arc::new(Payload::Elevation(Elevation {
                            version: 0,
                            heights: vec![0.],
                            width: 1,
                            height: 1,
                        })),
                    )
                });
                ready.insert(
                    id,
                    Tile {
                        id,
                        dem_id,
                        raster_id: id,
                        provider: Provider::OpenStreetMap,
                        raster: Some(raster.clone()),
                        elevation: Some(elevation),
                    },
                );
            }
        });
        let old = previous.get_value();
        let selected = Scene::handover(
            &ids,
            &ready,
            &old,
            Provider::OpenStreetMap,
            camera,
            orbit,
            size,
        );
        let scene = Scene::assembled(selected, &old, camera);
        has_height.set(
            scene
                .tiles
                .iter()
                .any(|t| t.elevation.as_ref().is_some_and(|e| e.version() != 0)),
        );
        producer.update(camera, orbit, Arc::new(scene.clone()));
        previous.set_value(scene);
        scene_revision.update(|n| *n += 1);
    });
    on_cleanup_local(move || {
        drop(reset);
        drop(demands);
        drop(draw);
        tiles.reset();
        service.demand_layers(2, &[], &[], Provider::OpenStreetMap, false);
    });
    let marker = Memo::new(move |_| {
        scene_revision.track();
        let (point, _) = s.position.get()?;
        let p = super::camera::project(point.lon, point.lat)?;
        let camera = camera.get();
        let orbit = orbit.get();
        let size = size.get();
        previous.with_value(|scene| {
            let height = scene
                .tiles
                .iter()
                .filter_map(|tile| tile.mesh_height_at(p).map(|h| (tile.id.z, h)))
                .max_by_key(|(z, _)| *z)?
                .1;
            let scale = super::terrain::ground_scale(camera);
            let v = [
                ((p[0] - camera.center[0]) * scale) as f32,
                (height - scene.center_height(camera)) * orbit.exaggeration,
                ((camera.center[1] - p[1]) * scale) as f32,
                1.,
            ];
            let m = orbit.matrix(camera, size);
            let clip: [f32; 4] =
                std::array::from_fn(|row| (0..4).map(|col| m[col * 4 + row] * v[col]).sum());
            if clip[3] <= 0. || clip[2] < 0. || clip[2] > clip[3] {
                return None;
            }
            let x = f64::from(clip[0] / clip[3]);
            let y = f64::from(clip[1] / clip[3]);
            (x.abs() <= 1. && y.abs() <= 1.)
                .then_some([(x + 1.) * size[0] / 2., (1. - y) * size[1] / 2.])
        })
    });
    let contacts = Rc::new(RefCell::new(Contacts::default()));
    let point = move |p: [f64; 2]| {
        let origin = bounds
            .get_untracked()
            .map(|b| {
                [
                    f64::from(b.origin.x.0 / host.scale()),
                    f64::from(b.origin.y.0 / host.scale()),
                ]
            })
            .unwrap_or_default();
        [p[0] - origin[0], p[1] - origin[1]]
    };
    let down = contacts.clone();
    let moved = contacts.clone();
    let released = contacts.clone();
    let release = move |id| {
        let mut c = released.borrow_mut();
        c.points.remove(&id);
        c.rebase(camera.get_untracked(), orbit.get_untracked());
    };
    let cancel = release.clone();
    view! {
        box(class="terrain-view",node_ref=host,a11y:label="3D map: one finger rotates and tilts; two fingers drag and pinch") {
            {surface}
            box(class="terrain-input",on:pointer_down=move |ev|{ev.capture_pointer();ev.prevent_default();ev.stop_propagation();s.follow.set(false);s.anchored.set(true);let mut c=down.borrow_mut();c.points.insert(ev.id.raw(),point([f64::from(ev.position.x.0),f64::from(ev.position.y.0)]));c.rebase(camera.get_untracked(),orbit.get_untracked());},
                on:pointer_move=move |ev|{ev.stop_propagation();let mut c=moved.borrow_mut();if let Some(p)=c.points.get_mut(&ev.id.raw()){*p=point([f64::from(ev.position.x.0),f64::from(ev.position.y.0)]);if let Some((next,angle))=c.view(size.get_untracked()){camera.set(next);orbit.set(angle);}}},
                on:pointer_up=move |ev|{ev.release_pointer();ev.stop_propagation();release(ev.id.raw());},
                on:pointer_cancel=move |ev|{ev.release_pointer();ev.stop_propagation();cancel(ev.id.raw());}) {}
            if move ||marker.get().is_some() {box(class="gps-dot",style:transform=move ||marker.get().map(|p|format!("translate({}px, {}px)",p[0]-8.,p[1]-8.))) {}}
            column(class="terrain-help") {
                text {"1 finger: tilt / rotate · 2 fingers: pan / pinch"}
                if move ||!has_height.get() {text {{move ||if offline.get(){"Cached elevation unavailable · flat preview"}else{"Loading elevation · flat preview"}}}}
                if move ||!tiles.error.get().is_empty()||service.status().contains("errors") {
                    Button(variant=ButtonVariant::Secondary,on:click=move |_|{tiles.reset();service.retry();retry.update(|n|*n+=1);}) {"Retry map / terrain"}
                }
            }
        }
    }
}

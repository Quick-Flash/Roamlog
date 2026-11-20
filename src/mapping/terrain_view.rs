//! Terrain surface composed from existing vector maps and Enlumi DEM/LOD machinery.
use super::{
    camera::TileId,
    terrain::{Orbit, Scene, Selector, Tile},
    terrain_gestures::Contacts,
    terrain_markers::TerrainMarkersProps,
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
    orbit: RwSignal<Orbit, LocalStorage>,
    map_style: Signal<Arc<Style>, LocalStorage>,
) -> impl IntoView {
    let s = UiState::current();
    let camera = s.camera;
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
    let tiles = Tiles::new();
    let scene = RwSignal::new_local(Arc::new(Scene::default()));
    let has_height = RwSignal::new_local(false);
    let retry = RwSignal::new_local(0u64);
    let reset = RenderEffect::new(move |_| {
        map_style.track();
        s.imagery.track();
        offline.track();
        s.basemap.archive.track();
        tiles.reset();
        scene.set(Arc::new(Scene::default()));
    });
    // Camera frames reuse retained scene; tile selection/downloads are bounded to 20 Hz.
    let throttle = super::throttle::Throttle::new(std::time::Duration::from_millis(50));
    let update = move || {
        let style = map_style.get_untracked();
        let source = s.imagery.get_untracked();
        let camera = camera.get_untracked();
        let orbit = orbit.get_untracked();
        let size = size.get_untracked();
        let ids = selector.with_value(|v| v.borrow_mut().select(orbit, camera, size));
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
        orbit.screen_first(camera, size, &mut wanted);
        // Coarse tiles first: show terrain promptly rather than wait for all leaves.
        wanted.sort_by_key(|t| t.z);
        wanted.truncate(192);
        // Imagery detail follows screen pixels rather than terrain mesh resolution.
        let mut rasters =
            super::terrain_tiles::detail_tiles(orbit, camera, size, f64::from(host.scale()), &ids);
        let mut cached = Vec::new();
        for &id in &ids {
            cached.extend(
                service
                    .cached_raster_children(id, source)
                    .into_iter()
                    .filter(|child| child.z <= id.z.saturating_add(3)),
            );
            tiles.ready.with_value(|ready| {
                cached.extend(ready.keys().copied().filter(|&child| {
                    super::terrain::contains(id, child)
                        && child.z > id.z
                        && child.z <= id.z.saturating_add(3)
                }))
            });
        }
        orbit.screen_first(camera, size, &mut cached);
        rasters = source.expand_tiles(super::terrain_tiles::prioritize_detail(
            rasters, cached, &wanted,
        ));
        service.demand_layers(
            2,
            if style.satellite { &rasters } else { &[] },
            &wanted,
            source,
            !offline.get_untracked(),
        );
        let mut imagery = std::collections::BTreeMap::new();
        if style.satellite {
            for &id in &rasters {
                for z in (id.z.saturating_sub(8)..=id.z).rev() {
                    let shift = id.z - z;
                    let parent = TileId {
                        z,
                        x: id.x.div_euclid(1i64 << shift),
                        y: id.y >> shift,
                    };
                    if let Some(payload) =
                        service.payload_untracked(Key::raster_from(parent, source))
                    {
                        imagery.insert(id, (parent, payload));
                        break;
                    }
                }
            }
        }
        tiles.request(
            &rasters,
            style,
            s.basemap.archive.get_untracked(),
            !offline.get_untracked(),
            &imagery,
        );
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
        let old = scene.get_untracked();
        let selected = Scene::handover(
            &ids,
            &ready,
            &old,
            Provider::OpenStreetMap,
            camera,
            orbit,
            size,
        );
        let next = Scene::assembled(selected, &old, camera);
        has_height.set(
            next.tiles
                .iter()
                .any(|t| t.elevation.as_ref().is_some_and(|e| e.version() != 0)),
        );
        scene.set(Arc::new(next));
    };
    let work = RenderEffect::new(move |_| {
        camera.track();
        orbit.track();
        size.track();
        map_style.track();
        s.imagery.track();
        offline.track();
        s.basemap.archive.track();
        retry.track();
        service.revision.track();
        tiles.revision.track();
        throttle.run(update);
    });
    let producer = Producer::new();
    let surface = zgui::elements::surface()
        .class("terrain-surface")
        .source(&producer.handle)
        .into_view();
    let draw = RenderEffect::new(move |_| producer.update(camera.get(), orbit.get(), scene.get()));
    on_cleanup_local(move || {
        drop(reset);
        drop(work);
        drop(draw);
        throttle.cancel();
        tiles.reset();
        service.demand_layers(2, &[], &[], Provider::OpenStreetMap, false);
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
    let pending: StoredValue<Option<zgui::view::FrameHandle>, LocalStorage> =
        StoredValue::new_local(None);
    let apply = {
        let contacts = contacts.clone();
        move || {
            if let Some((next, angle)) = contacts.borrow().view(size.get_untracked()) {
                camera.set(next);
                orbit.set(angle);
            }
        }
    };
    let down_apply = apply.clone();
    let schedule = {
        let apply = apply.clone();
        move || {
            if Timers::current().is_none() {
                apply();
                return;
            }
            if pending.with_value(Option::is_none) {
                let apply = apply.clone();
                pending.set_value(Some(request_frame(move |_| {
                    pending.set_value(None);
                    apply();
                })));
            }
        }
    };
    let release = {
        let apply = apply.clone();
        move |id| {
            pending.set_value(None);
            apply();
            let mut c = released.borrow_mut();
            c.points.remove(&id);
            c.rebase(camera.get_untracked(), orbit.get_untracked());
        }
    };
    let cancel = release.clone();
    on_cleanup_local(move || pending.set_value(None));
    view! {
        box(class="terrain-view",node_ref=host,a11y:label="3D map: one finger rotates and tilts; two fingers drag and pinch") {
            {surface}
            box(class="terrain-input",on:pointer_down=move |ev|{pending.set_value(None);down_apply();ev.capture_pointer();ev.prevent_default();ev.stop_propagation();s.follow.set(false);s.anchored.set(true);let mut c=down.borrow_mut();c.points.insert(ev.id.raw(),point([f64::from(ev.position.x.0),f64::from(ev.position.y.0)]));c.rebase(camera.get_untracked(),orbit.get_untracked());},
                on:pointer_move=move |ev|{ev.stop_propagation();let changed={let mut c=moved.borrow_mut();if let Some(p)=c.points.get_mut(&ev.id.raw()){*p=point([f64::from(ev.position.x.0),f64::from(ev.position.y.0)]);true}else{false}};if changed {schedule();}},
                on:pointer_up=move |ev|{ev.release_pointer();ev.stop_propagation();release(ev.id.raw());},
                on:pointer_cancel=move |ev|{ev.release_pointer();ev.stop_propagation();cancel(ev.id.raw());}) {}
            TerrainMarkers(scene=Signal::derive_local(move ||scene.get()),camera=camera,orbit=orbit,size=Signal::derive_local(move ||size.get()))
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

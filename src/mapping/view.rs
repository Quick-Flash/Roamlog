use super::{
    basemap::Kind,
    camera::{self, Camera, project},
    gestures::Contacts,
    map_gpu::{LineStyle, Lines, Scene},
    map_surface::Producer as MapProducer,
    terrain_tiles::Polyline,
    terrain_tiles::Style as TerrainStyle,
    terrain_view::TerrainViewProps,
    tiles::{Config, Provider, Service},
    vector_tiles::VectorTiles,
};
use crate::ui::icons::{COMPASS, EXPLORE, FIT, LOCATE, SLIDERS};
use crate::ui::{
    settings::SettingsProps,
    state::UiState,
    tools::{AreaBarProps, AreaDialogProps, AreasSheetProps, TripEditorDialogProps},
    view_options::ViewOptionsProps,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::Arc,
};
use zgui::{
    prelude::*,
    reactive::RenderEffect,
    vocab::{Key, NamedKey},
};
use zgui_ui::prelude::*;
use zgui_ui_icons::{
    prelude::*,
    set::mark::{MINUS, PLUS},
};

#[component]
pub fn MapView() -> impl IntoView {
    let s = UiState::current();
    let camera = s.camera;
    let three_d = RwSignal::new_local(false);
    let orbit = RwSignal::new_local(super::terrain::Orbit::default());
    let map_bearing = move || {
        if three_d.get() {
            f64::from(orbit.get().bearing).to_radians()
        } else {
            s.bearing.get()
        }
    };
    let service = Service::new(Config {
        cache: s.directory.get_value().join("maps"),
        local: None,
        network: true,
    });
    let offline = RwSignal::new_local(false);
    let retry = RenderEffect::new(move |_| {
        if !offline.get() {
            service.retry();
        }
    });
    on_cleanup_local(move || drop(retry));
    let host = NodeRef::new();
    let bounds = host.observe_border_box();
    let view_size = Memo::new(move |_| {
        bounds.get().map_or([0.0, 0.0], |b| {
            [
                f64::from(b.size.width.0 / host.scale()),
                f64::from(b.size.height.0 / host.scale()),
            ]
        })
    });
    // Map layers draw in rotor box: viewport itself when north up, else square of viewport
    // diagonal so rotated corners never show.
    let size = Memo::new(move |_| {
        let [w, h] = view_size.get();
        if s.bearing.get() == 0.0 {
            [w, h]
        } else {
            let d = w.hypot(h).ceil();
            [d, d]
        }
    });
    let vector = Memo::new(move |_| s.basemap.archive.with(Option::is_some));
    // Online raster tiles: no offline map at all, or satellite underlay. Beyond downloaded
    // area, basemap streams same vector tiles online instead.
    let raster = Memo::new(move |_| !three_d.get() && (!vector.get() || s.satellite.get()));
    let outside = Memo::new(move |_| vector.get() && s.basemap.outside(camera.get(), size.get()));
    let provider = Memo::new(move |_| {
        if s.satellite.get() {
            Provider::EsriImagery
        } else {
            Provider::OpenStreetMap
        }
    });
    let raster_tiles = Memo::new(move |_| {
        if !raster.get() {
            return Vec::new();
        }
        service.revision.track();
        let camera = camera.get();
        let size = size.get();
        let provider = provider.get();
        let visible = camera.map_tiles(size);
        let mut detail = std::collections::BTreeSet::new();
        for &tile in &visible {
            detail.extend(
                service
                    .cached_raster_children(tile, provider)
                    .into_iter()
                    .filter(|child| child.z <= tile.z + 2),
            );
        }
        let mut detail: Vec<_> = detail.into_iter().collect();
        camera.center_first(&mut detail);
        detail.truncate(256);
        detail.sort_unstable();
        camera
            .map_fallback_tiles(size)
            .into_iter()
            .chain(visible)
            .chain(detail)
            .collect::<Vec<_>>()
    });
    // Without trips or GPS, open offline map at its own center.
    let center = RenderEffect::new(move |_| {
        if let Some(archive) = s.basemap.archive.get()
            && !s.anchored.get_untracked()
            && let Some(center) = project(archive.center[0], archive.center[1])
        {
            camera.set(Camera { center, zoom: 14.0 });
            s.anchored.set(true);
        }
    });
    on_cleanup_local(move || drop(center));
    let demand = RenderEffect::new(move |_| {
        // Street raster without offline map, or optional satellite underlay. Visible tiles only.
        let mut tiles = Vec::new();
        if raster.get() {
            tiles = raster_tiles.get();
        }
        service.demand(1, &tiles, false, provider.get(), !offline.get());
    });
    on_cleanup_local(move || {
        drop(demand);
        service.demand(1, &[], false, provider.get_untracked(), false);
    });
    // Back (button or edge swipe) while an area is drawn or shown steps back inside map
    // instead of leaving app. Bubble leg: open dialogs answer Back first.
    let back_guard = Rc::new(RefCell::new(Vec::new()));
    let back = {
        let back_guard = back_guard.clone();
        RenderEffect::new(move |_| {
            if host.get().is_none() || !back_guard.borrow().is_empty() {
                return;
            }
            let Some(window) = host.window_root() else {
                return;
            };
            back_guard.borrow_mut().extend(window.listen(
                events::KEY_DOWN,
                ListenerOptions::DEFAULT,
                move |ev: &mut EventCx<'_, events::KeyDown>| {
                    if ev.key != Key::Named(NamedKey::BrowserBack) {
                        return;
                    }
                    if s.area_drawing.get_untracked() {
                        if s.area_history.with_value(Vec::is_empty) {
                            s.cancel_area();
                        } else {
                            s.undo_area();
                        }
                    } else if s.area_stats.with_untracked(Option::is_some) {
                        s.cancel_area();
                    } else if s.target.with_untracked(Option::is_some) {
                        s.target.set(None);
                    } else {
                        return;
                    }
                    ev.prevent_default();
                    ev.stop_propagation();
                },
            ));
        })
    };
    on_cleanup_local(move || {
        drop(back);
        back_guard.borrow_mut().clear();
    });
    let contacts = Rc::new(RefCell::new(Contacts::default()));
    // Single pointer that has not moved beyond slop: a tap, not a drag.
    let tap: Rc<Cell<Option<Press>>> = Rc::new(Cell::new(None));
    // Area corner held while editing: pointer, corner index, start, moved past slop.
    let held: Rc<Cell<Option<Held>>> = Rc::new(Cell::new(None));
    // Window CSS px -> viewport CSS px.
    // Window CSS px of viewport centre.
    let pivot = move || {
        let b = bounds.get_untracked()?;
        let zoom = f64::from(host.scale());
        let [w, h] = view_size.get_untracked();
        Some([
            f64::from(b.origin.x.0) / zoom + w / 2.0,
            f64::from(b.origin.y.0) / zoom + h / 2.0,
        ])
    };
    // Window CSS px -> rotor px (map layer frame, before its rotation).
    let viewport = move |at: [f64; 2]| {
        let c = pivot()?;
        let (sin, cos) = s.bearing.get_untracked().sin_cos();
        let v = [at[0] - c[0], at[1] - c[1]];
        let [w, h] = size.get_untracked();
        Some([
            v[0] * cos + v[1] * sin + w / 2.0,
            -v[0] * sin + v[1] * cos + h / 2.0,
        ])
    };
    // Viewport px -> geographic point, plus ground metres per CSS px there.
    let geo = move |[x, y]: [f64; 2]| {
        let camera = camera.get_untracked();
        let [w, h] = size.get_untracked();
        let [lon, lat] = camera::geographic([
            camera.center[0] + (x - w / 2.0) / camera.scale(),
            camera.center[1] - (y - h / 2.0) / camera.scale(),
        ]);
        (
            crate::geo::Point { lat, lon },
            lat.to_radians().cos() / camera.scale(),
        )
    };
    // Corner under viewport position while editing area.
    let corner_at = move |xy: [f64; 2]| {
        if !s.area_drawing.get_untracked() {
            return None;
        }
        let camera = camera.get_untracked();
        let [w, h] = size.get_untracked();
        s.area_points.with_untracked(|points| {
            points
                .iter()
                .enumerate()
                .filter_map(|(i, p)| {
                    let m = project(p.lon, p.lat)?;
                    let px = (m[0] - camera.center[0]) * camera.scale() + w / 2.0;
                    let py = h / 2.0 - (m[1] - camera.center[1]) * camera.scale();
                    Some((i, (px - xy[0]).hypot(py - xy[1])))
                })
                .filter(|(_, d)| *d <= 24.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, _)| i)
        })
    };
    let down = {
        let contacts = contacts.clone();
        let tap = tap.clone();
        let held = held.clone();
        move |ev: &mut EventCx<'_, events::PointerDown>| {
            ev.capture_pointer();
            ev.prevent_default();
            s.anchored.set(true);
            let mut contacts = contacts.borrow_mut();
            let at = [f64::from(ev.position.x.0), f64::from(ev.position.y.0)];
            if contacts.points.is_empty()
                && let Some(index) = viewport(at).and_then(corner_at)
            {
                // Corner press: drag moves it, release in place removes it.
                s.push_area_history();
                held.set(Some((ev.id.raw(), index, at, false)));
                tap.set(None);
                return;
            }
            tap.set(contacts.points.is_empty().then_some((ev.id.raw(), at)));
            contacts.points.insert(ev.id.raw(), at);
            contacts.rebase(
                camera.get_untracked(),
                s.bearing.get_untracked(),
                pivot().unwrap_or(at),
            );
        }
    };
    let moved = {
        let contacts = contacts.clone();
        let tap = tap.clone();
        let held = held.clone();
        move |ev: &mut EventCx<'_, events::PointerMove>| {
            let at = [f64::from(ev.position.x.0), f64::from(ev.position.y.0)];
            if let Some((id, index, start, moved)) = held.get()
                && id == ev.id.raw()
            {
                let moved = moved || (at[0] - start[0]).hypot(at[1] - start[1]) > 6.0;
                held.set(Some((id, index, start, moved)));
                if moved && let Some(xy) = viewport(at) {
                    s.move_corner(index, geo(xy).0);
                }
                return;
            }
            let mut contacts = contacts.borrow_mut();
            if let Some(point) = contacts.points.get_mut(&ev.id.raw()) {
                *point = at;
                if tap.get().is_some_and(|(_, start)| {
                    (point[0] - start[0]).hypot(point[1] - start[1]) > 10.0
                }) {
                    tap.set(None);
                }
                if let Some((next, bearing)) = contacts.camera() {
                    if next != camera.get_untracked() {
                        s.follow.set(false);
                    }
                    camera.set(next);
                    if bearing != s.bearing.get_untracked() {
                        s.bearing.set(bearing);
                    }
                }
            }
        }
    };
    let release = move |id: u64| {
        let mut contacts = contacts.borrow_mut();
        let at = contacts.points.remove(&id);
        contacts.rebase(
            camera.get_untracked(),
            s.bearing.get_untracked(),
            pivot().or(at).unwrap_or_default(),
        );
    };
    let cancel = release.clone();
    let tapped = move |id: u64, at: [f64; 2]| {
        if let Some((held_id, index, _, moved)) = held.get()
            && held_id == id
        {
            held.set(None);
            if !moved {
                s.remove_corner(index);
            }
            return;
        }
        let Some((tap_id, _)) = tap.take() else {
            return;
        };
        if tap_id != id {
            return;
        }
        if let Some(xy) = viewport(at) {
            let (point, meters_per_px) = geo(xy);
            s.tap(point, meters_per_px);
        }
    };
    let fit = move || s.show_history();
    // Screen position and accuracy radius (CSS px) of live position.
    let marker = Memo::new(move |_| {
        let (point, accuracy) = s.position.get()?;
        let camera = camera.get();
        let [w, h] = size.get();
        let xy = project(point.lon, point.lat)?;
        let (x, y) = (
            (xy[0] - camera.center[0]) * camera.scale() + w / 2.0,
            h / 2.0 - (xy[1] - camera.center[1]) * camera.scale(),
        );
        // Mercator meters grow by 1/cos(lat) relative to ground meters.
        let radius = accuracy / point.lat.to_radians().cos() * camera.scale();
        let cover = [(0.0, 0.0), (w, 0.0), (0.0, h), (w, h)]
            .iter()
            .all(|&(cx, cy): &(f64, f64)| (cx - x).hypot(cy - y) < radius);
        Some(Marker {
            x,
            y,
            radius,
            cover,
        })
    });
    // Friend markers: name, screen position, label, live. Separate overlay from trips.
    let friend_marks = Memo::new(move |_| {
        if !s.show_friends.get() {
            return Vec::new();
        }
        let camera = camera.get();
        let [w, h] = size.get();
        let now = s.now.get() as i64;
        s.friend_markers.with(|markers| {
            markers
                .iter()
                .filter_map(|f| {
                    let xy = project(f.point.lon, f.point.lat)?;
                    let x = (xy[0] - camera.center[0]) * camera.scale() + w / 2.0;
                    let y = h / 2.0 - (xy[1] - camera.center[1]) * camera.scale();
                    let age = (now - f.timestamp_ms).max(0) / 1000;
                    let when = match age {
                        0..60 => "now".to_owned(),
                        60..3600 => format!("{} min ago", age / 60),
                        _ => format!("{} h ago", age / 3600),
                    };
                    let live = f.live && age < 900;
                    Some((f.name.clone(), x, y, format!("{} · {when}", f.name), live))
                })
                .collect::<Vec<_>>()
        })
    });
    // Projection depends on history, not camera. Keep pan frames to affine placement
    // and clipping instead of projecting thousands of GPS/road coordinates again.
    // Layers: traveled by trips on road (1, 2, 3–4, 5+), GPS, inferred, area outline,
    // nearest-road target.
    let geometry = Memo::new(move |_| {
        let filter = s.history_filter.get();
        let mut lines: [Polylines; LAYERS] = Default::default();
        let mut add = |layer: usize, a: crate::geo::Point, b: crate::geo::Point| {
            if let (Some(a), Some(b)) = (project(a.lon, a.lat), project(b.lon, b.lat)) {
                lines[layer].segment(a, b);
            }
        };
        if s.show_roads.get() && s.coverage_filter.get() == filter {
            let heat = s.heatmap.get();
            s.sections.with(|sections| {
                for section in sections.iter().filter(|section| section.traveled) {
                    let layer = match section.visits {
                        _ if !heat => 0,
                        0 | 1 => 0,
                        2 => 1,
                        3 | 4 => 2,
                        _ => 3,
                    };
                    add(layer, section.start, section.end);
                }
            });
        }
        if s.show_gps.get() {
            s.data.with(|d| {
                for trip in d.trips_for(filter) {
                    for pair in trip.points.windows(2).filter(|p| p[1].connected) {
                        add(
                            if pair[1].estimated { 5 } else { 4 },
                            pair[0].fix.point,
                            pair[1].fix.point,
                        );
                    }
                }
            });
        }
        s.area_points.with(|points| {
            for pair in points.windows(2) {
                add(6, pair[0], pair[1]);
            }
            if points.len() >= 3 {
                add(6, points[points.len() - 1], points[0]);
            }
        });
        if let Some(target) = s.target.get() {
            add(7, target.start, target.end);
        }
        for layer in &mut lines {
            layer.finish();
        }
        lines
    });
    let terrain_style = Memo::new(move |_| {
        let mut layers =
            geometry.with(|layers| layers.iter().map(|p| p.lines.clone()).collect::<Vec<_>>());
        let mut missing = Polylines::default();
        s.area_stats.with(|stats| {
            for section in stats.iter().flat_map(|st| &st.untraveled) {
                if let (Some(a), Some(b)) = (
                    project(section.start.lon, section.start.lat),
                    project(section.end.lon, section.end.lat),
                ) {
                    missing.segment(a, b);
                }
            }
        });
        missing.finish();
        layers.push(missing.lines);
        TerrainStyle {
            dark: s.dark.get(),
            satellite: s.satellite.get(),
            layers,
        }
    });
    // Area editing remains in established flat-map interaction model.
    let flat_edit = RenderEffect::new(move |_| {
        if s.area_drawing.get() {
            three_d.set(false);
        }
    });
    on_cleanup_local(move || drop(flat_edit));
    // GPU map: base map tiles and names (offline vector map, not satellite) and trip lines
    // drawn by own wgpu surface. Geometry uploads once per change; camera moves only update
    // uniforms and label quads.
    let gpu_base = Memo::new(move |_| !three_d.get() && vector.get() && !s.satellite.get());
    let gpu_lines = Memo::new(move |_| {
        let mut lines = Lines::default();
        if three_d.get() {
            return Arc::new(lines);
        }
        s.area_stats.with(|stats| {
            for section in stats.iter().flat_map(|st| &st.untraveled) {
                if let (Some(a), Some(b)) = (
                    project(section.start.lon, section.start.lat),
                    project(section.end.lon, section.end.lat),
                ) {
                    lines.push(a, b, UNTRAVELED_STYLE);
                }
            }
        });
        geometry.with(|layers| {
            for (polylines, &style) in layers.iter().zip(&LINE_STYLE) {
                for line in &polylines.lines {
                    for pair in line.points.windows(2) {
                        lines.push(pair[0], pair[1], style);
                    }
                }
            }
        });
        Arc::new(lines)
    });
    let vector_tiles = VectorTiles::new();
    let gpu_scene = Memo::new(move |_| {
        let base = gpu_base.get();
        let tiles = match s.basemap.archive.get() {
            Some(archive) if base => {
                vector_tiles.visible(&archive, camera.get(), size.get(), !offline.get())
            }
            _ => Vec::new(),
        };
        Arc::new(Scene {
            tiles,
            lines: gpu_lines.get(),
            base: base.then(|| s.dark.get()),
        })
    });
    let map_producer = std::rc::Rc::new(MapProducer::new());
    let map_surface = zgui::elements::surface()
        .class("map-overlay")
        .source(&map_producer.handle)
        .into_view();
    let feed = {
        let producer = map_producer.clone();
        RenderEffect::new(move |_| {
            if !three_d.get() {
                producer.update(camera.get(), gpu_scene.get());
            }
        })
    };
    on_cleanup_local(move || {
        drop(feed);
        drop(map_producer);
    });
    // Area corners as plain boxes, like GPS marker.
    let corners = Memo::new(move |_| {
        let camera = camera.get();
        let [w, h] = size.get();
        s.area_points.with(|points| {
            points
                .iter()
                .filter_map(|p| project(p.lon, p.lat))
                .map(|xy| {
                    (
                        (xy[0] - camera.center[0]) * camera.scale() + w / 2.0,
                        h / 2.0 - (xy[1] - camera.center[1]) * camera.scale(),
                    )
                })
                .collect::<Vec<_>>()
        })
    });
    // Keep map size available to fits started outside map (trip sheet).
    let remember = RenderEffect::new(move |_| s.map_size.set_value(view_size.get()));
    on_cleanup_local(move || drop(remember));
    view! {
        box(class="map-card") {
            box(class="map-viewport",node_ref=host,a11y:label="Traveled roads map",on:pointer_down=down,on:pointer_move=moved,
                on:pointer_up=move |ev|{ev.release_pointer();tapped(ev.id.raw(),[f64::from(ev.position.x.0),f64::from(ev.position.y.0)]);release(ev.id.raw());},on:pointer_cancel=move |ev|{ev.release_pointer();cancel(ev.id.raw());}) {
                if move ||three_d.get() {
                    TerrainView(service=service,offline=offline,orbit=orbit,map_style=Signal::derive_local(move ||std::sync::Arc::new(terrain_style.get())))
                }
                box(class="map-rotor",class:map-hidden=move ||three_d.get(),
                    style:width=move ||Some(format!("{}px",size.get()[0])),style:height=move ||Some(format!("{}px",size.get()[1])),
                    style:left=move ||Some(format!("{}px",(view_size.get()[0]-size.get()[0])/2.0)),style:top=move ||Some(format!("{}px",(view_size.get()[1]-size.get()[1])/2.0)),
                    style:transform=move ||{let b=s.bearing.get();(b!=0.0).then(||format!("rotate({b}rad)"))}) {
                box(class="map-tiles",class:map-hidden=move ||!raster.get(),class:map-dark=move ||s.dark.get()&&!s.satellite.get()) {
                    for tile in move ||raster_tiles.get(),key=|tile|*tile {
                        box(class="map-tile",style:transform=move ||{let[x,y,scale]=camera.get().place(tile,size.get());Some(format!("translate({x}px, {y}px) scale({scale})"))}) {
                            {zgui::elements::image().class("map-tile-image").src(Signal::derive_local(move ||service.url(tile,provider.get()))).alt(Some("Basemap imagery tile".to_string())).into_view()}
                        }
                    }
                }
                box(class="map-vector-tiles") {
                    for item in move ||if three_d.get()||!s.satellite.get(){Vec::new()}else{s.basemap.visible(camera.get(),size.get(),true,Kind::Roads,f64::from(host.scale()),!offline.get())},key=|item|item.clone() {
                        // 512 CSS px box: nominal display size of rendered tile; place() scales 256 px boxes.
                        box(class="map-vector-tile",style:transform=move ||{let[x,y,scale]=camera.get().place(item.0,size.get());Some(format!("translate({x}px, {y}px) scale({})",scale/2.0))}) {
                            {zgui::elements::image().class("map-vector-image").src(Some(item.1.clone())).alt(Some("Map tile".to_string())).into_view()}
                        }
                    }
                }
                box(class="map-overlay",class:map-hidden=move ||three_d.get()) {{map_surface}}
                // Satellite mode: street names above trip and area lines.
                box(class="map-vector-tiles") {
                    for item in move ||if three_d.get()||!s.satellite.get(){Vec::new()}else{s.basemap.visible(camera.get(),size.get(),true,Kind::Labels,f64::from(host.scale()),!offline.get())},key=|item|item.clone() {
                        box(class="map-vector-tile",style:transform=move ||{let[x,y,scale]=camera.get().place(item.0,size.get());Some(format!("translate({x}px, {y}px) scale({})",scale/2.0))}) {
                            {zgui::elements::image().class("map-vector-image").src(Some(item.1.clone())).alt(Some("Map labels".to_string())).into_view()}
                        }
                    }
                }
                // GPS marker as plain rounded boxes: compositor draws them as quads, while
                // canvas shapes cost area x segments per frame on Mali (45 vs 60 fps at z18).
                if move ||marker.get().is_some_and(|m|m.cover) {box(class="gps-cover") {}}
                if move ||marker.get().is_some_and(|m|!m.cover&&m.radius>8.0) {
                    box(class="gps-accuracy",style:transform=move ||marker.get().map(|m|format!("translate({}px, {}px)",m.x-m.radius,m.y-m.radius)),
                        style:width=move ||marker.get().map(|m|format!("{}px",2.0*m.radius)),style:height=move ||marker.get().map(|m|format!("{}px",2.0*m.radius))) {}
                }
                for corner in move ||corners.get(),key=|c|(c.0.to_bits(),c.1.to_bits()) {
                    box(class="area-corner",style:transform=format!("translate({}px, {}px)",corner.0-9.0,corner.1-9.0)) {}
                }
                for friend in move ||friend_marks.get(),key=|f|(f.0.clone(),f.1.to_bits(),f.2.to_bits(),f.3.clone(),f.4) {
                    box(class="friend-marker",class:friend-stale=!friend.4,style:transform=format!("translate({}px, {}px)",friend.1-8.0,friend.2-8.0)) {
                        box(class="friend-dot") {}
                        text(class="friend-label") {{friend.3.clone()}}
                    }
                }
                if move ||marker.get().is_some() {
                    box(class="gps-dot",style:transform=move ||marker.get().map(|m|format!("translate({}px, {}px)",m.x-8.0,m.y-8.0))) {}
                }
                }
                text(class="attribution") {{move ||if three_d.get()&&s.satellite.get(){"Imagery: Esri, Vantor, Earthstar Geographics · Roads: © OpenStreetMap · Terrain: © Mapterhorn"}else if three_d.get(){"Protomaps © OpenStreetMap · Terrain: © Mapterhorn"}else if s.satellite.get(){"Imagery: Esri, Vantor, Earthstar Geographics, GIS User Community · Roads: © OpenStreetMap"}else if vector.get(){"Protomaps © OpenStreetMap"}else{"© OpenStreetMap contributors"}}}
            }
            if move ||s.area_drawing.get()||s.area_busy.get()||s.area_stats.with(Option::is_some) {
                AreaBar()
            } else {
                Button(variant=ButtonVariant::Secondary,class="map-history",on:click=move |_|s.trips_open.set(true)) {{move ||s.history_summary()}}
            }
            AreaDialog()
            Settings()
            TripEditorDialog()
            // Siblings of viewport, so taps reach buttons instead of starting a drag.
            row(class="map-top") {
                ViewOptions()
                AreasSheet()
                if move ||cfg!(target_os="android") {
                    Button(variant=ButtonVariant::Secondary,class="map-chip friends-chip",on:click=move |_|s.friends_open.set(true)) {"Friends"}
                }
                if move ||three_d.get()||raster.get()||outside.get() {
                    Button(variant=ButtonVariant::Secondary,class="map-chip",on:click=move |_|offline.update(|v| *v = !*v)) {{move ||if offline.get(){"Offline"}else{"Online"}}}
                }
                if move ||raster.get()&&service.status().contains("errors") {
                    Button(variant=ButtonVariant::Secondary,class="map-chip",on:click=move |_|service.retry()) {"Retry"}
                    Button(variant=ButtonVariant::Secondary,class="map-chip",on:click=move |_|s.open_settings()) {"Settings"}
                }
            }
            if move ||!s.message.get().is_empty() {
                text(class="map-message",a11y:role=Role::Status,on:click=move |_|s.message.set(String::new())) {{move ||s.message.get()}}
            }
            Button(variant=ButtonVariant::Secondary,class="map-button compass",on:click=move |_|{if three_d.get_untracked(){orbit.update(|v|v.bearing=0.0);}else{s.bearing.set(0.0);}},a11y:label=move ||format!("Map heading {:.0} degrees; reset north",(-map_bearing().to_degrees()).rem_euclid(360.0))) {
                box(class="compass-needle",style:transform=move ||Some(format!("rotate({}rad)",map_bearing()))) {Icon(icon=COMPASS)}
            }
            column(class="map-controls") {
                Button(variant=ButtonVariant::Secondary,class="map-button terrain-toggle",disabled=Signal::derive_local(move ||s.area_drawing.get()),on:click=move |_|three_d.update(|v|*v = !*v),a11y:label=move ||if three_d.get(){"Switch to 2D map"}else{"Switch to 3D terrain map"}) {{move ||if three_d.get(){"2D"}else{"3D"}}}

                if move ||s.follow.get() {
                    Button(class="map-button",on:click=move |_|s.locate(),a11y:label="Following my location") {Icon(icon=LOCATE)}
                } else {
                    Button(variant=ButtonVariant::Secondary,class="map-button",on:click=move |_|s.locate(),a11y:label="Follow my location") {Icon(icon=LOCATE)}
                }
                Button(variant=ButtonVariant::Secondary,class="map-button",on:click=move |_|camera.update(|c|c.zoom=(c.zoom+1.0).min(camera::MAX_INTERACTIVE_ZOOM)),a11y:label="Zoom in") {Icon(icon=PLUS)}
                Button(variant=ButtonVariant::Secondary,class="map-button",on:click=move |_|camera.update(|c|c.zoom=(c.zoom-1.0).max(0.0)),a11y:label="Zoom out") {Icon(icon=MINUS)}
                Button(variant=ButtonVariant::Secondary,class="map-button",on:click=move |_|fit(),a11y:label="Fit selected paths") {Icon(icon=FIT)}
                Button(variant=ButtonVariant::Secondary,class="map-button",on:click=move |_|s.settings_open.set(true),a11y:label="Settings") {Icon(icon=SLIDERS)}
                Button(variant=ButtonVariant::Secondary,class="map-button",on:click=move |_|s.nearest_road(),a11y:label="Nearest untraveled road") {Icon(icon=EXPLORE)}
            }
        }
    }
}

const LAYERS: usize = 8;

/// Colour, width (CSS px) and dashed, per line layer: traveled by trips on road (1, 2,
/// 3–4, 5+), GPS, inferred gap, area outline, nearest-road target.
const fn line(rgba: [u8; 4], width: f32, dashed: bool) -> LineStyle {
    LineStyle {
        rgba,
        width,
        dashed,
    }
}
const LINE_STYLE: [LineStyle; LAYERS] = [
    line([60, 220, 145, 255], 5.0, false),
    line([40, 185, 235, 255], 5.0, false),
    line([250, 205, 50, 255], 5.0, false),
    line([240, 100, 60, 255], 5.0, false),
    line([80, 155, 255, 255], 2.0, false),
    line([245, 171, 47, 255], 2.0, true),
    line([170, 110, 255, 255], 2.5, false),
    line([225, 60, 225, 255], 7.0, false),
];
/// Untraveled roads inside area.
const UNTRAVELED_STYLE: LineStyle = line([235, 64, 64, 255], 3.0, false);

/// Segments chained into polylines. Coverage sections are 20 m pieces of map edges, so
/// collinear continuations collapse back to the map's own vertices.
#[derive(Clone, Default, PartialEq)]
struct Polylines {
    lines: Vec<Polyline>,
}

impl Polylines {
    fn segment(&mut self, a: [f64; 2], b: [f64; 2]) {
        let line = match self.lines.last_mut() {
            Some(line) if line.points.last() == Some(&a) => line,
            _ => {
                self.lines.push(Polyline {
                    points: vec![a],
                    ..Polyline::default()
                });
                self.lines.last_mut().unwrap()
            }
        };
        let n = line.points.len();
        if n >= 2 {
            let (p, q) = (line.points[n - 2], line.points[n - 1]);
            let (u, v) = ([q[0] - p[0], q[1] - p[1]], [b[0] - q[0], b[1] - q[1]]);
            let cross = u[0] * v[1] - u[1] * v[0];
            let dot = u[0] * v[0] + u[1] * v[1];
            if dot > 0.0 && cross.abs() <= 1e-9 * u[0].hypot(u[1]) * v[0].hypot(v[1]) {
                line.points[n - 1] = b;
                return;
            }
        }
        line.points.push(b);
    }

    fn finish(&mut self) {
        for line in &mut self.lines {
            line.min = [f64::INFINITY; 2];
            line.max = [f64::NEG_INFINITY; 2];
            for p in &line.points {
                line.min = [line.min[0].min(p[0]), line.min[1].min(p[1])];
                line.max = [line.max[0].max(p[0]), line.max[1].max(p[1])];
            }
        }
    }
}

/// Pointer id and window position where it went down.
type Press = (u64, [f64; 2]);
/// Pointer id, area corner index, press position, moved past slop.
type Held = (u64, usize, [f64; 2], bool);

#[cfg(test)]
mod tests {
    use super::Polylines;

    #[test]
    fn chained_collinear_sections_collapse_to_vertices() {
        let mut lines = Polylines::default();
        for x in 0..10 {
            lines.segment([f64::from(x), 0.0], [f64::from(x + 1), 0.0]);
        }
        lines.segment([10.0, 0.0], [10.0, 5.0]);
        lines.segment([50.0, 50.0], [60.0, 50.0]);
        lines.finish();
        assert_eq!(lines.lines.len(), 2);
        assert_eq!(
            lines.lines[0].points,
            vec![[0.0, 0.0], [10.0, 0.0], [10.0, 5.0]]
        );
        assert_eq!(lines.lines[0].max, [10.0, 5.0]);
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Marker {
    x: f64,
    y: f64,
    radius: f64,
    /// Accuracy circle covers whole viewport.
    cover: bool,
}

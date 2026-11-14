use super::{
    basemap::Kind,
    camera::{self, Camera, project},
    gestures::Contacts,
    overlay_tiles::{OverlayTiles, Polyline},
    terrain_tiles::Style as TerrainStyle,
    terrain_view::TerrainViewProps,
    tiles::{Config, Provider, Service},
};
use crate::ui::icons::{COMPASS, EXPLORE, FIT, LOCATE, SLIDERS};
use crate::ui::{
    settings::SettingsProps,
    state::UiState,
    tools::{AreaBarProps, AreaDialogProps, AreasSheetProps, TripEditorDialogProps},
    view_options::ViewOptionsProps,
};
use kurbo::BezPath;
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use zgui::{
    canvas::{Brush, ShapeBuilder, zgui_color::Color},
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
    // Projection depends on history, not camera. Keep pan frames to affine placement
    // and clipping instead of projecting thousands of GPS/road coordinates again.
    // Layers: traveled by trips on road (1, 2, 3–4, 5+), GPS, inferred, area outline,
    // nearest-road target. Area untraveled roads render as image tiles instead.
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
    let overlay_tiles = OverlayTiles::new();
    let untraveled = RenderEffect::new(move |_| {
        let mut lines = Polylines::default();
        s.area_stats.with(|stats| {
            for section in stats.iter().flat_map(|st| &st.untraveled) {
                let a = project(section.start.lon, section.start.lat);
                let b = project(section.end.lon, section.end.lat);
                if let (Some(a), Some(b)) = (a, b) {
                    lines.segment(a, b);
                }
            }
        });
        lines.finish();
        overlay_tiles.set_lines(lines.lines);
    });
    on_cleanup_local(move || drop(untraveled));
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
    let overlay = zgui::elements::canvas()
        .class("map-overlay")
        .draw(move |cx| {
            let camera = camera.get();
            let [w, h] = [f64::from(cx.size.width.0), f64::from(cx.size.height.0)];
            let scale = camera.scale();
            let pixel = |xy: [f64; 2]| {
                (
                    (xy[0] - camera.center[0]) * scale + w / 2.0,
                    h / 2.0 - (xy[1] - camera.center[1]) * scale,
                )
            };
            let mut layers: [Cells; LAYERS] = std::array::from_fn(|_| Cells::new(w, h));
            // Visible map rectangle (projected metres) with stroke margin.
            let margin = 16.0 / scale;
            let view = [
                camera.center[0] - w / 2.0 / scale - margin,
                camera.center[1] - h / 2.0 / scale - margin,
                camera.center[0] + w / 2.0 / scale + margin,
                camera.center[1] + h / 2.0 / scale + margin,
            ];
            geometry.with(|layers_lines| {
                for (cells, polylines) in layers.iter_mut().zip(layers_lines) {
                    for line in &polylines.lines {
                        if line.max[0] < view[0]
                            || line.max[1] < view[1]
                            || line.min[0] > view[2]
                            || line.min[1] > view[3]
                        {
                            continue;
                        }
                        // Vertices closer than line width add work, not visible detail.
                        let mut last = pixel(line.points[0]);
                        let end = line.points.len() - 1;
                        for (i, &point) in line.points.iter().enumerate().skip(1) {
                            let next = pixel(point);
                            if i < end && (next.0 - last.0).hypot(next.1 - last.1) < 3.0 {
                                continue;
                            }
                            cells.segment(last, next);
                            last = next;
                        }
                    }
                }
            });
            let [one, two, few, many, tracks, inferred, outline, target] = layers;
            for (cells, color, width) in [
                (one, Color::srgb_u8(60, 220, 145, 255), 5.0),
                (two, Color::srgb_u8(40, 185, 235, 255), 5.0),
                (few, Color::srgb_u8(250, 205, 50, 255), 5.0),
                (many, Color::srgb_u8(240, 100, 60, 255), 5.0),
                (tracks, Color::srgb_u8(80, 155, 255, 255), 2.0),
                // Solid: dashing long outline edges costs every frame on Mali.
                (outline, Color::srgb_u8(170, 110, 255, 255), 2.5),
                (target, Color::srgb_u8(225, 60, 225, 255), 7.0),
            ] {
                for path in cells.paths() {
                    cx.scene.push(
                        ShapeBuilder::new(path)
                            .stroke(Brush::Solid(color), width)
                            .build(),
                    );
                }
            }
            for path in inferred.paths() {
                cx.scene.push(
                    ShapeBuilder::new(path)
                        .stroke_styled(
                            Brush::Solid(Color::srgb_u8(245, 171, 47, 255)),
                            kurbo::Stroke::new(2.0).with_dashes(0.0, [5.0, 4.0]),
                        )
                        .build(),
                );
            }
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
                    for item in move ||if three_d.get(){Vec::new()}else{s.basemap.visible(camera.get(),size.get(),s.dark.get()||s.satellite.get(),if s.satellite.get(){Kind::Roads}else{Kind::Map},f64::from(host.scale()),!offline.get())},key=|item|item.clone() {
                        // 512 CSS px box: nominal display size of rendered tile; place() scales 256 px boxes.
                        box(class="map-vector-tile",style:transform=move ||{let[x,y,scale]=camera.get().place(item.0,size.get());Some(format!("translate({x}px, {y}px) scale({})",scale/2.0))}) {
                            {zgui::elements::image().class("map-vector-image").src(Some(item.1.clone())).alt(Some("Map tile".to_string())).into_view()}
                        }
                    }
                }
                box(class="map-vector-tiles") {
                    for item in move ||if three_d.get(){Vec::new()}else{overlay_tiles.visible(camera.get(),size.get(),f64::from(host.scale()))},key=|item|item.clone() {
                        box(class="map-vector-tile",style:transform=move ||{let[x,y,scale]=camera.get().place(item.0,size.get());Some(format!("translate({x}px, {y}px) scale({})",scale/2.0))}) {
                            {zgui::elements::image().class("map-vector-image").src(Some(item.1.clone())).alt(Some("Untraveled roads in area".to_string())).into_view()}
                        }
                    }
                }
                {overlay}
                // Street names above trip and area lines.
                box(class="map-vector-tiles") {
                    for item in move ||if three_d.get(){Vec::new()}else{s.basemap.visible(camera.get(),size.get(),s.dark.get()||s.satellite.get(),Kind::Labels,f64::from(host.scale()),!offline.get())},key=|item|item.clone() {
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

/// Overlay lines clipped to viewport and split into small screen cells.
///
/// Zgui rasterizes canvas shapes on Mali-G715 at cost of shape area x segments, so one
/// path spanning screen (or far beyond it when zoomed in) stalls GPU. Small cells keep
/// each shape's box and segment count small.
struct Cells {
    w: f64,
    h: f64,
    paths: std::collections::HashMap<(i32, i32), BezPath>,
}

const CELL: f64 = 32.0;

impl Cells {
    fn new(w: f64, h: f64) -> Self {
        Self {
            w,
            h,
            paths: Default::default(),
        }
    }

    fn segment(&mut self, a: (f64, f64), b: (f64, f64)) {
        // Liang-Barsky clip to viewport plus stroke margin.
        let m = 8.0;
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (mut t0, mut t1) = (0.0_f64, 1.0_f64);
        for (p, q) in [
            (-dx, a.0 + m),
            (dx, self.w + m - a.0),
            (-dy, a.1 + m),
            (dy, self.h + m - a.1),
        ] {
            if p == 0.0 {
                if q < 0.0 {
                    return;
                }
            } else if p < 0.0 {
                t0 = t0.max(q / p);
            } else {
                t1 = t1.min(q / p);
            }
        }
        if t0 >= t1 {
            return;
        }
        let (a, b) = (
            (a.0 + t0 * dx, a.1 + t0 * dy),
            (a.0 + t1 * dx, a.1 + t1 * dy),
        );
        // Long segment: split so each piece stays within roughly one cell.
        let steps = ((b.0 - a.0).hypot(b.1 - a.1) / CELL).ceil().max(1.0) as usize;
        for i in 0..steps {
            let at = |t: f64| (a.0 + t * (b.0 - a.0), a.1 + t * (b.1 - a.1));
            let (p, q) = (
                at(i as f64 / steps as f64),
                at((i + 1) as f64 / steps as f64),
            );
            let cell = (
                ((p.0 + q.0) / 2.0 / CELL).floor() as i32,
                ((p.1 + q.1) / 2.0 / CELL).floor() as i32,
            );
            let path = self.paths.entry(cell).or_default();
            // Continue path when segments chain, so round joins render.
            if path.elements().last().and_then(|e| e.end_point()) != Some(p.into()) {
                path.move_to(p);
            }
            path.line_to(q);
        }
    }

    fn paths(self) -> impl Iterator<Item = BezPath> {
        self.paths.into_values()
    }
}

#[cfg(test)]
mod tests {
    use super::{Cells, Polylines};

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

    #[test]
    fn overlay_cells_clip_offscreen_and_split_long_lines() {
        let mut cells = Cells::new(400.0, 300.0);
        cells.segment((-5000.0, -5000.0), (-4000.0, -4000.0));
        assert_eq!(cells.paths().count(), 0);
        let mut cells = Cells::new(400.0, 300.0);
        // Zoomed far in: segment spans 100k px, crosses viewport horizontally.
        cells.segment((-50_000.0, 150.0), (50_000.0, 150.0));
        let paths: Vec<_> = cells.paths().collect();
        assert!(paths.len() >= 3);
        for path in paths {
            let b = kurbo::Shape::bounding_box(&path);
            assert!(b.x0 >= -8.0 && b.x1 <= 408.0 && b.width() <= 2.0 * super::CELL + 16.0);
        }
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

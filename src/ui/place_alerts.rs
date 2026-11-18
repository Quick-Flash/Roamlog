//! User-selected arrival/departure subscriptions, stored only on this phone.
use super::state::UiState;
use crate::{
    geo::Point,
    sharing::{
        android_app,
        peers::{Friend, Group},
        places::{self, Place, Who},
    },
};
use std::collections::BTreeSet;
use zgui::prelude::*;
use zgui_ui::prelude::*;

fn boundary_preview(place: &Place, [width, height]: [f64; 2]) -> impl IntoView {
    use crate::mapping::{
        basemap::Kind,
        camera::{Camera, project},
    };
    let s = UiState::current();
    let origin = project(place.center.lon, place.center.lat).unwrap_or_default();
    let raw: Vec<_> = place
        .outline()
        .into_iter()
        .filter_map(|p| project(p.lon, p.lat))
        .map(|mut p| {
            let world = 2. * std::f64::consts::PI * 6_378_137.;
            p[0] = origin[0] + (p[0] - origin[0] + world / 2.).rem_euclid(world) - world / 2.;
            p
        })
        .collect();
    let (lo, hi) = raw.iter().fold(
        ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]),
        |(mut lo, mut hi), p| {
            for axis in 0..2 {
                lo[axis] = lo[axis].min(p[axis]);
                hi[axis] = hi[axis].max(p[axis]);
            }
            (lo, hi)
        },
    );
    let pad = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.) * 0.18;
    let camera = Camera::from_ranges(
        plot::Ranges::new((lo[0] - pad, hi[0] + pad), (lo[1] - pad, hi[1] + pad)),
        [width, height],
    );
    let points: Vec<_> = camera
        .map(|camera| {
            raw.into_iter()
                .map(|[x, y]| {
                    [
                        width / 2. + (x - camera.center[0]) * camera.scale(),
                        height / 2. - (y - camera.center[1]) * camera.scale(),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    let lines: Vec<_> = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .enumerate()
        .map(|(id, (a, b))| {
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            (
                id,
                format!("{}px", dx.hypot(dy)),
                format!(
                    "translate({}px, {}px) rotate({}rad)",
                    a[0],
                    a[1],
                    dy.atan2(dx)
                ),
            )
        })
        .collect();
    view! {
        box(class="alert-shape",style:width=format!("{width}px"),style:height=format!("{height}px")) {
            for tile in move ||camera.map_or_else(Vec::new,|camera|
                s.basemap.visible(camera,[width,height],s.dark.get(),Kind::Map,1.,!s.map_offline.get())),key=|tile|tile.clone() {
                {{
                    let [x,y,scale]=camera.expect("tile requires camera").place(tile.0,[width,height]);
                    zgui::elements::image().class("map-vector-tile map-vector-image")
                        .style_property("transform",format!("translate({x}px, {y}px) scale({})",scale/2.))
                        .src(Some(tile.1.clone())).into_view()
                }}
            }
            for line in move ||lines.clone(),key=|line|line.0 {
                box(class="alert-shape-edge",style:width=line.1.clone(),style:transform=line.2.clone()) {}
            }
        }
    }
}

/// Friends (not blocked) and current groups, for choosing and filtering alert rules.
type People = (Vec<Friend>, Vec<Group>);

fn load(directory: std::path::PathBuf) -> Result<(Vec<Place>, People), String> {
    let manager = android_app::manager(&directory).map_err(|e| e.to_string())?;
    let friends = manager.friends();
    let people = friends
        .list()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|f| !f.blocked)
        .collect();
    let groups = friends
        .groups()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|g| g.leaving_since_ms.is_none())
        .collect();
    Ok((
        places::list(friends).map_err(|e| e.to_string())?,
        (people, groups),
    ))
}

/// Who a rule watches as one key: friend id, or "g:<group id>".
fn rule_key(place: &Place) -> String {
    place
        .group
        .as_ref()
        .map_or_else(|| place.peer.to_string(), |id| format!("g:{id}"))
}

/// Whether rule shows under `filter`: a group key matches that group's rules; a friend key
/// matches their own rules and their groups' rules unless they opted out.
fn shown(place: &Place, filter: Option<&str>, friends: &[Friend]) -> bool {
    let Some(key) = filter else {
        return true;
    };
    if key.starts_with("g:") {
        return !place.no_rule && rule_key(place) == key;
    }
    friends
        .iter()
        .find(|f| f.peer_id.to_string() == key)
        .is_some_and(|f| place.watches(f))
}

/// Filter choices: groups first, then friends.
fn filter_options((friends, groups): &People) -> Vec<(String, String)> {
    groups
        .iter()
        .map(|g| (format!("g:{}", g.id), format!("{} (group)", g.name)))
        .chain(
            friends
                .iter()
                .map(|f| (f.peer_id.to_string(), f.display_name.clone())),
        )
        .collect()
}

fn marks(
    list: Vec<Place>,
    filter: Option<&str>,
    friends: &[Friend],
) -> Vec<(u32, String, Vec<Point>)> {
    let mut seen = BTreeSet::new();
    list.into_iter()
        .filter(|p| shown(p, filter, friends))
        .filter(|p| seen.insert(p.area()))
        .map(|p| (p.area(), p.name.clone(), p.outline()))
        .collect()
}

/// Draw every alert area (all, or those watching `filter`: friend id or "g:<group id>") on
/// the map with tappable names, fit them in view and switch to the map.
pub fn show_on_map(s: UiState, filter: Option<String>) {
    let directory = s.directory.get_value();
    zgui::reactive::spawn_detached(async move {
        match blocking(move || load(directory)).await {
            Ok((list, people)) => {
                let marks = marks(list, filter.as_deref(), &people.0);
                s.area_filters.set(filter_options(&people));
                s.place_peer.set(filter);
                if marks.is_empty() {
                    s.message.set("No alert areas to show yet".into());
                    return;
                }
                s.fit(marks.iter().flat_map(|mark| mark.2.iter().copied()));
                s.place_outline.set(Vec::new());
                s.marks_friend.set_value(None);
                s.area_marks.set(marks);
                s.map_friend_selected.set(None);
                s.message.set(String::new());
                s.places_open.set(false);
                s.friends_open.set(false);
                s.trips_open.set(false);
                s.settings_open.set(false);
            }
            Err(error) => s.message.set(error),
        }
    });
}

/// Selecting a friend on the map outlines the areas that alert about them, without moving
/// the map; deselecting hides them again.
pub fn follow_selected_friend(s: UiState) {
    let effect = zgui::reactive::RenderEffect::new(move |_| {
        let Some(peer) = s.map_friend_selected.get() else {
            if s.marks_friend.get_value().is_some() {
                s.marks_friend.set_value(None);
                s.area_marks.set(Vec::new());
            }
            return;
        };
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            let Ok((list, people)) = blocking(move || load(directory)).await else {
                return;
            };
            if s.map_friend_selected
                .with_untracked(|p| p.as_ref() != Some(&peer))
            {
                return;
            }
            s.marks_friend.set_value(Some(peer.clone()));
            s.area_marks.set(marks(list, Some(&peer), &people.0));
        });
    });
    on_cleanup_local(move || drop(effect));
}

fn trigger_key(rule: places::Trigger) -> &'static str {
    match rule {
        places::Trigger::Entering => "entering",
        places::Trigger::Exiting => "exiting",
        places::Trigger::Both => "both",
    }
}

/// Radius slider on a log scale, so 10 m and 10 km are both easy to reach.
fn radius_slider_view(radius: RwSignal<f64, LocalStorage>) -> impl IntoView {
    let span = (places::MAX_RADIUS / places::MIN_RADIUS).ln();
    let to_position = move |r: f64| (r / places::MIN_RADIUS).ln() / span * 1000.;
    let to_radius = move |p: f64| {
        let r = places::MIN_RADIUS * (p / 1000. * span).exp();
        let step = if r < 100. {
            5.
        } else if r < 1000. {
            10.
        } else {
            50.
        };
        ((r / step).round() * step).clamp(places::MIN_RADIUS, places::MAX_RADIUS)
    };
    view! {
        Slider(value=Binding::controlled(Signal::derive_local(move ||to_position(radius.get())),move |p|radius.set(to_radius(p))),
            min=0.0,max=1000.0,step=1.0,label="Radius in meters")
    }
}

#[component]
pub fn PlaceAlerts(open: RwSignal<bool, LocalStorage>) -> impl IntoView {
    let s = UiState::current();
    let saved = RwSignal::new_local(Vec::<Place>::new());
    let people = RwSignal::new_local(Vec::<(String, String)>::new());
    let records = RwSignal::new_local((Vec::<Friend>::new(), Vec::<Group>::new()));
    // Group members the chosen group rule skips.
    let opt_out = RwSignal::new_local(Vec::<String>::new());
    let name = RwSignal::new_local(String::new());
    let peer = RwSignal::new_local("none".to_owned());
    let trigger = RwSignal::new_local("both".to_owned());
    let status = RwSignal::new_local(String::new());
    let area = RwSignal::new_local(None::<u32>);
    let creating = RwSignal::new_local(false);
    let rule_id = RwSignal::new_local(None::<u32>);
    let existing_rule = RwSignal::new_local(false);
    let editing_rule = RwSignal::new_local(false);
    let confirm_delete = RwSignal::new_local(false);
    let editing = RwSignal::new_local(false);
    let radius_slider = RwSignal::new_local(250.0);
    let edit_center = RwSignal::new_local(None::<crate::geo::Point>);
    let choosing_center = RwSignal::new_local(false);
    let reload = move || {
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            match blocking(move || load(directory)).await {
                Ok((list, loaded)) => {
                    let friends: Vec<_> = loaded
                        .0
                        .iter()
                        .map(|f| (f.peer_id.to_string(), f.display_name.clone()))
                        .collect();
                    if peer.get_untracked() != "none"
                        && !friends.iter().any(|(id, _)| *id == peer.get_untracked())
                        && let Some((id, _)) = friends.first()
                    {
                        peer.set(id.clone());
                    }
                    saved.set(list);
                    people.set(friends);
                    records.set(loaded);
                }
                Err(e) => status.set(e),
            }
        });
    };
    let save_area = move || {
        let (Some(id), Some(center)) = (area.get_untracked(), edit_center.get_untracked()) else {
            return;
        };
        let directory = s.directory.get_value();
        let area_name = name.get_untracked();
        let radius = radius_slider.get_untracked();
        let polygon = s.alert_polygon.get_untracked();
        let center = polygon.first().copied().unwrap_or(center);
        spawn_local(async move {
            let result = blocking(move || {
                let manager = android_app::manager(&directory)?;
                places::edit_area(
                    manager.friends(),
                    id,
                    &area_name,
                    center,
                    radius,
                    (!polygon.is_empty()).then_some(polygon),
                )
            })
            .await;
            match result {
                Ok(()) => {
                    editing.set(false);
                    name.set(String::new());
                    status.set("Alert area saved".into());
                    s.place_outline.set(Vec::new());
                    reload();
                }
                Err(error) => status.set(error.to_string()),
            }
        });
    };
    // Alert sheet opens without a result left over from last time.
    let fresh_rule = zgui::reactive::RenderEffect::new(move |_| {
        if editing_rule.get() {
            status.set(String::new());
        }
    });
    on_cleanup_local(move || drop(fresh_rule));
    let effect = zgui::reactive::RenderEffect::new(move |_| {
        if open.get() {
            status.set(String::new());
            if choosing_center.get_untracked() {
                choosing_center.set(false);
                if creating.get_untracked() && s.alert_polygon.with_untracked(Vec::is_empty) {
                    creating.set(false);
                }
                if let Some((center, radius)) = s.editing_circle.get_untracked() {
                    edit_center.set(Some(center));
                    radius_slider.set(radius);
                }
                s.editing_circle.set(None);
                s.place_outline.set(Vec::new());
            } else {
                creating.set(false);
                editing.set(false);
                editing_rule.set(false);
                confirm_delete.set(false);
                area.set(None);
            }
            if let Some(id) = s.open_area.get_untracked() {
                s.open_area.set(None);
                area.set(Some(id));
            }
            if let Some(id) = s.place_peer.get_untracked() {
                peer.set(id);
            }
            reload();
        }
    });
    on_cleanup_local(move || drop(effect));
    let add = move || {
        let selected = area.get_untracked();
        let radius = radius_slider.get_untracked();
        let trigger_value = match trigger.get_untracked().as_str() {
            "entering" => places::Trigger::Entering,
            "exiting" => places::Trigger::Exiting,
            _ => places::Trigger::Both,
        };
        let directory = s.directory.get_value();
        let name_value = name.get_untracked();
        let peer_value = peer.get_untracked();
        let skip = opt_out.get_untracked();
        let [lon, lat] = crate::mapping::camera::geographic(s.camera.get_untracked().center);
        let circle = s.editing_circle.get_untracked();
        let polygon = s.alert_polygon.get_untracked();
        let center = polygon
            .first()
            .copied()
            .or_else(|| circle.map(|(center, _)| center))
            .unwrap_or(crate::geo::Point { lat, lon });
        let radius = circle.map_or(radius, |(_, radius)| radius);
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || -> crate::sharing::Result<u32> {
                let manager = android_app::manager(&directory)?;
                places::set_rule(
                    manager.friends(),
                    selected,
                    &name_value,
                    center,
                    radius,
                    if selected.is_none() || peer_value == "none" {
                        None
                    } else if let Some(group) = peer_value.strip_prefix("g:") {
                        Some(Who::Group(
                            group.into(),
                            skip.iter().filter_map(|p| p.parse().ok()).collect(),
                        ))
                    } else {
                        Some(Who::Friend(peer_value.parse()?))
                    },
                    trigger_value,
                    if selected.is_none() {
                        polygon
                    } else {
                        Vec::new()
                    },
                )
            })
            .await;
            match result {
                Ok(id) => {
                    if selected.is_none() {
                        choosing_center.set(true);
                        s.editing_circle.set(None);
                        s.picking_place_center.set(false);
                        s.place_outline.set(Vec::new());
                        area.set(Some(id));
                        open.set(true);
                        if let Some((id, _)) =
                            records.with_untracked(filter_options).into_iter().next()
                        {
                            peer.set(id);
                            opt_out.set(Vec::new());
                            existing_rule.set(false);
                            editing_rule.set(true);
                        }
                    } else {
                        editing_rule.set(false);
                    }
                    status.set(if selected.is_some() {
                        "Alert saved".into()
                    } else {
                        "Alert area saved".into()
                    });
                    name.set(String::new());
                    creating.set(false);
                    reload();
                }
                Err(e) => status.set(e.to_string()),
            }
        });
    };
    let selected = Memo::new(move |_| {
        let id = area.get()?;
        saved.with(|list| list.iter().find(|place| place.area() == id).cloned())
    });
    let caption = |rule| match rule {
        places::Trigger::Entering => "Arrives",
        places::Trigger::Exiting => "Leaves",
        places::Trigger::Both => "Arrives + leaves",
    };
    // Rule's subject: friend name, or group name with opt-out count.
    let who_name = move |place: &Place| {
        records.with(|(friends, groups)| match &place.group {
            Some(id) => groups.iter().find(|g| g.id == *id).map_or_else(
                || "Removed group".into(),
                |g| match place.opt_out.len() {
                    0 => format!("{} (group)", g.name),
                    n => format!("{} (group, {n} skipped)", g.name),
                },
            ),
            None => friends
                .iter()
                .find(|f| f.peer_id == place.peer)
                .map_or_else(|| "Removed friend".into(), |f| f.display_name.clone()),
        })
    };
    // Rule already exists for chosen friend or group in this area: saving updates it.
    let updating = move || {
        existing_rule.get()
            || saved.with(|list| {
                list.iter().any(|p| {
                    Some(p.area()) == area.get() && !p.no_rule && peer.with(|k| rule_key(p) == *k)
                })
            })
    };
    let group_members = move |key: &str| {
        let Some(id) = key.strip_prefix("g:") else {
            return Vec::new();
        };
        records.with(|(friends, _)| {
            friends
                .iter()
                .filter(|f| f.groups.iter().any(|g| g == id))
                .map(|f| (f.peer_id.to_string(), f.display_name.clone()))
                .collect::<Vec<_>>()
        })
    };
    let view_boundary = move |place: Place| {
        let boundary = place.outline();
        s.fit(boundary.iter().copied());
        s.place_outline.set(boundary);
        open.set(false);
        s.friends_open.set(false);
    };
    let sync_radius = zgui::reactive::RenderEffect::new(move |_| {
        let radius = radius_slider.get();
        if creating.get_untracked() && !open.get_untracked() {
            s.editing_circle.update(|circle| {
                if let Some((_, current)) = circle {
                    *current = radius;
                }
            });
        }
    });
    on_cleanup_local(move || drop(sync_radius));
    let sync_drag = zgui::reactive::RenderEffect::new(move |_| {
        if let Some((_, radius)) = s.editing_circle.get()
            && creating.get_untracked()
            && radius_slider.get_untracked() != radius
        {
            radius_slider.set(radius);
        }
    });
    on_cleanup_local(move || drop(sync_drag));
    let use_circle = move || {
        if s.drawing_alert.get_untracked() {
            s.drawing_alert.set(false);
            s.cancel_area();
        }
        choosing_center.set(false);
        s.alert_draw_back_to_circle.set(false);
        let [lon, lat] = crate::mapping::camera::geographic(s.camera.get_untracked().center);
        let center = edit_center
            .get_untracked()
            .unwrap_or(crate::geo::Point { lat, lon });
        let radius = radius_slider.get_untracked();
        s.alert_polygon.set(Vec::new());
        s.editing_circle.set(Some((center, radius)));
        s.fit(places::circle_outline(center, radius));
        s.camera
            .update(|camera| camera.zoom = (camera.zoom - 0.65).max(0.));
        s.picking_place_center.set(true);
        creating.set(true);
        open.set(false);
        s.friends_open.set(false);
        s.follow.set(false);
    };
    s.alert_use_circle
        .set_value(Some(zgui::reactive::UnsyncCallback::new(move |()| {
            use_circle()
        })));
    on_cleanup_local(move || s.alert_use_circle.set_value(None));
    let new_area = move || {
        name.set(String::new());
        status.set(String::new());
        area.set(None);
        edit_center.set(None);
        radius_slider.set(200.);
        use_circle();
    };
    let draw = move || {
        if let Some((center, radius)) = s.editing_circle.get_untracked() {
            edit_center.set(Some(center));
            radius_slider.set(radius);
        }
        s.alert_draw_back_to_circle.set(creating.get_untracked());
        choosing_center.set(true);
        s.picking_place_center.set(false);
        open.set(false);
        s.friends_open.set(false);
        s.editing_circle.set(None);
        s.place_outline.set(Vec::new());
        let points = s.alert_polygon.get_untracked();
        if points.is_empty() {
            s.start_area();
        } else {
            s.area_points.set(points);
            s.edit_area();
        }
        s.drawing_alert.set(true);
    };
    let move_boundary = move |place: Place| {
        name.set(place.name);
        edit_center.set(Some(place.center));
        radius_slider.set(place.radius_m);
        s.alert_polygon.set(place.polygon);
        editing.set(true);
        if s.alert_polygon.with_untracked(Vec::is_empty) {
            s.fit(places::circle_outline(place.center, place.radius_m));
            s.editing_circle.set(Some((place.center, place.radius_m)));
            choosing_center.set(true);
            open.set(false);
            s.friends_open.set(false);
            s.follow.set(false);
        } else {
            draw();
        }
    };
    let remove_alert = move || {
        let Some(id) = rule_id.get_untracked() else {
            return;
        };
        let directory = s.directory.get_value();
        spawn_local(async move {
            let result = blocking(move || {
                let manager = android_app::manager(&directory)?;
                places::remove(manager.friends(), id)
            })
            .await;
            match result {
                Ok(()) => {
                    editing_rule.set(false);
                    reload();
                }
                Err(error) => status.set(error.to_string()),
            }
        });
    };
    let back = move || {
        if editing.get_untracked() {
            editing.set(false);
        } else if editing_rule.get_untracked() {
            editing_rule.set(false);
        } else if creating.get_untracked() {
            creating.set(false);
            s.editing_circle.set(None);
            s.picking_place_center.set(false);
            s.place_outline.set(Vec::new());
            open.set(true);
        } else if area.get_untracked().is_some() {
            area.set(None);
            confirm_delete.set(false);
        } else {
            open.set(false);
        }
    };
    s.alert_areas_back
        .set_value(Some(zgui::reactive::UnsyncCallback::new(move |()| back())));
    on_cleanup_local(move || s.alert_areas_back.set_value(None));
    view! {
        if move ||open.get() {
            column(class="tab-page alert-page") {
                row(class="page-heading") {
                    Button(variant=ButtonVariant::Ghost,class="alert-back page-back",a11y:label="Back",on:click=move |_|back()) {"‹"}
                    text(class="page-title grow") {{move ||selected.with(|p|p.as_ref().map(|p|p.name.clone()))
                        .unwrap_or_else(||if creating.get() {"New alert area".into()}else{"Alert areas".into()})}}
                    if move ||area.with(Option::is_none)&&!creating.get() {
                        Button(on:click=move |_|new_area()) {"+ New"}
                    }
                }
                scroll(class="page-scroll") {
                    column(class="list-body") {
                        if move ||editing.get() {
                            Input(value=name,label="Area name",placeholder="Name")
                            if move ||saved.with(|list|list.iter().any(|p|Some(p.area())==area.get()&&p.polygon.is_empty())) {
                                text {{move ||format!("{:.0} m radius",radius_slider.get())}}
                                {radius_slider_view(radius_slider)}
                                Button(variant=ButtonVariant::Outline,on:click=move |_|{
                                    let Some(center) = edit_center.get_untracked() else {return;};
                                    let radius = radius_slider.get_untracked();
                                    let outline = places::circle_outline(center, radius);
                                    s.fit(outline.iter().copied());
                                    s.place_outline.set(outline);
                                    s.editing_circle.set(Some((center, radius)));
                                    choosing_center.set(true);
                                    open.set(false);
                                    s.friends_open.set(false);
                                    s.follow.set(false);
                                }) {"Move or resize on map"}
                            }
                            if move ||!s.alert_polygon.with(Vec::is_empty) {
                                Button(variant=ButtonVariant::Outline,on:click=move |_|draw()) {"Edit area"}
                            }
                            row(class="actions") {
                                Button(variant=ButtonVariant::Outline,on:click=move |_|{editing.set(false);name.set(String::new());}) {"Cancel"}
                                Button(on:click=move |_|save_area()) {"Save area"}
                            }
                        } else {
                            if move ||selected.with(Option::is_some) {
                                column(class="alert-card") {
                                        row(class="alert-preview") {
                                        for place in move ||selected.get().into_iter().collect::<Vec<_>>(),key=|p|(p.id,p.center.lat.to_bits(),p.center.lon.to_bits(),p.radius_m.to_bits(),p.polygon.iter().map(|p|(p.lat.to_bits(),p.lon.to_bits())).collect::<Vec<_>>()) {
                                            {boundary_preview(&place,[320.,150.])}
                                        }
                                    }
                                    text(class="muted") {{move ||selected.with(|place|place.as_ref().map(|p|if p.polygon.is_empty(){format!("Circle · {:.0} m radius",p.radius_m)}else{"Drawn boundary".into()}).unwrap_or_default())}}
                                    row(class="actions") {
                                        Button(variant=ButtonVariant::Outline,on:click=move |_|{
                                            if let Some(place)=selected.get_untracked(){move_boundary(place);}
                                        }) {"Edit area"}
                                        Button(variant=ButtonVariant::Outline,on:click=move |_|{
                                            if let Some(place)=selected.get_untracked(){
                                                name.set(place.name);
                                                s.alert_polygon.set(place.polygon);
                                                edit_center.set(Some(place.center));
                                                radius_slider.set(place.radius_m);
                                                editing.set(true);
                                            }
                                        }) {"Edit name"}
                                    }
                                    Button(variant=ButtonVariant::Ghost,on:click=move |_|{
                                        if let Some(place)=selected.get_untracked(){view_boundary(place);}
                                    }) {"View on map"}
                                }
                                row(class="page-heading") {
                                    text(class="list-title grow") {"Alerts"}
                                    Button(variant=ButtonVariant::Outline,disabled=Signal::derive_local(move ||records.with(|r|r.0.is_empty()&&r.1.is_empty())),on:click=move |_|{
                                        // First friend or group without an alert here, so this is a new alert.
                                        let keys:Vec<String>=records.with_untracked(filter_options).into_iter().map(|o|o.0).collect();
                                        let free=keys.iter().find(|k|!saved.with_untracked(|list|list.iter().any(|p|Some(p.area())==area.get_untracked()&&!p.no_rule&&rule_key(p)==**k)));
                                        if let Some(key)=free.or(keys.first()){peer.set(key.clone());}
                                        existing_rule.set(false);
                                        opt_out.set(Vec::new());
                                        trigger.set("both".into());
                                        editing_rule.set(true);
                                    }) {"+ Add alert"}
                                }
                                if move || !saved.with(|list|list.iter().any(|p|Some(p.area())==area.get()&&!p.no_rule)) {
                                    text(class="muted") {"No alerts yet."}
                                }
                            } else {
                                if move ||!creating.get() {
                                    text(class="muted") {"Get notified when friends arrive at or leave an area."}
                                    if move ||!saved.with(Vec::is_empty) {
                                        Button(variant=ButtonVariant::Outline,class="wide show-areas",on:click=move |_|show_on_map(s,s.place_peer.get_untracked())) {
                                            {move ||if s.place_peer.with(Option::is_some) {"Show these areas on map"} else {"Show all areas on map"}}
                                        }
                                    }
                                    if move ||saved.with(Vec::is_empty) {
                                        column(class="alert-card") {
                                            text(class="list-title") {"No alert areas yet"}
                                            text(class="muted") {"Tap + New, place a circle or draw a shape on the map, then choose which friends to watch."}
                                        }
                                    }
                                    if move ||records.with(|r|!r.0.is_empty()||!r.1.is_empty())&&!saved.with(Vec::is_empty) {
                                    scroll(class="alert-filter") {
                                        row(class="friend-strip-row") {
                                            Button(variant=ButtonVariant::Outline,class:seg-on=move ||s.place_peer.with(Option::is_none),on:click=move |_|s.place_peer.set(None)) {"All"}
                                            for person in move ||records.with(filter_options),key=|p|p.clone() {
                                                {{
                                                    let id=person.0.clone();
                                                    let selected_id=id.clone();
                                                    view! {
                                                        Button(variant=ButtonVariant::Outline,class:seg-on=move ||s.place_peer.with(|filter|filter.as_ref()==Some(&selected_id)),on:click=move |_|s.place_peer.set(Some(id.clone()))) {{person.1.clone()}}
                                                    }
                                                }}
                                            }
                                        }
                                    }
                                    }
                                }
                                if move ||creating.get() {
                                    Input(value=name,label="Area name",placeholder="Home, school, campsite…")
                                    text(class="muted") {{move ||format!("Drawn shape · {} corners",s.alert_polygon.with(Vec::len))}}
                                    row(class="friends-timer-presets") {
                                        Button(variant=ButtonVariant::Outline,on:click=move |_|use_circle()) {"Use circle"}
                                        Button(variant=ButtonVariant::Outline,on:click=move |_|draw()) {"Edit area"}
                                        Button(disabled=Signal::derive_local(move ||name.with(|n|n.trim().is_empty())||s.alert_polygon.with(Vec::is_empty)),on:click=move |_|add()) {"Next: add alerts"}
                                    }
                                }
                            }
                        }
                        if move ||!status.with(String::is_empty) {
                            text(class="muted small",a11y:role=Role::Status) {{move ||status.get()}}
                        }
                        Dialog(open=editing_rule) {
                            DialogContent(class="sheet") {
                                DialogHeader {
                                    DialogTitle {{move ||if updating() {"Update alert"} else {"New alert"}}}
                                    DialogDescription {{move ||selected.with(|p|p.as_ref().map(|p|p.name.clone()).unwrap_or_default())}}
                                }
                                text(class="section-title") {"Who"}
                                scroll(class="sheet-scroll alert-who") {
                                    column(class="friends-nav-group") {
                                        for person in move ||records.with(filter_options),key=|p|p.clone() {
                                            {{
                                                let id=person.0.clone();
                                                let selected_id=id.clone();
                                                let alert_id=id.clone();
                                                view! {
                                                    Button(variant=ButtonVariant::Ghost,class="friends-nav",class:seg-on=move ||peer.with(|p|*p==selected_id),
                                                        disabled=Signal::derive_local(move ||existing_rule.get()),a11y:label=person.1.clone(),on:click=move |_|{
                                                            // Picking someone with an alert here edits that alert.
                                                            let current=saved.with_untracked(|list|list.iter().find(|p|Some(p.area())==area.get_untracked()&&!p.no_rule&&rule_key(p)==id).cloned());
                                                            opt_out.set(current.as_ref().map(|p|p.opt_out.iter().map(ToString::to_string).collect()).unwrap_or_default());
                                                            if let Some(rule)=current {trigger.set(trigger_key(rule.trigger).into());}
                                                            peer.set(id.clone());
                                                        }) {
                                                        text(class="grow") {{person.1.clone()}}
                                                        if move ||saved.with(|list|list.iter().any(|p|Some(p.area())==area.get()&&!p.no_rule&&rule_key(p)==alert_id)) {
                                                            text(class="friend-badge") {"Has alert"}
                                                        }
                                                    }
                                                }
                                            }}
                                        }
                                        if move ||!group_members(&peer.get()).is_empty() {
                                            text(class="section-title") {"Alert for these members"}
                                            for member in move ||group_members(&peer.get()),key=|m|m.clone() {
                                                {{
                                                    let id=member.0.clone();
                                                    let on_id=id.clone();
                                                    let off_id=id.clone();
                                                    view! {
                                                        Button(variant=ButtonVariant::Ghost,class="friends-nav",class:seg-on=move ||opt_out.with(|o|!o.contains(&on_id)),a11y:label=member.1.clone(),
                                                            on:click=move |_|opt_out.update(|o|if let Some(i)=o.iter().position(|p|*p==id){o.remove(i);}else{o.push(id.clone());})) {
                                                            text(class="grow") {{member.1.clone()}}
                                                            text(class="muted") {{move ||if opt_out.with(|o|o.contains(&off_id)) {"Skipped"} else {"✓"}}}
                                                        }
                                                    }
                                                }}
                                            }
                                        }
                                    }
                                }
                                text(class="section-title") {"Alert me when they"}
                                row(class="segmented") {
                                    for option in ||vec![("entering","Arrive"),("exiting","Leave"),("both","Both")],key=|option|option.0 {
                                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||trigger.with(|value|value==option.0),on:click=move |_|trigger.set(option.0.into())) {{option.1}}
                                    }
                                }
                                text(class="muted") {{move ||if peer.with(|p|p.starts_with("g:")) {"Covers members who join later too. Works while each member shares their location with you."} else {"Alerts work while this friend shares their location with you."}}}
                                if move ||!status.with(String::is_empty) {text(class="muted") {{move ||status.get()}}}
                                DialogFooter {
                                    if move ||existing_rule.get() {
                                        Button(variant=ButtonVariant::Outline,class="danger-text",on:click=move |_|remove_alert()) {"Remove"}
                                    }
                                    Button(disabled=Signal::derive_local(move ||peer.with(|p|p=="none")),on:click=move |_|add()) {{move ||if updating() {"Update alert"}else{"Add alert"}}}
                                }
                            }
                        }
                        if move ||!editing.get() {
                        for entry in move || {
                            let filter = s.place_peer.get();
                            let selected = area.get();
                            let mut seen = BTreeSet::new();
                            saved.get().into_iter().filter(|p| {
                                if let Some(id) = selected { return p.area() == id && !p.no_rule; }
                                records.with(|r| shown(p, filter.as_deref(), &r.0)) && seen.insert(p.area())
                            }).map(|p|(selected.is_some(),p)).collect::<Vec<_>>()
                        },key=|entry|{
                            let (details,p)=entry;
                            (*details,p.id,p.peer,p.trigger as u8,p.name.clone(),p.center.lat.to_bits(),p.center.lon.to_bits(),p.radius_m.to_bits(),p.polygon.iter().map(|p|(p.lat.to_bits(),p.lon.to_bits())).collect::<Vec<_>>())
                        } {
                            {{
                                let (details,place)=entry;
                                let area_id = place.area();
                                let rule_id_saved = place.id;
                                let rule_peer = rule_key(&place);
                                let rule_skip: Vec<String> = place.opt_out.iter().map(ToString::to_string).collect();
                                let rule_who = who_name(&place);
                                let rule_trigger = place.trigger;
                                if details {
                                    AnyView::new(view! {
                                        column { row(class="list-row") {
                                            column(class="grow") {
                                                text(class="list-title") {{rule_who.clone()}}
                                                text(class="muted") {{caption(rule_trigger)}}
                                            }
                                            Button(variant=ButtonVariant::Ghost,on:click=move |_|{
                                                rule_id.set(Some(rule_id_saved));
                                                existing_rule.set(true);
                                                peer.set(rule_peer.clone());
                                                opt_out.set(rule_skip.clone());
                                                trigger.set(trigger_key(rule_trigger).into());
                                                editing_rule.set(true);
                                            }) {"Edit"}
                                        }
                                        }
                                    })
                                } else {
                                    AnyView::new(view! {
                                        // Whole card opens the area, not just a small chevron.
                                        Button(variant=ButtonVariant::Ghost,class="alert-card alert-card-button",on:click=move |_|{
                                            creating.set(false);
                                            confirm_delete.set(false);
                                            area.set(Some(area_id));
                                        },a11y:label=format!("Open {}",place.name)) {
                                            row(class="page-heading") {
                                                {boundary_preview(&place,[52.,52.])}
                                                column(class="grow") {
                                                    text(class="list-title") {{place.name.clone()}}
                                                    text(class="muted") {{if place.polygon.is_empty() {format!("Circle · {:.0} m radius",place.radius_m)} else {"Drawn boundary".into()}}}
                                                }
                                                text(class="chevron") {"›"}
                                            }
                                            for tag in move ||saved.with(|list|list.iter().filter(|p|p.area()==area_id&&!p.no_rule)
                                                .map(|p|(who_name(p),caption(p.trigger))).collect::<Vec<_>>()),key=|tag|tag.clone() {
                                                text(class="alert-tag") {{format!("{} · {}",tag.0,tag.1)}}
                                            }
                                        }
                                    })
                                }
                            }}
                        }
                        if move ||selected.with(Option::is_some) {
                            Button(variant=ButtonVariant::Outline,class="danger-text wide",on:click=move |_|{
                                if !confirm_delete.get_untracked() {
                                    confirm_delete.set(true);
                                    return;
                                }
                                let Some(id) = area.get_untracked() else {return;};
                                let directory=s.directory.get_value();
                                spawn_local(async move {
                                    let result=blocking(move ||{
                                        let manager=android_app::manager(&directory)?;
                                        places::remove_area(manager.friends(),id)
                                    }).await;
                                    match result {
                                        Ok(())=>{area.set(None);editing_rule.set(false);confirm_delete.set(false);s.place_outline.set(Vec::new());reload();}
                                        Err(error)=>status.set(error.to_string()),
                                    }
                                });
                            }) {{move ||if confirm_delete.get() {"Tap again to delete area and its alerts"} else {"Delete alert area"}}}
                        }
                        }
                    }
                }
            }
        }
        if move ||creating.get()&&!open.get()&&!s.drawing_alert.get()&&s.editing_circle.with(Option::is_some) {
            column(class="alert-create-header") {
                row(class="alert-create-title") {
                    text(class="list-title grow") {"New alert area"}
                    Button(variant=ButtonVariant::Secondary,on:click=move |_|{
                        creating.set(false);
                        s.picking_place_center.set(false);
                        s.editing_circle.set(None);
                        s.place_outline.set(Vec::new());
                        open.set(true);
                    }) {"Cancel"}
                }
                row(class="alert-create-hint") {
                    text(class="muted grow") {"Pan the map to place the center"}
                    Button(variant=ButtonVariant::Ghost,on:click=move |_|draw()) {"Draw shape"}
                }
            }
            column(class="alert-create-panel") {
                box(class="alert-create-grip") {}
                column(class="alert-create-field") {
                    text(class="section-title") {"Name"}
                    Input(value=name,label="Alert area name",placeholder="Home, school, campsite…")
                }
                column(class="alert-create-field alert-radius") {
                    row(class="page-heading") {
                        text(class="section-title grow") {"Radius"}
                        text(class="list-title") {{move ||format!("{:.0} m",radius_slider.get())}}
                    }
                    {radius_slider_view(radius_slider)}
                }
                if move ||!status.with(String::is_empty) {text(class="muted") {{move ||status.get()}}}
                row(class="alert-create-actions") {
                    Button(variant=ButtonVariant::Outline,disabled=Signal::derive_local(move ||s.position.with(Option::is_none)),on:click=move |_|{
                        if let Some((point,_))=s.position.get_untracked(){
                            s.center_on(point);
                            s.editing_circle.update(|circle|if let Some((center,_))=circle{*center=point});
                        }
                    }) {"Use my location"}
                    Button(class="alert-create-next",disabled=Signal::derive_local(move ||name.with(|n|n.trim().is_empty())),on:click=move |_|add()) {"Next: add alerts"}
                }
            }

        }
    }
}

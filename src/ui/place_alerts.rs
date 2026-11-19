//! User-selected arrival/departure subscriptions, stored only on this phone.
use super::{
    state::{Mark, Setup, UiState},
    tools::shape_preview,
};
use crate::sharing::{
    app,
    peers::{Friend, Group},
    places::{self, Place, Who},
};
use std::collections::BTreeSet;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Friends (not blocked) and current groups, for choosing and filtering alert rules.
type People = (Vec<Friend>, Vec<Group>);

fn load(directory: std::path::PathBuf) -> Result<(Vec<Place>, People), String> {
    let manager = app::manager(&directory).map_err(|e| e.to_string())?;
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

fn marks(list: &[Place], filter: Option<&str>, friends: &[Friend]) -> Vec<Mark> {
    let mut seen = BTreeSet::new();
    list.iter()
        .filter(|p| shown(p, filter, friends))
        .filter(|p| seen.insert(p.area()))
        .map(|p| Mark {
            alert: true,
            id: p.area().into(),
            name: p.name.clone(),
            outline: p.outline(),
        })
        .collect()
}

/// Filter choices that have at least one alert area.
fn used_options(list: &[Place], people: &People) -> Vec<(String, String)> {
    filter_options(people)
        .into_iter()
        .filter(|(key, _)| !marks(list, Some(key), &people.0).is_empty())
        .collect()
}

/// Draw every alert area (all, or those watching `filter`: friend id or "g:<group id>") on
/// the map with tappable names, fit them in view and switch to the map.
pub fn show_on_map(s: UiState, filter: Option<String>) {
    let directory = s.directory.get_value();
    zgui::reactive::spawn_detached(async move {
        match blocking(move || load(directory)).await {
            // None yet: the Alert areas page explains how to add one.
            Ok((list, _)) if list.is_empty() => {
                s.place_peer.set(None);
                s.places_open.set(true);
            }
            Ok((list, people)) => {
                let shown = marks(&list, filter.as_deref(), &people.0);
                s.area_filters.set(used_options(&list, &people));
                s.place_peer.set(filter);
                if shown.is_empty() {
                    s.message.set("No alert areas for them yet".into());
                    return;
                }
                s.show_marks(shown);
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
            s.area_marks.set(marks(&list, Some(&peer), &people.0));
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

#[component]
pub fn PlaceAlerts(open: RwSignal<bool, LocalStorage>) -> impl IntoView {
    let s = UiState::current();
    let saved = RwSignal::new_local(Vec::<Place>::new());
    let records = RwSignal::new_local((Vec::<Friend>::new(), Vec::<Group>::new()));
    // Group members the chosen group rule skips.
    let opt_out = RwSignal::new_local(Vec::<String>::new());
    let peer = RwSignal::new_local("none".to_owned());
    let trigger = RwSignal::new_local("both".to_owned());
    let status = RwSignal::new_local(String::new());
    let area = RwSignal::new_local(None::<u32>);
    let rule_id = RwSignal::new_local(None::<u32>);
    let existing_rule = RwSignal::new_local(false);
    let editing_rule = RwSignal::new_local(false);
    let confirm_delete = RwSignal::new_local(false);
    // Area just created: its page opens with the alert sheet once loaded.
    let ask_rule = StoredValue::new_local(false);
    let has_rule = move |key: &str| {
        saved.with_untracked(|list| {
            list.iter()
                .any(|p| Some(p.area()) == area.get_untracked() && !p.no_rule && rule_key(p) == key)
        })
    };
    // New alert for page filter's friend or group, else first without an alert here.
    let new_rule = move || {
        let keys: Vec<String> = records
            .with_untracked(filter_options)
            .into_iter()
            .map(|o| o.0)
            .collect();
        let wanted = s
            .place_peer
            .get_untracked()
            .filter(|k| keys.contains(k) && !has_rule(k));
        if let Some(key) = wanted
            .or_else(|| keys.iter().find(|k| !has_rule(k)).cloned())
            .or_else(|| keys.first().cloned())
        {
            peer.set(key);
        }
        existing_rule.set(false);
        opt_out.set(Vec::new());
        trigger.set("both".into());
        editing_rule.set(true);
    };
    let reload = move || {
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            match blocking(move || load(directory)).await {
                Ok((list, loaded)) => {
                    saved.set(list);
                    records.set(loaded);
                    if ask_rule.get_value() {
                        ask_rule.set_value(false);
                        new_rule();
                    }
                }
                Err(e) => status.set(e),
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
            editing_rule.set(false);
            confirm_delete.set(false);
            area.set(s.open_area.get_untracked());
            s.open_area.set(None);
            reload();
        }
    });
    on_cleanup_local(move || drop(effect));
    // Open area page keeps its "last seen" lines current.
    let refresh = set_interval(std::time::Duration::from_secs(10), move || {
        if open.get_untracked() && area.with_untracked(Option::is_some) {
            reload();
        }
    });
    on_cleanup_local(move || drop(refresh));
    // Setup's circle, or drawn shape (its first corner as nominal center).
    let setup_place = move || {
        let (circle, polygon) = s.setup_shape();
        let (center, radius) = circle.or_else(|| polygon.first().map(|p| (*p, 200.)))?;
        Some((s.area_name.get_untracked(), center, radius, polygon))
    };
    let reopen = move |id: u32| {
        s.close_setup();
        s.open_area.set(Some(id));
        open.set(true);
    };
    let create = move || {
        let Some((name, center, radius, polygon)) = setup_place() else {
            return;
        };
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || -> crate::sharing::Result<u32> {
                let manager = app::manager(&directory)?;
                places::set_rule(
                    manager.friends(),
                    None,
                    &name,
                    center,
                    radius,
                    None,
                    places::Trigger::Both,
                    polygon,
                )
            })
            .await;
            match result {
                Ok(id) => {
                    ask_rule.set_value(true);
                    reopen(id);
                }
                Err(e) => s.message.set(e.to_string()),
            }
        });
    };
    let save_edit = move |id: u32| {
        let Some((name, center, radius, polygon)) = setup_place() else {
            return;
        };
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || {
                let manager = app::manager(&directory)?;
                places::edit_area(
                    manager.friends(),
                    id,
                    &name,
                    center,
                    radius,
                    (!polygon.is_empty()).then_some(polygon),
                )
            })
            .await;
            match result {
                Ok(()) => reopen(id),
                Err(e) => s.message.set(e.to_string()),
            }
        });
    };
    let new_area = move || {
        s.open_setup(
            Setup {
                title: "New alert area",
                done: "Next: add alerts",
                finish: std::rc::Rc::new(create),
                cancel: std::rc::Rc::new(move || {
                    s.close_setup();
                    open.set(true);
                }),
            },
            String::new(),
            None,
            Vec::new(),
        );
    };
    let edit_area = move |place: Place| {
        let id = place.area();
        s.open_setup(
            Setup {
                title: "Edit alert area",
                done: "Save area",
                finish: std::rc::Rc::new(move || save_edit(id)),
                cancel: std::rc::Rc::new(move || reopen(id)),
            },
            place.name.clone(),
            place
                .polygon
                .is_empty()
                .then_some((place.center, place.radius_m)),
            place.polygon,
        );
    };
    let add = move || {
        let selected = area.get_untracked();
        let trigger_value = match trigger.get_untracked().as_str() {
            "entering" => places::Trigger::Entering,
            "exiting" => places::Trigger::Exiting,
            _ => places::Trigger::Both,
        };
        let directory = s.directory.get_value();
        let peer_value = peer.get_untracked();
        let skip = opt_out.get_untracked();
        let Some(place) =
            saved.with_untracked(|list| list.iter().find(|p| Some(p.area()) == selected).cloned())
        else {
            return;
        };
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || -> crate::sharing::Result<u32> {
                let manager = app::manager(&directory)?;
                places::set_rule(
                    manager.friends(),
                    selected,
                    &place.name,
                    place.center,
                    place.radius_m,
                    Some(if let Some(group) = peer_value.strip_prefix("g:") {
                        Who::Group(
                            group.into(),
                            skip.iter().filter_map(|p| p.parse().ok()).collect(),
                        )
                    } else {
                        Who::Friend(peer_value.parse()?)
                    }),
                    trigger_value,
                    Vec::new(),
                )
            })
            .await;
            match result {
                Ok(_) => {
                    editing_rule.set(false);
                    status.set("Alert saved".into());
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
    // What the alert last saw for each watched person, so it is clear it is working.
    let last_seen = move |place: &Place| {
        let now = crate::sharing::manager::now_ms();
        let ago = |at: i64| match (now - at).max(0) / 60_000 {
            0 => "just now".to_string(),
            m @ 1..=59 => format!("{m} min ago"),
            m => format!("{} h ago", m / 60),
        };
        let lines = records.with(|(friends, _)| {
            friends
                .iter()
                .filter(|f| place.watches(f))
                .map(|f| match place.side(f.peer_id) {
                    Some((inside, at)) => format!(
                        "{} {} · {}",
                        f.display_name,
                        if inside { "inside" } else { "outside" },
                        ago(at)
                    ),
                    None => format!("{}: no location yet", f.display_name),
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        if lines.is_empty() {
            "Nobody to watch: all members skipped".into()
        } else {
            lines
        }
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
    let view_on_map = move |place: Place| {
        s.show_marks(vec![Mark {
            alert: true,
            id: place.area().into(),
            name: place.name.clone(),
            outline: place.outline(),
        }]);
    };
    let remove_alert = move || {
        let Some(id) = rule_id.get_untracked() else {
            return;
        };
        let directory = s.directory.get_value();
        spawn_local(async move {
            let result = blocking(move || {
                let manager = app::manager(&directory)?;
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
        if editing_rule.get_untracked() {
            editing_rule.set(false);
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
    let filters = move || saved.with(|list| records.with(|people| used_options(list, people)));
    view! {
        if move ||open.get() {
            column(class="tab-page alert-page") {
                row(class="page-heading") {
                    Button(variant=ButtonVariant::Ghost,class="alert-back page-back",a11y:label="Back",on:click=move |_|back()) {"‹"}
                    text(class="page-title grow") {{move ||selected.with(|p|p.as_ref().map(|p|p.name.clone())).unwrap_or_else(||"Alert areas".into())}}
                }
                scroll(class="page-scroll") {
                    column(class="list-body") {
                        if move ||selected.with(Option::is_some) {
                            column(class="alert-card") {
                                row(class="alert-preview") {
                                    for place in move ||selected.get().into_iter().collect::<Vec<_>>(),key=|p|(p.id,p.center.lat.to_bits(),p.center.lon.to_bits(),p.radius_m.to_bits(),p.polygon.iter().map(|p|(p.lat.to_bits(),p.lon.to_bits())).collect::<Vec<_>>()) {
                                        {shape_preview(place.center,place.outline(),[320.,150.])}
                                    }
                                }
                                text(class="muted") {{move ||selected.with(|place|place.as_ref().map(|p|if p.polygon.is_empty(){format!("Circle · {:.0} m radius",p.radius_m)}else{format!("Drawn shape · {} corners",p.polygon.len())}).unwrap_or_default())}}
                                row(class="actions") {
                                    Button(variant=ButtonVariant::Outline,class="edit-alert-area",on:click=move |_|if let Some(place)=selected.get_untracked() {edit_area(place)}) {"Edit area"}
                                    Button(variant=ButtonVariant::Outline,on:click=move |_|if let Some(place)=selected.get_untracked() {view_on_map(place)}) {"View on map"}
                                }
                            }
                            row(class="page-heading") {
                                text(class="list-title grow") {"Alerts"}
                                Button(class="add-alert",disabled=Signal::derive_local(move ||records.with(|r|r.0.is_empty()&&r.1.is_empty())),on:click=move |_|new_rule()) {"+ Add alert"}
                            }
                            if move || !saved.with(|list|list.iter().any(|p|Some(p.area())==area.get()&&!p.no_rule)) {
                                text(class="muted") {"No alerts yet."}
                            }
                        } else {
                            text(class="muted") {"Get notified when friends arrive at or leave an area."}
                            // Same actions row as Areas: create first, then view on map.
                            row(class="actions") {
                                Button(class="new-alert-area",on:click=move |_|new_area()) {"+ New alert area"}
                                Button(variant=ButtonVariant::Outline,class="show-areas",disabled=Signal::derive_local(move ||saved.with(Vec::is_empty)),on:click=move |_|show_on_map(s,s.place_peer.get_untracked())) {
                                    {move ||if s.place_peer.with(Option::is_some) {"Show these on map"} else {"Show all on map"}}
                                }
                            }
                            if move ||saved.with(Vec::is_empty) {
                                column(class="alert-card") {
                                    text(class="list-title") {"No alert areas yet"}
                                    text(class="muted") {"Tap + New alert area, place a circle or draw a shape on the map, then choose which friends to watch."}
                                }
                            }
                            if move ||!filters().is_empty() {
                                scroll(class="alert-filter") {
                                    row(class="friend-strip-row") {
                                        Button(variant=ButtonVariant::Outline,class:seg-on=move ||s.place_peer.with(Option::is_none),on:click=move |_|s.place_peer.set(None)) {"All"}
                                        for person in move ||saved.with(|list|records.with(|people|used_options(list,people))),key=|p|p.clone() {
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
                        for entry in move || {
                            let filter = s.place_peer.get();
                            let selected = area.get();
                            let mut seen = BTreeSet::new();
                            saved.get().into_iter().filter(|p| {
                                if let Some(id) = selected { return p.area() == id && !p.no_rule; }
                                records.with(|r| shown(p, filter.as_deref(), &r.0)) && seen.insert(p.area())
                            }).map(|p|(selected.is_some(),p)).collect::<Vec<_>>()
                        },key=move |entry|{
                            let (details,p)=entry;
                            (*details,last_seen(p),p.id,p.peer,p.trigger as u8,p.name.clone(),p.center.lat.to_bits(),p.center.lon.to_bits(),p.radius_m.to_bits(),p.polygon.iter().map(|p|(p.lat.to_bits(),p.lon.to_bits())).collect::<Vec<_>>())
                        } {
                            {{
                                let (details,place)=entry;
                                let area_id = place.area();
                                let rule_id_saved = place.id;
                                let rule_peer = rule_key(&place);
                                let rule_skip: Vec<String> = place.opt_out.iter().map(ToString::to_string).collect();
                                let rule_who = who_name(&place);
                                let rule_seen = last_seen(&place);
                                let rule_trigger = place.trigger;
                                if details {
                                    AnyView::new(view! {
                                        column { row(class="list-row") {
                                            column(class="grow") {
                                                text(class="list-title") {{rule_who.clone()}}
                                                text(class="muted") {{caption(rule_trigger)}}
                                                text(class="muted small rule-seen") {{rule_seen.clone()}}
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
                                            confirm_delete.set(false);
                                            area.set(Some(area_id));
                                        },a11y:label=format!("Open {}",place.name)) {
                                            // One column: tags under the heading, not beside it.
                                            column(class="alert-card-body") {
                                                row(class="page-heading") {
                                                    {shape_preview(place.center,place.outline(),[52.,52.])}
                                                    column(class="grow") {
                                                        text(class="list-title") {{place.name.clone()}}
                                                        text(class="muted") {{if place.polygon.is_empty() {format!("Circle · {:.0} m radius",place.radius_m)} else {format!("Drawn shape · {} corners",place.polygon.len())}}}
                                                    }
                                                    text(class="chevron") {"›"}
                                                }
                                                for tag in move ||saved.with(|list|list.iter().filter(|p|p.area()==area_id&&!p.no_rule)
                                                    .map(|p|(who_name(p),caption(p.trigger))).collect::<Vec<_>>()),key=|tag|tag.clone() {
                                                    text(class="alert-tag") {{format!("{} · {}",tag.0,tag.1)}}
                                                }
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
                                        let manager=app::manager(&directory)?;
                                        places::remove_area(manager.friends(),id)
                                    }).await;
                                    match result {
                                        Ok(())=>{area.set(None);editing_rule.set(false);confirm_delete.set(false);s.area_marks.set(Vec::new());reload();}
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
}

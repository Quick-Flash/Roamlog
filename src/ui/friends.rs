//! Friends sheet: sharing mode, pairing by invitation, per-friend access, block and remove.
// view! needs `move ||` closures for reactive `if`/`for`, even around plain functions.
#![allow(clippy::redundant_closure)]
use super::place_alerts::PlaceAlertsProps;
use super::state::{UiState, now_ms};
use crate::sharing::{
    app::{self as sharing, PairStep, Pairing},
    groups::Policy,
    location::UpdatePolicy,
    manager::Mode,
    node::{FriendState, read_published},
    pairing::{FriendInvite, GroupRef},
    peers::{Friend, Permission},
};
use std::path::PathBuf;
use std::time::Duration;
use zgui::prelude::*;
use zgui::reactive::UnsyncCallback;
use zgui_image::ImageBytes;
use zgui_ui::prelude::*;
use zgui_ui_icons::{
    prelude::*,
    set::{mark::PLUS, ui::ELLIPSIS},
};

#[derive(Clone, PartialEq)]
struct Row {
    id: String,
    name: String,
    permission: Permission,
    /// Highest grant from this friend's groups, and their names.
    group_permission: Permission,
    groups: String,
    group_ids: Vec<String>,
    blocked: bool,
    status: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct GroupRow {
    id: String,
    name: String,
    /// Member names besides you.
    members: Vec<String>,
    share: Permission,
    leaving: bool,
    policy: Option<Policy>,
    admin: bool,
    active: bool,
    details: Vec<MemberRow>,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct MemberRow {
    id: String,
    name: String,
    added_by: String,
    admin: bool,
    creator: bool,
    me: bool,
}

#[derive(Clone, PartialEq)]
struct Loaded {
    mode: Mode,
    expires_at_ms: Option<i64>,
    share_battery: bool,
    alert_names: Vec<String>,
    alerts_by_friend: std::collections::BTreeMap<String, Vec<String>>,
    rows: Vec<Row>,
    groups: Vec<GroupRow>,
}

fn permission_value(permission: Permission) -> &'static str {
    match permission {
        Permission::Off => "off",
        Permission::TripsOnly => "trips",
        Permission::Always => "always",
    }
}

fn permission_name(permission: Permission) -> &'static str {
    match permission {
        Permission::Off => "Hidden",
        Permission::TripsOnly => "Trips",
        Permission::Always => "Always",
    }
}

fn age(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match s {
        0..60 => format!("{s} s ago"),
        60..3600 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

fn status(friend: &Friend, published: Option<&FriendState>, now: i64) -> String {
    if friend.blocked {
        return "Blocked".into();
    }
    let Some(state) = published else {
        return "Not connected".into();
    };
    if state.update_required {
        return "Update Roamlog".into();
    }
    let seen = state.location.map(|l| age(now - l.timestamp_ms));
    let path = match state.path.as_deref() {
        Some("direct") => " · direct",
        Some("relay") => " · via relay",
        _ => "",
    };
    let text = match (state.connected, state.sharing_active, seen) {
        (true, true, Some(seen)) => format!("Live · updated {seen}"),
        (true, _, Some(seen)) => format!("Connected · last location {seen}"),
        (true, _, None) => "Connected · not sharing with you".into(),
        (false, _, Some(seen)) => format!("Offline · last seen {seen}"),
        (false, _, None) => "Offline".into(),
    };
    if state.connected {
        format!("{text}{path}")
    } else {
        text
    }
}

fn load(directory: PathBuf) -> Result<Loaded, String> {
    let manager = sharing::manager(&directory).map_err(|e| e.to_string())?;
    let published = read_published(&directory);
    let now = now_ms() as i64;
    let friends = manager.friends().list().map_err(|e| e.to_string())?;
    let groups = manager.friends().groups().map_err(|e| e.to_string())?;
    let rows: Vec<Row> = friends
        .iter()
        .map(|f| {
            let state = published.friends.iter().find(|p| p.peer_id == f.peer_id);
            Row {
                id: f.peer_id.to_string(),
                status: if !f.blocked
                    && !state.is_some_and(|s| s.update_required)
                    && let Some(via) = state.and_then(|s| s.via)
                {
                    let via = friends
                        .iter()
                        .find(|f| f.peer_id == via)
                        .map(|f| f.display_name.clone())
                        .unwrap_or_else(|| via.to_string());
                    format!(
                        "via {via}, {}",
                        state
                            .and_then(|s| s.location)
                            .map(|l| age(now - l.timestamp_ms))
                            .unwrap_or_else(|| "unavailable".into())
                    )
                } else {
                    status(f, state, now)
                },
                name: f.display_name.clone(),
                permission: f.permission,
                group_permission: f.group_permission(&groups),
                blocked: f.blocked,
                groups: groups
                    .iter()
                    .filter(|g| f.groups.contains(&g.id) && g.leaving_since_ms.is_none())
                    .map(|g| g.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
                group_ids: groups
                    .iter()
                    .filter(|g| f.groups.contains(&g.id) && g.leaving_since_ms.is_none())
                    .map(|g| g.id.clone())
                    .collect(),
            }
        })
        .collect();
    let groups = groups
        .into_iter()
        .map(|g| {
            let state = manager
                .friends()
                .group_state(&g.id)
                .map_err(|e| e.to_string())?;
            let details = state
                .as_ref()
                .map(|state| {
                    state
                        .members
                        .iter()
                        .map(|(id, member)| MemberRow {
                            id: id.to_string(),
                            name: member.name.clone(),
                            added_by: state
                                .names
                                .get(&member.added_by)
                                .cloned()
                                .unwrap_or_else(|| member.added_by.to_string()),
                            admin: state.admins.contains(id),
                            creator: *id == state.creator,
                            me: *id == manager.owner(),
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Ok(GroupRow {
                members: details
                    .iter()
                    .filter(|m| !m.me)
                    .map(|m| m.name.clone())
                    .collect(),
                leaving: g.leaving_since_ms.is_some(),
                share: g.share,
                policy: state.as_ref().map(|s| s.policy),
                admin: state
                    .as_ref()
                    .is_some_and(|s| s.admins.contains(&manager.owner())),
                active: state
                    .as_ref()
                    .is_some_and(|s| s.members.contains_key(&manager.owner())),
                details,
                id: g.id,
                name: g.name,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let rows = rows
        .into_iter()
        .map(|mut row: Row| {
            if let Some(state) = published
                .friends
                .iter()
                .find(|f| f.peer_id.to_string() == row.id)
                && let Some(battery) = state
                    .battery
                    .filter(|b| now.saturating_sub(b.timestamp_ms) <= 120_000)
                && let Some(percent) = battery.percent
            {
                row.status.push_str(&format!(" · Battery {percent}%"));
            }
            if let Ok(peer) = row.id.parse()
                && let Some(request) = crate::sharing::requests::read(manager.friends(), peer)
                && now.saturating_sub(request.at_ms) < 60_000
            {
                row.status.push_str(&format!(" · {}", request.status));
            }
            row
        })
        .collect();
    let mut alerts = std::collections::BTreeMap::new();
    let mut alerts_by_friend = std::collections::BTreeMap::<String, Vec<String>>::new();
    for area in crate::sharing::places::list(manager.friends()).unwrap_or_default() {
        alerts.insert(area.area(), area.name.clone());
        for friend in friends.iter().filter(|f| area.watches(f)) {
            let names = alerts_by_friend
                .entry(friend.peer_id.to_string())
                .or_default();
            if !names.contains(&area.name) {
                names.push(area.name.clone());
            }
        }
    }
    let alert_names = alerts.into_values().collect();
    Ok(Loaded {
        mode: manager.mode().map_err(|e| e.to_string())?,
        expires_at_ms: manager.expires_at_ms().map_err(|e| e.to_string())?,
        share_battery: manager.shares_battery().map_err(|e| e.to_string())?,
        alert_names,
        alerts_by_friend,
        rows,
        groups,
    })
}

/// Runs a friends/settings change off the UI thread, then reloads and wakes the service.
fn change(
    s: UiState,
    loaded: RwSignal<Option<Loaded>, LocalStorage>,
    restart: bool,
    work: impl FnOnce(&crate::sharing::manager::Manager) -> crate::sharing::Result<()> + Send + 'static,
) {
    let directory = s.directory.get_value();
    zgui::reactive::spawn_detached(async move {
        let dir = directory.clone();
        let result = blocking(move || {
            let manager = sharing::manager(&dir).map_err(|e| e.to_string())?;
            work(&manager).map_err(|e| e.to_string())?;
            load(dir)
        })
        .await;
        match result {
            Ok(value) => loaded.set(Some(value)),
            Err(error) => s.message.set(format!("Sharing: {error}")),
        }
        let outcome = if restart {
            crate::platform::sharing_changed(directory)
        } else {
            crate::platform::nudge_sharing(&directory);
            Ok(())
        };
        if let Err(error) = outcome {
            s.message.set(error);
        }
    });
}

pub fn sheet() -> impl IntoView {
    view! {
        Friends()
        FriendTrips()
    }
}

#[component]
pub fn Friends() -> impl IntoView {
    let s = UiState::current();
    let directory = s.directory.get_value();
    let saved_name = std::fs::read_to_string(directory.join("sharing-name")).unwrap_or_default();
    let name = RwSignal::new_local(saved_name);
    let loaded: RwSignal<Option<Loaded>, LocalStorage> = RwSignal::new_local(None);
    let pairing: StoredValue<Option<Pairing>, LocalStorage> = StoredValue::new_local(None);
    let step = RwSignal::new_local(PairStep::Idle);
    let qr: StoredValue<Option<ImageBytes>, LocalStorage> = StoredValue::new_local(None);
    let qr_url = RwSignal::new_local(None::<String>);
    let edit = Edit {
        id: RwSignal::new_local(None),
        text: RwSignal::new_local(String::new()),
    };
    let reload = move || {
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            match blocking(move || load(directory)).await {
                Ok(value) => {
                    if loaded.with_untracked(|old| old.as_ref() != Some(&value)) {
                        loaded.set(Some(value));
                    }
                }
                Err(error) => s.message.set(format!("Sharing: {error}")),
            }
        });
    };
    // Pairing panel: open while adding a friend, joining from a link, or pairing.
    let adding = RwSignal::new_local(false);
    // Who and which group the last confirm step was about; Done arrives after Waiting.
    let last_confirm = StoredValue::new_local(None::<(String, Option<String>, bool)>);
    // Poll pairing progress and friend status while sheet is open.
    let timer = set_interval(Duration::from_millis(500), move || {
        if !s.friends_open.get_untracked() {
            return;
        }
        let next = pairing.with_value(|p| p.as_ref().map(Pairing::step));
        if let Some(next) = next
            && step.with_untracked(|old| old != &next)
        {
            if let PairStep::Invite { qr: rows, .. } = &next
                && let Ok(png) = sharing::qr_png(rows)
            {
                let image = ImageBytes::new(png);
                qr_url.set(Some(image.url()));
                qr.set_value(Some(image));
            }
            let finished = matches!(
                next,
                PairStep::Done(_) | PairStep::Failed(_) | PairStep::Idle
            );
            // Success: close the pairing panel and report on the Friends list itself; group
            // invites say what happened to the group instead of the generic pairing text.
            if let PairStep::Confirm {
                name,
                group,
                joining,
                ..
            } = &next
            {
                last_confirm.set_value(Some((name.clone(), group.clone(), *joining)));
            }
            if let PairStep::Done(message) = &next {
                let text = match last_confirm.get_value() {
                    Some((_, Some(group), true)) => format!("Joined {group}."),
                    Some((name, Some(group), false)) => format!("Added {name} to {group}."),
                    _ => message.clone(),
                };
                s.message.set(text);
                adding.set(false);
            }
            step.set(next);
            if finished {
                pairing.set_value(None);
                qr.set_value(None);
                qr_url.set(None);
                // Pairing endpoint closed; let service rebind its node.
                if let Err(error) = crate::platform::sharing_changed(s.directory.get_value()) {
                    s.message.set(error);
                }
                reload();
            }
        }
    });
    on_cleanup_local(move || drop(timer));
    let status_timer = set_interval(Duration::from_secs(3), move || {
        if s.friends_open.get_untracked() {
            reload();
        }
    });
    on_cleanup_local(move || drop(status_timer));
    let opened = zgui::reactive::RenderEffect::new(move |_| {
        if s.friends_open.get() {
            reload();
        }
    });
    on_cleanup_local(move || drop(opened));
    let join_access = RwSignal::new_local(Permission::TripsOnly);
    // Group the shown invitation is for; page title says so.
    let inviting = RwSignal::new_local(None::<String>);
    let begin = move |link: Option<String>, group: Option<GroupRef>| {
        let display = name.get_untracked().trim().to_owned();
        if display.is_empty() {
            s.message
                .set("Enter your name first; friend sees it while pairing.".into());
            return;
        }
        let directory = s.directory.get_value();
        let _ = std::fs::write(directory.join("sharing-name"), &display);
        crate::platform::suspend_sharing(&directory);
        join_access.set(Permission::TripsOnly);
        inviting.set(group.as_ref().map(|g| g.name.clone()));
        step.set(PairStep::Starting);
        pairing.set_value(Some(Pairing::start(directory, display, link, group)));
    };
    let cancel = move || {
        pairing.set_value(None);
        qr.set_value(None);
        qr_url.set(None);
        step.set(PairStep::Idle);
        if let Err(error) = crate::platform::sharing_changed(s.directory.get_value()) {
            s.message.set(error);
        }
    };
    let set_mode = move |mode: Mode| change(s, loaded, true, move |m| m.set_mode(mode));
    let options_open = RwSignal::new_local(false);
    let profile_open = s.profile_open;
    let direct_access = RwSignal::new_local(Permission::Off);
    let invite_until = RwSignal::new_local(None::<u64>);
    let invite_clock = zgui::reactive::RenderEffect::new(move |_| {
        if matches!(step.get(), PairStep::Invite { .. }) {
            if invite_until.with_untracked(Option::is_none) {
                invite_until.set(Some(now_ms() + 300_000));
            }
        } else {
            invite_until.set(None);
            if !matches!(
                step.get_untracked(),
                PairStep::Confirm { .. } | PairStep::Waiting
            ) {
                direct_access.set(Permission::Off);
            }
        }
    });
    on_cleanup_local(move || drop(invite_clock));
    let duration = RwSignal::new_local("1".to_string());
    let duration_unit = RwSignal::new_local("60".to_string());
    let places_open = s.places_open;
    let group_name = RwSignal::new_local(String::new());
    let group_policy = RwSignal::new_local("managed".to_string());
    let group_access = RwSignal::new_local(Permission::Always);
    let groups_open = RwSignal::new_local(false);
    let new_group = RwSignal::new_local(false);
    let detail = s.friend_detail;
    // Opened straight to a friend (e.g. from the map): drop sub-pages left open underneath,
    // so Back returns to the Friends list.
    let deep_link = zgui::reactive::RenderEffect::new(move |_| {
        if s.friends_open.get() && detail.with_untracked(Option::is_some) {
            options_open.set(false);
            profile_open.set(false);
            groups_open.set(false);
        }
        if !s.friends_open.get() {
            s.profile_from_map.set_value(false);
            profile_open.set(false);
        }
    });
    on_cleanup_local(move || drop(deep_link));
    let custom_timer = RwSignal::new_local(false);
    // Join with link box. A whole link (pasted, or opened from another app) starts at once when
    // a name is set; one typed key by key waits until complete; Join reports a bad one.
    let pasted = RwSignal::new_local(String::new());
    let join = move || {
        adding.set(true);
        if name.get_untracked().trim().is_empty() {
            s.message
                .set("Enter your name, then tap Join; they see it while pairing.".into());
            return;
        }
        let link = pasted.get_untracked().trim().to_owned();
        pasted.set(String::new());
        begin(Some(link), None);
    };
    let whole_link = zgui::reactive::RenderEffect::new(move |_| {
        let complete = pasted.with(|l| FriendInvite::from_link(l.trim()).is_ok());
        let ready = matches!(
            step.get_untracked(),
            PairStep::Idle | PairStep::Done(_) | PairStep::Failed(_)
        );
        if complete && ready && !name.get_untracked().trim().is_empty() {
            join();
        }
    });
    on_cleanup_local(move || drop(whole_link));
    let invited = zgui::reactive::RenderEffect::new(move |_| {
        let link = s.invite_link.get();
        if !link.trim().is_empty() {
            s.invite_link.set(String::new());
            adding.set(true);
            pasted.set(link);
        }
    });
    on_cleanup_local(move || drop(invited));
    let confirm = RwSignal::new_local(None::<String>);
    let name_value = move || name.get_untracked().trim().to_owned();
    let begin_group = Callback::new(move |group: GroupRef| {
        adding.set(true);
        begin(None, Some(group))
    });
    let groups = move || loaded.with(|l| l.as_ref().map(|l| l.groups.clone()).unwrap_or_default());
    let mode = move || loaded.with(|l| l.as_ref().map_or(Mode::Off, |l| l.mode));
    let search = RwSignal::new_local(String::new());
    let all_rows = move || loaded.with(|l| l.as_ref().map_or(0, |l| l.rows.len()));
    // Friends list limited to one group's members: group id, None for everyone.
    let group_filter = RwSignal::new_local(None::<String>);
    let rows = move || {
        let wanted = search.with(|q| q.trim().to_lowercase());
        let group = group_filter.get();
        loaded.with(|l| {
            l.as_ref()
                .map(|l| {
                    l.rows
                        .iter()
                        .filter(|r| wanted.is_empty() || r.name.to_lowercase().contains(&wanted))
                        .filter(|r| group.as_ref().is_none_or(|g| r.group_ids.contains(g)))
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
    };
    let idle = move || {
        matches!(
            step.get(),
            PairStep::Idle | PairStep::Done(_) | PairStep::Failed(_)
        )
    };
    let close_panel = move || {
        if !idle() {
            cancel();
        }
        step.set(PairStep::Idle);
        adding.set(false);
    };
    let back = move || -> bool {
        if adding.get_untracked() || !idle() {
            close_panel();
        } else if detail.with_untracked(Option::is_some) {
            // Profile opened from map card: Back returns to map with that friend selected.
            if s.profile_from_map.get_value() {
                s.map_friend_selected.set(detail.get_untracked());
                s.friends_open.set(false);
            }
            detail.set(None);
        } else if options_open.get_untracked() {
            options_open.set(false);
        } else if profile_open.get_untracked() {
            profile_open.set(false);
        } else if groups_open.get_untracked() {
            groups_open.set(false);
        } else {
            return false;
        }
        true
    };
    s.friends_page_back
        .set_value(Some(UnsyncCallback::new(move |()| {
            if !back() {
                s.friends_open.set(false);
            }
        })));
    on_cleanup_local(move || s.friends_page_back.set_value(None));
    let title = move || {
        if adding.get() || !idle() {
            match step.get() {
                PairStep::Confirm {
                    group: Some(group),
                    joining: true,
                    ..
                } => format!("Join {group}"),
                PairStep::Confirm {
                    group: Some(group), ..
                } => format!("Invite to {group}"),
                _ => inviting
                    .get()
                    .map_or_else(|| "Add friend".into(), |group| format!("Invite to {group}")),
            }
        } else if let Some(id) = detail.get() {
            rows()
                .into_iter()
                .find(|row| row.id == id)
                .map(|row| row.name)
                .unwrap_or_else(|| "Friend".into())
        } else if options_open.get() {
            "Sharing options".into()
        } else if profile_open.get() {
            "Your profile".into()
        } else if groups_open.get() {
            "Groups".into()
        } else {
            "Friends".into()
        }
    };
    let home = move || {
        !adding.get()
            && idle()
            && detail.with(Option::is_none)
            && !options_open.get()
            && !profile_open.get()
            && !groups_open.get()
    };
    let timer_text = move || {
        loaded
            .with(|l| l.as_ref().and_then(|l| l.expires_at_ms))
            .map(|until| {
                let mins = ((until - s.now.get() as i64).max(0) + 59_999) / 60_000;
                if mins >= 1440 {
                    format!("Stops in {} d {} h", mins / 1440, mins / 60 % 24)
                } else if mins >= 60 {
                    format!("Stops in {} h {} min", mins / 60, mins % 60)
                } else {
                    format!("Stops in {mins} min")
                }
            })
    };
    view! {
        if move ||s.friends_open.get() {
            column(class="tab-page friends-sheet") {
                row(class="page-heading") {
                    if move ||!home() {
                        Button(variant=ButtonVariant::Ghost,class="page-back",a11y:label="Back",on:click=move |_|{back();}) {"‹"}
                    }
                    text(class="page-title grow") {{title}}
                    if move ||home() {
                        Button(class="add-friend",a11y:label="Add friend",on:click=move |_|{
                            // Fresh start: no result or message left from an earlier pairing.
                            s.message.set(String::new());
                            if matches!(step.get_untracked(),PairStep::Done(_)|PairStep::Failed(_)) {step.set(PairStep::Idle);}
                            adding.set(true);
                        }) {Icon(icon=PLUS) text {"Add friend"}}
                    }
                    if move ||groups_open.get() && !adding.get() && idle() {
                        Button(on:click=move |_|new_group.set(true)) {"+ New group"}
                    }
                }
                scroll(class="page-scroll") {
                    column(class="page-list") {
                        if move ||!s.message.with(String::is_empty) {
                            text(class="muted",a11y:role=Role::Status,on:click=move |_|s.message.set(String::new())) {{move ||s.message.get()}}
                        }
                        if move ||adding.get() || !idle() {
                            text(class="section-title") {{move ||if matches!(step.get(),PairStep::Confirm{..}|PairStep::Waiting) {"Step 2 of 2 · Confirm code"} else {"Step 1 of 2 · Connect"}}}
                        if move ||idle() {
                            text(class="muted") {"Pair in person: one of you shows a code, the other scans it. Or send the link instead."}
                            Input(value=name,label="Your name",placeholder="Your name (friends see it)")
                            row(class="row-actions") {
                                Button(class="invite-friend",on:click=move |_|begin(None,None)) {"Show my code"}
                                Button(variant=ButtonVariant::Outline,class="scan-invite",on:click=move |_|{if let Err(error)=crate::platform::scan_invite(s.directory.get_value()){s.message.set(error)}}) {"Scan theirs"}
                            }
                            row(class="create-row") {
                                Input(value=pasted,label="Join with link",placeholder="Or paste a friend or group link",class="grow")
                                Button(variant=ButtonVariant::Outline,class="join-invite",disabled=Signal::derive_local(move ||pasted.with(|l|l.trim().is_empty())),on:click=move |_|join()) {"Join"}
                            }
                        }
                        if move ||matches!(step.get(),PairStep::Starting) {
                            text(class="muted") {"Connecting… up to 30 seconds."}
                        }
                        if move ||matches!(step.get(),PairStep::Invite{..}) {
                            text(class="muted") {{move ||{
                                let left=invite_until.get().unwrap_or_default().saturating_sub(s.now.get())/1000;
                                format!("Code expires in {}:{:02}",left/60,left%60)
                            }}}
                            if move ||qr_url.get().is_some() {
                                box(class="qr-frame") {
                                    {zgui::elements::image().class("qr-image").src(Signal::derive_local(move ||qr_url.get())).alt(Some("Invitation QR code".to_string())).into_view()}
                                }
                            }
                            text(class="muted") {"Not together? Share the link. They paste it into Join with link, under Add friend or Groups."}
                            Button(variant=ButtonVariant::Outline,class="share-invite",on:click=move |_|{if let PairStep::Invite{link,..}=step.get_untracked(){crate::platform::share_text(&link)}}) {"Share link"}
                        }
                        if move ||matches!(step.get(),PairStep::Confirm{..}) {
                            text(class="pair-name") {{move ||match step.get(){PairStep::Confirm{name,group:Some(group),joining:true,..}=>format!("Pair with {name} and join {group}?"),PairStep::Confirm{name,group:Some(group),..}=>format!("Pair with {name} and add them to {group}?"),PairStep::Confirm{name,..}=>format!("Pair with {name}?"),_=>String::new()}}}
                            if move ||matches!(step.get(),PairStep::Confirm{group_details:Some(_),..}) {
                                text(class="muted") {{move ||match step.get(){PairStep::Confirm{group_details,..}=>group_details.unwrap_or_default(),_=>String::new()}}}
                            }
                            text(class="muted") {"Check this code matches their screen exactly."}
                            text(class="pair-code") {{move ||if let PairStep::Confirm{code,..}=step.get(){code}else{String::new()}}}
                            if move ||matches!(step.get(),PairStep::Confirm{group:Some(_),joining:true,..}) {
                                text(class="section-title") {"What members see"}
                                row(class="segmented") {
                                    for choice in ||vec![(Permission::Off,"Nothing yet"),(Permission::TripsOnly,"During trips"),(Permission::Always,"Always")],key=|choice|choice.1 {
                                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||join_access.get()==choice.0,on:click=move |_|join_access.set(choice.0)) {{choice.1}}
                                    }
                                }
                            }
                            if move ||matches!(step.get(),PairStep::Confirm{group:None,..}) {
                                text(class="section-title") {"What they see"}
                                row(class="segmented") {
                                    for choice in ||vec![(Permission::Off,"Nothing yet"),(Permission::TripsOnly,"During trips"),(Permission::Always,"Always")],key=|choice|choice.1 {
                                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||direct_access.get()==choice.0,on:click=move |_|direct_access.set(choice.0)) {{choice.1}}
                                    }
                                }
                            }
                            row(class="row-actions") {
                                Button(class="confirm-pair",on:click=move |_|pairing.with_value(|p|if let Some(p)=p{p.vote(true,if matches!(step.get_untracked(),PairStep::Confirm{group:None,..}) {direct_access.get_untracked()} else {join_access.get_untracked()})})) {"Codes match"}
                                Button(variant=ButtonVariant::Outline,class="reject-pair",on:click=move |_|pairing.with_value(|p|if let Some(p)=p{p.vote(false,join_access.get_untracked())})) {"Codes differ — reject"}
                            }
                        }
                        if move ||matches!(step.get(),PairStep::Waiting) {
                            text(class="muted") {"Waiting for friend to confirm…"}
                        }
                        if move ||matches!(step.get(),PairStep::Done(_)|PairStep::Failed(_)) {
                            text(class="muted pair-result") {{move ||match step.get(){PairStep::Done(m)=>m,PairStep::Failed(e)=>pairing_error(&e),_=>String::new()}}}
                        }
                        } else {
                        if move ||detail.with(Option::is_some) {
                            for friend in move ||rows().into_iter().filter(|r|Some(&r.id)==detail.get().as_ref()).collect::<Vec<_>>(),key=|r|(r.id.clone(),r.name.clone(),r.permission,r.group_permission,r.blocked,r.status.clone(),r.groups.clone()) {
                                FriendRow(friend=friend.clone(),loaded=loaded,confirm=confirm,edit=edit)
                            }
                        } else {
                        if move ||options_open.get() {
                            text(class="section-title") {"Share my location"}
                            for choice in ||vec![
                                (Mode::Off,"Off","Close sharing connections."),
                                (Mode::TripsOnly,"During trips","Send location while recording."),
                                (Mode::Always,"Always","Send location in the background.")
                            ],key=|choice|choice.1 {
                                Button(variant=ButtonVariant::Outline,class="friends-mode-choice",a11y:label=choice.1,class:seg-on=move ||mode()==choice.0,on:click=move |_|set_mode(choice.0)) {
                                    text {{choice.1}}
                                    text(class="muted") {{choice.2}}
                                }
                            }
                            text(class="section-title") {"Share for a while"}
                            if move ||timer_text().is_some() {
                                row(class="alert-card page-heading") {
                                    text(class="grow") {{move ||timer_text().unwrap_or_default()}}
                                    Button(variant=ButtonVariant::Outline,on:click=move |_|set_mode(Mode::Off)) {"Stop now"}
                                }
                            }
                            row(class="friends-timer-presets") {
                                for preset in ||vec![(15u32,"15 min"),(60,"1 hour"),(480,"8 hours"),(1440,"1 day")],key=|preset|preset.0 {
                                    Button(variant=ButtonVariant::Outline,on:click=move |_|change(s,loaded,true,move |m|m.set_timed(preset.0))) {{preset.1}}
                                }
                                Button(variant=ButtonVariant::Outline,on:click=move |_|custom_timer.update(|open|*open = !*open)) {"Custom…"}
                            }
                            if move ||custom_timer.get() {
                row(class="row-actions") {
                    Input(value=duration,label="Sharing duration",placeholder="Amount")
                    Select(value=duration_unit) {
                        SelectTrigger(label="Duration unit") {SelectValue()}
                        SelectContent {
                            SelectItem(value="1") {"Minutes"}
                            SelectItem(value="60") {"Hours"}
                            SelectItem(value="1440") {"Days"}
                        }
                    }
                    Button(variant=ButtonVariant::Outline,on:click=move |_|{
                        let minutes = duration.get_untracked().trim().parse::<u32>().ok()
                            .and_then(|amount| amount.checked_mul(duration_unit.get_untracked().parse::<u32>().unwrap_or(60)));
                        let Some(minutes) = minutes.filter(|m| (1..=525_600).contains(m)) else {
                            s.message.set("Enter a whole-number duration from 1 minute to 365 days".into());
                            return;
                        };
                        change(s, loaded, true, move |m|m.set_timed(minutes));
                    }) {"Share for this long"}
                }

                            }
                            text(class="section-title") {"Updates"}
                            row(class="view-toggle") {
                                Label {"Share battery level"}
                                Switch(a11y:label="Share battery level",checked=Binding::controlled(
                                    Signal::derive_local(move ||loaded.with(|l|l.as_ref().is_some_and(|l|l.share_battery))),
                                    move |enabled|change(s,loaded,false,move |m|m.set_share_battery(enabled))))
                            }
                            YouCard(confirm=confirm)
                        } else {
                        if move ||profile_open.get() {
                            ProfileCard(name=name)
                        } else {
                        if move ||groups_open.get() {
                            for group in move ||groups(),key=|g|g.clone() {
                                GroupItem(group=group.clone(),loaded=loaded,confirm=confirm,invite=begin_group,edit=edit)
                            }
                            if move ||groups().is_empty() {text(class="muted") {"No groups yet. A group shares locations among several friends at once: create one, then invite people."}}
                            text(class="section-title") {"Invited to a group?"}
                            row(class="create-row") {
                                Input(value=pasted,label="Join with link",placeholder="Paste group link",class="grow")
                                Button(variant=ButtonVariant::Outline,class="join-group-link",disabled=Signal::derive_local(move ||pasted.with(|l|l.trim().is_empty())),on:click=move |_|join()) {"Join"}
                            }
                            Dialog(open=new_group) {
                                DialogContent(class="sheet") {
                                    DialogHeader {DialogTitle {"New group"}}
                                    Input(value=group_name,label="New group name",placeholder="New group: Family…",class="wide")
                                text(class="section-title") {"Who can invite · members see"}
                                row(class="create-row") {
                                    Select(value=group_policy) {
                                        SelectTrigger(label="Who may invite",class="access-select") {SelectValue()}
                                        SelectContent {
                                            SelectItem(value="managed") {"Admins only"}
                                            SelectItem(value="open") {"Any member"}
                                        }
                                    }
                                    AccessSelect(current=Permission::Always,label="New group access".into(),pick=Callback::new(move |p|group_access.set(p)))
                                }
                                text(class="muted") {{move ||if group_policy.get()=="open" {"Any member may invite. Admins remove members."}else{"Only admins may invite or remove members."}}}
                                    Button(class="create-group wide",disabled=Signal::derive_local(move ||group_name.with(|n|n.trim().is_empty())),on:click=move |_|{
                                        let name=group_name.get_untracked().trim().to_owned();
                                        let me=name_value();
                                        if me.is_empty(){adding.set(true);s.message.set("Enter your name first; members see it.".into());return;}
                                        group_name.set(String::new());
                                        new_group.set(false);
                                        let policy=if group_policy.get_untracked()=="open" {Policy::Open}else{Policy::Managed};
                                        let access=group_access.get_untracked();
                                        change(s,loaded,false,move |m|m.friends().create_group(&sharing::key()?,&name,&me,policy,access).map(drop));
                                    }) {"Create group"}

                                }
                            }
                        } else {
                            column(class="share-card") {
                                // Whole row opens your profile: name and picture friends see.
                                Button(variant=ButtonVariant::Ghost,class="profile-entry",a11y:label="Your profile",on:click=move |_|profile_open.set(true)) {
                                    {friend_avatar("me".into(),name.get_untracked(),mode()!=Mode::Off)}
                                    column(class="grow") {
                                        text(class="friend-name") {{move ||{let n=name.get();if n.trim().is_empty(){"Set your name and picture".into()}else{n}}}}
                                        text(class="muted") {{move ||{let state=match mode(){Mode::Off=>"Sharing off",Mode::TripsOnly=>"Sharing during trips",Mode::Always=>"Sharing always"};timer_text().map_or(state.to_string(),|timer|format!("{state} · {timer}"))}}}
                                    }
                                    text(class="muted") {"Profile ›"}
                                }
                    row(class="segmented") {
                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::Off,class="share-off",on:click=move |_|set_mode(Mode::Off)) {"Off"}
                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::TripsOnly,class="share-trips",on:click=move |_|set_mode(Mode::TripsOnly)) {"During trips"}
                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::Always,class="share-always",on:click=move |_|set_mode(Mode::Always)) {"Always"}
                    }

                                Button(variant=ButtonVariant::Ghost,class="friends-nav",a11y:label="Sharing options",on:click=move |_|options_open.set(true)) {text {"Sharing options"} spacer() text {"›"}}
                            }
                            column(class="friends-nav-group") {
                                Button(variant=ButtonVariant::Ghost,class="friends-nav",a11y:label="Alert areas",on:click=move |_|{s.place_peer.set(None);places_open.set(true)}) {
                                    Icon(icon=super::icons::LAYERS)
                                    column(class="grow") {
                                        text(class="friend-name") {"Alert areas"}
                                        text(class="muted") {{move ||loaded.with(|l|l.as_ref().map(|l|l.alert_names.join(", ")).filter(|n|!n.is_empty()).unwrap_or_else(||"No alert areas".into()))}}
                                    }
                                    text(class="friend-badge") {{move ||loaded.with(|l|l.as_ref().map_or(0,|l|l.alert_names.len())).to_string()}}
                                    text {"›"}
                                }
                                Button(variant=ButtonVariant::Ghost,class="friends-nav",a11y:label="Groups",on:click=move |_|groups_open.set(true)) {
                                    Icon(icon=super::icons::PEOPLE)
                                    column(class="grow") {
                                        text(class="friend-name") {"Groups"}
                                        text(class="muted") {{move ||{let names=groups().into_iter().map(|g|g.name).collect::<Vec<_>>();if names.is_empty(){"No groups".into()}else{names.join(", ")}}}}
                                    }
                                    text(class="friend-badge") {{move ||groups().len().to_string()}}
                                    text {"›"}
                                }
                            }
                            text(class="section-title") {{move ||format!("Friends · {}",all_rows())}}
                            if move ||all_rows()>=4 {Input(value=search,label="Search friends",placeholder="Search friends")}
                            if move ||!groups().is_empty() {
                                scroll(class="group-filter") {
                                    row(class="friend-strip-row") {
                                        for option in move ||std::iter::once((None,"All".to_string())).chain(groups().into_iter().map(|g|(Some(g.id),g.name))).collect::<Vec<_>>(),key=|o|o.clone() {
                                            {{
                                                let (id,label)=option;
                                                let on=id.clone();
                                                view! {
                                                    Button(variant=ButtonVariant::Outline,class="area-button",class:seg-on=move ||group_filter.with(|g|*g==on),on:click=move |_|group_filter.set(id.clone())) {{label.clone()}}
                                                }
                                            }}
                                        }
                                    }
                                }
                            }
                            if move ||rows().is_empty() {text(class="muted") {{move ||if all_rows()==0 {"No friends yet. Tap Add friend to pair with someone nearby."} else {"No friend matches."}}}}
                            if move ||!rows().is_empty() {
                            column(class="friends-nav-group") {
                                for friend in move ||rows(),key=|r|(r.id.clone(),r.name.clone(),r.permission,r.group_permission,r.blocked,r.status.clone()) {
                                    {{
                                        let id=friend.id.clone();
                                        view! {
                                            Button(variant=ButtonVariant::Ghost,class="friends-nav",a11y:label=format!("Open {}",friend.name),on:click=move |_|detail.set(Some(id.clone()))) {
                                                {friend_avatar(friend.id.clone(),friend.name.clone(),friend.status.starts_with("Live"))}
                                                column(class="grow") {
                                                    text(class="friend-name") {{friend.name.clone()}}
                                                    text(class="muted") {{friend.status.clone()}}
                                                }
                                                text(class="friend-badge") {{if friend.blocked {"Blocked"} else {match friend.permission.max(friend.group_permission) {
                                                    Permission::Off=>"Hidden",Permission::TripsOnly=>"Sees trips",Permission::Always=>"Sees always"
                                                }}}}
                                                text {"›"}
                                            }
                                        }
                                    }}
                                }
                            }
                            }
                        } }
                        } } }
                    }
                }
            }
        }
        PlaceAlerts(open=places_open)
    }
}

/// Pairing failure in plain words; network detail kept short at the end.
fn pairing_error(error: &str) -> String {
    let network = [
        "handshake",
        "connect",
        "timed out",
        "timeout",
        "unreachable",
        "protocol",
    ]
    .iter()
    .any(|word| error.to_lowercase().contains(word));
    if network {
        "Couldn't reach your friend. Keep both phones online with Roamlog open, then try again."
            .into()
    } else {
        format!("Pairing failed: {error}")
    }
}

fn friend_avatar(id: String, name: String, live: bool) -> impl IntoView {
    let s = UiState::current();
    let initial = name
        .chars()
        .next()
        .unwrap_or('?')
        .to_uppercase()
        .to_string();
    let picture = Memo::new(move |_| {
        if id == "me" {
            s.own_avatar.get()
        } else {
            s.friend_markers.with(|markers| {
                markers
                    .iter()
                    .find(|friend| friend.id == id)
                    .and_then(|friend| friend.avatar.clone())
            })
        }
    });
    view! {
        row(class="friends-avatar",class:avatar-live=live) {
            if move ||picture.with(Option::is_some) {
                {zgui::elements::image().class("friend-avatar-image").src(Signal::derive_local(move ||picture.get())).into_view()}
            } else {
                text {{initial.clone()}}
            }
        }
    }
}

/// Compact access chooser: Hidden, Trips, Always.
#[component]
fn AccessSelect(current: Permission, label: String, pick: Callback<Permission>) -> impl IntoView {
    let value = RwSignal::new_local(permission_value(current).to_string());
    let previous = StoredValue::new_local(current);
    let chosen = zgui::reactive::RenderEffect::new(move |_| {
        let next = match value.get().as_str() {
            "trips" => Permission::TripsOnly,
            "always" => Permission::Always,
            _ => Permission::Off,
        };
        if next != previous.get_value() {
            previous.set_value(next);
            pick.run(next);
        }
    });
    on_cleanup_local(move || drop(chosen));
    view! {
        Select(value=value) {
            SelectTrigger(label=label,class="access-select") {SelectValue()}
            SelectContent {
                SelectItem(value="off") {"Hidden"}
                SelectItem(value="trips") {"Trips"}
                SelectItem(value="always") {"Always"}
            }
        }
    }
}

/// Two-tap destructive action shared by friends and groups: (id, action) awaiting confirm.
fn confirm_line(
    confirm: RwSignal<Option<String>, LocalStorage>,
    key: String,
    question: String,
    action: impl Fn() + Clone + 'static,
) -> impl IntoView {
    let shown = StoredValue::new_local(key);
    view! {
        if move ||shown.with_value(|k|confirm.get().as_deref()==Some(k.as_str())) {
            row(class="confirm-line") {
                text(class="grow") {{question.clone()}}
                Button(variant=ButtonVariant::Destructive,on:click={let action=action.clone();move |_|{confirm.set(None);action()}}) {"Yes"}
                Button(variant=ButtonVariant::Ghost,on:click=move |_|confirm.set(None)) {"No"}
            }
        }
    }
}

#[component]
fn FriendRow(
    friend: Row,
    loaded: RwSignal<Option<Loaded>, LocalStorage>,
    confirm: RwSignal<Option<String>, LocalStorage>,
    edit: Edit,
) -> impl IntoView {
    let s = UiState::current();
    let id = friend.id.clone();
    let save_id = id.clone();
    let pick = Callback::new({
        let id = id.clone();
        move |permission: Permission| {
            let id = id.clone();
            change(s, loaded, false, move |m| {
                m.friends().set_permission(id.parse()?, permission)
            })
        }
    });
    let note = if friend.blocked {
        String::new()
    } else if friend.group_permission > friend.permission {
        format!(
            "Sees {} through {}",
            permission_name(friend.group_permission).to_lowercase(),
            friend.groups
        )
    } else if !friend.groups.is_empty() {
        format!("In {}", friend.groups)
    } else {
        String::new()
    };
    let remove = {
        let id = id.clone();
        move || {
            let id = id.clone();
            change(s, loaded, false, move |m| m.friends().remove(id.parse()?))
        }
    };
    let block = {
        let id = id.clone();
        move || {
            let id = id.clone();
            change(s, loaded, false, move |m| m.friends().block(id.parse()?))
        }
    };
    let (menu_id, menu_name) = (id.clone(), friend.name.clone());
    let chosen = Callback::new(move |item: &'static str| match item {
        "map" => {
            let point = s.friend_markers.with_untracked(|markers| {
                markers.iter().find(|m| m.id == menu_id).map(|m| m.point)
            });
            match point {
                Some(point) => {
                    s.friends_open.set(false);
                    s.set_show_friends(true);
                    s.center_on(point);
                }
                None => s.message.set(format!("No location from {menu_name} yet")),
            }
        }
        "locate" => {
            let id = menu_id.clone();
            change(s, loaded, false, move |m| {
                if m.mode()? == Mode::Off {
                    return Err("Enable sharing connections first".into());
                }
                crate::sharing::requests::queue(
                    m.friends(),
                    id.parse()?,
                    crate::sharing::manager::now_ms(),
                )
            });
        }
        "trips" => s
            .friend_trips
            .set(Some((menu_id.clone(), menu_name.clone()))),
        "rename" => edit.start(&menu_id, &menu_name),
        "block" => confirm.set(Some(format!("block:{menu_id}"))),
        _ => confirm.set(Some(format!("remove:{menu_id}"))),
    });
    let access_label = format!("What {} sees", friend.name);
    let note = StoredValue::new_local(note);
    let alert_peer = StoredValue::new_local(id.clone());
    let alert_label = StoredValue::new_local(format!("Alert areas for {}", friend.name));
    let alert_names = move || {
        loaded.with(|loaded| {
            loaded
                .as_ref()
                .and_then(|data| {
                    alert_peer
                        .with_value(|id| data.alerts_by_friend.get(id))
                        .cloned()
                })
                .unwrap_or_default()
        })
    };
    let remove_question = if friend.groups.is_empty() {
        format!("Remove {}?", friend.name)
    } else if friend.blocked {
        format!(
            "Remove {} locally? This clears Block; shared-group grants apply again. Remove them from the group to end that access.",
            friend.name
        )
    } else {
        format!(
            "Remove {} locally? Shared-group access remains. Use group Members to remove them, or Block.",
            friend.name
        )
    };
    let (blocked, permission) = (friend.blocked, friend.permission);
    view! {
        column(class="friend-detail") {
            column(class="alert-card") {
                row(class="page-heading") {
                    {friend_avatar(friend.id.clone(),friend.name.clone(),friend.status.starts_with("Live"))}
                    text(class="muted") {{friend.status.clone()}}
                }
                if move ||!blocked {
                    row(class="friends-timer-presets") {
                        Button(variant=ButtonVariant::Outline,on:click=move |_|chosen.run("map")) {"Show on map"}
                        Button(variant=ButtonVariant::Outline,on:click=move |_|chosen.run("locate")) {"Try to locate"}
                        Button(variant=ButtonVariant::Outline,on:click=move |_|chosen.run("trips")) {"Their trips"}
                    }
                }
            }
            if move ||!blocked {
                text(class="section-title") {{access_label.clone()}}
                row(class="segmented") {
                    for choice in ||vec![(Permission::Off,"Hidden"),(Permission::TripsOnly,"During trips"),(Permission::Always,"Always")],key=|choice|choice.1 {
                        Button(variant=ButtonVariant::Ghost,class:seg-on=permission==choice.0,on:click=move |_|pick.run(choice.0)) {{choice.1}}
                    }
                }
                if move ||note.with_value(|n|!n.is_empty()) {
                    text(class="muted") {{move ||note.get_value()}}
                }
                row(class="page-heading") {
                    text(class="section-title grow") {{move ||alert_label.get_value()}}
                    Button(variant=ButtonVariant::Outline,class="add-alert",a11y:label="Add alert area for friend",on:click=move |_|{
                        s.place_peer.set(Some(alert_peer.get_value()));
                        s.places_open.set(true);
                    }) {"+ Add alert"}
                }
                if move ||!alert_names().is_empty() {
                    // Each area opens its own page; whole set can be viewed on the map.
                    column(class="friends-nav-group") {
                        for name in move ||alert_names(),key=|name|name.clone() {
                            Button(variant=ButtonVariant::Ghost,class="friends-nav",on:click=move |_|{
                                s.place_peer.set(Some(alert_peer.get_value()));
                                s.places_open.set(true);
                            }) {text(class="grow") {{name.clone()}} text {"›"}}
                        }
                    }
                    Button(variant=ButtonVariant::Outline,class="wide",on:click=move |_|super::place_alerts::show_on_map(s,Some(alert_peer.get_value()))) {"Show on map"}
                } else {
                    text(class="muted") {"No alerts yet. Add one to hear when they arrive or leave a place."}
                }
            }
            row(class="friends-timer-presets") {
                Button(variant=ButtonVariant::Outline,on:click=move |_|chosen.run("rename")) {"Rename"}
                if move ||!blocked {
                    Button(variant=ButtonVariant::Outline,class="danger-text",on:click=move |_|chosen.run("block")) {"Block…"}
                }
                Button(variant=ButtonVariant::Outline,class="danger-text",on:click=move |_|chosen.run("remove")) {"Remove…"}
            }
            {edit.field(friend.id.clone(), move |name| {let id=save_id.clone();change(s,loaded,false,move |m|m.friends().rename(id.parse()?,&name))})}
            {confirm_line(confirm,format!("block:{id}"),format!("Block {}? They stop seeing new updates and cannot pair again. Relay of your location stops for every group containing them. Existing copies cannot be recalled.",friend.name),block)}
            {confirm_line(confirm,format!("remove:{id}"),remove_question.clone(),remove)}
        }
    }
}

/// "⋯" menu of row actions; reports the chosen item's key.
#[component]
fn RowMenu(
    label: String,
    items: Vec<(&'static str, &'static str)>,
    chosen: Callback<&'static str>,
) -> impl IntoView {
    let items = StoredValue::new_local(items);
    view! {
        DropdownMenu {
            DropdownMenuTrigger(variant=ButtonVariant::Ghost,class="row-menu",a11y:label=label) {Icon(icon=ELLIPSIS)}
            DropdownMenuContent {
                for item in move ||items.get_value(),key=|i|i.0 {
                    MenuItem(on_select=UnsyncCallback::new(move |()|chosen.run(item.0))) {{item.1}}
                }
            }
        }
    }
}

#[component]
fn GroupItem(
    group: GroupRow,
    loaded: RwSignal<Option<Loaded>, LocalStorage>,
    confirm: RwSignal<Option<String>, LocalStorage>,
    invite: Callback<GroupRef>,
    edit: Edit,
) -> impl IntoView {
    let s = UiState::current();
    let id = group.id.clone();
    let save_id = id.clone();
    let field = edit.field(id.clone(), move |name| {
        let id = save_id.clone();
        change(s, loaded, false, move |m| {
            m.friends().rename_group(&id, &name)
        })
    });
    let pick = Callback::new({
        let id = id.clone();
        move |share: Permission| {
            let id = id.clone();
            change(s, loaded, false, move |m| {
                m.friends().set_group_permission(&id, share)
            })
        }
    });
    let leave = {
        let id = id.clone();
        move || {
            let id = id.clone();
            change(s, loaded, false, move |m| {
                m.friends().leave_group(&sharing::key()?, &id)
            })
        }
    };
    let reference = GroupRef {
        id: group.id.clone(),
        name: group.name.clone(),
    };
    let members_open = RwSignal::new_local(false);
    let sheet_group = StoredValue::new_local(group.clone());
    let (menu_id, menu_name) = (id.clone(), group.name.clone());
    let chosen = Callback::new(move |item: &'static str| match item {
        "invite" => invite.run(reference.clone()),
        "members" => members_open.set(true),
        "rename" => edit.start(&menu_id, &menu_name),
        _ => confirm.set(Some(format!("leave:{menu_id}"))),
    });
    let access_label = format!("What {} sees", group.name);
    let menu_label = format!("More for {}", group.name);
    let (leaving, share) = (group.leaving, group.share);
    let legacy = group.policy.is_none();
    let active = group.active;
    let admin = group.admin;
    let items = vec![("rename", "Rename"), ("leave", "Leave…")];
    let remove_id = id.clone();
    let remove = move || {
        let id = remove_id.clone();
        change(s, loaded, false, move |m| {
            m.friends().forget_inactive_group(&id)
        });
    };
    let member_line = format!(
        "{} {} · {}",
        group.details.len(),
        if group.details.len() == 1 {
            "member"
        } else {
            "members"
        },
        if admin { "Admin" } else { "Member" }
    );
    let members = if legacy {
        "Made with an older Roamlog and no longer shares. Remove it, then create a new group."
            .to_string()
    } else if !active && !leaving {
        "You were removed from this group, so it no longer shares. Remove it to tidy up."
            .to_string()
    } else if group.leaving {
        "Leaving…".to_string()
    } else if group.members.is_empty() {
        "Only you so far".to_string()
    } else {
        format!("You, {}", group.members.join(", "))
    };
    view! {
        column(class="alert-card") {
            row(class="friend-line") {
                column(class="friend-text") {
                    text(class="friend-name") {{group.name.clone()}}
                    if move ||active {
                        text(class="muted") {{member_line.clone()}}
                    }
                    text(class="muted small") {{if active {match group.policy {
                        Some(Policy::Managed)=>"Admins invite".into(),
                        Some(Policy::Open)=>"Any member can invite".into(),
                        None=>members.clone(),
                    }}else{members.clone()}}}
                }
                if move ||legacy || (!active && !leaving) {
                    Button(variant=ButtonVariant::Outline,class="danger-text",on:click={let remove=remove.clone();move |_|remove()}) {"Remove"}
                }
                if move ||!leaving && active && !legacy {
                    AccessSelect(current=share,label=access_label.clone(),pick=pick)
                    RowMenu(label=menu_label.clone(),items=items.clone(),chosen=chosen)
                }
            }
            if move ||active && !leaving {
                row(class="friends-timer-presets") {
                    for member in move ||sheet_group.with_value(|g|g.details.iter().take(6).cloned().collect::<Vec<_>>()),key=|member|member.id.clone() {
                        {friend_avatar(if member.me {"me".into()}else{member.id.clone()},member.name.clone(),false)}
                    }
                }
                row(class="actions") {
                    if move ||admin ||sheet_group.with_value(|g|g.policy==Some(Policy::Open)) {
                        Button(variant=ButtonVariant::Outline,on:click=move |_|chosen.run("invite")) {"Invite"}
                    }
                    Button(variant=ButtonVariant::Outline,on:click=move |_|members_open.set(true)) {"Members"}
                }
            }
            {if legacy {None}else{Some(field.into_view())}}
            {confirm_line(confirm,format!("leave:{id}"),format!("Leave {}? Access stops now; signed leave stays until delivered or 7 days.",group.name),leave)}
            Dialog(open=members_open) {
                DialogContent(class="sheet") {
                    DialogHeader {DialogTitle {{sheet_group.with_value(|g|format!("{} members",g.name))}} DialogDescription {"Signed membership. Admins control removals."}}
                    scroll(class="sheet-scroll") {
                        column {
                            for member in move ||sheet_group.with_value(|g|g.details.clone()),key=|m|m.clone() {
                                GroupMemberItem(member=member.clone(),group=sheet_group.with_value(|g|g.id.clone()),admin=admin,loaded=loaded,confirm=confirm)
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Your own profile as friends see it: picture, name, update interval, identity reset.
#[component]
fn GroupMemberItem(
    member: MemberRow,
    group: String,
    admin: bool,
    loaded: RwSignal<Option<Loaded>, LocalStorage>,
    confirm: RwSignal<Option<String>, LocalStorage>,
) -> impl IntoView {
    let s = UiState::current();
    let peer = member.id.clone();
    let key = format!("{group}:{peer}");
    let menu_key = key.clone();
    let chosen = Callback::new(move |action: &'static str| {
        confirm.set(Some(format!("{action}:{menu_key}")))
    });
    let mut items = vec![("block", "Block…")];
    if admin && !member.creator {
        items.push(("remove-member", "Remove…"));
        items.push(("cascade", "Remove + people they added…"));
    }
    if admin && !member.admin {
        items.push(("promote", "Make admin…"));
    }
    let action = move |kind: &'static str| {
        let group = group.clone();
        let peer = peer.clone();
        change(s, loaded, false, move |m| match kind {
            "block" => m.friends().block(peer.parse()?),
            "promote" => m
                .friends()
                .promote_member(&sharing::key()?, &group, peer.parse()?),
            _ => m.friends().remove_member(
                &sharing::key()?,
                &group,
                peer.parse()?,
                kind == "cascade",
            ),
        });
    };
    let remove = action.clone();
    let cascade = action.clone();
    let promote = action.clone();
    let block = action;
    let me = member.me;
    let menu_label = format!("Actions for {}", member.name);
    view! {
        column(class="friend-row") {
            row(class="friend-line") {
                column(class="friend-text") {
                    text(class="friend-name") {{format!("{}{}{}",member.name,if me {" (you)"}else{""},if member.admin {" · Admin"}else{""})}}
                    text(class="muted small") {{format!("Added by {}",member.added_by)}}
                }
                if move ||!me { RowMenu(label=menu_label.clone(),items=items.clone(),chosen=chosen) }
            }
            {confirm_line(confirm,format!("remove-member:{key}"),format!("Remove {}? Backdated admits can survive; use cascade to remove their additions.",member.name),move ||remove("remove-member"))}
            {confirm_line(confirm,format!("cascade:{key}"),format!("Remove {} and people they added?",member.name),move ||cascade("cascade"))}
            {confirm_line(confirm,format!("promote:{key}"),format!("Make {} admin? Admins may invite, promote, and remove members.",member.name),move ||promote("promote"))}
            {confirm_line(confirm,format!("block:{key}"),format!("Block {}? This turns off relay of your location for this group. Previously shared copies cannot be recalled.",member.name),move ||block("block"))}
        }
    }
}

#[component]
fn YouCard(confirm: RwSignal<Option<String>, LocalStorage>) -> impl IntoView {
    let s = UiState::current();
    let interval =
        RwSignal::new_local(sharing::policy(&s.directory.get_value()).minimum_interval_ms / 1000);
    let set_interval_s = move |seconds: i64| {
        interval.set(seconds);
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            let saved = blocking(move || {
                sharing::manager(&directory)
                    .and_then(|m| {
                        m.set_policy(UpdatePolicy {
                            minimum_interval_ms: seconds * 1000,
                            ..UpdatePolicy::default()
                        })
                    })
                    .map_err(|e| e.to_string())
            })
            .await;
            if let Err(error) = saved {
                s.message.set(format!("Sharing: {error}"));
            }
        });
    };
    let reset = move || {
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            match blocking(move || crate::platform::reset_identity(&directory)).await {
                Ok(()) => s
                    .message
                    .set("New sharing identity. Pair with friends again.".into()),
                Err(error) => s.message.set(error),
            }
        });
    };
    view! {
        text(class="muted") {"Send location every, while moving"}
        row(class="segmented") {
            for seconds in ||vec![15i64,30,60],key=|seconds|*seconds {
                Button(variant=ButtonVariant::Ghost,class:seg-on=move ||interval.get()==seconds,on:click=move |_|set_interval_s(seconds)) {{format!("{seconds} s")}}
            }
        }
        Button(variant=ButtonVariant::Outline,class="danger-text",on:click=move |_|confirm.set(Some("reset:me".into()))) {"Reset sharing identity…"}
        {confirm_line(confirm,"reset:me".to_string(),"Reset identity? All friends and groups are removed; everyone pairs again.".to_string(),reset)}
    }
}

/// Your name and picture as friends see them. A picked photo opens a square crop first.
#[component]
fn ProfileCard(name: RwSignal<String, LocalStorage>) -> impl IntoView {
    /// Crop frame side, px.
    const FRAME: f64 = 260.;
    let s = UiState::current();
    let draft = RwSignal::new_local(name.get_untracked());
    // Photo being cropped: full image, its shown copy, and size in px.
    let picked = StoredValue::new_local(None::<(image::DynamicImage, zgui_image::ImageBytes)>);
    let photo = RwSignal::new_local(None::<(String, [f64; 2])>);
    // Crop center in photo px, and zoom (1 = photo's short side fills frame).
    let center = RwSignal::new_local([0.; 2]);
    let zoom = RwSignal::new_local(1.);
    let drag = StoredValue::new_local(None::<(u64, [f64; 2])>);
    let busy = RwSignal::new_local(false);
    let scale_of = |[w, h]: [f64; 2], zoom: f64| FRAME / w.min(h) * zoom;
    // Keeps frame inside photo.
    let clamp = move |[x, y]: [f64; 2], size: [f64; 2], zoom: f64| {
        let half = FRAME / 2. / scale_of(size, zoom);
        [x.clamp(half, size[0] - half), y.clamp(half, size[1] - half)]
    };
    let size = move || photo.with(|p| p.as_ref().map_or([1.; 2], |p| p.1));
    let pick = move || {
        if let Err(error) = crate::platform::pick_avatar() {
            s.message.set(error);
        }
    };
    // Picker result arrives after UI remounts on return, so poll for it.
    let poll = set_interval(std::time::Duration::from_millis(300), move || {
        let result = crate::sharing::app::PICKED_AVATAR
            .lock()
            .ok()
            .and_then(|mut p| p.take());
        match result {
            Some(Ok(picked_image)) => {
                let size = [
                    f64::from(picked_image.image.width()),
                    f64::from(picked_image.image.height()),
                ];
                let shown = zgui_image::ImageBytes::new(picked_image.png);
                photo.set(Some((shown.url(), size)));
                picked.set_value(Some((picked_image.image, shown)));
                zoom.set(1.);
                center.set([size[0] / 2., size[1] / 2.]);
            }
            Some(Err(error)) => s.message.set(error),
            None => {}
        }
    });
    on_cleanup_local(move || drop(poll));
    let close_crop = move || {
        photo.set(None);
        picked.set_value(None);
    };
    let use_crop = move || {
        let Some(image) = picked.with_value(|p| p.as_ref().map(|p| p.0.clone())) else {
            return;
        };
        let half = FRAME / 2. / scale_of(size(), zoom.get_untracked());
        let [x, y] = center.get_untracked();
        let square = [
            (x - half).max(0.) as u32,
            (y - half).max(0.) as u32,
            (2. * half) as u32,
        ];
        let directory = s.directory.get_value();
        busy.set(true);
        spawn_local(async move {
            let saved =
                blocking(move || crate::sharing::app::save_avatar(&directory, &image, square))
                    .await;
            busy.set(false);
            match saved {
                Ok(()) => {
                    close_crop();
                    s.reload_own_avatar();
                    s.message
                        .set("Picture saved; friends see it within 30 minutes.".into());
                }
                Err(error) => s.message.set(error),
            }
        });
    };
    let save_name = move || {
        let value = draft.get_untracked().trim().to_owned();
        if !value.is_empty()
            && std::fs::write(s.directory.get_value().join("sharing-name"), &value).is_ok()
        {
            name.set(value);
            s.message.set("Name saved.".into());
        }
    };
    view! {
        if move ||photo.with(Option::is_some) {
            text(class="section-title") {"Crop picture"}
            text(class="muted") {"Drag to move · slide to zoom. Friends see the circle."}
            box(class="crop-frame",style:width=format!("{FRAME}px"),style:height=format!("{FRAME}px"),
                on:pointer_down=move |ev|{ev.capture_pointer();drag.set_value(Some((ev.id.raw(),[f64::from(ev.position.x.0),f64::from(ev.position.y.0)])));},
                on:pointer_move=move |ev|{
                    let at=[f64::from(ev.position.x.0),f64::from(ev.position.y.0)];
                    let Some(last)=drag.with_value(|d|d.filter(|d|d.0==ev.id.raw()).map(|d|d.1)) else {return;};
                    drag.set_value(Some((ev.id.raw(),at)));
                    let size=photo.with_untracked(|p|p.as_ref().map_or([1.;2],|p|p.1));
                    let zoom=zoom.get_untracked();
                    let k=scale_of(size,zoom);
                    center.update(|c|*c=clamp([c[0]-(at[0]-last[0])/k,c[1]-(at[1]-last[1])/k],size,zoom));
                },
                on:pointer_up=move |ev|{ev.release_pointer();drag.set_value(None);},
                on:pointer_cancel=move |ev|{ev.release_pointer();drag.set_value(None);}) {
                box(class="crop-image",
                    style:width=move ||Some(format!("{}px",size()[0]*scale_of(size(),zoom.get()))),
                    style:height=move ||Some(format!("{}px",size()[1]*scale_of(size(),zoom.get()))),
                    style:transform=move ||{
                        let k=scale_of(size(),zoom.get());
                        let [x,y]=center.get();
                        Some(format!("translate({}px, {}px)",FRAME/2.-x*k,FRAME/2.-y*k))
                    }) {
                    {zgui::elements::image().class("friend-avatar-image").src(Signal::derive_local(move ||photo.with(|p|p.as_ref().map(|p|p.0.clone())))).into_view()}
                }
                box(class="crop-ring") {}
            }
            // Padding keeps the thumb inside the scroll area at both ends.
            column(class="crop-zoom") {
            Slider(value=Binding::controlled(Signal::derive_local(move ||zoom.get()),move |z:f64|{
                    zoom.set(z);
                    let size=photo.with_untracked(|p|p.as_ref().map_or([1.;2],|p|p.1));
                    center.update(|c|*c=clamp(*c,size,z));
                }),min=1.0,max=4.0,step=0.05,label="Zoom")
            }
            row(class="actions") {
                Button(variant=ButtonVariant::Outline,on:click=move |_|close_crop()) {"Cancel"}
                Button(class="crop-save",disabled=Signal::derive_local(move ||busy.get()),on:click=move |_|use_crop()) {"Use picture"}
            }
        } else {
            column(class="profile-card") {
                row(class="profile-avatar") {
                    if move ||s.own_avatar.with(Option::is_some) {
                        {zgui::elements::image().class("friend-avatar-image").src(Signal::derive_local(move ||s.own_avatar.get())).alt(Some("Your picture".to_string())).into_view()}
                    } else {
                        text {{move ||name.with(|n|n.trim().chars().next().map(|c|c.to_uppercase().to_string()).unwrap_or_else(||"?".into()))}}
                    }
                }
                row(class="actions") {
                    Button(class="choose-picture",on:click=move |_|pick()) {{move ||if s.own_avatar.with(Option::is_some) {"Change picture"} else {"Choose picture"}}}
                    if move ||s.own_avatar.with(Option::is_some) {
                        Button(variant=ButtonVariant::Outline,class="no-picture",on:click=move |_|{
                            let _ = std::fs::remove_file(crate::sharing::transport::own_avatar_path(&s.directory.get_value().join("sharing")));
                            s.reload_own_avatar();
                        }) {"No picture"}
                    }
                }
            }
            text(class="section-title") {"Name"}
            row(class="create-row") {
                Input(value=draft,label="Your name",placeholder="Your name (friends see it)",class="grow")
                Button(class="save-name",disabled=Signal::derive_local(move ||draft.with(|d|d.trim().is_empty()||d.trim()==name.with(|n|n.trim().to_owned()))),on:click=move |_|save_name()) {"Save"}
            }
            text(class="muted") {"Friends see your name and picture on their map. Changes reach them within 30 minutes."}
        }
    }
}

/// One rename at a time across friends and groups.
#[derive(Clone, Copy)]
struct Edit {
    id: RwSignal<Option<String>, LocalStorage>,
    text: RwSignal<String, LocalStorage>,
}

impl Edit {
    fn start(self, id: &str, name: &str) {
        self.text.set(name.to_owned());
        self.id.set(Some(id.to_owned()));
    }

    /// Name input and Save, shown while `id` is being renamed.
    fn field(self, id: String, save: impl Fn(String) + Clone + 'static) -> impl IntoView {
        let shown = StoredValue::new_local(id);
        view! {
            if move ||shown.with_value(|id|self.id.get().as_deref()==Some(id.as_str())) {
                Input(value=self.text,label="New name",placeholder="Name")
                row(class="actions") {
                    Button(class="save-name",on:click={let save=save.clone();move |_|{let name=self.text.get_untracked().trim().to_owned();if !name.is_empty(){save(name)};self.id.set(None)}}) {"Save name"}
                    Button(variant=ButtonVariant::Outline,on:click=move |_|self.id.set(None)) {"Cancel"}
                }
            }
        }
    }
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct SharedTrip {
    #[serde(default)]
    name: String,
    started_ms: u64,
    finished_ms: Option<u64>,
    distance_m: f64,
    fixes: Vec<crate::model::Fix>,
}

#[derive(Clone, PartialEq, serde::Deserialize)]
struct TripsReply {
    #[serde(default)]
    trips: Vec<SharedTrip>,
    #[serde(default)]
    error: Option<String>,
}

/// Friend's finished trips, fetched by the sharing service; any can be copied into own history.
#[component]
pub fn FriendTrips() -> impl IntoView {
    let s = UiState::current();
    let open = RwSignal::new_local(false);
    let reply: RwSignal<Option<TripsReply>, LocalStorage> = RwSignal::new_local(None);
    let paths = move || {
        let (id, _) = s.friend_trips.get_untracked()?;
        let peer: iroh::EndpointId = id.parse().ok()?;
        let dir = s.directory.get_value().join("sharing");
        Some((
            crate::sharing::transport::trip_request_path(&dir, peer),
            crate::sharing::transport::friend_trips_path(&dir, peer),
        ))
    };
    // Request on open: drop old answer, create request file, wake service.
    let requested = zgui::reactive::RenderEffect::new(move |_| {
        let wanted = s.friend_trips.get().is_some();
        // Write only on change: a set notifies even when equal, and `closed` below
        // answers every notification by writing `friend_trips`, a loop at full CPU.
        if open.get_untracked() != wanted {
            open.set(wanted);
        }
        reply.set(None);
        if wanted && let Some((request, _)) = paths() {
            let created = request
                .parent()
                .map(std::fs::create_dir_all)
                .transpose()
                .and_then(|_| std::fs::write(&request, b""));
            if created.is_err() {
                s.message.set("Could not request trips".into());
            }
            crate::platform::nudge_sharing(&s.directory.get_value());
        }
    });
    on_cleanup_local(move || drop(requested));
    // Cached copy shows at once; a refreshed copy replaces it when it arrives.
    let poll = set_interval(Duration::from_secs(1), move || {
        if !open.get_untracked() {
            return;
        }
        if let Some((_, answer)) = paths()
            && let Ok(bytes) = std::fs::read(&answer)
            && let Ok(value) = serde_json::from_slice::<TripsReply>(&bytes)
            && reply.with_untracked(|old| old.as_ref() != Some(&value))
        {
            reply.set(Some(value));
        }
    });
    on_cleanup_local(move || drop(poll));
    let closed = zgui::reactive::RenderEffect::new(move |_| {
        if !open.get() && s.friend_trips.with_untracked(Option::is_some) {
            s.friend_trips.set(None);
        }
    });
    on_cleanup_local(move || drop(closed));
    let name = move || s.friend_trips.get().map(|f| f.1).unwrap_or_default();
    let trips = move || reply.with(|r| r.as_ref().map(|r| r.trips.clone()).unwrap_or_default());
    view! {
        Dialog(open=open) {
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {{move ||format!("{}'s trips",name())}} DialogDescription {"Add a trip to your history when you traveled together."}}
                scroll(class="sheet-scroll settings-scroll") {
                    column(class="tab-body") {
                        Button(class="view-friend-history",disabled=Signal::derive_local(move ||reply.with(|r|r.as_ref().is_none_or(|r|r.trips.is_empty()))),on:click=move |_|{
                            if let Some(friend)=s.friend_trips.get_untracked(){
                                s.set_people(vec![friend]);
                                open.set(false);
                                s.friends_open.set(false);
                            }
                        }) {"Show their trips on map"}
                        text(class="muted") {{move ||match reply.get(){
                            None=>"Asking friend's phone… needs both phones online with sharing on.".to_string(),
                            Some(r) if r.error.is_some()=>r.error.unwrap_or_default(),
                            Some(r) if r.trips.is_empty()=>"No finished trips shared.".to_string(),
                            Some(r)=>format!("{} trips",r.trips.len()),
                        }}}
                        for trip in move ||trips().into_iter().enumerate().collect::<Vec<_>>(),key=|t|(t.0,t.1.started_ms) {
                            SharedTripRow(trip=trip.1.clone(),friend=name())
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn SharedTripRow(trip: SharedTrip, friend: String) -> impl IntoView {
    let s = UiState::current();
    let title = if trip.name.is_empty() {
        format!("Trip on {}", super::tools::date(trip.started_ms))
    } else {
        trip.name.clone()
    };
    let minutes = trip
        .finished_ms
        .unwrap_or(trip.started_ms)
        .saturating_sub(trip.started_ms)
        / 60_000;
    let detail = format!(
        "{} {} · {} · {} min",
        super::tools::date(trip.started_ms),
        super::tools::clock(trip.started_ms),
        s.distance(trip.distance_m),
        minutes
    );
    let added = RwSignal::new_local(false);
    let import_name = format!("{title} (from {friend})");
    view! {
        column(class="friend-row") {
            text(class="friend-name") {{title.clone()}}
            text(class="muted") {{detail.clone()}}
            Button(variant=ButtonVariant::Outline,class="import-trip",disabled=Signal::derive_local(move ||added.get()),on:click={let fixes=trip.fixes.clone();let name=import_name.clone();move |_|{
                let command=serde_json::json!({"op":"import_trip","name":name,"fixes":fixes});
                added.set(true);
                zgui::reactive::spawn_detached(async move {
                    match s.request(command).await {
                        Ok(reply)=>{
                            if let (Some(trip_id),Some((peer,_)))=(reply["trip_id"].as_u64(),s.friend_trips.get_untracked()){
                                super::trip_sources::add(&s.directory.get_value(),trip_id,&peer);
                            }
                            s.message.set("Trip added to your history".into());
                            s.refresh(true);
                        }
                        Err(error)=>{added.set(false);s.message.set(error);}
                    }
                });
            }}) {{move ||if added.get(){"Added to my trips"}else{"Add to my trips"}}}
        }
    }
}

/// History filters for trips added from friends: one per friend, and one per group
/// covering trips added from any of its members. (value, label, trip IDs).
pub fn history_sources(directory: &std::path::Path) -> Vec<(String, String, Vec<u64>)> {
    let sources = super::trip_sources::load(directory);
    if sources.is_empty() {
        return Vec::new();
    }
    let Ok(manager) = sharing::manager(directory) else {
        return Vec::new();
    };
    let friends = manager.friends().list().unwrap_or_default();
    let groups = manager.friends().groups().unwrap_or_default();
    let trips_from = |peers: &[String]| -> Vec<u64> {
        sources
            .iter()
            .filter(|(_, peer)| peers.contains(peer))
            .map(|(trip, _)| *trip)
            .collect()
    };
    let mut out = Vec::new();
    for group in &groups {
        let members: Vec<String> = friends
            .iter()
            .filter(|f| f.groups.contains(&group.id))
            .map(|f| f.peer_id.to_string())
            .collect();
        let trips = trips_from(&members);
        if !trips.is_empty() {
            out.push((
                format!("group:{}", group.id),
                format!("Group: {}", group.name),
                trips,
            ));
        }
    }
    for friend in &friends {
        let trips = trips_from(&[friend.peer_id.to_string()]);
        if !trips.is_empty() {
            out.push((
                format!("friend:{}", friend.peer_id),
                format!("From {}", friend.display_name),
                trips,
            ));
        }
    }
    out
}

/// Groups with synced members: (name, members as (peer ID, name)).
pub fn groups_with_trips(directory: &std::path::Path) -> Vec<(String, Vec<(String, String)>)> {
    let Ok(manager) = sharing::manager(directory) else {
        return Vec::new();
    };
    let synced = people(directory);
    let friends = manager.friends().list().unwrap_or_default();
    manager
        .friends()
        .groups()
        .unwrap_or_default()
        .into_iter()
        .filter(|g| g.leaving_since_ms.is_none())
        .map(|g| {
            let members = synced
                .iter()
                .filter(|(peer, _)| {
                    friends
                        .iter()
                        .any(|f| &f.peer_id.to_string() == peer && f.groups.contains(&g.id))
                })
                .cloned()
                .collect::<Vec<_>>();
            (g.name, members)
        })
        .filter(|(_, members)| !members.is_empty())
        .collect()
}

/// Friends whose trips have synced: (peer ID, name).
pub fn people(directory: &std::path::Path) -> Vec<(String, String)> {
    let Ok(manager) = sharing::manager(directory) else {
        return Vec::new();
    };
    let dir = directory.join("sharing");
    manager
        .friends()
        .list()
        .unwrap_or_default()
        .into_iter()
        .filter(|f| {
            !f.blocked && crate::sharing::transport::friend_history_path(&dir, f.peer_id).exists()
        })
        .map(|f| (f.peer_id.to_string(), f.display_name))
        .collect()
}

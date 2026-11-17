//! Friends sheet: sharing mode, pairing by invitation, per-friend access, block and remove.
// view! needs `move ||` closures for reactive `if`/`for`, even around plain functions.
#![allow(clippy::redundant_closure)]
use super::state::{UiState, now_ms};
use crate::sharing::{
    android_app::{self as sharing, PairStep, Pairing},
    groups::Policy,
    location::UpdatePolicy,
    manager::Mode,
    node::{FriendState, read_published},
    pairing::GroupRef,
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
    set::{
        mark::{CROSS, PLUS},
        ui::ELLIPSIS,
    },
};

#[derive(Clone, PartialEq)]
struct Row {
    id: String,
    name: String,
    permission: Permission,
    /// Highest grant from this friend's groups, and their names.
    group_permission: Permission,
    groups: String,
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
    let rows = friends
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
    Ok(Loaded {
        mode: manager.mode().map_err(|e| e.to_string())?,
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
            crate::android::sharing_changed(directory)
        } else {
            crate::android::nudge_sharing(&directory);
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
            step.set(next);
            if finished {
                pairing.set_value(None);
                qr.set_value(None);
                qr_url.set(None);
                // Pairing endpoint closed; let service rebind its node.
                if let Err(error) = crate::android::sharing_changed(s.directory.get_value()) {
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
    let begin = move |link: Option<String>, group: Option<GroupRef>| {
        let display = name.get_untracked().trim().to_owned();
        if display.is_empty() {
            s.message
                .set("Enter your name first; friend sees it while pairing.".into());
            return;
        }
        let directory = s.directory.get_value();
        let _ = std::fs::write(directory.join("sharing-name"), &display);
        crate::android::suspend_sharing(&directory);
        join_access.set(Permission::TripsOnly);
        step.set(PairStep::Starting);
        pairing.set_value(Some(Pairing::start(directory, display, link, group)));
    };
    let cancel = move || {
        pairing.set_value(None);
        qr.set_value(None);
        qr_url.set(None);
        step.set(PairStep::Idle);
        if let Err(error) = crate::android::sharing_changed(s.directory.get_value()) {
            s.message.set(error);
        }
    };
    let set_mode = move |mode: Mode| change(s, loaded, true, move |m| m.set_mode(mode));
    let group_name = RwSignal::new_local(String::new());
    let group_policy = RwSignal::new_local("managed".to_string());
    let group_access = RwSignal::new_local(Permission::Always);
    let tab = RwSignal::new_local("people".to_string());
    // Pairing panel: open while adding a friend, joining from a link, or pairing.
    let adding = RwSignal::new_local(false);
    let invited = zgui::reactive::RenderEffect::new(move |_| {
        if s.invite_link.with(|l| !l.is_empty()) {
            tab.set("people".into());
            adding.set(true);
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
    let rows = move || {
        let wanted = search.with(|q| q.trim().to_lowercase());
        loaded.with(|l| {
            l.as_ref()
                .map(|l| {
                    l.rows
                        .iter()
                        .filter(|r| wanted.is_empty() || r.name.to_lowercase().contains(&wanted))
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
    view! {
        Dialog(open=s.friends_open) {
            DialogContent(class="sheet friends-sheet") {
                DialogHeader {DialogTitle {"Friends"} DialogDescription {"Encrypted phone-to-phone location sharing. No account or Roamlog server."}}
                column(class="share-card") {
                    YouCard(name=name,edit=edit,confirm=confirm)
                    text(class="section-title") {"Share my location"}
                    row(class="segmented") {
                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::Off,class="share-off",on:click=move |_|set_mode(Mode::Off)) {"Off"}
                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::TripsOnly,class="share-trips",on:click=move |_|set_mode(Mode::TripsOnly)) {"During trips"}
                        Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::Always,class="share-always",on:click=move |_|set_mode(Mode::Always)) {"Always"}
                    }
                    text(class="muted") {{move ||match mode(){
                        Mode::Off=>"No sharing connections. Pairing still works.",
                        Mode::TripsOnly=>"Sends only while recording a trip. Friends still show.",
                        Mode::Always=>"Sends in background; slower when you are still.",
                    }}}
                }
                if move ||adding.get()||!idle() {
                    column(class="pair-panel") {
                        row(class="pair-head") {
                            text(class="section-title grow") {"Add friend"}
                            Button(variant=ButtonVariant::Ghost,class="icon-button",on:click=move |_|close_panel(),a11y:label="Close add friend") {Icon(icon=CROSS)}
                        }
                        if move ||idle() {
                            Input(value=name,label="Your name",placeholder="Your name (friends see it)")
                            row(class="row-actions") {
                                Button(class="invite-friend",on:click=move |_|begin(None,None)) {"Show my QR"}
                                Button(variant=ButtonVariant::Outline,class="scan-invite",on:click=move |_|{if let Err(error)=crate::android::scan_invite(s.directory.get_value()){s.message.set(error)}}) {"Scan QR image"}
                            }
                            row(class="create-row") {
                                Input(value=s.invite_link,label="Invitation link",placeholder="Or paste roamlog://pair/… link",class="grow")
                                Button(variant=ButtonVariant::Outline,class="join-invite",disabled=Signal::derive_local(move ||s.invite_link.with(|l|l.trim().is_empty())),on:click=move |_|{let link=s.invite_link.get_untracked().trim().to_owned();s.invite_link.set(String::new());begin(Some(link),None)}) {"Pair"}
                            }
                        }
                        if move ||matches!(step.get(),PairStep::Starting) {
                            text(class="muted") {"Connecting… up to 30 seconds."}
                        }
                        if move ||matches!(step.get(),PairStep::Invite{..}) {
                            text(class="muted") {"Friend scans this with phone camera or opens shared link. Lasts 5 minutes."}
                            if move ||qr_url.get().is_some() {
                                box(class="qr-frame") {
                                    {zgui::elements::image().class("qr-image").src(Signal::derive_local(move ||qr_url.get())).alt(Some("Invitation QR code".to_string())).into_view()}
                                }
                            }
                            Button(variant=ButtonVariant::Outline,class="share-invite",on:click=move |_|{if let PairStep::Invite{link,..}=step.get_untracked(){crate::android::share_text(&link)}}) {"Share link"}
                        }
                        if move ||matches!(step.get(),PairStep::Confirm{..}) {
                            text(class="pair-name") {{move ||match step.get(){PairStep::Confirm{name,group:Some(group),joining:true,..}=>format!("Pair with {name} and join {group}?"),PairStep::Confirm{name,group:Some(group),..}=>format!("Pair with {name} and add them to {group}?"),PairStep::Confirm{name,..}=>format!("Pair with {name}?"),_=>String::new()}}}
                            text(class="muted") {{move ||match step.get(){PairStep::Confirm{group_details,..}=>group_details.unwrap_or_default(),_=>String::new()}}}
                            if move ||matches!(step.get(),PairStep::Confirm{group:Some(_),joining:true,..}) {
                                AccessSelect(current=Permission::TripsOnly,label="What group members see".into(),pick=Callback::new(move |p|join_access.set(p)))
                            }
                            text(class="muted") {"Code must match friend's screen exactly."}
                            text(class="pair-code") {{move ||if let PairStep::Confirm{code,..}=step.get(){code}else{String::new()}}}
                            row(class="row-actions") {
                                Button(class="confirm-pair",on:click=move |_|pairing.with_value(|p|if let Some(p)=p{p.vote(true,join_access.get_untracked())})) {"Codes match"}
                                Button(variant=ButtonVariant::Outline,class="reject-pair",on:click=move |_|pairing.with_value(|p|if let Some(p)=p{p.vote(false,join_access.get_untracked())})) {"Reject"}
                            }
                        }
                        if move ||matches!(step.get(),PairStep::Waiting) {
                            text(class="muted") {"Waiting for friend to confirm…"}
                        }
                        if move ||matches!(step.get(),PairStep::Done(_)|PairStep::Failed(_)) {
                            text(class="muted pair-result") {{move ||match step.get(){PairStep::Done(m)=>m,PairStep::Failed(e)=>format!("Pairing failed: {e}"),_=>String::new()}}}
                        }
                    }
                }
                Tabs(value=tab,class="history-tabs friends-tabs") {
                    TabsList(class="tabs-list") {
                        TabsTrigger(value="people") {{move ||format!("Friends ({})",all_rows())}}
                        TabsTrigger(value="groups") {{move ||format!("Groups ({})",groups().len())}}
                    }
                    TabsContent(value="people") {
                        scroll(class="sheet-scroll friends-scroll") {
                            column(class="tab-body list-body") {
                                if move ||!adding.get()&&idle() {
                                    Button(variant=ButtonVariant::Outline,class="add-friend",on:click=move |_|adding.set(true)) {
                                        Icon(icon=PLUS)
                                        text {"Add friend"}
                                    }
                                }
                                if move ||all_rows()>=4 {
                                    Input(value=search,label="Search friends",placeholder="Search friends")
                                }
                                if move ||all_rows()==0 {
                                    text(class="muted") {"No friends yet. New friends see nothing until you choose access."}
                                }
                                if move ||all_rows()>0&&rows().is_empty() {
                                    text(class="muted") {"No friend matches."}
                                }
                                for friend in move ||rows(),key=|r|(r.id.clone(),r.name.clone(),r.permission,r.group_permission,r.blocked,r.status.clone(),r.groups.clone()) {
                                    FriendRow(friend=friend.clone(),loaded=loaded,confirm=confirm,edit=edit)
                                }
                            }
                        }
                    }
                    TabsContent(value="groups") {
                        scroll(class="sheet-scroll friends-scroll") {
                            column(class="tab-body list-body") {
                                row(class="create-row") {
                                    Input(value=group_name,label="New group name",placeholder="New group: Family…",class="grow")
                                    Button(variant=ButtonVariant::Outline,class="create-group",disabled=Signal::derive_local(move ||group_name.with(|n|n.trim().is_empty())),on:click=move |_|{
                                        let name=group_name.get_untracked().trim().to_owned();
                                        let me=name_value();
                                        if me.is_empty(){adding.set(true);s.message.set("Enter your name first; members see it.".into());return;}
                                        group_name.set(String::new());
                                        let policy=if group_policy.get_untracked()=="open" {Policy::Open}else{Policy::Managed};
                                        let access=group_access.get_untracked();
                                        change(s,loaded,false,move |m|m.friends().create_group(&sharing::key()?,&name,&me,policy,access).map(drop));
                                    }) {"Create"}
                                }
                                row(class="create-row") {
                                    Select(value=group_policy) {
                                        SelectTrigger(label="Who may invite",class="access-select") {SelectValue()}
                                        SelectContent {
                                            SelectItem(value="managed") {"Managed"}
                                            SelectItem(value="open") {"Open"}
                                        }
                                    }
                                    AccessSelect(current=Permission::Always,label="New group access".into(),pick=Callback::new(move |p|group_access.set(p)))
                                }
                                text(class="muted") {{move ||if group_policy.get()=="open" {"Any member may invite. Admins remove members."}else{"Only admins may invite or remove members."}}}
                                for group in move ||groups(),key=|g|g.clone() {
                                    GroupItem(group=group.clone(),loaded=loaded,confirm=confirm,invite=begin_group,edit=edit)
                                }
                                text(class="muted") {"Members share with each other using the group's access, or a person's own if higher. Managed groups let admins invite; Open groups let any member invite."}
                            }
                        }
                    }
                }
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
                    s.center_on(point);
                }
                None => s.message.set(format!("No location from {menu_name} yet")),
            }
        }
        "trips" => s
            .friend_trips
            .set(Some((menu_id.clone(), menu_name.clone()))),
        "rename" => edit.start(&menu_id, &menu_name),
        "block" => confirm.set(Some(format!("block:{menu_id}"))),
        _ => confirm.set(Some(format!("remove:{menu_id}"))),
    });
    let items = if friend.blocked {
        vec![("rename", "Rename"), ("remove", "Remove…")]
    } else {
        vec![
            ("map", "Show on map"),
            ("trips", "Their trips"),
            ("rename", "Rename"),
            ("block", "Block…"),
            ("remove", "Remove…"),
        ]
    };
    let access_label = format!("What {} sees", friend.name);
    let menu_label = format!("More for {}", friend.name);
    let note = StoredValue::new_local(note);
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
        column(class="friend-row") {
            row(class="friend-line") {
                column(class="friend-text",a11y:label=format!("{}: {}",friend.name,friend.status)) {
                    text(class="friend-name") {{friend.name.clone()}}
                    text(class="muted small") {{friend.status.clone()}}
                    if move ||note.with_value(|n|!n.is_empty()) {
                        text(class="muted small") {{move ||note.get_value()}}
                    }
                }
                if move ||!blocked {
                    AccessSelect(current=permission,label=access_label.clone(),pick=pick)
                }
                RowMenu(label=menu_label,items=items,chosen=chosen)
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
            DropdownMenuTrigger(variant=ButtonVariant::Ghost,class="icon-button row-menu",a11y:label=label) {Icon(icon=ELLIPSIS)}
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
    let mut items = vec![
        ("members", "Members"),
        ("rename", "Rename"),
        ("leave", "Leave…"),
    ];
    if admin || group.policy == Some(Policy::Open) {
        items.insert(0, ("invite", "Invite someone"));
    }
    let remove_id = id.clone();
    let remove = move || {
        let id = remove_id.clone();
        change(s, loaded, false, move |m| {
            m.friends().forget_inactive_group(&id)
        });
    };
    let members = if legacy {
        "Recreate this group".to_string()
    } else if !active && !leaving {
        "Removed from group".to_string()
    } else if group.leaving {
        "Leaving…".to_string()
    } else if group.members.is_empty() {
        "Only you so far".to_string()
    } else {
        format!("You, {}", group.members.join(", "))
    };
    view! {
        column(class="friend-row") {
            row(class="friend-line") {
                column(class="friend-text") {
                    text(class="friend-name") {{group.name.clone()}}
                    text(class="muted small") {{members.clone()}}
                }
                if move ||legacy || (!active && !leaving) {
                    Button(variant=ButtonVariant::Destructive,on:click={let remove=remove.clone();move |_|remove()}) {"Remove"}
                }
                if move ||!leaving && active && !legacy {
                    AccessSelect(current=share,label=access_label.clone(),pick=pick)
                    RowMenu(label=menu_label.clone(),items=items.clone(),chosen=chosen)
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
fn YouCard(
    name: RwSignal<String, LocalStorage>,
    edit: Edit,
    confirm: RwSignal<Option<String>, LocalStorage>,
) -> impl IntoView {
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
    let pick = move || {
        if let Err(error) = crate::android::pick_avatar(s.directory.get_value()) {
            s.message.set(error);
        }
    };
    let chosen = Callback::new(move |item: &'static str| match item {
        "picture" => pick(),
        "no_picture" => {
            let _ = std::fs::remove_file(crate::sharing::transport::own_avatar_path(
                &s.directory.get_value().join("sharing"),
            ));
            s.reload_own_avatar();
        }
        "name" => edit.start("me", &name.get_untracked()),
        "15" => set_interval_s(15),
        "30" => set_interval_s(30),
        "60" => set_interval_s(60),
        _ => confirm.set(Some("reset:me".into())),
    });
    let reset = move || {
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            match blocking(move || crate::android::reset_identity(&directory)).await {
                Ok(()) => s
                    .message
                    .set("New sharing identity. Pair with friends again.".into()),
                Err(error) => s.message.set(error),
            }
        });
    };
    view! {
        row(class="friend-line you-row") {
            Button(variant=ButtonVariant::Ghost,class="you-avatar",on:click=move |_|pick(),a11y:label="Choose your picture") {
                if move ||s.own_avatar.get().is_some() {
                    box(class="friend-avatar settings-avatar") {
                        {zgui::elements::image().class("friend-avatar-image").src(Signal::derive_local(move ||s.own_avatar.get())).alt(Some("Your picture".to_string())).into_view()}
                    }
                }
                if move ||s.own_avatar.get().is_none() {
                    box(class="settings-avatar avatar-empty") {Icon(icon=PLUS)}
                }
            }
            column(class="friend-text") {
                text(class="friend-name") {{move ||{let n=name.get();if n.trim().is_empty(){"Set your name".to_string()}else{n}}}}
                text(class="muted small") {{move ||format!("Friends see this · updates every {} s while moving",interval.get())}}
            }
            RowMenu(label="Your profile and sharing options".to_string(),items=vec![("picture","Choose picture"),("no_picture","Remove picture"),("name","Change name"),("15","Update every 15 s"),("30","Update every 30 s"),("60","Update every 60 s"),("reset","Reset identity…")],chosen=chosen)
        }
        {edit.field("me".to_string(), move |value: String| {
            if std::fs::write(s.directory.get_value().join("sharing-name"), &value).is_ok() {
                name.set(value);
            }
        })}
        {confirm_line(confirm,"reset:me".to_string(),"Reset identity? All friends and groups are removed; everyone pairs again.".to_string(),reset)}
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
        open.set(wanted);
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
            crate::android::nudge_sharing(&s.directory.get_value());
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
        if !open.get() {
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

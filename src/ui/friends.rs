//! Friends sheet: sharing mode, pairing by invitation, per-friend access, block and remove.
// view! needs `move ||` closures for reactive `if`/`for`, even around plain functions.
#![allow(clippy::redundant_closure)]
use super::state::{UiState, now_ms};
use crate::sharing::{
    android_app::{self as sharing, PairStep, Pairing},
    location::UpdatePolicy,
    manager::Mode,
    node::{FriendState, read_published},
    pairing::GroupRef,
    peers::{Friend, Permission},
};
use std::path::PathBuf;
use std::time::Duration;
use zgui::prelude::*;
use zgui_image::ImageBytes;
use zgui_ui::prelude::*;

#[derive(Clone, PartialEq)]
struct Row {
    id: String,
    name: String,
    permission: Permission,
    blocked: bool,
    status: String,
    /// Names of groups sharing with this friend automatically.
    groups: String,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct GroupRow {
    id: String,
    name: String,
    members: usize,
    leaving: bool,
}

#[derive(Clone, PartialEq)]
struct Loaded {
    mode: Mode,
    interval_s: i64,
    rows: Vec<Row>,
    groups: Vec<GroupRow>,
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
                status: status(f, state, now),
                name: f.display_name.clone(),
                permission: f.permission,
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
        .map(|g| GroupRow {
            members: 1 + friends.iter().filter(|f| f.groups.contains(&g.id)).count(),
            leaving: g.leaving_since_ms.is_some(),
            id: g.id,
            name: g.name,
        })
        .collect();
    Ok(Loaded {
        mode: manager.mode().map_err(|e| e.to_string())?,
        interval_s: sharing::policy(&directory).minimum_interval_ms / 1000,
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
    let confirm_remove = RwSignal::new_local(None::<String>);
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
    let set_interval_s = move |seconds: i64| {
        change(s, loaded, false, move |m| {
            m.set_policy(UpdatePolicy {
                minimum_interval_ms: seconds * 1000,
                ..UpdatePolicy::default()
            })
        })
    };
    let group_name = RwSignal::new_local(String::new());
    let confirm_reset = RwSignal::new_local(false);
    let confirm_leave = RwSignal::new_local(None::<String>);
    let name_value = move || name.get_untracked().trim().to_owned();
    let begin_group = Callback::new(move |group: GroupRef| begin(None, Some(group)));
    let groups = move || loaded.with(|l| l.as_ref().map(|l| l.groups.clone()).unwrap_or_default());
    let mode = move || loaded.with(|l| l.as_ref().map_or(Mode::Off, |l| l.mode));
    let interval = move || loaded.with(|l| l.as_ref().map_or(30, |l| l.interval_s));
    let rows = move || loaded.with(|l| l.as_ref().map(|l| l.rows.clone()).unwrap_or_default());
    let idle = move || {
        matches!(
            step.get(),
            PairStep::Idle | PairStep::Done(_) | PairStep::Failed(_)
        )
    };
    view! {
        Dialog(open=s.friends_open) {
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Friends"} DialogDescription {"Encrypted phone-to-phone location sharing. No account or Roamlog server."}}
                scroll(class="sheet-scroll settings-scroll") {
                    column(class="tab-body") {
                        text(class="section-title") {"Share my location"}
                        row(class="segmented") {
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::Off,class="share-off",on:click=move |_|set_mode(Mode::Off)) {"Off"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::TripsOnly,class="share-trips",on:click=move |_|set_mode(Mode::TripsOnly)) {"During trips"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||mode()==Mode::Always,class="share-always",on:click=move |_|set_mode(Mode::Always)) {"Always"}
                        }
                        text(class="muted") {{move ||match mode(){
                            Mode::Off=>"Off: no sharing connections. Pairing still works.",
                            Mode::TripsOnly=>"Sends position only while recording a trip. Friends' positions still arrive; notification stays while on.",
                            Mode::Always=>"Sends position in background, slower when you are still. Uses some battery; Pause in notification stops it.",
                        }}}
                        text(class="section-title") {"Update while moving"}
                        row(class="segmented") {
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||interval()==15,on:click=move |_|set_interval_s(15)) {"15 s"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||interval()==30,on:click=move |_|set_interval_s(30)) {"30 s"}
                            Button(variant=ButtonVariant::Ghost,class:seg-on=move ||interval()==60,on:click=move |_|set_interval_s(60)) {"60 s"}
                        }
                        text(class="section-title") {"Add friend"}
                        Input(value=name,label="Your name",placeholder="Name friends see")
                        if move ||matches!(step.get(),PairStep::Starting) {
                            text(class="muted") {"Connecting… up to 30 seconds."}
                            Button(variant=ButtonVariant::Outline,on:click=move |_|cancel()) {"Cancel"}
                        }
                        if move ||matches!(step.get(),PairStep::Invite{..}) {
                            text(class="muted") {"Friend scans this with phone camera, or opens shared link. Invitation lasts 5 minutes."}
                            if move ||qr_url.get().is_some() {
                                box(class="qr-frame") {
                                    {zgui::elements::image().class("qr-image").src(Signal::derive_local(move ||qr_url.get())).alt(Some("Invitation QR code".to_string())).into_view()}
                                }
                            }
                            row(class="actions") {
                                Button(variant=ButtonVariant::Outline,class="share-invite",on:click=move |_|{if let PairStep::Invite{link,..}=step.get_untracked(){crate::android::share_text(&link)}}) {"Share link"}
                                Button(variant=ButtonVariant::Outline,on:click=move |_|cancel()) {"Cancel"}
                            }
                        }
                        if move ||matches!(step.get(),PairStep::Confirm{..}) {
                            text(class="pair-name") {{move ||match step.get(){PairStep::Confirm{name,group:Some(group),..}=>format!("Pair with {name} and join {group}?"),PairStep::Confirm{name,..}=>format!("Pair with {name}?"),_=>String::new()}}}
                            text(class="muted") {"Code must match friend's screen exactly. Compare in person or by call."}
                            text(class="pair-code") {{move ||if let PairStep::Confirm{code,..}=step.get(){code}else{String::new()}}}
                            row(class="actions") {
                                Button(class="confirm-pair",on:click=move |_|pairing.with_value(|p|if let Some(p)=p{p.vote(true)})) {"Codes match"}
                                Button(variant=ButtonVariant::Outline,class="reject-pair",on:click=move |_|pairing.with_value(|p|if let Some(p)=p{p.vote(false)})) {"Reject"}
                            }
                        }
                        if move ||matches!(step.get(),PairStep::Waiting) {
                            text(class="muted") {"Waiting for friend to confirm…"}
                            Button(variant=ButtonVariant::Outline,on:click=move |_|cancel()) {"Cancel"}
                        }
                        if move ||matches!(step.get(),PairStep::Done(_)|PairStep::Failed(_)) {
                            text(class="muted pair-result") {{move ||match step.get(){PairStep::Done(m)=>m,PairStep::Failed(e)=>format!("Pairing failed: {e}"),_=>String::new()}}}
                        }
                        if move ||idle() {
                            row(class="actions") {
                                Button(class="invite-friend",on:click=move |_|begin(None,None)) {"Show invitation"}
                                Button(variant=ButtonVariant::Outline,class="scan-invite",on:click=move |_|{if let Err(error)=crate::android::scan_invite(s.directory.get_value()){s.message.set(error)}}) {"Scan QR image"}
                            }
                            Input(value=s.invite_link,label="Invitation link",placeholder="roamlog://pair/…")
                            Button(variant=ButtonVariant::Outline,class="join-invite",disabled=Signal::derive_local(move ||s.invite_link.with(|l|l.trim().is_empty())),on:click=move |_|{let link=s.invite_link.get_untracked().trim().to_owned();s.invite_link.set(String::new());begin(Some(link),None)}) {"Pair from link"}
                        }
                        text(class="section-title") {"Groups"}
                        text(class="muted") {"Members of a group, like family, all share with each other automatically. Any member can invite others."}
                        for group in move ||groups(),key=|g|g.clone() {
                            GroupItem(group=group.clone(),loaded=loaded,confirm_leave=confirm_leave,invite=begin_group,edit=edit)
                        }
                        Input(value=group_name,label="New group name",placeholder="Family, Hiking crew…")
                        Button(variant=ButtonVariant::Outline,class="create-group",disabled=Signal::derive_local(move ||group_name.with(|n|n.trim().is_empty())),on:click=move |_|{
                            let name=group_name.get_untracked().trim().to_owned();
                            let me=name_value();
                            if me.is_empty(){s.message.set("Enter your name first; members see it.".into());return;}
                            group_name.set(String::new());
                            change(s,loaded,false,move |m|m.friends().create_group(&name,&me).map(drop));
                        }) {"Create group"}
                        text(class="section-title") {"Friends"}
                        if move ||rows().is_empty() {
                            text(class="muted") {"No friends yet. New friends see nothing until you choose access below."}
                        }
                        for friend in move ||rows(),key=|r|(r.id.clone(),r.name.clone(),r.permission,r.blocked,r.status.clone(),r.groups.clone()) {
                            FriendRow(friend=friend.clone(),loaded=loaded,confirm_remove=confirm_remove,edit=edit)
                        }
                        text(class="section-title") {"Identity"}
                        text(class="muted") {"Reset creates a new identity for this phone and removes all friends and groups; everyone must pair again. Use after a lost or possibly exposed key, or if identity cannot load."}
                        Button(variant=ButtonVariant::Outline,class="reset-identity",on:click=move |_|{
                            if !confirm_reset.get_untracked(){confirm_reset.set(true);return;}
                            confirm_reset.set(false);
                            let directory=s.directory.get_value();
                            zgui::reactive::spawn_detached(async move {
                                match blocking(move || crate::android::reset_identity(&directory)).await {
                                    Ok(())=>{s.message.set("New sharing identity. Pair with friends again.".into());reload();}
                                    Err(error)=>s.message.set(error),
                                }
                            });
                        }) {{move ||if confirm_reset.get(){"Confirm: reset identity"}else{"Reset sharing identity"}}}
                        text(class="muted") {"Access is enforced on this phone before anything is sent. Removing or blocking stops future updates; it cannot erase positions friend already received. Received positions stay only on this phone, for at most 24 hours."}
                    }
                }
            }
        }
    }
}

#[component]
fn FriendRow(
    friend: Row,
    loaded: RwSignal<Option<Loaded>, LocalStorage>,
    confirm_remove: RwSignal<Option<String>, LocalStorage>,
    edit: Edit,
) -> impl IntoView {
    let s = UiState::current();
    let rename_id = friend.id.clone();
    let rename_name = friend.name.clone();
    let trips_id = friend.id.clone();
    let trips_name = friend.name.clone();
    let save_id = friend.id.clone();
    let id = friend.id.clone();
    let permit = move |permission: Permission| {
        let id = id.clone();
        change(s, loaded, false, move |m| {
            m.friends().set_permission(id.parse()?, permission)
        })
    };
    let (p_off, p_trips, p_always) = (permit.clone(), permit.clone(), permit);
    let block_id = friend.id.clone();
    let remove_id = friend.id.clone();
    let confirm_id = friend.id.clone();
    let current = friend.permission;
    let grouped = !friend.groups.is_empty();
    let through = format!("Sees my location through {}", friend.groups);
    let label = format!("{} access", friend.name);
    view! {
        column(class="friend-row") {
            row(class="friend-head") {
                column(class="toggle-text",a11y:label=format!("{}: {}",friend.name,friend.status)) {
                    text(class="friend-name") {{friend.name.clone()}}
                    text(class="muted") {{friend.status.clone()}}
                }
            }
            if move ||!friend.blocked&&grouped {
                text(class="muted") {{through.clone()}}
            }
            if move ||!friend.blocked&&!grouped {
                text(class="muted") {"Can see my location"}
                row(class="segmented",a11y:label=label.clone()) {
                    Button(variant=ButtonVariant::Ghost,class:seg-on=current==Permission::Off,class="perm-off",on:click={let f=p_off.clone();move |_|f(Permission::Off)}) {"Never"}
                    Button(variant=ButtonVariant::Ghost,class:seg-on=current==Permission::TripsOnly,class="perm-trips",on:click={let f=p_trips.clone();move |_|f(Permission::TripsOnly)}) {"Trips"}
                    Button(variant=ButtonVariant::Ghost,class:seg-on=current==Permission::Always,class="perm-always",on:click={let f=p_always.clone();move |_|f(Permission::Always)}) {"Always"}
                }
            }
            {edit.field(friend.id.clone(), move |name| {let id=save_id.clone();change(s,loaded,false,move |m|m.friends().rename(id.parse()?,&name))})}
            row(class="actions") {
                Button(variant=ButtonVariant::Outline,class="rename-friend",on:click=move |_|edit.start(&rename_id,&rename_name)) {"Rename"}
                if move ||!friend.blocked {
                    Button(variant=ButtonVariant::Outline,class="friend-trips",on:click={let id=trips_id.clone();let name=trips_name.clone();move |_|s.friend_trips.set(Some((id.clone(),name.clone())))}) {"Trips"}
                }
            }
            row(class="actions") {
                if move ||!friend.blocked {
                    Button(variant=ButtonVariant::Outline,class="block-friend",on:click={let id=block_id.clone();move |_|{let id=id.clone();change(s,loaded,false,move |m|m.friends().block(id.parse()?))}}) {"Block"}
                }
                Button(variant=ButtonVariant::Outline,class="remove-friend",on:click={let id=remove_id.clone();move |_|{
                    if confirm_remove.get_untracked().as_deref()==Some(id.as_str()){
                        confirm_remove.set(None);
                        let id=id.clone();
                        change(s,loaded,false,move |m|m.friends().remove(id.parse()?));
                    }else{confirm_remove.set(Some(id.clone()))}
                }}) {{move ||if confirm_remove.get().as_deref()==Some(confirm_id.as_str()){"Confirm remove"}else{"Remove"}}}
            }
        }
    }
}

#[component]
fn GroupItem(
    group: GroupRow,
    loaded: RwSignal<Option<Loaded>, LocalStorage>,
    confirm_leave: RwSignal<Option<String>, LocalStorage>,
    invite: Callback<GroupRef>,
    edit: Edit,
) -> impl IntoView {
    let s = UiState::current();
    let rename_id = group.id.clone();
    let rename_name = group.name.clone();
    let save_id = group.id.clone();
    let field = edit.field(group.id.clone(), move |name| {
        let id = save_id.clone();
        change(s, loaded, false, move |m| {
            m.friends().rename_group(&id, &name)
        })
    });
    let id = group.id.clone();
    let confirm_id = StoredValue::new_local(group.id.clone());
    let leave_label = move || {
        if confirm_id.with_value(|id| confirm_leave.get().as_deref() == Some(id.as_str())) {
            "Confirm leave"
        } else {
            "Leave"
        }
    };
    let reference = GroupRef {
        id: group.id.clone(),
        name: group.name.clone(),
    };
    view! {
        column(class="friend-row") {
            text(class="friend-name") {{group.name.clone()}}
            {field}
            text(class="muted") {{if group.leaving{"Leaving…".to_string()}else{if group.members==1{"Only you so far".to_string()}else{format!("{} members including you",group.members)}}}}
            if move ||!group.leaving {
                Button(variant=ButtonVariant::Outline,class="rename-group",on:click={let id=rename_id.clone();let name=rename_name.clone();move |_|edit.start(&id,&name)}) {"Rename"}
                row(class="actions") {
                    Button(variant=ButtonVariant::Outline,class="invite-group",on:click={let r=reference.clone();move |_|invite.run(r.clone())}) {"Invite"}
                    Button(variant=ButtonVariant::Outline,class="leave-group",on:click={let id=id.clone();move |_|{
                        if confirm_leave.get_untracked().as_deref()==Some(id.as_str()){
                            confirm_leave.set(None);
                            let id=id.clone();
                            // Running node tells connected members first; otherwise forget now.
                            let directory=s.directory.get_value();
                            let immediately=sharing::mode(&directory)==Mode::Off;
                            change(s,loaded,false,move |m|m.friends().leave_group(&id,immediately));
                        }else{confirm_leave.set(Some(id.clone()))}
                    }}) {{leave_label}}
                }
            }
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
        open.set(wanted);
        reply.set(None);
        if wanted && let Some((request, answer)) = paths() {
            let _ = std::fs::remove_file(&answer);
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
    let poll = set_interval(Duration::from_secs(1), move || {
        if reply.with_untracked(Option::is_some) {
            return;
        }
        if let Some((_, answer)) = paths()
            && let Ok(bytes) = std::fs::read(&answer)
            && let Ok(value) = serde_json::from_slice::<TripsReply>(&bytes)
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
                        Ok(_)=>{s.message.set("Trip added to your history".into());s.refresh(true);}
                        Err(error)=>{added.set(false);s.message.set(error);}
                    }
                });
            }}) {{move ||if added.get(){"Added to my trips"}else{"Add to my trips"}}}
        }
    }
}

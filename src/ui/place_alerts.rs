//! User-selected arrival/departure subscriptions, stored only on this phone.
use super::state::UiState;
use crate::sharing::{
    android_app,
    places::{self, Place},
};
use zgui::prelude::*;
use zgui_ui::prelude::*;

#[component]
pub fn PlaceAlerts(open: RwSignal<bool, LocalStorage>) -> impl IntoView {
    let s = UiState::current();
    let saved = RwSignal::new_local(Vec::<Place>::new());
    let people = RwSignal::new_local(Vec::<(String, String)>::new());
    let name = RwSignal::new_local(String::new());
    let peer = RwSignal::new_local(String::new());
    let trigger = RwSignal::new_local("both".to_owned());
    let reload = move || {
        let directory = s.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || -> Result<_, String> {
                let manager = android_app::manager(&directory).map_err(|e| e.to_string())?;
                let people = manager
                    .friends()
                    .list()
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .filter(|f| !f.blocked)
                    .map(|f| (f.peer_id.to_string(), f.display_name))
                    .collect::<Vec<_>>();
                Ok((
                    places::list(manager.friends()).map_err(|e| e.to_string())?,
                    people,
                ))
            })
            .await;
            match result {
                Ok((list, friends)) => {
                    if peer.with_untracked(String::is_empty)
                        && let Some((id, _)) = friends.first()
                    {
                        peer.set(id.clone());
                    }
                    saved.set(list);
                    people.set(friends);
                }
                Err(e) => s.message.set(e),
            }
        });
    };
    let effect = zgui::reactive::RenderEffect::new(move |_| {
        if open.get() {
            reload();
        }
    });
    on_cleanup_local(move || drop(effect));
    let add = move || {
        let polygon = s.area_points.get_untracked();
        let trigger_value = match trigger.get_untracked().as_str() {
            "entering" => places::Trigger::Entering,
            "exiting" => places::Trigger::Exiting,
            _ => places::Trigger::Both,
        };
        let directory = s.directory.get_value();
        let name_value = name.get_untracked();
        let peer_value = peer.get_untracked();
        let [lon, lat] = crate::mapping::camera::geographic(s.camera.get_untracked().center);
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || -> crate::sharing::Result<()> {
                let manager = android_app::manager(&directory)?;
                places::add(
                    manager.friends(),
                    &name_value,
                    crate::geo::Point { lat, lon },
                    250.,
                    peer_value.parse()?,
                )?;
                let id = places::list(manager.friends())?
                    .last()
                    .ok_or("Place missing")?
                    .id;
                places::configure(manager.friends(), id, polygon, trigger_value)
            })
            .await;
            match result {
                Ok(()) => {
                    name.set(String::new());
                    reload();
                }
                Err(e) => s.message.set(e.to_string()),
            }
        });
    };
    view! {
        Dialog(open=open) {
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Place alerts"} DialogDescription {"Arrival and departure notifications on this phone. Places stay private."}}
                scroll(class="sheet-scroll") {
                    column(class="list-body") {
                        text(class="muted") {"Draw boundary using Draw place, then return here after Done. Alerts need fresh, direct location updates; first observation sets baseline."}
                        text(class="muted small") {{move ||{let [lon,lat]=crate::mapping::camera::geographic(s.camera.get().center);format!("Map center: {lat:.5}, {lon:.5}")}}}
                        Input(value=name,label="Place name",placeholder="Home, school, campsite…")
                        Select(value=peer) {
                            SelectTrigger(label="Person for place alert") {SelectValue()}
                            SelectContent {
                                for person in move ||people.get(),key=|p|p.0.clone() {
                                    SelectItem(value=person.0.clone()) {{person.1.clone()}}
                                }
                            }
                        }
                        Button(on:click=move |_|{open.set(false);s.friends_open.set(false);s.start_area();}) {"Draw place"}
                        text {{move ||format!("{} boundary corners",s.area_points.with(Vec::len))}}
                        Select(value=trigger) {
                            SelectTrigger(label="Alert when") {SelectValue()}
                            SelectContent {
                                SelectItem(value="entering") {"Entering"}
                                SelectItem(value="exiting") {"Exiting"}
                                SelectItem(value="both") {"Entering and exiting"}
                            }
                        }
                        Button(disabled=Signal::derive_local(move ||name.with(|n|n.trim().is_empty())||peer.with(String::is_empty)||s.area_points.with(Vec::len)<3),on:click=move |_|add()) {"Add drawn place"}
                        text(class="muted small") {"Keep sharing connections enabled and allow notifications. Confirmed arrivals/departures need two accurate updates; delayed or missing GPS can delay alerts."}
                        for place in move ||saved.get(),key=|p|p.id {
                            row(class="friend-line") {
                                column(class="grow") {
                                    text {{place.name.clone()}}
                                    text(class="muted small") {{format!("{} · {:?} · {} corners", people.with_untracked(|p|p.iter().find(|p|p.0==place.peer.to_string()).map(|p|p.1.clone()).unwrap_or_else(||"Removed friend".into())),place.trigger, place.polygon.len())}}
                                }
                                Button(variant=ButtonVariant::Outline,on:click=move |_|{
                                    let id=place.id;
                                    let directory=s.directory.get_value();
                                    zgui::reactive::spawn_detached(async move {
                                        let result=blocking(move ||{let manager=android_app::manager(&directory)?;places::remove(manager.friends(),id)}).await;
                                        if let Err(e)=result{s.message.set(e.to_string());}
                                        reload();
                                    });
                                },a11y:label=format!("Remove place alert {}",place.name)) {"Remove"}
                            }
                        }
                    }
                }
            }
        }
    }
}

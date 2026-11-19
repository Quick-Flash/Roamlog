use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Bottom map strip: areas shown on map, or the shown saved area's traveled share.
#[component]
pub fn AreaBar() -> impl IntoView {
    let s = UiState::current();
    let percent = move || {
        s.area_stats.with(|stats| {
            stats.as_ref().map_or_else(String::new, |st| {
                if st.total_m == 0.0 {
                    "No roads in offline map".into()
                } else {
                    format!(
                        "{:.0}% of roads traveled",
                        st.traveled_m / st.total_m * 100.0
                    )
                }
            })
        })
    };
    let alerts = move || s.area_marks.with(|m| m.first().is_some_and(|m| m.alert));
    let area_name = move || {
        s.data.with(|d| {
            d.areas
                .iter()
                .find(|a| Some(a.id) == s.area_id.get())
                .map(|a| a.name.clone())
                .unwrap_or_default()
        })
    };
    view! {
        // Friend card already explains areas drawn for a selected friend.
        if move ||!s.area_marks.with(Vec::is_empty)&&s.map_friend_selected.with(Option::is_none) {
            row(class="area-bar") {
                column(class="grow") {
                    text(class="area-count") {{move ||{
                        let n=s.area_marks.with(Vec::len);
                        let kind=if alerts() {"alert area"} else {"area"};
                        format!("{n} {kind}{} · tap a name to open",if n==1 {""} else {"s"})
                    }}}
                    if move ||alerts()&&!s.area_filters.with(Vec::is_empty) {
                        scroll(class="alert-filter area-filter") {
                            row(class="friend-strip-row") {
                                for option in move ||std::iter::once(("all".to_string(),"Everyone".to_string())).chain(s.area_filters.get()).collect::<Vec<_>>(),key=|o|o.clone() {
                                    {{
                                        let key=option.0.clone();
                                        let on_key=key.clone();
                                        view! {
                                            Button(variant=ButtonVariant::Outline,class="area-button",
                                                class:seg-on=move ||s.place_peer.with(|p|p.as_deref().unwrap_or("all")==on_key),
                                                on:click=move |_|{
                                                    #[cfg(target_os="android")]
                                                    crate::ui::place_alerts::show_on_map(s,(key!="all").then(||key.clone()));
                                                }) {{option.1.clone()}}
                                        }
                                    }}
                                }
                            }
                        }
                    }
                }
                Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.area_marks.set(Vec::new())) {"Hide"}
            }
        } else {
            if move ||s.area_busy.get() {
                row(class="area-bar") {text {"Calculating area roads…"}}
            } else {
                if move ||s.area_stats.with(Option::is_some) {
                    row(class="area-bar") {
                        column(class="grow") {
                            text(class="area-result-title") {{move ||format!("{} · {}",area_name(),percent())}}
                            box(class="stat-track") {
                                box(class="stat-bar",style:width=move ||s.area_stats.with(|stats|stats.as_ref().map(|stats|format!("{:.0}%",if stats.total_m>0. {100.*stats.traveled_m/stats.total_m}else{0.}))))
                            }
                        }
                        Button(variant=ButtonVariant::Outline,class="area-button edit-area",on:click=move |_|if let Some(id)=s.area_id.get_untracked() {s.edit_area(id)}) {"Edit area"}
                        Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.area_open.set(true)) {"Details"}
                        Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.cancel_area()) {"Hide"}
                    }
                }
            }
        }
    }
}

use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Bottom map strip for drawing an area, or the calculated percentage.
#[component]
pub fn AreaBar() -> impl IntoView {
    let s = UiState::current();
    let percent = move || {
        s.area_stats.with(|stats| {
            stats.as_ref().map_or_else(String::new, |st| {
                if st.total_m == 0.0 {
                    "Area · no roads in offline map".into()
                } else {
                    format!(
                        "Area · {:.0}% of roads traveled",
                        st.traveled_m / st.total_m * 100.0
                    )
                }
            })
        })
    };
    view! {
        // One bar at a time: picking a place center, then place boundary, then area tool.
        if move ||s.editing_circle.with(Option::is_some) {
            row(class="area-bar") {
                text(class="area-count") {{move ||s.editing_circle.with(|circle|circle.map(|(_,radius)|format!("{radius:.0} m · Drag center or edge")).unwrap_or_default())}}
                Button(class="area-button",on:click=move |_|s.places_open.set(true)) {"Done"}
            }
        } else { if move ||s.picking_place_center.get() {
            row(class="area-bar") {
                text(class="area-count") {"Pan to circle center"}
                spacer()
                Button(class="area-button",on:click=move |_|{
                    s.picking_place_center.set(false);
                    s.places_open.set(true);
                }) {"Use center"}
            }
        } else {
            if move ||!s.area_marks.with(Vec::is_empty) {
                row(class="area-bar") {
                    text(class="area-count grow") {{move ||{let n=s.area_marks.with(Vec::len);format!("{n} alert {} · tap a name to edit",if n==1 {"area"} else {"areas"})}}}
                    Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.area_marks.set(Vec::new())) {"Hide"}
                }
            } else {
            if move ||!s.place_outline.with(Vec::is_empty) {
                row(class="area-bar") {
                    text(class="area-count") {"Place boundary"}
                    spacer()
                    Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.places_open.set(true)) {"Alert areas"}
                    Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.place_outline.set(Vec::new())) {"Hide"}
                }
            } else {
                if move ||s.area_drawing.get() {
                    row(class="area-bar") {
                        text(class="area-count grow") {{move ||match s.area_points.with(Vec::len) {
                            0 => "Tap map to add corners".to_string(),
                            n @ (1 | 2) => format!("{n} of 3 corners"),
                            n => format!("{n} corners"),
                        }}}
                        spacer()
                        Button(variant=ButtonVariant::Outline,class="area-button",disabled=Signal::derive_local(move ||{s.area_points.track();s.area_history.with_value(Vec::is_empty)}),on:click=move |_|s.undo_area()) {"Undo"}
                        if move ||s.drawing_alert.get()&&s.alert_draw_back_to_circle.get() {
                            Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|{
                                if let Some(use_circle)=s.alert_use_circle.get_value() {use_circle.run(());}
                            }) {"Use circle"}
                        } else {
                            Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.cancel_area()) {"Cancel"}
                        }
                        Button(class="area-button finish-area",disabled=Signal::derive_local(move ||s.area_points.with(Vec::len)<3),on:click=move |_|s.finish_area()) {"Done"}
                    }
                } else {
                    if move ||s.area_busy.get() {
                        row(class="area-bar") {text {"Calculating area roads…"}}
                    } else {
                        if move ||s.area_stats.with(Option::is_some) {
                            row(class="area-bar") {
                                column(class="grow") {
                                    text(class="area-result-title") {{percent}}
                                    box(class="stat-track") {
                                        box(class="stat-bar",style:width=move ||s.area_stats.with(|stats|stats.as_ref().map(|stats|format!("{:.0}%",if stats.total_m>0. {100.*stats.traveled_m/stats.total_m}else{0.}))))
                                    }
                                }
                                spacer()
                                Button(variant=ButtonVariant::Outline,class="area-button edit-area",on:click=move |_|s.edit_area()) {"Edit corners"}
                                Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|{s.area_naming.set(false);s.area_open.set(true)}) {"Details"}
                                Button(class="area-button",on:click=move |_|{s.area_naming.set(true);s.area_open.set(true)}) {"Save"}
                            }
                        }
                    }
                }
            }
            }
        }
        }
    }
}

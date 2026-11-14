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
        if move ||s.area_drawing.get() {
            row(class="area-bar") {
                text(class="area-count") {{move ||format!("{} corners",s.area_points.with(Vec::len))}}
                spacer()
                Button(variant=ButtonVariant::Outline,class="area-button",disabled=Signal::derive_local(move ||{s.area_points.track();s.area_history.with_value(Vec::is_empty)}),on:click=move |_|s.undo_area()) {"Undo"}
                Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.cancel_area()) {"Cancel"}
                Button(class="area-button finish-area",disabled=Signal::derive_local(move ||s.area_points.with(Vec::len)<3),on:click=move |_|s.finish_area()) {"Done"}
            }
        } else {
            if move ||s.area_busy.get() {
                row(class="area-bar") {text {"Calculating area roads…"}}
            } else {
                if move ||s.area_stats.with(Option::is_some) {
                    row(class="area-bar") {
                        Button(variant=ButtonVariant::Ghost,class="area-result",on:click=move |_|s.area_open.set(true)) {{percent}}
                        spacer()
                        Button(variant=ButtonVariant::Outline,class="area-button edit-area",on:click=move |_|s.edit_area()) {"Edit"}
                        Button(variant=ButtonVariant::Outline,class="area-button",on:click=move |_|s.cancel_area(),a11y:label="Clear area") {"Clear"}
                    }
                }
            }
        }
    }
}

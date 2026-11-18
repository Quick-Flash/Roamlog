use super::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;
#[component]
pub fn UpdateNotice() -> impl IntoView {
    let s = UiState::current();
    view! {
        if move ||s.updates.available.with(Option::is_some) && !s.area_drawing.get() && !s.picking_place_center.get() && s.editing_circle.with(Option::is_none) && s.area_stats.with(Option::is_none) && s.place_outline.with(Vec::is_empty) && s.message.with(String::is_empty) && s.map_friend_selected.with(Option::is_none) {
            row(class="update-notice") {
                text {"Roamlog update available"}
                spacer()
                Button(variant=ButtonVariant::Outline,on:click=move |_|s.settings_open.set(true)) {"Review update"}
            }
        }
    }
}

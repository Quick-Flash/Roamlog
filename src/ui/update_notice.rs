use super::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;
#[component]
pub fn UpdateNotice() -> impl IntoView {
    let s = UiState::current();
    view! {
        if move ||s.updates.available.with(Option::is_some) {
            row(class="update-notice") {
                text {"Roamlog update available"}
                spacer()
                Button(variant=ButtonVariant::Outline,on:click=move |_|s.settings_open.set(true)) {"Review update"}
            }
        }
    }
}

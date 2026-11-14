use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

use super::trip_editor::TripEditorProps;

/// Edit dialog host: content rebuilds per trip so sliders match its points.
#[component]
pub fn TripEditorDialog() -> impl IntoView {
    let s = UiState::current();
    view! {
        Dialog(open=s.edit_open) {
            DialogContent(class="sheet") {
                for id in move ||s.editing.get().into_iter().collect::<Vec<_>>(),key=|id|*id {
                    TripEditor(id=id)
                }
            }
        }
    }
}

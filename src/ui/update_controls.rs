use super::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;
use zgui_ui_icons::{prelude::*, set::mark::CHECK};
#[component]
pub fn UpdateControls() -> impl IntoView {
    let s = UiState::current();
    let u = s.updates;
    view! {
        text(class="section-title") {"App updates"}
        text(class="muted") {{format!("Installed: {}",u.version())}}
        // Whole row is the tap target; the box alone is too small to hit reliably.
        Button(variant=ButtonVariant::Ghost,class="check-row",
            a11y:label=move ||format!("Check automatically while app runs: {}",if u.automatic.get(){"on"}else{"off"}),
            on:click=move |_|u.set_automatic(!u.automatic.get_untracked())) {
            box(class="check-square",class:check-square-on=move ||u.automatic.get()) {
                if move ||u.automatic.get() {
                    Icon(icon=CHECK,class="check-square-tick")
                }
            }
            text {"Check automatically while app runs"}
        }
        text(class="muted") {"Checks GitHub on launch and every 6 hours. Downloads only after you tap. Android asks before installing. Finish trip first."}
        Button(variant=ButtonVariant::Outline,class="check-updates",disabled=Signal::derive_local(move ||u.busy.get()||!cfg!(target_os="android")),on:click=move |_|u.check(s.now.get_untracked())) {"Check for updates"}
        text(class="muted") {{move ||u.message.get()}}
        for update in move ||u.available.get().into_iter().collect::<Vec<_>>(),key=|u|u.code {
            text {{format!("Build {} · {:.1} MB",update.code,update.size as f64/1048576.)}}
            text(class="muted") {{update.notes.chars().take(2000).collect::<String>()}}
            if move ||u.ready.with(Option::is_some) {
                Button(class="install-update",disabled=Signal::derive_local(move ||u.busy.get()||s.status().is_some()),on:click=move |_|u.install()) {"Install update"}
            } else {
                Button(class="download-update",disabled=Signal::derive_local(move ||u.busy.get()),on:click=move |_|u.download()) {"Download update"}
            }
            if move ||s.status().is_some() {text(class="muted") {"Finish current trip to enable installation."}}
        }
    }
}

use super::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;
#[component]
pub fn UpdateControls() -> impl IntoView {
    let s = UiState::current();
    let u = s.updates;
    let label = NodeRef::new();
    view! {
        text(class="section-title") {"App updates"}
        text(class="muted") {{format!("Installed: {}",u.version())}}
        row(class="filter-row") {
            Checkbox(labelled_by=label,checked=Binding::controlled(Signal::derive_local(move ||if u.automatic.get(){Checked::Yes}else{Checked::No}),move |value|{
                u.set_automatic(value==Checked::Yes);
            }))
            Label(node_ref=label) {"Check automatically while app runs"}
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

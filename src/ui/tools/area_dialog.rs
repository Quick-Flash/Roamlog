use crate::ui::state::UiState;
use zgui::prelude::*;
use zgui_ui::prelude::*;

use crate::area;

fn area_classes(s: UiState) -> Vec<area::ClassStats> {
    s.area_stats
        .with(|st| st.as_ref().map(|st| st.classes.clone()).unwrap_or_default())
}

/// Area result details and save.
#[component]
pub fn AreaDialog() -> impl IntoView {
    let s = UiState::current();
    let naming = s.area_naming;
    let summary = move || {
        s.area_stats.with(|st| {
            st.as_ref().map_or_else(String::new, |st| {
                if st.total_m == 0.0 {
                    return "No coverage roads inside. Download offline map for this area.".into();
                }
                format!(
                    "{:.0}% · {} of {} traveled · {}",
                    st.traveled_m / st.total_m * 100.0,
                    s.distance(st.traveled_m),
                    s.distance(st.total_m),
                    s.history_label()
                )
            })
        })
    };
    view! {
        Dialog(open=s.area_open) {
            DialogContent(class="sheet") {
                DialogHeader {DialogTitle {"Area roads"} DialogDescription {{summary}}}
                text(class="muted") {{move ||s.area_stats.with(|st|st.as_ref().map(|st|format!("Named streets: {} done, {} started, {} total",st.streets_done,st.streets_started,st.streets)).unwrap_or_default())}}
                scroll(class="area-scroll",class:map-hidden=move ||naming.get()) {
                    column(class="history-content") {
                        for class in move ||area_classes(s),key=|c|c.class.clone() {
                            row(class="history-row") {
                                text {{class.class.replace('_'," ")}}
                                spacer()
                                text(class="muted") {{format!("{:.0}% of {}",if class.total_m>0.0{class.traveled_m/class.total_m*100.0}else{0.0},s.distance(class.total_m))}}
                            }
                        }
                    }
                }
                text(class="muted") {"Red lines on map show untraveled roads inside. Each side of a divided road counts separately, so one direction covers half."}
                if move ||s.area_stats.with(|st|st.as_ref().is_some_and(|st|st.large)) {
                    text(class="muted") {"Large area: totals only. Draw area under about 20 km across to see untraveled roads in red."}
                }
                if move ||s.area_id.get().is_some() {
                    Button(class="update-area",on:click=move |_|s.update_area()) {{move ||s.data.with(|d|d.areas.iter().find(|a|Some(a.id)==s.area_id.get()).map_or_else(||"Update saved area".into(),|a|format!("Update “{}”",a.name)))}}
                }
                // Name field appears on request: focusing it first would cover results with keyboard.
                if move ||naming.get() {
                    row(class="category-row") {
                        Input(value=s.area_name,label="Area name",placeholder="Neighborhood, town…",class="category-select")
                        Button(class="save-area",disabled=Signal::derive_local(move ||s.area_name.with(|n|n.trim().is_empty())),on:click=move |_|{naming.set(false);s.save_area()}) {"Save"}
                    }
                } else {
                    Button(variant=ButtonVariant::Outline,class="name-area",on:click=move |_|naming.set(true)) {{move ||if s.area_id.get().is_some(){"Save as new area…"}else{"Save area…"}}}
                }
            }
        }
    }
}

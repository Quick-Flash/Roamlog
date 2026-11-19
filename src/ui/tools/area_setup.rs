use crate::{
    geo::{MAX_RADIUS, MIN_RADIUS, Point},
    ui::state::UiState,
};
use zgui::prelude::*;
use zgui_ui::prelude::*;

/// Small map of an area's outline, for list rows and area pages.
pub fn shape_preview(
    center: Point,
    outline: Vec<Point>,
    [width, height]: [f64; 2],
) -> impl IntoView {
    use crate::mapping::{
        basemap::Kind,
        camera::{Camera, project},
    };
    let s = UiState::current();
    let origin = project(center.lon, center.lat).unwrap_or_default();
    let raw: Vec<_> = outline
        .into_iter()
        .filter_map(|p| project(p.lon, p.lat))
        .map(|mut p| {
            let world = 2. * std::f64::consts::PI * 6_378_137.;
            p[0] = origin[0] + (p[0] - origin[0] + world / 2.).rem_euclid(world) - world / 2.;
            p
        })
        .collect();
    let (lo, hi) = raw.iter().fold(
        ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]),
        |(mut lo, mut hi), p| {
            for axis in 0..2 {
                lo[axis] = lo[axis].min(p[axis]);
                hi[axis] = hi[axis].max(p[axis]);
            }
            (lo, hi)
        },
    );
    let pad = (hi[0] - lo[0]).max(hi[1] - lo[1]).max(1.) * 0.18;
    let camera = Camera::from_ranges(
        plot::Ranges::new((lo[0] - pad, hi[0] + pad), (lo[1] - pad, hi[1] + pad)),
        [width, height],
    );
    let points: Vec<_> = camera
        .map(|camera| {
            raw.into_iter()
                .map(|[x, y]| {
                    [
                        width / 2. + (x - camera.center[0]) * camera.scale(),
                        height / 2. - (y - camera.center[1]) * camera.scale(),
                    ]
                })
                .collect()
        })
        .unwrap_or_default();
    let lines: Vec<_> = points
        .iter()
        .zip(points.iter().cycle().skip(1))
        .enumerate()
        .map(|(id, (a, b))| {
            let dx = b[0] - a[0];
            let dy = b[1] - a[1];
            (
                id,
                format!("{}px", dx.hypot(dy)),
                format!(
                    "translate({}px, {}px) rotate({}rad)",
                    a[0],
                    a[1],
                    dy.atan2(dx)
                ),
            )
        })
        .collect();
    view! {
        box(class="alert-shape",style:width=format!("{width}px"),style:height=format!("{height}px")) {
            for tile in move ||camera.map_or_else(Vec::new,|camera|
                s.basemap.visible(camera,[width,height],s.dark.get(),Kind::Map,1.,!s.map_offline.get())),key=|tile|tile.clone() {
                {{
                    let [x,y,scale]=camera.expect("tile requires camera").place(tile.0,[width,height]);
                    zgui::elements::image().class("map-vector-tile map-vector-image")
                        .style_property("transform",format!("translate({x}px, {y}px) scale({})",scale/2.))
                        .src(Some(tile.1.clone())).into_view()
                }}
            }
            for line in move ||lines.clone(),key=|line|line.0 {
                box(class="alert-shape-edge",style:width=line.1.clone(),style:transform=line.2.clone()) {}
            }
        }
    }
}

/// Circle radius on a log scale, so 10 m and 10 km are both easy to reach.
fn radius_slider(s: UiState) -> impl IntoView {
    let span = (MAX_RADIUS / MIN_RADIUS).ln();
    let to_position = move |r: f64| (r / MIN_RADIUS).ln() / span * 1000.;
    let to_radius = move |p: f64| {
        let r = MIN_RADIUS * (p / 1000. * span).exp();
        let step = if r < 100. {
            5.
        } else if r < 1000. {
            10.
        } else {
            50.
        };
        ((r / step).round() * step).clamp(MIN_RADIUS, MAX_RADIUS)
    };
    let radius = move || s.editing_circle.with(|c| c.map_or(MIN_RADIUS, |c| c.1));
    view! {
        Slider(value=Binding::controlled(Signal::derive_local(move ||to_position(radius())),move |p|
                s.editing_circle.update(|c|if let Some((_,r))=c {*r=to_radius(p)})),
            min=0.0,max=1000.0,step=1.0,label="Radius in meters")
    }
}

/// Full-screen map step for creating or editing any area: circle or drawn shape, and name.
#[component]
pub fn AreaSetup() -> impl IntoView {
    let s = UiState::current();
    let circle = move || s.editing_circle.with(Option::is_some);
    let run = move |done: bool| {
        if let Some(setup) = s.setup.get_untracked() {
            if done {
                (setup.finish)();
            } else {
                (setup.cancel)();
            }
        }
    };
    view! {
        if move ||s.setup.with(Option::is_some) {
            column(class="alert-create-header") {
                row(class="alert-create-title") {
                    text(class="list-title grow") {{move ||s.setup.with(|v|v.as_ref().map(|v|v.title).unwrap_or_default())}}
                    Button(variant=ButtonVariant::Secondary,class="setup-cancel",on:click=move |_|run(false)) {"Cancel"}
                }
                row(class="segmented setup-shape") {
                    Button(variant=ButtonVariant::Ghost,class="setup-circle",class:seg-on=circle,on:click=move |_|s.setup_use_circle()) {"Circle"}
                    Button(variant=ButtonVariant::Ghost,class="setup-draw",class:seg-on=move ||!circle(),on:click=move |_|s.setup_use_drawing()) {"Draw shape"}
                }
                row(class="alert-create-hint") {
                    text(class="muted") {{move ||if circle() {"Pan the map to place the center"} else {"Tap the map to add corners · drag one to move it"}}}
                }
            }
            column(class="alert-create-panel") {
                box(class="alert-create-grip") {}
                column(class="alert-create-field") {
                    text(class="section-title") {"Name"}
                    Input(value=s.area_name,label="Area name",placeholder="Home, park, town…")
                }
                // Same height for both shapes, so switching does not move the buttons.
                column(class="alert-create-field setup-size") {
                    if move ||s.editing_circle.with(Option::is_some) {
                        row(class="page-heading") {
                            text(class="section-title grow") {"Radius"}
                            text(class="list-title") {{move ||s.editing_circle.with(|c|c.map(|c|format!("{:.0} m",c.1)).unwrap_or_default())}}
                        }
                        {radius_slider(s)}
                    } else {
                        row(class="page-heading") {
                            text(class="section-title grow") {"Corners"}
                            text(class="list-title") {{move ||match s.area_points.with(Vec::len) {
                                n @ 0..=2 => format!("{n} of 3"),
                                n => n.to_string(),
                            }}}
                        }
                    }
                }
                if move ||!s.message.with(String::is_empty) {
                    text(class="muted",a11y:role=Role::Status) {{move ||s.message.get()}}
                }
                row(class="alert-create-actions") {
                    if move ||s.editing_circle.with(Option::is_some) {
                        Button(variant=ButtonVariant::Outline,disabled=Signal::derive_local(move ||s.position.with(Option::is_none)),on:click=move |_|{
                            if let Some((point,_))=s.position.get_untracked(){
                                s.center_on(point);
                                s.editing_circle.update(|c|if let Some((center,_))=c {*center=point});
                            }
                        }) {"Use my location"}
                    } else {
                        Button(variant=ButtonVariant::Outline,class="setup-undo",disabled=Signal::derive_local(move ||{s.area_points.track();s.area_history.with_value(Vec::is_empty)}),on:click=move |_|s.undo_area()) {"Undo"}
                    }
                    Button(class="alert-create-next setup-done",
                        disabled=Signal::derive_local(move ||s.area_name.with(|n|n.trim().is_empty())||(!circle()&&s.area_points.with(Vec::len)<3)),
                        on:click=move |_|run(true)) {{move ||s.setup.with(|v|v.as_ref().map(|v|v.done).unwrap_or_default())}}
                }
            }
        }
    }
}

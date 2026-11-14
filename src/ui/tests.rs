use super::*;
use super::{settings::SettingsProps, view_options::ViewOptionsProps};
use crate::{
    model::{Event, Fix, Status, TripFilter},
    store::Store,
};
use zgui_testkit_view::Window;

fn find(window: &Window, class: &str) -> NodeId {
    let mut nodes = vec![window.root];
    while let Some(node) = nodes.pop() {
        if window
            .dom
            .tree()
            .classes(node)
            .iter()
            .any(|c| c.as_str() == class)
        {
            return node;
        }
        nodes.extend(window.dom.tree().children(node));
    }
    panic!("Missing {class}");
}

#[test]
fn zgui_controls_follow_trip_state() {
    let directory = tempfile::tempdir().unwrap();
    let window = Window::open();
    let s = window.scope.with(|| {
        let s = UiState::new(directory.path().to_path_buf());
        provide_local_context(s);
        s
    });
    let mut mounted = window.scope.with(|| {
        view! {
            ThemeProvider(scheme=Signal::derive_local(move ||if s.dark.get(){ColorScheme::Dark}else{ColorScheme::Light})) {
                column {Recorder() ViewOptions() Settings()}
            }
        }
        .into_view()
        .build(&mut window.cx.cx())
    });
    mounted.mount(&window.dom_handle, window.root, None);
    window.frame();
    let start = find(&window, "start-trip");
    assert!(
        !window
            .dom
            .tree()
            .ui_state(start)
            .contains(zgui::vocab::UiState::DISABLED)
    );
    let mut store = Store::open(directory.path().join("test-trips.jsonl")).unwrap();
    store
        .commit(Event::Start {
            timestamp_ms: 0,
            category_id: None,
        })
        .unwrap();
    for (timestamp_ms, lon) in [(1000, 0.0), (2000, 0.0001)] {
        store
            .commit(Event::Fix {
                fix: Fix {
                    point: crate::geo::Point { lat: 0.0, lon },
                    timestamp_ms,
                    accuracy_m: 3.0,
                },
            })
            .unwrap();
    }
    s.data.set(store.state.clone());
    window.frame();
    assert_eq!(s.status(), Some(Status::Recording));
    assert!(
        window
            .dom
            .tree()
            .ui_state(start)
            .contains(zgui::vocab::UiState::DISABLED)
    );
    store.commit(Event::Pause { timestamp_ms: 3000 }).unwrap();
    s.data.set(store.state.clone());
    window.frame();
    assert!(
        !window
            .dom
            .tree()
            .ui_state(start)
            .contains(zgui::vocab::UiState::DISABLED)
    );
    let click = |class: &str| {
        window.dispatcher().send_to(
            find(&window, class),
            EventKind::Click,
            zgui::vocab::Payload::Pointer(zgui::vocab::PointerEvent::mouse(
                zgui::geom::Point::new(zgui::geom::CssPx(0.0), zgui::geom::CssPx(0.0)),
            )),
        );
        window.frame();
    };
    s.settings_open.set(true);
    window.frame();
    click("theme-light");
    assert!(!s.dark.get_untracked());
    s.settings_open.set(false);
    window.frame();
    click("map-chip");
    click("layer-satellite");
    assert!(s.satellite.get_untracked());
    assert!(s.show_gps.get_untracked());
    assert!(s.show_roads.get_untracked());
    click("show-gps");
    assert!(!s.show_gps.get_untracked());
    assert!(
        s.show_roads.get_untracked(),
        "roads-only keeps road overlay"
    );
    click("show-roads");
    assert!(!s.show_roads.get_untracked());
    // Let detached file writes finish before reopening preferences.
    for _ in 0..100 {
        window.frame();
        if std::fs::read_to_string(directory.path().join("theme")).is_ok_and(|v| v == "light")
            && std::fs::read_to_string(directory.path().join("map-layer"))
                .is_ok_and(|v| v == "satellite")
            && std::fs::read_to_string(directory.path().join("show-gps")).is_ok_and(|v| v == "off")
            && std::fs::read_to_string(directory.path().join("show-roads"))
                .is_ok_and(|v| v == "off")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let restored = window
        .scope
        .with(|| UiState::new(directory.path().to_path_buf()));
    assert!(!restored.dark.get_untracked());
    assert!(restored.satellite.get_untracked());
    assert!(!restored.show_gps.get_untracked());
    assert!(!restored.show_roads.get_untracked());
    click("show-gps");
    assert!(s.show_gps.get_untracked());
    assert!(!s.show_roads.get_untracked(), "GPS-only keeps roads hidden");
    // Keep backend I/O separate from component test; simulate refresh already running.
    s.busy.set(true);
    s.map_size.set_value([400.0, 400.0]);
    let recording_category = s.selected.get_untracked();
    s.show_trip(1);
    assert_eq!(
        s.history_filter.get_untracked(),
        TripFilter::Trip { trip_id: 1 }
    );
    assert!(!s.trips_open.get_untracked());
    assert!(
        !s.follow.get_untracked(),
        "reviewing trip must stop live camera following"
    );
    s.trips_open.set(true);
    window.frame();
    s.set_history_filter(TripFilter::All);
    assert_eq!(s.history_filter.get_untracked(), TripFilter::All);
    assert_eq!(
        s.selected.get_untracked(),
        recording_category,
        "history view cannot change recording category"
    );
}

#[test]
fn area_tool_collects_corners_and_inspects_roads() {
    use super::tools::{
        AreaBarProps, AreaDialogProps, AreasSheetProps, StatsTabProps, TripEditorDialogProps,
    };
    let directory = tempfile::tempdir().unwrap();
    let window = Window::open();
    let s = window.scope.with(|| {
        let s = UiState::new(directory.path().to_path_buf());
        provide_local_context(s);
        s
    });
    let mut mounted = window.scope.with(|| {
        view! {
            ThemeProvider(scheme=Signal::derive_local(move ||ColorScheme::Dark)) {
                column {AreaBar() AreaDialog() AreasSheet() StatsTab() TripEditorDialog()}
            }
        }
        .into_view()
        .build(&mut window.cx.cx())
    });
    mounted.mount(&window.dom_handle, window.root, None);
    window.frame();
    let p = |lat, lon| crate::geo::Point { lat, lon };
    s.start_area();
    window.frame();
    let done = find(&window, "finish-area");
    for (lat, lon) in [(0.0, 0.0), (0.0, 0.01), (0.01, 0.01)] {
        assert!(
            window
                .dom
                .tree()
                .ui_state(done)
                .contains(zgui::vocab::UiState::DISABLED)
        );
        s.tap(p(lat, lon), 1.0);
        window.frame();
    }
    assert_eq!(s.area_points.with_untracked(Vec::len), 3);
    assert!(
        !window
            .dom
            .tree()
            .ui_state(done)
            .contains(zgui::vocab::UiState::DISABLED)
    );
    // Fourth tap near bottom edge (0,0)-(0,0.01) splits that edge.
    s.tap(p(-0.001, 0.005), 1.0);
    assert_eq!(s.area_points.with_untracked(|p| p[1]), p(-0.001, 0.005));
    s.push_area_history();
    s.remove_corner(1);
    assert_eq!(s.area_points.with_untracked(Vec::len), 3);
    s.undo_area();
    s.undo_area();
    assert_eq!(s.area_points.with_untracked(Vec::len), 3);
    s.undo_area();
    assert_eq!(s.area_points.with_untracked(Vec::len), 2);
    s.cancel_area();
    // Not drawing: tap describes nearest traveled road.
    s.sections.set(vec![crate::coverage::Section {
        road_id: "residential:Elm Street".into(),
        start: p(0.0, 0.0),
        end: p(0.0, 0.0002),
        traveled: true,
        visits: 3,
        first_trip: None,
    }]);
    s.tap(p(0.00003, 0.0001), 1.0);
    assert_eq!(s.message.get_untracked(), "Elm Street · 3 trips");
    assert!(s.area_points.with_untracked(Vec::is_empty));
}

#[test]
fn terrain_toggle_preserves_flat_camera_and_recorded_history() {
    use crate::mapping::view::MapViewProps;
    let directory = tempfile::tempdir().unwrap();
    let window = Window::open();
    let s = window.scope.with(|| {
        let s = UiState::new(directory.path().to_path_buf());
        provide_local_context(s);
        s
    });
    let camera = crate::mapping::camera::Camera {
        center: [0., 0.],
        zoom: 12.,
    };
    s.camera.set(camera);
    s.bearing.set(0.5);
    let mut mounted = window.scope.with(|| {
        view! { ThemeProvider { MapView() } }
            .into_view()
            .build(&mut window.cx.cx())
    });
    mounted.mount(&window.dom_handle, window.root, None);
    window.frame();
    let compass = find(&window, "compass");
    let label = || {
        window
            .dom
            .tree()
            .semantics(compass)
            .and_then(|s| s.label)
            .unwrap_or_default()
    };
    let north = || {
        window.dispatcher().send_to(
            compass,
            EventKind::Click,
            zgui::vocab::Payload::Pointer(zgui::vocab::PointerEvent::mouse(
                zgui::geom::Point::new(zgui::geom::CssPx(0.), zgui::geom::CssPx(0.)),
            )),
        );
        window.frame();
    };
    assert!(label().contains("331"), "{}", label());
    north();
    assert_eq!(s.bearing.get_untracked(), 0.0);
    assert!(label().contains("0 degrees"), "{}", label());
    s.bearing.set(0.5);
    window.frame();
    let toggle = find(&window, "terrain-toggle");
    let click = || {
        window.dispatcher().send_to(
            toggle,
            EventKind::Click,
            zgui::vocab::Payload::Pointer(zgui::vocab::PointerEvent::mouse(
                zgui::geom::Point::new(zgui::geom::CssPx(0.), zgui::geom::CssPx(0.)),
            )),
        );
        window.frame();
    };
    click();
    find(&window, "terrain-view");
    assert!(label().contains("25 degrees"), "{}", label());
    let input = find(&window, "terrain-input");
    let pointer = |kind, x, y| {
        window.dispatcher().send_to(
            input,
            kind,
            zgui::vocab::Payload::Pointer(zgui::vocab::PointerEvent::mouse(
                zgui::geom::Point::new(zgui::geom::CssPx(x), zgui::geom::CssPx(y)),
            )),
        )
    };
    pointer(EventKind::PointerDown, 100., 100.);
    pointer(EventKind::PointerMove, 160., 130.);
    pointer(EventKind::PointerUp, 160., 130.);
    window.frame();
    assert!(label().contains("43 degrees"), "{}", label());
    north();
    assert!(label().contains("0 degrees"), "{}", label());
    assert_eq!(s.camera.get_untracked(), camera);
    click();
    assert_eq!(s.camera.get_untracked(), camera);
    assert_eq!(s.bearing.get_untracked(), 0.5);
    assert!(s.data.with_untracked(|d| d.trips.is_empty()));
    assert!(!directory.path().join("trips.jsonl").exists());
    mounted.unmount(&window.dom_handle);
}

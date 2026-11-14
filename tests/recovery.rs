use roamlog::{
    coverage::{Coverage, Road},
    geo::Point,
    model::{Event, Fix, State},
    recovery::Recovery,
    store::Store,
};

fn point(x: f64, y: f64) -> Point {
    Point {
        lon: x / 111_195.080,
        lat: y / 111_195.080,
    }
}
fn fix(x: f64, y: f64, t: u64, accuracy_m: f64) -> Fix {
    Fix {
        point: point(x, y),
        timestamp_ms: t,
        accuracy_m,
    }
}
fn road(id: &str, points: &[(f64, f64)]) -> Road {
    Road {
        half_width_m: 0.0,
        id: id.into(),
        name: id.into(),
        points: points.iter().map(|&(x, y)| point(x, y)).collect(),
    }
}
fn recorded(fixes: &[Fix]) -> State {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("trips.jsonl")).unwrap();
    store
        .commit(Event::Start {
            timestamp_ms: 0,
            category_id: None,
        })
        .unwrap();
    for &fix in fixes {
        store.commit(Event::Fix { fix }).unwrap();
    }
    store
        .commit(Event::Finish {
            timestamp_ms: fixes.last().unwrap().timestamp_ms + 1000,
        })
        .unwrap();
    store.state
}

#[test]
fn short_accuracy_dropout_recovers_curved_road_distance_and_coverage() {
    let roads = vec![road("bend", &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)])];
    let raw = recorded(&[
        fix(10.0, 0.0, 1000, 3.0),
        fix(500.0, 500.0, 2000, 150.0),
        fix(100.0, 90.0, 11000, 3.0),
    ]);
    assert_eq!(
        raw.trips[0].points.len(),
        2,
        "weak GPS is not inserted as real position"
    );
    assert!(raw.trips[0].points[1].estimated);
    let mut recovery = Recovery::new(&roads);
    let recovered = recovery.apply(&raw);
    let trip = &recovered.trips[0];
    assert!((trip.distance_m - 180.0).abs() < 0.1);
    assert!((trip.estimated_distance_m - 180.0).abs() < 0.1);
    assert!(
        trip.points.len() > 10,
        "curved route should include intervening road geometry"
    );
    assert!(
        trip.points
            .iter()
            .filter(|p| p.road_matched)
            .all(|p| p.estimated)
    );
    let mut coverage = Coverage::new(&roads);
    coverage.update(&raw);
    assert!(
        coverage.sections.iter().all(|s| !s.traveled),
        "raw inferred chord cannot claim guessed road"
    );
    coverage = Coverage::new(&roads);
    coverage.update(&recovered);
    assert!(coverage.sections.iter().filter(|s| s.traveled).count() >= 8);
    assert_eq!(
        recovery.apply(&raw),
        recovered,
        "cached reconstruction must not add distance twice"
    );
    assert!(
        (raw.trips[0].distance_m - 127.279).abs() < 0.1,
        "source state remains unchanged"
    );
    let export = roamlog::gpx::export(&recovered);
    assert!(export.contains("<traveler:estimated>true</traveler:estimated>"));
}

#[test]
fn equally_plausible_turns_do_not_claim_unknown_streets() {
    let roads = vec![
        road("start", &[(-20.0, 0.0), (0.0, 0.0)]),
        road("a", &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)]),
        road("b", &[(0.0, 0.0), (0.0, 100.0), (100.0, 100.0)]),
        road("end", &[(100.0, 100.0), (120.0, 100.0)]),
    ];
    let raw = recorded(&[
        fix(-10.0, 0.0, 1000, 3.0),
        fix(500.0, 500.0, 2000, 150.0),
        fix(110.0, 100.0, 21000, 3.0),
    ]);
    let recovered = Recovery::new(&roads).apply(&raw);
    assert_eq!(recovered.trips[0].points.len(), 2);
    assert_eq!(recovered.trips[0].distance_m, raw.trips[0].distance_m);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&recovered);
    assert!(coverage.sections.iter().all(|s| !s.traveled));
}

#[test]
fn parallel_roads_and_disconnected_network_stay_unresolved() {
    let raw = recorded(&[
        fix(0.0, 0.0, 1000, 3.0),
        fix(0.0, 0.0, 2000, 80.0),
        fix(100.0, 0.0, 11000, 3.0),
    ]);
    let roads = [
        road("left", &[(-20.0, -4.0), (120.0, -4.0)]),
        road("right", &[(-20.0, 4.0), (120.0, 4.0)]),
    ];
    assert!(
        Recovery::new(&roads).apply(&raw).trips[0]
            .points
            .iter()
            .all(|p| !p.road_matched)
    );
    let raw = recorded(&[
        fix(50.0, 0.0, 1000, 3.0),
        fix(0.0, 0.0, 2000, 80.0),
        fix(150.0, 100.0, 11000, 3.0),
    ]);
    let roads = [
        road("first", &[(0.0, 0.0), (100.0, 0.0)]),
        road("second", &[(100.0, 100.0), (200.0, 100.0)]),
    ];
    assert!(
        Recovery::new(&roads).apply(&raw).trips[0]
            .points
            .iter()
            .all(|p| !p.road_matched)
    );
}

#[test]
fn preserves_anchor_through_teleport_without_counting_spike_or_stationary_noise() {
    let raw = recorded(&[
        fix(0.0, 0.0, 1000, 3.0),
        fix(5000.0, 5000.0, 2000, 3.0),
        fix(20.0, 0.0, 3000, 3.0),
    ]);
    assert!((raw.trips[0].distance_m - 20.0).abs() < 0.1);
    assert_eq!(raw.trips[0].points.len(), 2);
    let stopped = recorded(&[
        fix(0.0, 0.0, 1000, 5.0),
        fix(500.0, 500.0, 2000, 100.0),
        fix(0.2, 0.2, 21000, 5.0),
    ]);
    assert_eq!(stopped.trips[0].distance_m, 0.0);
    assert_eq!(stopped.trips[0].points.len(), 1);
}

#[test]
fn pause_and_unbounded_loss_are_never_connected() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = Store::open(dir.path().join("trips.jsonl")).unwrap();
    for event in [
        Event::Start {
            timestamp_ms: 0,
            category_id: None,
        },
        Event::Fix {
            fix: fix(0.0, 0.0, 1000, 3.0),
        },
        Event::Pause { timestamp_ms: 2000 },
        Event::Resume { timestamp_ms: 3000 },
        Event::Fix {
            fix: fix(100.0, 0.0, 4000, 3.0),
        },
        Event::Finish { timestamp_ms: 5000 },
    ] {
        store.commit(event).unwrap();
    }
    let roads = [road("straight", &[(0.0, 0.0), (500.0, 0.0)])];
    assert_eq!(
        Recovery::new(&roads).apply(&store.state).trips[0].distance_m,
        0.0
    );
    let raw = recorded(&[fix(0.0, 0.0, 1000, 3.0), fix(100.0, 0.0, 201000, 3.0)]);
    assert_eq!(Recovery::new(&roads).apply(&raw).trips[0].distance_m, 0.0);
    assert!(!raw.trips[0].points[1].connected);
}

#[test]
fn fast_fixes_cover_short_map_edges_and_sequential_named_roads() {
    let raw = recorded(&[fix(0.0, 0.0, 1000, 3.0), fix(120.0, 0.0, 6000, 3.0)]);
    let first: Vec<_> = (0..=6).map(|i| (i as f64 * 10.0, 0.0)).collect();
    let second: Vec<_> = (6..=12).map(|i| (i as f64 * 10.0, 0.0)).collect();
    let roads = [road("first", &first), road("second", &second)];
    let mut coverage = Coverage::new(&roads);
    coverage.update(&raw);
    assert_eq!(coverage.sections.len(), 12);
    assert!(
        coverage.sections.iter().all(|s| s.traveled),
        "fast fix must cover all crossed map edges, including road-name boundary"
    );
}

#[test]
fn partial_parallel_ambiguity_keeps_unambiguous_parts_without_extending_trip() {
    let raw = recorded(&[fix(20.0, 0.0, 1000, 3.0), fix(140.0, 0.0, 6000, 3.0)]);
    let roads = [
        road("main", &[(0.0, 0.0), (200.0, 0.0)]),
        road("parallel", &[(60.0, 3.0), (100.0, 3.0)]),
    ];
    let mut coverage = Coverage::new(&roads);
    coverage.update(&raw);
    for s in &coverage.sections {
        let a = s.start.lon * 111_195.080;
        let b = s.end.lon * 111_195.080;
        let overlap = |lo: f64, hi: f64| (b.min(hi) - a.max(lo)).max(0.0);
        let supported = overlap(20.0, 60.0) + overlap(100.0, 140.0);
        let expected = s.road_id == "main" && supported / (b - a) >= 0.8 - 1e-5;
        assert_eq!(s.traveled, expected, "{} at {a:.1}..{b:.1} m", s.road_id);
    }
}

#[test]
fn ramp_ending_mid_edge_of_highway_stays_connected() {
    // Map tiles end ramp on highway line between highway vertices, 0.3 m off.
    let roads = [
        road("highway", &[(0.0, 0.0), (1000.0, 0.0)]),
        road("ramp", &[(100.0, 60.0), (300.0, 0.3)]),
    ];
    let raw = recorded(&[
        fix(150.0, 45.0, 1000, 3.0),
        fix(500.0, 500.0, 2000, 150.0),
        fix(500.0, 0.0, 11000, 3.0),
    ]);
    let recovered = Recovery::new(&roads).apply(&raw);
    assert!(recovered.trips[0].points.iter().any(|p| p.road_matched));
    let mut coverage = Coverage::new(&roads);
    coverage.update(&recovered);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "ramp" && s.traveled)
            .count()
            >= 6,
        "ramp merge must route onto highway"
    );
}

#[test]
fn inferred_route_credits_short_connecting_edge() {
    let roads = [road(
        "bend",
        &[(0.0, 0.0), (100.0, 0.0), (100.0, 2.0), (200.0, 2.0)],
    )];
    let raw = recorded(&[
        fix(10.0, 0.0, 1000, 3.0),
        fix(500.0, 500.0, 2000, 150.0),
        fix(190.0, 2.0, 11000, 3.0),
    ]);
    let recovered = Recovery::new(&roads).apply(&raw);
    assert!(recovered.trips[0].points.iter().any(|p| p.road_matched));
    let mut coverage = Coverage::new(&roads);
    coverage.update(&recovered);
    let short = coverage
        .sections
        .iter()
        .find(|s| s.start.distance(s.end) < 3.0)
        .unwrap();
    assert!(
        short.traveled,
        "real short graph connector must not leave hole in matched route"
    );
}

#[test]
fn highway_gap_snap_uses_heading_instead_of_slightly_closer_crossing_ramp() {
    let roads = [
        road("highway", &[(0.0, 1.0), (300.0, 1.0)]),
        road("crossing ramp", &[(100.0, -30.0), (100.0, 30.0)]),
    ];
    let raw = recorded(&[
        fix(80.0, 0.0, 1000, 3.0),
        fix(100.0, 0.0, 2000, 3.0),
        fix(500.0, 500.0, 3000, 150.0),
        fix(260.0, 0.0, 10000, 3.0),
    ]);
    let recovered = Recovery::new(&roads).apply(&raw);
    assert!(recovered.trips[0].points.iter().any(|p| p.road_matched));
    let mut coverage = Coverage::new(&roads);
    coverage.update(&recovered);
    assert!(
        coverage
            .sections
            .iter()
            .any(|s| s.road_id == "highway" && s.traveled)
    );
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "crossing ramp")
            .all(|s| !s.traveled)
    );
    assert!((recovered.trips[0].distance_m - raw.trips[0].distance_m).abs() < 0.01);
}

#[test]
fn established_road_continues_past_parallel_frontage_without_claiming_frontage() {
    let roads = [
        road("main", &[(0.0, 0.0), (160.0, 0.0)]),
        road("frontage", &[(60.0, 4.0), (160.0, 4.0)]),
    ];
    let fixes: Vec<_> = (0..=7)
        .map(|i| fix(i as f64 * 20.0, 0.0, (i + 1) * 1000, 3.0))
        .collect();
    let raw = recorded(&fixes);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&raw);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "main" && s.end.lon * 111_195.080 < 140.1)
            .all(|s| s.traveled)
    );
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "frontage")
            .all(|s| !s.traveled)
    );
    // Context cannot override new GPS clearly closer to other road.
    let changed = recorded(&[
        fix(0.0, 0.0, 1000, 3.0),
        fix(40.0, 0.0, 2000, 3.0),
        fix(80.0, 4.0, 3000, 3.0),
        fix(120.0, 4.0, 4000, 3.0),
    ]);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&changed);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "main" && s.start.lon * 111_195.080 > 80.1)
            .all(|s| !s.traveled)
    );
    let mut paused = recorded(&[
        fix(0.0, 0.0, 1000, 3.0),
        fix(20.0, 0.0, 2000, 3.0),
        fix(80.0, 0.0, 3000, 3.0),
        fix(120.0, 0.0, 4000, 3.0),
    ]);
    paused.trips[0].points[2].connected = false;
    let mut coverage = Coverage::new(&roads);
    coverage.update(&paused);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "main" && s.start.lon * 111_195.080 > 80.1)
            .all(|s| !s.traveled),
        "context must reset across disconnected path"
    );
}

#[test]
fn isolated_road_matches_within_reported_accuracy_but_rejects_farther_road() {
    let raw = recorded(&[fix(0.0, 18.0, 1000, 22.0), fix(100.0, 18.0, 6000, 22.0)]);
    let roads = [
        road("main", &[(0.0, 0.0), (100.0, 0.0)]),
        road("far", &[(0.0, -30.0), (100.0, -30.0)]),
    ];
    let mut coverage = Coverage::new(&roads);
    coverage.update(&raw);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "main")
            .all(|s| s.traveled)
    );
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "far")
            .all(|s| !s.traveled)
    );
}

#[test]
fn tiny_outage_fills_short_road_section_without_inventing_detour() {
    let roads = [road(
        "main",
        &[(0.0, 0.0), (20.0, 0.0), (26.0, 0.0), (100.0, 0.0)],
    )];
    let raw = recorded(&[
        fix(0.0, 0.0, 1000, 3.0),
        fix(20.0, 0.0, 2000, 3.0),
        fix(500.0, 500.0, 3000, 150.0),
        fix(26.0, 0.0, 4000, 3.0),
        fix(60.0, 0.0, 5000, 3.0),
    ]);
    let recovered = Recovery::new(&roads).apply(&raw);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&recovered);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.start.lon * 111_195.080 >= 19.9 && s.end.lon * 111_195.080 <= 26.1)
            .all(|s| s.traveled)
    );
    assert!((recovered.trips[0].distance_m - raw.trips[0].distance_m).abs() < 0.01);
    let detour = [road(
        "detour",
        &[(0.0, 0.0), (0.0, 40.0), (6.0, 40.0), (6.0, 0.0)],
    )];
    let raw = recorded(&[
        fix(0.0, 0.0, 1000, 3.0),
        fix(500.0, 500.0, 2000, 150.0),
        fix(6.0, 0.0, 11000, 3.0),
    ]);
    assert!(
        Recovery::new(&detour).apply(&raw).trips[0]
            .points
            .iter()
            .all(|p| !p.road_matched)
    );
}

#[test]
fn nearby_lane_split_keeps_shared_road_corridor() {
    let roads = [
        road("approach", &[(0.0, 0.0), (40.0, 0.0)]),
        road("lane a", &[(40.0, 0.0), (100.0, -2.0), (160.0, -2.0)]),
        road("lane b", &[(40.0, 0.0), (100.0, 2.0), (160.0, 2.0)]),
    ];
    let raw = recorded(&[
        fix(20.0, 0.0, 1000, 6.0),
        fix(500.0, 500.0, 2000, 150.0),
        fix(100.0, 0.0, 10000, 6.0),
    ]);
    let recovered = Recovery::new(&roads).apply(&raw);
    assert!(
        recovered.trips[0].points.iter().any(|p| p.road_matched),
        "lane ambiguity within GPS accuracy must not discard shared road"
    );
    // Same proximity without shared approach is still two unknown streets.
    let separate = [
        road("a", &[(0.0, -2.0), (160.0, -2.0)]),
        road("b", &[(0.0, 2.0), (160.0, 2.0)]),
    ];
    assert!(
        Recovery::new(&separate).apply(&raw).trips[0]
            .points
            .iter()
            .all(|p| !p.road_matched)
    );
}

#[test]
fn coverage_uses_matched_geometry_without_rejecting_it_as_noisy_gps() {
    let roads = [
        road("chosen", &[(0.0, 0.0), (100.0, 0.0)]),
        road("nearby", &[(0.0, 3.0), (100.0, 3.0)]),
    ];
    let mut state = recorded(&[fix(0.0, 0.0, 1000, 3.0), fix(100.0, 0.0, 6000, 3.0)]);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&state);
    assert!(
        coverage.sections.iter().all(|s| !s.traveled),
        "unmatched noisy observation remains ambiguous"
    );
    state.trips[0].points[1].estimated = true;
    state.trips[0].points[1].road_matched = true;
    let mut coverage = Coverage::new(&roads);
    coverage.update(&state);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "chosen")
            .all(|s| s.traveled)
    );
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "nearby")
            .all(|s| !s.traveled)
    );
}

#[test]
fn continuous_gps_uses_road_geometry_for_coverage_without_changing_gps_or_mileage() {
    let roads = [road("bend", &[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0)])];
    let raw = recorded(&[fix(10.0, 0.0, 1000, 3.0), fix(100.0, 90.0, 6000, 3.0)]);
    assert!(!raw.trips[0].points[1].estimated);
    let mut recovery = Recovery::new(&roads);
    let display = recovery.apply(&raw);
    assert_eq!(
        display, raw,
        "GPS display and observed distance must stay untouched"
    );
    let matched = recovery.coverage_state(&raw);
    assert_eq!(matched.trips[0].distance_m, raw.trips[0].distance_m);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&matched);
    assert!(
        coverage.sections.iter().filter(|s| s.traveled).count() >= 8,
        "road bend must stay continuous between ordinary fixes"
    );
    assert_eq!(
        recovery.apply(&raw),
        display,
        "road-matching cache cannot rewrite observed GPS"
    );
}

#[test]
fn continuous_road_matching_preserves_pauses_and_parallel_street_ambiguity() {
    let roads = [road("main", &[(0.0, 0.0), (300.0, 0.0)])];
    let mut raw = recorded(&[
        fix(0.0, 0.0, 1000, 3.0),
        fix(20.0, 0.0, 2000, 3.0),
        fix(220.0, 0.0, 5000, 3.0),
        fix(240.0, 0.0, 6000, 3.0),
    ]);
    raw.trips[0].points[2].connected = false;
    let matched = Recovery::new(&roads).coverage_state(&raw);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&matched);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.start.lon * 111_195.080 > 25.0 && s.end.lon * 111_195.080 < 215.0)
            .all(|s| !s.traveled)
    );
    let roads = [
        road("a", &[(0.0, -4.0), (120.0, -4.0)]),
        road("b", &[(0.0, 4.0), (120.0, 4.0)]),
    ];
    let raw = recorded(&[fix(10.0, 0.0, 1000, 3.0), fix(100.0, 0.0, 6000, 3.0)]);
    let matched = Recovery::new(&roads).coverage_state(&raw);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&matched);
    assert!(coverage.sections.iter().all(|s| !s.traveled));
}

#[test]
fn consecutive_fixes_keep_continuous_lane_anchor_through_gps_sideways_jitter() {
    let roads = [
        road("approach", &[(0.0, 0.0), (40.0, 0.0)]),
        road("a", &[(40.0, 0.0), (60.0, -3.0), (160.0, -3.0)]),
        road("b", &[(40.0, 0.0), (60.0, 3.0), (160.0, 3.0)]),
    ];
    let raw = recorded(&[
        fix(20.0, 0.0, 1000, 4.0),
        fix(60.0, -3.0, 2000, 4.0),
        fix(80.0, 0.8, 3000, 4.0),
        fix(100.0, -0.8, 4000, 4.0),
        fix(140.0, 0.8, 5000, 4.0),
    ]);
    let mut recovery = Recovery::new(&roads);
    let display = recovery.apply(&raw);
    assert_eq!(display, raw);
    let matched = recovery.coverage_state(&raw);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&matched);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "a"
                && s.start.lon * 111_195.080 >= 59.9
                && s.end.lon * 111_195.080 <= 140.1)
            .all(|s| s.traveled),
        "matching must carry chosen road between fixes, not alternate lanes"
    );
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "b")
            .all(|s| !s.traveled)
    );
    assert_eq!(recovery.apply(&raw), display);
}

#[test]
fn ordinary_match_cannot_turn_onto_crossing_street_because_gps_is_offset() {
    let roads = [
        road("main", &[(0.0, 0.0), (100.0, 0.0), (200.0, 0.0)]),
        road("crossing", &[(100.0, -30.0), (100.0, 0.0), (100.0, 30.0)]),
    ];
    let raw = recorded(&[fix(80.0, 8.0, 1000, 10.0), fix(100.0, 8.0, 2000, 10.0)]);
    let matched = Recovery::new(&roads).coverage_state(&raw);
    assert!(
        matched.trips[0]
            .points
            .iter()
            .filter(|p| p.road_matched)
            .all(|p| p.fix.point.lat.abs() < 1e-9),
        "GPS heading must keep matched geometry on main road, not nearer crossing street"
    );
    assert!(matched.trips[0].points.iter().any(|p| p.road_matched));
}

#[test]
fn wide_arterial_matches_off_center_riding_instead_of_nearby_narrow_way() {
    let mut main = road("arterial", &[(0.0, 0.0), (200.0, 0.0)]);
    main.half_width_m = 8.0;
    let mut narrow = road("narrow", &[(0.0, 24.8), (200.0, 24.8)]);
    narrow.half_width_m = 1.5;
    let roads = [main, narrow];
    let raw = recorded(&[fix(20.0, 12.5, 1000, 5.0), fix(120.0, 12.5, 6000, 5.0)]);
    let mut recovery = Recovery::new(&roads);
    assert_eq!(recovery.apply(&raw), raw);
    let matched = recovery.coverage_state(&raw);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&matched);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "arterial"
                && s.start.lon * 111_195.080 >= 19.9
                && s.end.lon * 111_195.080 <= 120.1)
            .all(|s| s.traveled)
    );
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "narrow")
            .all(|s| !s.traveled)
    );
}

#[test]
fn several_consistent_fixes_resolve_parallel_street_without_guessing_equal_evidence() {
    let roads = [
        road("a", &[(0.0, 0.0), (300.0, 0.0)]),
        road("b", &[(0.0, 4.0), (300.0, 4.0)]),
    ];
    let fixes: Vec<_> = (0..=10)
        .map(|i| fix(i as f64 * 20.0, 0.0, (i + 1) * 1000, 3.0))
        .collect();
    let raw = recorded(&fixes);
    let matched = Recovery::new(&roads).coverage_state(&raw);
    let mut coverage = Coverage::new(&roads);
    coverage.update(&matched);
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "a" && s.end.lon * 111_195.080 < 199.9)
            .all(|s| s.traveled)
    );
    assert!(
        coverage
            .sections
            .iter()
            .filter(|s| s.road_id == "b")
            .all(|s| !s.traveled)
    );
    let equal: Vec<_> = (0..=10)
        .map(|i| fix(i as f64 * 20.0, 2.0, (i + 1) * 1000, 3.0))
        .collect();
    let matched = Recovery::new(&roads).coverage_state(&recorded(&equal));
    let mut coverage = Coverage::new(&roads);
    coverage.update(&matched);
    assert!(coverage.sections.iter().all(|s| !s.traveled));
}

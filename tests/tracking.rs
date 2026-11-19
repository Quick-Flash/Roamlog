use roamlog::{
    coverage::{self, Road},
    geo::Point,
    model::{Event, Fix, Status, TripFilter},
    store::Store,
};

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("trips.jsonl")).unwrap();
    (dir, store)
}

fn start(store: &mut Store, time: u64, category_id: Option<u64>) {
    store
        .commit(Event::Start {
            timestamp_ms: time,
            category_id,
        })
        .unwrap();
}

fn fix(store: &mut Store, time: u64, lat: f64, lon: f64, accuracy: f64) {
    store
        .commit(Event::Fix {
            fix: Fix {
                point: Point { lat, lon },
                timestamp_ms: time,
                accuracy_m: accuracy,
            },
        })
        .unwrap();
}

#[test]
fn distance_repeats_but_coverage_does_not() {
    let (_dir, mut s) = store();
    assert!(!s.state.total_distance(None).is_sign_negative());
    for category in ["Walking", "Motorcycle"] {
        s.commit(Event::Category {
            name: category.into(),
        })
        .unwrap();
    }
    for i in 0..2 {
        let time = i * 100_000;
        start(&mut s, time, Some(i + 1));
        // Small walking samples must accumulate into road sections.
        for n in 0..=20 {
            let position = if i == 0 { n } else { 20 - n };
            fix(&mut s, time + n * 1000, 0.0, position as f64 * 0.00005, 3.0);
        }
        s.commit(Event::Finish {
            timestamp_ms: time + 21_000,
        })
        .unwrap();
    }
    let roads = vec![Road {
        half_width_m: 0.0,
        id: "town-road".into(),
        name: "Main".into(),
        points: vec![
            Point { lat: 0.0, lon: 0.0 },
            Point {
                lat: 0.0,
                lon: 0.002,
            },
        ],
    }];
    let result = coverage::sections(&roads, &s.state);
    let traveled = result.iter().filter(|s| s.traveled).count();
    assert!(traveled >= 5 && traveled < result.len());
    // Both trips ride same stretch: each section counts two trips, first by trip 1.
    for section in result.iter().filter(|s| s.traveled) {
        assert_eq!(section.first_trip, Some(1));
        assert_eq!(section.visits, 2);
    }
    assert!((s.state.total_distance(None) - 222.39).abs() < 0.1);
    assert!((s.state.total_distance(Some(1)) - 111.195).abs() < 0.1);
    let mut first_trip = s.state.clone();
    first_trip.trips.truncate(1);
    assert_eq!(
        traveled,
        coverage::sections(&roads, &first_trip)
            .iter()
            .filter(|s| s.traveled)
            .count()
    );
}

#[test]
fn trip_category_and_all_history_keep_individual_routes_totals_and_coverage() {
    let (dir, mut s) = store();
    for name in ["Walking", "Biking"] {
        s.commit(Event::Category { name: name.into() }).unwrap();
    }
    for (i, category) in [Some(1), Some(1), Some(2), None].into_iter().enumerate() {
        let time = i as u64 * 100_000;
        start(&mut s, time, category);
        let lat = match category {
            Some(1) => 0.0,
            Some(2) => 0.001,
            _ => 0.002,
        };
        for n in 0..=20 {
            fix(&mut s, time + n * 1000, lat, n as f64 * 0.00005, 3.0);
        }
        s.commit(Event::Finish {
            timestamp_ms: time + 21_000,
        })
        .unwrap();
    }
    drop(s);
    let s = Store::open(dir.path().join("trips.jsonl")).unwrap();
    assert_eq!(s.state.trips.len(), 4);
    assert!(s.state.trips.iter().all(|t| t.points.len() == 21));
    let walking = TripFilter::Category {
        category_id: Some(1),
    };
    let biking = TripFilter::Category {
        category_id: Some(2),
    };
    let only = TripFilter::Trip { trip_id: 1 };
    let uncategorized = TripFilter::Category { category_id: None };
    let total = |filter| s.state.trips_for(filter).map(|t| t.distance_m).sum::<f64>();
    assert_eq!(s.state.trips_for(walking.clone()).count(), 2);
    assert_eq!(
        s.state
            .trips_for(only.clone())
            .map(|t| t.id)
            .collect::<Vec<_>>(),
        vec![1]
    );
    assert!((total(walking.clone()) - 2.0 * total(only.clone())).abs() < 0.01);
    assert!(
        (total(TripFilter::All)
            - total(walking.clone())
            - total(biking.clone())
            - total(uncategorized.clone()))
        .abs()
            < 0.01
    );
    let roads: Vec<_> = [("walk", 0.0), ("bike", 0.001), ("other", 0.002)]
        .into_iter()
        .map(|(id, lat)| Road {
            half_width_m: 0.0,
            id: id.into(),
            name: id.into(),
            points: vec![Point { lat, lon: 0.0 }, Point { lat, lon: 0.001 }],
        })
        .collect();
    let mut coverage = coverage::Coverage::new(&roads);
    for (filter, expected) in [
        (TripFilter::All, vec!["bike", "other", "walk"]),
        (walking, vec!["walk"]),
        (biking, vec!["bike"]),
        (only, vec!["walk"]),
        (
            TripFilter::Trips {
                trip_ids: vec![1, 3],
            },
            vec!["bike", "walk"],
        ),
        (TripFilter::Trips { trip_ids: vec![] }, vec![]),
        (uncategorized, vec!["other"]),
        (TripFilter::All, vec!["bike", "other", "walk"]),
    ] {
        coverage.update_filtered(&s.state, filter);
        let traveled: std::collections::BTreeSet<_> = coverage
            .sections
            .iter()
            .filter(|s| s.traveled)
            .map(|s| s.road_id.as_str())
            .collect();
        assert_eq!(traveled, expected.into_iter().collect());
    }
    assert_eq!(
        s.state.trips.len(),
        4,
        "viewing history must not alter saved trips"
    );
}

#[test]
fn incremental_coverage_matches_full_replay_without_counting_repeat_updates() {
    let (_dir, mut s) = store();
    let roads = vec![Road {
        half_width_m: 0.0,
        id: "main".into(),
        name: "Main".into(),
        points: vec![
            Point { lat: 0.0, lon: 0.0 },
            Point {
                lat: 0.0,
                lon: 0.002,
            },
        ],
    }];
    let mut coverage = coverage::Coverage::new(&roads);
    start(&mut s, 0, None);
    for n in 0..=20 {
        fix(&mut s, n * 1000, 0.0, n as f64 * 0.00005, 3.0);
        coverage.update(&s.state);
        coverage.update(&s.state);
        let actual: Vec<bool> = coverage.sections.iter().map(|s| s.traveled).collect();
        let expected: Vec<bool> = coverage::sections(&roads, &s.state)
            .iter()
            .map(|s| s.traveled)
            .collect();
        assert_eq!(actual, expected);
    }
}

#[test]
fn pause_resume_excludes_distance_and_time_between_sessions() {
    let (_dir, mut s) = store();
    start(&mut s, 0, None);
    fix(&mut s, 1000, 0.0, 0.0, 3.0);
    fix(&mut s, 2000, 0.0, 0.0001, 3.0);
    s.commit(Event::Pause { timestamp_ms: 3000 }).unwrap();
    assert!(s.commit(Event::Resume { timestamp_ms: 2000 }).is_err());
    s.commit(Event::Resume {
        timestamp_ms: 100_000,
    })
    .unwrap();
    fix(&mut s, 101_000, 1.0, 1.0, 3.0);
    fix(&mut s, 102_000, 1.0, 1.0001, 3.0);
    s.commit(Event::Finish {
        timestamp_ms: 103_000,
    })
    .unwrap();
    let trip = &s.state.trips[0];
    assert!(trip.distance_m < 23.0);
    assert_eq!(trip.elapsed_at(200_000), 6000);
    assert!(!trip.points[2].connected);
}

#[test]
fn poor_accuracy_stale_samples_gaps_and_teleports_do_not_add_distance() {
    let (_dir, mut s) = store();
    start(&mut s, 0, None);
    fix(&mut s, 1000, 0.0, 0.0, 3.0);
    fix(&mut s, 500, 50.0, 50.0, 3.0);
    fix(&mut s, 2000, 0.0, 0.000001, 3.0);
    fix(&mut s, 3000, 1.0, 1.0, 100.0);
    fix(&mut s, 4000, 1.0, 1.0, 3.0);
    fix(&mut s, 5000, 20.0, 20.0, 3.0);
    fix(&mut s, 100_000, 30.0, 30.0, 3.0);
    assert_eq!(s.state.trips[0].distance_m, 0.0);
    assert!(s.state.trips[0].points.iter().all(|p| !p.connected));
}

#[test]
fn persisted_recording_recovers_paused_without_bridging_restart() {
    let (dir, mut s) = store();
    start(&mut s, 0, None);
    fix(&mut s, 1000, 0.0, 0.0, 3.0);
    fix(&mut s, 2000, 0.0, 0.0001, 3.0);
    let distance = s.state.trips[0].distance_m;
    drop(s);
    let mut s = Store::open(dir.path().join("trips.jsonl")).unwrap();
    s.recover_recording().unwrap();
    assert_eq!(s.state.active().unwrap().status, Status::Paused);
    assert_eq!(s.state.active().unwrap().elapsed_ms, 2000);
    assert_eq!(s.state.total_distance(None), distance);
    s.commit(Event::Resume { timestamp_ms: 9000 }).unwrap();
    fix(&mut s, 10_000, 1.0, 1.0, 3.0);
    assert_eq!(s.state.total_distance(None), distance);
}

#[test]
fn partial_final_write_recovers_but_committed_corruption_is_an_error() {
    use std::io::Write;
    let (dir, mut s) = store();
    s.commit(Event::Category { name: "Run".into() }).unwrap();
    drop(s);
    let path = dir.path().join("trips.jsonl");
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"{\"op\":")
        .unwrap();
    let s = Store::open(&path).unwrap();
    assert_eq!(s.state.categories[0].name, "Run");
    drop(s);
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"corrupt\n")
        .unwrap();
    assert!(Store::open(path).is_err());
}

#[test]
fn rejected_events_leave_journal_unchanged() {
    let (dir, mut s) = store();
    s.commit(Event::Category {
        name: "Bike".into(),
    })
    .unwrap();
    let before = std::fs::read(dir.path().join("trips.jsonl")).unwrap();
    assert!(
        s.commit(Event::Category {
            name: " bike ".into()
        })
        .is_err()
    );
    assert!(
        s.commit(Event::Start {
            timestamp_ms: 0,
            category_id: Some(9)
        })
        .is_err()
    );
    assert!(s.commit(Event::Finish { timestamp_ms: 0 }).is_err());
    assert_eq!(
        before,
        std::fs::read(dir.path().join("trips.jsonl")).unwrap()
    );
}

#[test]
fn parallel_roads_and_crossing_streets_are_not_claimed() {
    let (_dir, mut s) = store();
    start(&mut s, 0, None);
    fix(&mut s, 1000, 0.0, 0.0, 3.0);
    fix(&mut s, 11_000, 0.0, 0.001, 3.0);
    let road = |id: &str, lat| Road {
        half_width_m: 0.0,
        id: id.into(),
        name: id.into(),
        points: vec![Point { lat, lon: 0.0 }, Point { lat, lon: 0.001 }],
    };
    assert!(
        coverage::sections(&[road("a", 0.0), road("b", 0.00003)], &s.state)
            .iter()
            .all(|s| !s.traveled)
    );
    let crossing = Road {
        half_width_m: 0.0,
        id: "crossing".into(),
        name: "Crossing".into(),
        points: vec![
            Point {
                lat: -0.001,
                lon: 0.0005,
            },
            Point {
                lat: 0.001,
                lon: 0.0005,
            },
        ],
    };
    let result = coverage::sections(&[road("a", 0.0), crossing], &s.state);
    assert!(
        result
            .iter()
            .filter(|s| s.road_id == "a")
            .all(|s| s.traveled)
    );
    assert!(
        result
            .iter()
            .filter(|s| s.road_id == "crossing")
            .all(|s| !s.traveled)
    );
}

#[test]
fn geojson_validates_geometry_and_identity() {
    let valid = br#"{"type":"FeatureCollection","features":[{"id":"way/1","properties":{"name":"Main"},"geometry":{"type":"LineString","coordinates":[[0,0],[0.001,0]]}}]}"#;
    assert_eq!(coverage::import_geojson(valid).unwrap()[0].name, "Main");
    assert!(
        coverage::import_geojson(
            br#"{"type":"FeatureCollection","features":[{"geometry":{"type":"Point"}}]}"#
        )
        .is_err()
    );
    assert!(coverage::import_geojson(br#"{}"#).is_err());
}

#[test]
fn haversine_handles_dateline_and_invalid_coordinates() {
    let a = Point {
        lat: 0.0,
        lon: 179.999,
    };
    let b = Point {
        lat: 0.0,
        lon: -179.999,
    };
    assert!((a.distance(b) - 222.39).abs() < 0.1);
    assert!(
        !Point {
            lat: f64::NAN,
            lon: 0.0
        }
        .valid()
    );
}

#[test]
fn deleted_trips_vanish_on_replay_and_ids_are_not_reused() {
    let (dir, mut store) = store();
    start(&mut store, 0, None);
    assert!(
        store.commit(Event::DeleteTrip { trip_id: 1 }).is_err(),
        "active trip"
    );
    store.commit(Event::Finish { timestamp_ms: 10 }).unwrap();
    start(&mut store, 20, None);
    store.commit(Event::Finish { timestamp_ms: 30 }).unwrap();
    store.commit(Event::DeleteTrip { trip_id: 2 }).unwrap();
    start(&mut store, 40, None);
    store.commit(Event::Finish { timestamp_ms: 50 }).unwrap();
    let ids: Vec<u64> = store.state.trips.iter().map(|t| t.id).collect();
    assert_eq!(ids, [1, 3]);
    drop(store);
    let replayed = Store::open(dir.path().join("trips.jsonl")).unwrap();
    assert_eq!(
        replayed
            .state
            .trips
            .iter()
            .map(|t| t.id)
            .collect::<Vec<_>>(),
        [1, 3]
    );
}

#[test]
fn two_processes_share_journal_without_losing_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trips.jsonl");
    let mut ui = Store::open(&path).unwrap();
    let mut recorder = Store::open(&path).unwrap();
    start(&mut recorder, 0, None);
    fix(&mut recorder, 1000, 0.0, 0.0, 3.0);
    fix(&mut recorder, 11000, 0.0, 0.001, 3.0);
    // UI sees recorder's events, and validates its own write against them.
    ui.catch_up().unwrap();
    assert_eq!(ui.state.active().unwrap().points.len(), 2);
    assert!(
        ui.commit(Event::Start {
            timestamp_ms: 20_000,
            category_id: None
        })
        .is_err(),
        "trip already active in other process"
    );
    ui.commit(Event::Category {
        name: "Walk".into(),
    })
    .unwrap();
    recorder
        .commit(Event::Finish {
            timestamp_ms: 30_000,
        })
        .unwrap();
    ui.catch_up().unwrap();
    assert_eq!(ui.state, recorder.state);
    assert_eq!(Store::open(&path).unwrap().state, ui.state);
}

#[test]
fn trip_edits_and_saved_areas_replay_from_journal() {
    let (dir, mut s) = store();
    s.commit(Event::Category {
        name: "Bike".into(),
    })
    .unwrap();
    start(&mut s, 0, None);
    for n in 0..=10 {
        fix(&mut s, 1000 + n * 1000, 0.0, n as f64 * 0.0001, 3.0);
    }
    s.commit(Event::Finish {
        timestamp_ms: 12_000,
    })
    .unwrap();
    let full = s.state.trips[0].distance_m;
    s.commit(Event::SetCategory {
        trip_id: 1,
        category_id: Some(1),
    })
    .unwrap();
    // Drop first and last fix.
    s.commit(Event::TrimTrip {
        trip_id: 1,
        start_ms: 2000,
        end_ms: 10_000,
    })
    .unwrap();
    let trimmed = s.state.trips[0].clone();
    assert_eq!(trimmed.points.len(), 9);
    assert!(!trimmed.points[0].connected);
    assert!((trimmed.distance_m - full * 0.8).abs() < 0.1);
    assert_eq!(trimmed.started_ms, 2000);
    s.commit(Event::SplitTrip {
        trip_id: 1,
        at_ms: 6000,
    })
    .unwrap();
    let (a, b) = (&s.state.trips[0], &s.state.trips[1]);
    assert_eq!((a.id, b.id), (1, 2));
    assert_eq!((a.points.len(), b.points.len()), (4, 5));
    assert_eq!(b.category_id, Some(1));
    assert_eq!(b.status, Status::Finished);
    assert!(
        (a.distance_m + b.distance_m - full * 0.7).abs() < 0.1,
        "split drops gap"
    );
    assert!(
        s.commit(Event::SplitTrip {
            trip_id: 2,
            at_ms: 7000
        })
        .is_err(),
        "each part needs two points"
    );
    let square = vec![
        Point { lat: 0.0, lon: 0.0 },
        Point {
            lat: 0.0,
            lon: 0.01,
        },
        Point {
            lat: 0.01,
            lon: 0.01,
        },
    ];
    s.commit(Event::SaveArea {
        name: "Home".into(),
        points: square.clone(),
        circle: None,
    })
    .unwrap();
    assert!(
        s.commit(Event::SaveArea {
            name: "Line".into(),
            points: square[..2].to_vec(),
            circle: None,
        })
        .is_err()
    );
    let mut moved = square.clone();
    moved[2].lat = 0.02;
    s.commit(Event::UpdateArea {
        area_id: 1,
        points: moved.clone(),
        name: None,
        circle: None,
    })
    .unwrap();
    // Switching to circle keeps outline as points, plus center and radius.
    let center = Point { lat: 0.0, lon: 0.0 };
    s.commit(Event::UpdateArea {
        area_id: 1,
        points: roamlog::geo::circle_outline(center, 5.),
        name: Some("Yard".into()),
        circle: Some((center, 5.)),
    })
    .unwrap_err();
    s.commit(Event::UpdateArea {
        area_id: 1,
        points: roamlog::geo::circle_outline(center, 50.),
        name: Some("Yard".into()),
        circle: Some((center, 50.)),
    })
    .unwrap();
    s.commit(Event::UpdateArea {
        area_id: 1,
        points: moved,
        name: None,
        circle: None,
    })
    .unwrap();
    drop(s);
    let replay = Store::open(dir.path().join("trips.jsonl")).unwrap();
    assert_eq!(replay.state.areas[0].points[2].lat, 0.02);
    assert_eq!(replay.state.trips.len(), 2);
    assert_eq!(replay.state.areas[0].name, "Yard");
    assert_eq!(replay.state.areas[0].circle, None);
    assert_eq!(replay.state.trips[1].points.len(), 5);
}

#[test]
fn empty_and_short_trips_can_be_renamed_recategorized_and_deleted_after_reopen() {
    let (dir, mut s) = store();
    s.commit(Event::Category {
        name: "Walking".into(),
    })
    .unwrap();
    for n in 0..3 {
        let time = n * 10_000;
        start(&mut s, time, None);
        for i in 0..n {
            fix(&mut s, time + i * 1000, 0., i as f64 * 0.0001, 3.);
        }
        s.commit(Event::Finish {
            timestamp_ms: time + 3000,
        })
        .unwrap();
        s.commit(Event::RenameTrip {
            trip_id: n + 1,
            name: format!("  Short trip {}  ", n + 1),
        })
        .unwrap();
        s.commit(Event::SetCategory {
            trip_id: n + 1,
            category_id: Some(1),
        })
        .unwrap();
    }
    assert!(
        s.commit(Event::RenameTrip {
            trip_id: 1,
            name: "  ".into()
        })
        .is_err()
    );
    assert!(
        s.commit(Event::RenameTrip {
            trip_id: 99,
            name: "Missing".into()
        })
        .is_err()
    );
    drop(s);
    let mut s = Store::open(dir.path().join("trips.jsonl")).unwrap();
    for t in &s.state.trips {
        assert_eq!(t.name, format!("Short trip {}", t.id));
        assert_eq!(t.category_id, Some(1));
    }
    assert!(roamlog::gpx::export(&s.state).contains("Short trip 1"));
    for trip_id in 1..=3 {
        s.commit(Event::DeleteTrip { trip_id }).unwrap();
    }
    drop(s);
    assert!(
        Store::open(dir.path().join("trips.jsonl"))
            .unwrap()
            .state
            .trips
            .is_empty()
    );
}

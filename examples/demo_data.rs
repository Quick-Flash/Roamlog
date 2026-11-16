//! Made-up demo history in a public city, for demo videos without real GPS data.
//!
//! Usage: demo_data OUT_DIR [LAT LON SIDE_KM]
//! Writes OUT_DIR/basemap.pmtiles (offline map downloaded around center) and
//! OUT_DIR/trips.jsonl (categories, trips routed along real streets, one saved area).
use roamlog::{
    coverage::Road,
    geo::Point,
    mapping::download,
    model::{Event, Fix},
    pmtiles::Archive,
    roads,
    store::Store,
};
use std::{
    collections::{BinaryHeap, HashMap},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

/// Deterministic xorshift, so same command makes same demo.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.unit()
    }
}

struct Graph {
    nodes: Vec<Point>,
    /// (neighbor, metres).
    edges: Vec<Vec<(usize, f64)>>,
}

fn class(id: &str) -> &str {
    id.split_once(':').map_or("", |c| c.0)
}

/// Street graph. `allow` picks road classes the travel mode uses.
fn graph(roads: &[Road], allow: impl Fn(&str) -> bool) -> Graph {
    let mut index: HashMap<(i64, i64), usize> = HashMap::new();
    let mut graph = Graph {
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    let mut node = |p: Point, g: &mut Graph| {
        // ~1 m grid joins pieces split at tile edges.
        let key = ((p.lat * 1e5).round() as i64, (p.lon * 1e5).round() as i64);
        *index.entry(key).or_insert_with(|| {
            g.nodes.push(p);
            g.edges.push(Vec::new());
            g.nodes.len() - 1
        })
    };
    for road in roads.iter().filter(|r| allow(class(&r.id))) {
        for pair in road.points.windows(2) {
            let (a, b) = (node(pair[0], &mut graph), node(pair[1], &mut graph));
            if a != b {
                let d = pair[0].distance(pair[1]);
                graph.edges[a].push((b, d));
                graph.edges[b].push((a, d));
            }
        }
    }
    graph
}

/// Nodes in largest connected piece; isolated footways would make routes fail.
fn main_component(g: &Graph) -> Vec<bool> {
    let mut label = vec![usize::MAX; g.nodes.len()];
    let mut sizes = Vec::new();
    for start in 0..g.nodes.len() {
        if label[start] != usize::MAX {
            continue;
        }
        let id = sizes.len();
        let (mut stack, mut size) = (vec![start], 0);
        label[start] = id;
        while let Some(n) = stack.pop() {
            size += 1;
            for &(m, _) in &g.edges[n] {
                if label[m] == usize::MAX {
                    label[m] = id;
                    stack.push(m);
                }
            }
        }
        sizes.push(size);
    }
    let best = (0..sizes.len()).max_by_key(|&i| sizes[i]).unwrap_or(0);
    label.iter().map(|&l| l == best).collect()
}

fn nearest(g: &Graph, main: &[bool], p: Point) -> usize {
    (0..g.nodes.len())
        .filter(|&i| main[i])
        .min_by(|&a, &b| g.nodes[a].distance(p).total_cmp(&g.nodes[b].distance(p)))
        .unwrap()
}

fn route(g: &Graph, from: usize, to: usize) -> Option<Vec<Point>> {
    #[derive(PartialEq)]
    struct E(f64, usize);
    impl Eq for E {}
    impl PartialOrd for E {
        fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(o))
        }
    }
    impl Ord for E {
        fn cmp(&self, o: &Self) -> std::cmp::Ordering {
            o.0.total_cmp(&self.0)
        }
    }
    let mut dist = vec![f64::INFINITY; g.nodes.len()];
    let mut prev = vec![usize::MAX; g.nodes.len()];
    let mut heap = BinaryHeap::from([E(0.0, from)]);
    dist[from] = 0.0;
    while let Some(E(d, n)) = heap.pop() {
        if n == to {
            break;
        }
        if d > dist[n] {
            continue;
        }
        for &(m, w) in &g.edges[n] {
            if d + w < dist[m] {
                dist[m] = d + w;
                prev[m] = n;
                heap.push(E(d + w, m));
            }
        }
    }
    if !dist[to].is_finite() {
        return None;
    }
    let mut path = vec![g.nodes[to]];
    let mut n = to;
    while n != from {
        n = prev[n];
        path.push(g.nodes[n]);
    }
    path.reverse();
    Some(path)
}

/// Point `east`/`north` metres from `p`.
fn offset(p: Point, east: f64, north: f64) -> Point {
    Point {
        lat: p.lat + north / 111_195.0,
        lon: p.lon + east / (111_195.0 * p.lat.to_radians().cos()),
    }
}

/// Fixes every second along `path` at `speed` m/s with GPS-like jitter.
fn fixes(path: &[Point], start_ms: u64, speed: f64, rng: &mut Rng) -> Vec<Fix> {
    let mut out = Vec::new();
    let (mut t, mut carry) = (start_ms, 0.0);
    for pair in path.windows(2) {
        let length = pair[0].distance(pair[1]);
        let mut at = carry;
        while at < length {
            let p = pair[0].interpolate(pair[1], at / length);
            let accuracy = rng.range(4.0, 9.0);
            out.push(Fix {
                point: offset(
                    p,
                    rng.range(-1.5, 1.5) * accuracy / 4.0,
                    rng.range(-1.5, 1.5) * accuracy / 4.0,
                ),
                timestamp_ms: t,
                accuracy_m: accuracy,
            });
            t += 1000;
            at += speed * rng.range(0.8, 1.15);
        }
        carry = at - length;
    }
    out
}

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = PathBuf::from(
        args.first()
            .ok_or("Usage: demo_data OUT_DIR [LAT LON SIDE_KM]")?,
    );
    let num = |i: usize, d: f64| {
        args.get(i)
            .map_or(Ok(d), |v| v.parse().map_err(|_| "bad number"))
    };
    // Default: downtown Portland, Oregon.
    let center = Point {
        lat: num(1, 45.5202)?,
        lon: num(2, -122.6742)?,
    };
    let side = num(3, 10.0)?;
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let map = out.join("basemap.pmtiles");
    if !map.exists() {
        let plan = download::plan(download::square([center.lon, center.lat], side), &map)?;
        eprintln!("downloading {} MB map", plan.bytes >> 20);
        let partial = out.join("basemap.pmtiles.tmp");
        download::fetch(plan, &map, &partial, &download::Progress::default())?;
        std::fs::rename(&partial, &map).map_err(|e| e.to_string())?;
    }
    let archive = Archive::open(&map)?;
    let near = |p: Point, r: f64| {
        let ring: Vec<Point> = [(-r, -r), (r, -r), (r, r), (-r, r)]
            .iter()
            .map(|&(e, n)| offset(p, e, n))
            .collect();
        roads::bbox_tiles(&ring, 0.0, archive.max_zoom, 2000).unwrap()
    };
    let roads = roads::roads(&archive, &near(center, 3500.0))?;
    eprintln!("{} roads", roads.len());
    let drive = graph(&roads, |c| {
        !matches!(
            c,
            "path" | "footway" | "cycleway" | "steps" | "pedestrian" | "track"
        )
    });
    let walk = graph(&roads, |c| {
        !matches!(c, "motorway" | "trunk" | "motorway_link" | "trunk_link")
    });
    let (drive_main, walk_main) = (main_component(&drive), main_component(&walk));

    let journal = out.join("trips.jsonl");
    let _ = std::fs::remove_file(&journal);
    let mut store = Store::open(&journal)?;
    for name in ["Motorcycle", "Bicycle", "Walking"] {
        store.commit(Event::Category { name: name.into() })?;
    }
    let mut rng = Rng(0x5eed_1234_abcd);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let day = 86_400_000;
    // (category, speed m/s, distance range m). Home base reused so roads repeat.
    let home = offset(center, -900.0, 600.0);
    let plans = [
        (1, 11.0, 3000.0, 5000.0),
        (2, 5.0, 1500.0, 3000.0),
        (1, 12.0, 2500.0, 4500.0),
        (3, 1.4, 600.0, 1400.0),
        (2, 5.5, 2000.0, 3200.0),
        (1, 11.0, 2500.0, 3500.0),
        (3, 1.3, 500.0, 1100.0),
        (2, 5.0, 1800.0, 2800.0),
        (1, 12.0, 3000.0, 4500.0),
        (3, 1.4, 700.0, 1200.0),
    ];
    let mut made = 0;
    for (i, &(category, speed, min, max)) in plans.iter().enumerate() {
        let (g, main) = if category == 3 {
            (&walk, &walk_main)
        } else {
            (&drive, &drive_main)
        };
        // Daytime start, spread over past six weeks, oldest first.
        let start = now - (42 - i as u64 * 4) * day + (rng.range(9.0, 18.0) * 3_600_000.0) as u64;
        let a = nearest(
            g,
            main,
            offset(home, rng.range(-200.0, 200.0), rng.range(-200.0, 200.0)),
        );
        let angle = rng.range(0.0, std::f64::consts::TAU);
        let reach = rng.range(min, max) * 0.7;
        let b = nearest(
            g,
            main,
            offset(home, reach * angle.cos(), reach * angle.sin()),
        );
        let Some(mut path) = route(g, a, b) else {
            continue;
        };
        // Round trips cover same roads twice; others end away from home.
        if i % 2 == 0
            && let Some(back) = route(g, b, a)
        {
            path.extend(back.into_iter().skip(1));
        }
        let fixes = fixes(&path, start + 4000, speed, &mut rng);
        if fixes.len() < 10 {
            continue;
        }
        store.commit(Event::Start {
            timestamp_ms: start,
            category_id: Some(category),
        })?;
        let last = fixes.last().unwrap().timestamp_ms;
        for fix in fixes {
            store.commit(Event::Fix { fix })?;
        }
        store.commit(Event::Finish {
            timestamp_ms: last + 3000,
        })?;
        made += 1;
    }
    // Simulated live GPS loop for demo app: home and back through downtown streets.
    let mut lap: Vec<Point> = Vec::new();
    let stops: Vec<usize> = [
        (0.0, 0.0),
        (700.0, -300.0),
        (1200.0, -900.0),
        (300.0, -1200.0),
        (-400.0, -500.0),
        (0.0, 0.0),
    ]
    .iter()
    .map(|&(e, n)| nearest(&drive, &drive_main, offset(home, e, n)))
    .collect();
    for pair in stops.windows(2) {
        if let Some(leg) = route(&drive, pair[0], pair[1]) {
            let skip = usize::from(!lap.is_empty());
            lap.extend(leg.into_iter().skip(skip));
        }
    }
    std::fs::write(
        out.join(roamlog::demo::FILE),
        serde_json::to_vec(&lap).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    eprintln!("demo GPS loop {} points", lap.len());
    // Saved area around downtown.
    let area = [
        (-1800.0, -1500.0),
        (1500.0, -1500.0),
        (1700.0, 1600.0),
        (-1500.0, 1900.0),
    ]
    .iter()
    .map(|&(e, n)| offset(center, e, n))
    .collect();
    store.commit(Event::SaveArea {
        name: "Downtown".into(),
        points: area,
    })?;
    let total: f64 = store.state.trips.iter().map(|t| t.distance_m).sum();
    eprintln!(
        "{made} trips, {:.1} km, journal {}",
        total / 1000.0,
        journal.display()
    );
    Ok(())
}

//! Recover bounded GPS outages from local roads, retaining unresolved street ambiguity.
//! Journal fixes remain untouched. Returned state explicitly marks inferred segments.
use crate::{
    coverage::Road,
    geo::{Point, local, segment_distance},
    model::{Fix, State, TrackPoint},
    spatial::Index,
};
use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap, HashSet},
};

struct Edge {
    half_width_m: f64,
    a: usize,
    b: usize,
    length: f64,
}
#[derive(Clone, Copy)]
struct Snap {
    edge: usize,
    t: f64,
    point: Point,
    offset: f64,
}
struct Graph {
    nodes: Vec<Point>,
    edges: Vec<Edge>,
    adjacent: Vec<Vec<(usize, usize)>>,
    index: Index,
}

#[derive(Clone, Copy, PartialEq)]
struct Entry {
    score: f64,
    node: usize,
}
impl Eq for Entry {}
impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .score
            .total_cmp(&self.score)
            .then_with(|| other.node.cmp(&self.node))
    }
}

#[derive(Clone)]
struct Route {
    points: Vec<Point>,
    widths: Vec<f64>,
    length: f64,
    end: Snap,
}

pub struct Recovery {
    graph: Graph,
    cache: HashMap<(u64, u64, u64, Option<usize>), Option<Route>>,
}

impl Recovery {
    pub fn new(roads: &[Road]) -> Self {
        Self {
            graph: Graph::new(roads),
            cache: HashMap::new(),
        }
    }

    /// Replaying raw state makes old trips benefit without rewriting their journal.
    pub fn apply(&mut self, raw: &State) -> State {
        self.reconstruct(raw, false)
    }

    /// Geometry for road coverage. Ordinary GPS stays untouched in display/distance state.
    /// Match each observed segment too, so bends and offsets do not create road holes.
    pub fn coverage_state(&mut self, raw: &State) -> State {
        let mut state = self.reconstruct(raw, true);
        for (source, trip) in raw.trips.iter().zip(&mut state.trips) {
            let times: Vec<_> = trip
                .points
                .iter()
                .filter(|p| p.road_matched)
                .map(|p| p.fix.timestamp_ms)
                .collect();
            for (i, pair) in source.points.windows(2).enumerate() {
                if !pair[1].connected || pair[1].estimated {
                    continue;
                }
                let n = times.partition_point(|&t| t <= pair[0].fix.timestamp_ms);
                if times.get(n).is_some_and(|&t| t < pair[1].fix.timestamp_ms) {
                    continue;
                }
                // Resolve a locally ambiguous fix using bounded observed approach/exit.
                // Every intervening fix must support route; pauses are never crossed.
                for radius in [2, 5, 10, 20] {
                    let begin = i.saturating_sub(radius);
                    let end = (i + 1 + radius).min(source.points.len() - 1);
                    let window = &source.points[begin..=end];
                    let a = window[0].fix;
                    let b = window.last().unwrap().fix;
                    if b.timestamp_ms - a.timestamp_ms > 30_000
                        || window[1..].iter().any(|p| !p.connected)
                    {
                        continue;
                    }
                    let key = (source.id, a.timestamp_ms, b.timestamp_ms, None);
                    let incoming = begin
                        .checked_sub(1)
                        .filter(|_| window[0].connected)
                        .map(|n| source.points[n].fix);
                    let route = self.cache.entry(key).or_insert_with(|| {
                        self.graph.route(a, b, incoming, None, true, Some(window))
                    });
                    if let Some(route) = route.as_ref().filter(|r| supports_window(r, window)) {
                        let mut points = route_points(route, a, b);
                        if let Some(first) = points.first_mut() {
                            first.connected = false;
                        }
                        trip.points.extend(points);
                        break;
                    }
                }
            }
        }
        state
    }

    fn reconstruct(&mut self, raw: &State, match_recorded: bool) -> State {
        let mut state = raw.clone();
        for trip in &mut state.trips {
            let mut road_anchor: Option<Snap> = None;
            let mut points = Vec::with_capacity(trip.points.len());
            for (i, point) in trip.points.iter().enumerate() {
                let mut observed = point.clone();
                if !point.connected {
                    road_anchor = None;
                }
                if i > 0 && point.connected {
                    let before = &trip.points[i - 1];
                    let key = (
                        trip.id,
                        before.fix.timestamp_ms,
                        point.fix.timestamp_ms,
                        road_anchor.map(|s| s.edge),
                    );
                    let incoming = i
                        .checked_sub(2)
                        .filter(|_| before.connected && !before.estimated)
                        .map(|n| trip.points[n].fix);
                    let route = self.cache.entry(key).or_insert_with(|| {
                        self.graph.route(
                            before.fix,
                            point.fix,
                            incoming,
                            road_anchor,
                            !point.estimated,
                            None,
                        )
                    });
                    let chord = before.fix.point.distance(point.fix.point);
                    let route = route.as_ref();
                    if let Some(route) = route {
                        road_anchor = Some(route.end);
                        if !match_recorded && !point.estimated {
                            // Keep observed path exactly as received, but retain road anchor
                            // for next segment so display and coverage infer same outages.
                            points.push(observed);
                            continue;
                        }
                        if point.estimated {
                            let extra = (route.length - chord).max(0.0);
                            trip.distance_m += extra;
                            trip.estimated_distance_m += extra;
                        }
                        if match_recorded {
                            observed.connected = false;
                        }
                        // Road endpoints are separate from actual GPS endpoints. Snap connectors
                        // are drawn as inferred, but are not counted as visited roads/distance.
                        points.extend(route_points(route, before.fix, point.fix));
                    } else {
                        road_anchor = None;
                    }
                }
                points.push(observed);
            }
            trip.points = points;
        }
        state
    }
}

fn route_points(route: &Route, a: Fix, b: Fix) -> Vec<TrackPoint> {
    let mut geometry = Vec::new();
    for pair in route.points.windows(2) {
        let count = (pair[0].distance(pair[1]) / 15.0).ceil().max(1.0) as usize;
        for n in 0..count {
            geometry.push(interpolate(pair[0], pair[1], n as f64 / count as f64));
        }
    }
    geometry.extend(route.points.last().copied());
    let count = geometry.len() + 1;
    geometry
        .into_iter()
        .enumerate()
        .map(|(n, point)| TrackPoint {
            fix: Fix {
                point,
                timestamp_ms: a.timestamp_ms
                    + (b.timestamp_ms - a.timestamp_ms) * (n + 1) as u64 / count as u64,
                accuracy_m: a.accuracy_m.max(b.accuracy_m),
            },
            connected: true,
            estimated: true,
            road_matched: n > 0,
        })
        .collect()
}

fn window_score(route: &Route, window: &[TrackPoint]) -> f64 {
    let origin = window[0].fix.point;
    let road: Vec<_> = route.points.iter().map(|&p| local(origin, p)).collect();
    window
        .iter()
        .map(|fix| {
            let p = local(origin, fix.fix.point);
            let d = road
                .windows(2)
                .map(|q| segment_distance(p, q[0], q[1]))
                .fold(f64::INFINITY, f64::min);
            (d / fix.fix.accuracy_m.clamp(3.0, 30.0)).powi(2)
        })
        .sum()
}

fn supports_window(route: &Route, window: &[TrackPoint]) -> bool {
    let origin = window[0].fix.point;
    let road: Vec<_> = route.points.iter().map(|&p| local(origin, p)).collect();
    let gps: Vec<_> = window.iter().map(|p| local(origin, p.fix.point)).collect();
    let observed: f64 = window
        .windows(2)
        .map(|p| p[0].fix.point.distance(p[1].fix.point))
        .sum();
    if route.length > observed * 1.5 + 10.0 {
        return false;
    }
    // Same road-edge allowance as endpoint snapping: wide roads permit lane offset.
    if window.iter().zip(&gps).any(|(fix, &p)| {
        !road.windows(2).zip(&route.widths).any(|(q, width)| {
            segment_distance(p, q[0], q[1]) <= fix.fix.accuracy_m.clamp(12.0, 30.0) + width
        })
    }) {
        return false;
    }
    let tolerance = window
        .iter()
        .map(|p| p.fix.accuracy_m)
        .fold(12.0, f64::max)
        .min(30.0)
        + route.widths.iter().copied().fold(0.0, f64::max);
    route_points(route, window[0].fix, window.last().unwrap().fix)
        .iter()
        .all(|p| {
            let p = local(origin, p.fix.point);
            gps.windows(2)
                .any(|q| segment_distance(p, q[0], q[1]) <= tolerance)
        })
}

/// Near-coincident alternatives sharing approach/exit represent lane uncertainty,
/// not evidence for two different streets. Check full paths in both directions.
fn same_corridor(a: &Route, b: &Route, tolerance: f64) -> bool {
    let origin = a.points[0];
    let a: Vec<_> = a.points.iter().map(|&p| local(origin, p)).collect();
    let b: Vec<_> = b.points.iter().map(|&p| local(origin, p)).collect();
    let within = |path: &[[f64; 2]], other: &[[f64; 2]]| {
        path.windows(2).all(|pair| {
            let length = (pair[1][0] - pair[0][0]).hypot(pair[1][1] - pair[0][1]);
            let steps = (length / 5.0).ceil().max(1.0) as usize;
            (0..=steps).all(|i| {
                let t = i as f64 / steps as f64;
                let p = std::array::from_fn(|j| pair[0][j] + t * (pair[1][j] - pair[0][j]));
                other
                    .windows(2)
                    .any(|q| segment_distance(p, q[0], q[1]) <= tolerance + 1e-6)
            })
        })
    };
    within(&a, &b) && within(&b, &a)
}

fn interpolate(a: Point, b: Point, t: f64) -> Point {
    let dlon = (b.lon - a.lon + 180.0).rem_euclid(360.0) - 180.0;
    Point {
        lat: a.lat + (b.lat - a.lat) * t,
        lon: (a.lon + dlon * t + 180.0).rem_euclid(360.0) - 180.0,
    }
}

impl Graph {
    fn new(roads: &[Road]) -> Self {
        let mut nodes: Vec<Point> = Vec::new();
        let mut node_index = Index::default();
        let mut edges = Vec::new();
        let mut unique = HashSet::new();
        let mut ends = Vec::new();
        for road in roads {
            let mut previous: Option<usize> = None;
            for &point in &road.points {
                let found = node_index
                    .near(point, point)
                    .into_iter()
                    .filter(|&i| nodes[i].distance(point) <= 1.5)
                    .min_by(|&a, &b| {
                        nodes[a]
                            .distance(point)
                            .total_cmp(&nodes[b].distance(point))
                    });
                let node = found.unwrap_or_else(|| {
                    let id = nodes.len();
                    nodes.push(point);
                    node_index.insert(id, point, point);
                    id
                });
                if let Some(a) = previous
                    && a != node
                    && unique.insert((a.min(node), a.max(node)))
                {
                    edges.push(Edge {
                        half_width_m: road.half_width_m,
                        a,
                        b: node,
                        length: nodes[a].distance(nodes[node]),
                    });
                }
                if previous.is_none() {
                    ends.push(node);
                }
                previous = Some(node);
            }
            ends.extend(previous);
        }
        // Map tiles end ramps/side streets on another road's edge, not on its vertex.
        // Split that edge so T-junctions stay connected.
        let mut edge_index = Index::default();
        for (id, edge) in edges.iter().enumerate() {
            edge_index.insert(id, nodes[edge.a], nodes[edge.b]);
        }
        for &node in &ends {
            let p = nodes[node];
            for id in edge_index.near(p, p) {
                let Edge { a, b, .. } = edges[id];
                if a == node || b == node {
                    continue;
                }
                let (pa, pb) = (local(p, nodes[a]), local(p, nodes[b]));
                if segment_distance([0.0, 0.0], pa, pb) > 1.5
                    || nodes[a].distance(p) <= 1.5
                    || nodes[b].distance(p) <= 1.5
                {
                    continue;
                }
                let half_width_m = edges[id].half_width_m;
                edges[id] = Edge {
                    half_width_m,
                    a,
                    b: node,
                    length: nodes[a].distance(p),
                };
                edge_index.insert(edges.len(), p, nodes[b]);
                edges.push(Edge {
                    half_width_m,
                    a: node,
                    b,
                    length: p.distance(nodes[b]),
                });
                break;
            }
        }
        let mut adjacent = vec![Vec::new(); nodes.len()];
        let mut index = Index::default();
        for (id, edge) in edges.iter().enumerate() {
            adjacent[edge.a].push((edge.b, id));
            adjacent[edge.b].push((edge.a, id));
            index.insert(id, nodes[edge.a], nodes[edge.b]);
        }
        Self {
            nodes,
            edges,
            adjacent,
            index,
        }
    }

    fn snaps(
        &self,
        p: Point,
        tolerance: f64,
        heading: [f64; 2],
        minimum_alignment: f64,
    ) -> Vec<Snap> {
        let mut candidates = Vec::new();
        for id in self.index.near(p, p) {
            let edge = &self.edges[id];
            let a = local(p, self.nodes[edge.a]);
            let b = local(p, self.nodes[edge.b]);
            let v = [b[0] - a[0], b[1] - a[1]];
            let l2 = v[0] * v[0] + v[1] * v[1];
            if l2 < 0.01 {
                continue;
            }
            let alignment = (v[0] * heading[0] + v[1] * heading[1]).abs()
                / (l2.sqrt() * heading[0].hypot(heading[1]));
            if alignment < minimum_alignment {
                continue;
            }
            let t = (-(a[0] * v[0] + a[1] * v[1]) / l2).clamp(0.0, 1.0);
            let point = interpolate(self.nodes[edge.a], self.nodes[edge.b], t);
            let offset = p.distance(point);
            if offset <= tolerance + edge.half_width_m {
                candidates.push(Snap {
                    edge: id,
                    t,
                    point,
                    offset,
                });
            }
        }
        // Crossings can be centimetres closer than road being followed. Use heading
        // as bounded (<=4 m) tie-break, never as reason to snap to distant road.
        let score = |s: &Snap| {
            let edge = &self.edges[s.edge];
            let v = local(self.nodes[edge.a], self.nodes[edge.b]);
            let alignment = ((v[0] * heading[0] + v[1] * heading[1]).abs()
                / (v[0].hypot(v[1]) * heading[0].hypot(heading[1])))
            .clamp(0.0, 1.0);
            (s.offset - edge.half_width_m).max(0.0) + 4.0 * (1.0 - alignment)
        };
        candidates.sort_by(|a, b| score(a).total_cmp(&score(b)));
        let Some(&best) = candidates.first() else {
            return Vec::new();
        };
        let mut plausible = vec![best];
        let edge = &self.edges[best.edge];
        let v = local(self.nodes[edge.a], self.nodes[edge.b]);
        for alternative in candidates.iter().skip(1).filter(|s| {
            (s.offset - self.edges[s.edge].half_width_m).max(0.0)
                <= (best.offset - edge.half_width_m).max(0.0) + 4.0
        }) {
            let other = &self.edges[alternative.edge];
            if [edge.a, edge.b]
                .iter()
                .any(|n| *n == other.a || *n == other.b)
            {
                continue;
            }
            let w = local(self.nodes[other.a], self.nodes[other.b]);
            let alignment =
                (v[0] * w[0] + v[1] * w[1]).abs() / (v[0].hypot(v[1]) * w[0].hypot(w[1]));
            // Parallel means laterally separated; collinear pieces of same line are not.
            let q = local(self.nodes[edge.a], alternative.point);
            let lateral = (v[0] * q[1] - v[1] * q[0]).abs() / v[0].hypot(v[1]);
            if alignment > 0.95 && lateral > 3.0 {
                plausible.push(*alternative);
            }
        }
        plausible
    }

    fn route(
        &self,
        a: Fix,
        b: Fix,
        incoming: Option<Fix>,
        anchor: Option<Snap>,
        ordinary: bool,
        observations: Option<&[TrackPoint]>,
    ) -> Option<Route> {
        let chord = a.point.distance(b.point);
        let seconds = b.timestamp_ms.checked_sub(a.timestamp_ms)? as f64 / 1000.0;
        if !(3.0..=5000.0).contains(&chord) || seconds <= 0.0 || seconds > 180.0 {
            return None;
        }
        let chord_heading = local(a.point, b.point);
        let incoming_heading = incoming
            .filter(|p| {
                a.timestamp_ms.saturating_sub(p.timestamp_ms) <= 10_000
                    && p.point.distance(a.point) >= 8.0
            })
            .map_or(chord_heading, |p| local(p.point, a.point));
        let alignment = if ordinary { 0.5 } else { 0.0 };
        let tolerance = |accuracy: f64| {
            if ordinary {
                (accuracy + 3.0).clamp(6.0, 30.0)
            } else {
                (accuracy + 8.0).clamp(10.0, 35.0)
            }
        };
        let starts = anchor.map_or_else(
            || {
                self.snaps(
                    a.point,
                    tolerance(a.accuracy_m),
                    incoming_heading,
                    alignment,
                )
            },
            |s| vec![s],
        );
        let ends = self.snaps(b.point, tolerance(b.accuracy_m), chord_heading, alignment);
        // An endpoint near two parallel roads is not necessarily an ambiguous route.
        // Keep candidates until both endpoints and bounded network travel can be checked.
        let mut choices = Vec::new();
        for start in starts {
            for &end in &ends {
                let limit = if chord < 8.0 {
                    if start.offset > 12.0 || end.offset > 12.0 {
                        continue;
                    }
                    chord + 2.0
                } else {
                    let limit = (chord * 2.5 + 80.0).min(seconds * 100.0);
                    if ordinary {
                        limit.min(chord * 1.5 + 10.0)
                    } else {
                        limit
                    }
                };
                if let Some((route, edges)) = self.between(start, end, limit) {
                    choices.push((route, edges, start, end));
                }
            }
        }
        choices.sort_by(|a, b| a.0.length.total_cmp(&b.0.length));
        let shortest = choices.first()?.0.length;
        choices.retain(|choice| choice.0.length <= shortest * 1.15 + 15.0);
        if let Some(window) = observations {
            choices.retain(|choice| supports_window(&choice.0, window));
            choices
                .sort_by(|a, b| window_score(&a.0, window).total_cmp(&window_score(&b.0, window)));
        } else {
            choices.sort_by(|a, b| (a.2.offset + a.3.offset).total_cmp(&(b.2.offset + b.3.offset)));
        }
        let (route, edges, start, end) = choices.first()?;
        let tolerance = a.accuracy_m.min(b.accuracy_m).clamp(1.5, 8.0);
        for (other, other_edges, other_start, other_end) in choices.iter().skip(1) {
            let shared = start.edge == other_start.edge
                || end.edge == other_end.edge
                || edges.iter().any(|edge| other_edges.contains(edge));
            if shared && same_corridor(route, other, tolerance) {
                continue;
            }
            // Same bounded 4 m tie-break as snapping: clearly farther endpoints do not
            // compete, otherwise a junction detour or overshoot discards matched road.
            if other_start.offset + other_end.offset > start.offset + end.offset + 4.0 {
                continue;
            }
            // Several observed fixes can establish street even when one noisy fix
            // lies between parallels. Equal evidence never chooses arbitrary street.
            if observations.is_none_or(|window| {
                window_score(other, window) - window_score(route, window) < 6.0
            }) {
                return None;
            }
        }

        // Equal alternatives through a block do not identify which road was ridden.
        // Keep their lower-bound GPS estimate, without crediting guessed streets.
        let alternative_limit = route.length * 1.15 + 15.0;
        for &edge in edges.iter().filter(|&&e| e != start.edge && e != end.edge) {
            if let Some((other, _)) = self.shortest(*start, *end, Some(edge), alternative_limit)
                && !same_corridor(route, &other, tolerance)
                && observations.is_none_or(|window| {
                    window.iter().any(|p| p.estimated)
                        || (supports_window(&other, window)
                            && window_score(&other, window) - window_score(route, window) < 6.0)
                })
            {
                return None;
            }
        }
        Some(route.clone())
    }

    fn between(&self, start: Snap, end: Snap, limit: f64) -> Option<(Route, Vec<usize>)> {
        if start.edge == end.edge {
            let length = (start.t - end.t).abs() * self.edges[start.edge].length;
            return (length <= limit).then(|| {
                (
                    Route {
                        points: vec![start.point, end.point],
                        widths: vec![self.edges[start.edge].half_width_m],
                        length,
                        end,
                    },
                    Vec::new(),
                )
            });
        }
        self.shortest(start, end, None, limit)
    }

    fn shortest(
        &self,
        start: Snap,
        end: Snap,
        banned: Option<usize>,
        limit: f64,
    ) -> Option<(Route, Vec<usize>)> {
        let mut distance = vec![f64::INFINITY; self.nodes.len()];
        let mut previous = vec![None; self.nodes.len()];
        let mut queue = BinaryHeap::new();
        let a = &self.edges[start.edge];
        let b = &self.edges[end.edge];
        for (node, d) in [(a.a, start.t * a.length), (a.b, (1.0 - start.t) * a.length)] {
            distance[node] = d;
            queue.push(Entry {
                score: d + self.nodes[node].distance(end.point),
                node,
            });
        }
        let mut best = limit;
        let mut finish = None;
        while let Some(Entry { score, node }) = queue.pop() {
            if score > best {
                continue;
            }
            let tail = if node == b.a {
                Some(end.t * b.length)
            } else if node == b.b {
                Some((1.0 - end.t) * b.length)
            } else {
                None
            };
            if let Some(tail) = tail
                && distance[node] + tail <= best
            {
                best = distance[node] + tail;
                finish = Some(node);
            }
            for &(next, id) in &self.adjacent[node] {
                if Some(id) == banned {
                    continue;
                }
                let d = distance[node] + self.edges[id].length;
                if d < distance[next] && d <= best {
                    distance[next] = d;
                    previous[next] = Some((node, id));
                    queue.push(Entry {
                        score: d + self.nodes[next].distance(end.point),
                        node: next,
                    });
                }
            }
        }
        let mut node = finish?;
        let mut path = vec![node];
        let mut used = Vec::new();
        while let Some((prev, edge)) = previous[node] {
            used.push(edge);
            node = prev;
            path.push(node);
        }
        path.reverse();
        used.reverse();
        let mut points = vec![start.point];
        let mut widths = Vec::new();
        for (i, node) in path.into_iter().enumerate() {
            let p = self.nodes[node];
            if points.last().unwrap().distance(p) >= 0.1 {
                points.push(p);
                widths.push(self.edges[if i == 0 { start.edge } else { used[i - 1] }].half_width_m);
            }
        }
        if points.last().unwrap().distance(end.point) >= 0.1 {
            points.push(end.point);
            widths.push(self.edges[end.edge].half_width_m);
        }
        Some((
            Route {
                points,
                widths,
                length: best,
                end,
            },
            used,
        ))
    }
}

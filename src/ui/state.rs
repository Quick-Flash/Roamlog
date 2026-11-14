use crate::{
    area::Stats,
    backend,
    coverage::Section,
    geo::Point,
    mapping::{
        basemap::Basemap,
        camera::{Camera, geographic, project},
        download::{self, Plan, Progress},
    },
    model::{State, Status, TripFilter},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, atomic::Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
use zgui::prelude::*;

#[derive(Clone, Copy)]
pub struct UiState {
    pub directory: StoredValue<PathBuf, LocalStorage>,
    pub data: RwSignal<State, LocalStorage>,
    pub sections: RwSignal<Vec<Section>, LocalStorage>,
    pub selected: RwSignal<String, LocalStorage>,
    pub draft: RwSignal<String, LocalStorage>,
    pub dialog: RwSignal<bool, LocalStorage>,
    pub busy: RwSignal<bool, LocalStorage>,
    pub message: RwSignal<String, LocalStorage>,
    pub now: RwSignal<u64, LocalStorage>,
    pub camera: RwSignal<Camera, LocalStorage>,
    /// Map rotation, radians clockwise on screen; 0 is north up.
    pub bearing: RwSignal<f64, LocalStorage>,
    pub anchored: RwSignal<bool, LocalStorage>,
    /// Latest live position and accuracy radius in meters.
    pub position: RwSignal<Option<(Point, f64)>, LocalStorage>,
    /// Camera tracks position until user pans.
    pub follow: RwSignal<bool, LocalStorage>,
    pub basemap: Basemap,
    /// Backend import counter; change reopens basemap.
    basemap_revision: StoredValue<u64, LocalStorage>,
    platform_status: StoredValue<String, LocalStorage>,
    /// Running area download, if any.
    download: StoredValue<Option<Arc<Progress>>, LocalStorage>,
    /// Settings sheet (appearance, units, offline map, data). Map download lives there.
    pub settings_open: RwSignal<bool, LocalStorage>,
    plan: StoredValue<Option<Plan>, LocalStorage>,
    pub plan_ready: RwSignal<bool, LocalStorage>,
    pub plan_info: RwSignal<String, LocalStorage>,
    /// Latest size check; older results are ignored.
    plan_seq: StoredValue<u64, LocalStorage>,
    /// Kilometers, else miles. Saved in `units` file.
    pub metric: RwSignal<bool, LocalStorage>,
    pub dark: RwSignal<bool, LocalStorage>,
    pub satellite: RwSignal<bool, LocalStorage>,
    pub show_gps: RwSignal<bool, LocalStorage>,
    pub show_roads: RwSignal<bool, LocalStorage>,
    pub trips_open: RwSignal<bool, LocalStorage>,
    pub history_filter: RwSignal<TripFilter, LocalStorage>,
    pub coverage_filter: RwSignal<TripFilter, LocalStorage>,
    /// Trip awaiting second tap to delete.
    pub confirm_delete: RwSignal<Option<u64>, LocalStorage>,
    /// Map viewport CSS size, for fitting from outside map view.
    pub map_size: StoredValue<[f64; 2], LocalStorage>,
    /// Coverage totals for loaded roads: all and traveled, metres.
    pub road_totals: RwSignal<(f64, f64), LocalStorage>,
    /// Color traveled roads by trips on them. Saved in `heatmap` file.
    pub heatmap: RwSignal<bool, LocalStorage>,
    /// Area tool: tapping adds corners while drawing; stats once calculated.
    pub area_drawing: RwSignal<bool, LocalStorage>,
    pub area_points: RwSignal<Vec<Point>, LocalStorage>,
    /// Saved area being shown or edited.
    pub area_id: RwSignal<Option<u64>, LocalStorage>,
    /// Corner lists before each change, for Undo.
    pub area_history: StoredValue<Vec<Vec<Point>>, LocalStorage>,
    pub area_stats: RwSignal<Option<Stats>, LocalStorage>,
    pub area_open: RwSignal<bool, LocalStorage>,
    pub area_name: RwSignal<String, LocalStorage>,
    pub area_busy: RwSignal<bool, LocalStorage>,
    /// Highlighted nearest untraveled road section.
    pub target: RwSignal<Option<Section>, LocalStorage>,
    pub areas_open: RwSignal<bool, LocalStorage>,
    /// Saved area awaiting second tap to delete.
    pub confirm_area_delete: RwSignal<Option<u64>, LocalStorage>,
    /// Last measured share traveled per saved area, this session.
    pub area_percent: RwSignal<std::collections::HashMap<u64, f64>, LocalStorage>,
    pub confirm_restore: RwSignal<bool, LocalStorage>,
    /// Finished trip being edited.
    pub editing: RwSignal<Option<u64>, LocalStorage>,
    pub edit_open: RwSignal<bool, LocalStorage>,
    /// Trips finished since last coverage refresh; announced with their new roads.
    pub(super) announce: StoredValue<Vec<u64>, LocalStorage>,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

impl UiState {
    pub fn new(directory: PathBuf) -> Self {
        let metric =
            std::fs::read_to_string(directory.join("units")).map_or(true, |u| u != "imperial");
        let dark = std::fs::read_to_string(directory.join("theme")).map_or(true, |v| v != "light");
        let satellite =
            std::fs::read_to_string(directory.join("map-layer")).is_ok_and(|v| v == "satellite");
        let show_gps =
            std::fs::read_to_string(directory.join("show-gps")).map_or(true, |v| v != "off");
        let show_roads =
            std::fs::read_to_string(directory.join("show-roads")).map_or(true, |v| v != "off");
        let heatmap = std::fs::read_to_string(directory.join("heatmap")).is_ok_and(|v| v == "on");
        Self {
            basemap: Basemap::new(directory.join("basemap.pmtiles")),
            directory: StoredValue::new_local(directory),
            data: RwSignal::new_local(State::default()),
            sections: RwSignal::new_local(Vec::new()),
            selected: RwSignal::new_local("0".into()),
            draft: RwSignal::new_local(String::new()),
            dialog: RwSignal::new_local(false),
            busy: RwSignal::new_local(false),
            message: RwSignal::new_local(String::new()),
            now: RwSignal::new_local(now_ms()),
            camera: RwSignal::new_local(Camera::default()),
            bearing: RwSignal::new_local(0.0),
            anchored: RwSignal::new_local(false),
            position: RwSignal::new_local(None),
            follow: RwSignal::new_local(true),
            basemap_revision: StoredValue::new_local(0),
            platform_status: StoredValue::new_local(String::new()),
            download: StoredValue::new_local(None),
            settings_open: RwSignal::new_local(false),
            plan: StoredValue::new_local(None),
            plan_ready: RwSignal::new_local(false),
            plan_info: RwSignal::new_local(String::new()),
            plan_seq: StoredValue::new_local(0),
            metric: RwSignal::new_local(metric),
            dark: RwSignal::new_local(dark),
            satellite: RwSignal::new_local(satellite),
            show_gps: RwSignal::new_local(show_gps),
            show_roads: RwSignal::new_local(show_roads),
            trips_open: RwSignal::new_local(false),
            history_filter: RwSignal::new_local(TripFilter::All),
            coverage_filter: RwSignal::new_local(TripFilter::All),
            confirm_delete: RwSignal::new_local(None),
            map_size: StoredValue::new_local([0.0, 0.0]),
            road_totals: RwSignal::new_local((0.0, 0.0)),
            heatmap: RwSignal::new_local(heatmap),
            area_drawing: RwSignal::new_local(false),
            area_points: RwSignal::new_local(Vec::new()),
            area_id: RwSignal::new_local(None),
            area_history: StoredValue::new_local(Vec::new()),
            area_stats: RwSignal::new_local(None),
            area_open: RwSignal::new_local(false),
            area_name: RwSignal::new_local(String::new()),
            area_busy: RwSignal::new_local(false),
            target: RwSignal::new_local(None),
            areas_open: RwSignal::new_local(false),
            confirm_area_delete: RwSignal::new_local(None),
            area_percent: RwSignal::new_local(Default::default()),
            confirm_restore: RwSignal::new_local(false),
            editing: RwSignal::new_local(None),
            edit_open: RwSignal::new_local(false),
            announce: StoredValue::new_local(Vec::new()),
        }
    }

    pub fn current() -> Self {
        use_local_context().expect("Roamlog application context")
    }

    pub fn status(self) -> Option<Status> {
        self.data.with(|s| s.active().map(|t| t.status))
    }

    pub async fn request(self, command: Value) -> Result<Value, String> {
        let path = self.directory.get_value();
        blocking(move || backend::request(path, command)).await
    }

    pub fn refresh(self, map: bool) {
        if self.busy.get_untracked() {
            return;
        }
        self.busy.set(true);
        let filter = self.history_filter.get_untracked();
        let directory = self.directory.get_value();
        // History actions close their dialog; refresh must survive that owner disappearing.
        zgui::reactive::spawn_detached(async move {
            let result = super::snapshot::load(directory, filter, map).await;
            match result {
                Ok(value) => {
                    let state = value.state;
                    // Trips finished since last refresh get their new roads announced.
                    let finished: Vec<u64> = self.data.with_untracked(|old| {
                        old.trips
                            .iter()
                            .filter(|t| t.status != Status::Finished)
                            .filter(|t| {
                                state
                                    .trips
                                    .iter()
                                    .any(|n| n.id == t.id && n.status == Status::Finished)
                            })
                            .map(|t| t.id)
                            .collect()
                    });
                    self.announce.update_value(|a| a.extend(finished));
                    if self.data.with_untracked(|old| old != &state) {
                        self.data.set(state);
                    }
                    if map && filter == self.history_filter.get_untracked() {
                        let sections = value.sections.unwrap_or_default();
                        if self.sections.with_untracked(|old| old != &sections) {
                            self.sections.set(sections);
                        }
                        let totals = (value.road_total_m, value.road_traveled_m);
                        if self.road_totals.get_untracked() != totals {
                            self.road_totals.set(totals);
                        }
                        if self.coverage_filter.get_untracked() != filter {
                            self.coverage_filter.set(filter);
                        }
                        if filter == TripFilter::All {
                            self.announce_new_roads();
                        }
                    }
                    let revision = value.basemap;
                    if revision != self.basemap_revision.get_value() {
                        self.basemap_revision.set_value(revision);
                        self.reload_basemap();
                    }
                    let message = value.platform_status.as_str();
                    if self.platform_status.with_value(|old| old != message) {
                        self.platform_status.set_value(message.to_owned());
                        self.message.set(message.to_owned());
                    }
                    let position = serde_json::from_value::<Point>(value.position.clone())
                        .ok()
                        .filter(|p| p.valid())
                        .map(|p| (p, value.position["accuracy_m"].as_f64().unwrap_or(0.0)))
                        .or_else(|| {
                            // Recording fixes still count when live feed is absent.
                            self.data.with_untracked(|d| {
                                d.active()
                                    .filter(|t| t.status == Status::Recording)
                                    .and_then(|t| t.points.last())
                                    .map(|p| (p.fix.point, p.fix.accuracy_m))
                            })
                        });
                    if self.position.get_untracked() != position {
                        self.position.set(position);
                    }
                    if self.follow.get_untracked() {
                        self.center_on_position();
                    }
                    if !self.anchored.get_untracked() {
                        let point = self
                            .data
                            .with_untracked(|d| {
                                d.trips
                                    .iter()
                                    .rev()
                                    .find_map(|t| t.points.last().map(|p| p.fix.point))
                            })
                            .or_else(|| {
                                self.sections.with_untracked(|s| s.first().map(|s| s.start))
                            });
                        if let Some(point) = point {
                            self.camera.set(Camera {
                                center: project(point.lon, point.lat).unwrap(),
                                zoom: 15.0,
                            });
                            self.anchored.set(true);
                        }
                    }
                }
                Err(error) => self.message.set(error),
            }
            self.busy.set(false);
            // Selection may change while worker is loading old coverage.
            if filter != self.history_filter.get_untracked() {
                self.refresh(true);
            }
        });
    }

    pub fn record(self, op: &str) {
        let category = self.selected.get_untracked().parse::<u64>().unwrap_or(0);
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::record(self.directory.get_value(), op, category) {
            self.message.set(error);
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (op, category);
            self.message.set("Trip recording needs Android GPS.".into());
        }
    }

    /// Starts the live position feed once location permission exists.
    pub fn start_position(self) {
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::follow(self.directory.get_value(), false) {
            self.message.set(error);
        }
    }

    /// Re-enables follow and centers on current position.
    pub fn locate(self) {
        self.follow.set(true);
        self.center_on_position();
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::follow(self.directory.get_value(), true) {
            self.message.set(error);
        }
    }

    fn center_on_position(self) {
        let Some((point, _)) = self.position.get_untracked() else {
            return;
        };
        let Some(center) = project(point.lon, point.lat) else {
            return;
        };
        let zoom = self.camera.get_untracked().zoom;
        let zoom = if self.anchored.get_untracked() {
            zoom
        } else {
            16.0
        };
        let next = Camera { center, zoom };
        if self.camera.get_untracked() != next {
            self.camera.set(next);
        }
        self.anchored.set(true);
    }

    pub fn reload_basemap(self) {
        self.basemap
            .reload(move |error| self.message.set(format!("Offline map: {error}")));
    }

    pub fn import_basemap(self) {
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::import_basemap(self.directory.get_value()) {
            self.message.set(error);
        }
        #[cfg(not(target_os = "android"))]
        self.message.set(format!(
            "Copy PMTiles file to {}",
            self.directory.get_value().join("basemap.pmtiles").display()
        ));
    }

    /// Sizes download of `side_km` square around map center.
    pub fn plan_download(self, side_km: f64) {
        let bounds = download::square(geographic(self.camera.get_untracked().center), side_km);
        self.plan.set_value(None);
        self.plan_ready.set(false);
        self.plan_seq.update_value(|n| *n += 1);
        let seq = self.plan_seq.get_value();
        self.plan_info
            .set(format!("Checking size of {side_km:.0} km area…"));
        // Detached: dialog closing disposes its scope, which would cancel scoped task.
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || download::plan(bounds)).await;
            if seq != self.plan_seq.get_value() {
                return;
            }
            match result {
                Ok(plan) => {
                    self.plan_info.set(format!(
                        "{side_km:.0} × {side_km:.0} km around map center · {} MB",
                        (plan.bytes >> 20).max(1)
                    ));
                    self.plan.set_value(Some(plan));
                    self.plan_ready.set(true);
                }
                Err(error) => self.plan_info.set(error),
            }
        });
    }

    /// Downloads sized plan and merges it into offline map.
    pub fn start_download(self) {
        let mut plan = None;
        self.plan.update_value(|p| plan = p.take());
        let Some(plan) = plan else {
            return;
        };
        self.plan_ready.set(false);
        self.plan_info.set(String::new());
        self.settings_open.set(false);
        if self.download.with_value(Option::is_some) {
            self.message.set("Map download already running.".into());
            return;
        }
        let progress = Arc::new(Progress::default());
        self.download.set_value(Some(progress.clone()));
        self.message.set("Downloading map…".into());
        let directory = self.directory.get_value();
        // Detached: dialog closing disposes its scope, which would cancel scoped task.
        zgui::reactive::spawn_detached(async move {
            let result = blocking(move || {
                let (map, partial) = (
                    directory.join("basemap.pmtiles"),
                    directory.join("basemap.pmtiles.tmp"),
                );
                download::fetch(plan, &map, &partial, &progress)?;
                std::fs::rename(&partial, &map).map_err(|e| e.to_string())
            })
            .await;
            self.download.set_value(None);
            match result {
                Ok(()) => {
                    self.message.set("Offline map downloaded.".into());
                    if let Err(error) = self.request(json!({"op":"basemap_imported"})).await {
                        self.message.set(error);
                    }
                }
                Err(error) => self.message.set(error),
            }
        });
    }

    /// Shows download progress; called each second.
    pub fn download_status(self) {
        let Some(p) = self.download.get_value() else {
            return;
        };
        let (done, total) = (
            p.done.load(Ordering::Relaxed) >> 20,
            p.total.load(Ordering::Relaxed) >> 20,
        );
        if total > 0 {
            self.message
                .set(format!("Downloading map · {done} of {total} MB"));
        }
    }

    pub fn distance(self, meters: f64) -> String {
        if self.metric.get() {
            format!("{:.2} km", meters / 1000.0)
        } else {
            format!("{:.2} mi", meters / 1609.344)
        }
    }

    pub fn set_units(self, metric: bool) {
        self.metric.set(metric);
        let path = self.directory.get_value().join("units");
        spawn_local(async move {
            let units = if metric { "metric" } else { "imperial" };
            if let Err(error) = blocking(move || std::fs::write(path, units)).await {
                self.message.set(format!("Could not save units: {error}"));
            }
        });
    }

    pub fn set_dark(self, dark: bool) {
        self.dark.set(dark);
        self.save_preference("theme", if dark { "dark" } else { "light" });
    }

    pub fn set_satellite(self, satellite: bool) {
        self.satellite.set(satellite);
        self.save_preference("map-layer", if satellite { "satellite" } else { "map" });
    }

    pub fn set_show_gps(self, visible: bool) {
        self.show_gps.set(visible);
        self.save_preference("show-gps", if visible { "on" } else { "off" });
    }

    pub fn set_heatmap(self, on: bool) {
        self.heatmap.set(on);
        self.save_preference("heatmap", if on { "on" } else { "off" });
    }

    pub fn set_show_roads(self, visible: bool) {
        self.show_roads.set(visible);
        self.save_preference("show-roads", if visible { "on" } else { "off" });
    }

    fn save_preference(self, name: &str, value: &'static str) {
        let path = self.directory.get_value().join(name);
        // Detached so closing view options cannot cancel preference save.
        zgui::reactive::spawn_detached(async move {
            if let Err(error) = blocking(move || std::fs::write(path, value)).await {
                self.message
                    .set(format!("Could not save preference: {error}"));
            }
        });
    }

    /// Fits map to points (lon/lat), stopping follow.
    pub fn fit(self, points: impl IntoIterator<Item = Point>) {
        let mut min = [f64::INFINITY; 2];
        let mut max = [f64::NEG_INFINITY; 2];
        for p in points {
            if let Some(xy) = project(p.lon, p.lat) {
                for i in 0..2 {
                    min[i] = min[i].min(xy[i]);
                    max[i] = max[i].max(xy[i]);
                }
            }
        }
        if !min[0].is_finite() {
            return;
        }
        let ranges = plot::Ranges::new(
            (min[0] - 40.0, max[0] + 40.0),
            (min[1] - 40.0, max[1] + 40.0),
        );
        if let Some(next) = Camera::from_ranges(ranges, self.map_size.get_value()) {
            self.follow.set(false);
            self.camera.set(next);
            self.anchored.set(true);
        }
    }

    pub fn show_trip(self, id: u64) {
        let points = self.data.with_untracked(|d| {
            d.trips
                .iter()
                .find(|t| t.id == id)
                .map(|t| t.points.iter().map(|p| p.fix.point).collect::<Vec<_>>())
                .unwrap_or_default()
        });
        if points.is_empty() {
            self.message.set("Trip has no GPS path.".into());
            return;
        }
        self.set_history_filter(TripFilter::Trip { trip_id: id });
        self.trips_open.set(false);
        self.fit(points);
    }

    pub fn set_history_filter(self, filter: TripFilter) {
        self.history_filter.set(filter);
        self.refresh(true);
        // Area percentage follows selection.
        if self.area_stats.with_untracked(Option::is_some) {
            self.compute_area(false);
        }
    }

    pub fn history_label(self) -> String {
        let filter = self.history_filter.get();
        self.data.with(|d| match filter {
            TripFilter::All => "All trips".into(),
            TripFilter::Category { category_id: None } => "Uncategorized".into(),
            TripFilter::Category {
                category_id: Some(id),
            } => d
                .categories
                .iter()
                .find(|c| c.id == id)
                .map_or_else(|| "Category".into(), |c| c.name.clone()),
            TripFilter::Trip { trip_id } => format!("Trip #{trip_id}"),
        })
    }

    pub fn history_summary(self) -> String {
        let filter = self.history_filter.get();
        let (count, distance) = self.data.with(|d| {
            d.trips_for(filter).fold((0, 0.0), |(count, total), trip| {
                (count + 1, total + trip.distance_m)
            })
        });
        format!(
            "{} · {count} {} · {}",
            self.history_label(),
            if count == 1 { "trip" } else { "trips" },
            self.distance(distance)
        )
    }

    pub fn show_history(self) {
        let filter = self.history_filter.get_untracked();
        let points = self.data.with_untracked(|d| {
            d.trips_for(filter)
                .flat_map(|t| t.points.iter().map(|p| p.fix.point))
                .collect::<Vec<_>>()
        });
        self.trips_open.set(false);
        self.fit(points);
    }

    pub fn delete_trip(self, id: u64) {
        self.confirm_delete.set(None);
        if self.history_filter.get_untracked() == (TripFilter::Trip { trip_id: id }) {
            self.history_filter.set(TripFilter::All);
        }
        zgui::reactive::spawn_detached(async move {
            match self.request(json!({"op":"delete_trip","trip_id":id})).await {
                Ok(_) => self.refresh(true),
                Err(error) => self.message.set(error),
            }
        });
    }

    pub fn export_gpx(self) {
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::export_gpx(self.directory.get_value()) {
            self.message.set(error);
        }
        #[cfg(not(target_os = "android"))]
        zgui::reactive::spawn_detached(async move {
            let path = self.directory.get_value().join("roamlog-trips.gpx");
            let result = match self.request(json!({"op":"gpx"})).await {
                Ok(value) => {
                    let gpx = value["gpx"].as_str().unwrap_or_default().to_owned();
                    let target = path.clone();
                    blocking(move || std::fs::write(target, gpx))
                        .await
                        .map_err(|e| e.to_string())
                }
                Err(error) => Err(error),
            };
            self.message.set(match result {
                Ok(()) => format!("Exported to {}", path.display()),
                Err(error) => error,
            });
        });
    }

    pub fn open_settings(self) {
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::open_settings() {
            self.message.set(error);
        }
    }
}

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

/// Friend position received by the sharing service.
#[derive(Clone, Debug, PartialEq)]
pub struct FriendMarker {
    /// Peer ID.
    pub id: String,
    pub name: String,
    pub via: Option<String>,
    /// Profile picture URL while its image handle lives in `UiState::avatars`.
    pub avatar: Option<String>,
    pub point: Point,
    pub accuracy_m: f64,
    pub timestamp_ms: i64,
    /// Connected and friend currently shares.
    pub live: bool,
}

#[derive(Clone, Copy)]
pub struct UiState {
    pub updates: super::update_state::Updater,
    pub directory: StoredValue<PathBuf, LocalStorage>,
    pub data: RwSignal<State, LocalStorage>,
    pub sections: RwSignal<Vec<Section>, LocalStorage>,
    pub selected: RwSignal<String, LocalStorage>,
    pub draft: RwSignal<String, LocalStorage>,
    pub dialog: RwSignal<bool, LocalStorage>,
    pub busy: RwSignal<bool, LocalStorage>,
    /// First snapshot arrived; until then counts would wrongly read as zero.
    pub loaded: RwSignal<bool, LocalStorage>,
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
    pub area_naming: RwSignal<bool, LocalStorage>,
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
    pub map_offline: RwSignal<bool, LocalStorage>,
    pub terrain_enabled: RwSignal<bool, LocalStorage>,
    pub map_friend_selected: RwSignal<Option<String>, LocalStorage>,
    pub alert_draw_back_to_circle: RwSignal<bool, LocalStorage>,
    pub alert_use_circle: StoredValue<Option<zgui::reactive::UnsyncCallback<()>>, LocalStorage>,
    pub drawing_alert: RwSignal<bool, LocalStorage>,
    pub alert_polygon: RwSignal<Vec<Point>, LocalStorage>,
    pub editing_circle: RwSignal<Option<(Point, f64)>, LocalStorage>,
    pub picking_place_center: RwSignal<bool, LocalStorage>,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub place_peer: RwSignal<Option<String>, LocalStorage>,
    pub alert_areas_back: StoredValue<Option<zgui::reactive::UnsyncCallback<()>>, LocalStorage>,
    pub places_open: RwSignal<bool, LocalStorage>,
    pub place_outline: RwSignal<Vec<Point>, LocalStorage>,
    /// Alert areas drawn together on map, each with a tappable name: (area id, name, outline).
    pub area_marks: RwSignal<Vec<(u32, String, Vec<Point>)>, LocalStorage>,
    /// Who the shown areas can be filtered by: (key, label); "g:<id>" keys are groups.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub area_filters: RwSignal<Vec<(String, String)>, LocalStorage>,
    /// Friend whose areas were drawn by selecting them on map; hidden again on deselect.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub marks_friend: StoredValue<Option<String>, LocalStorage>,
    /// Area the Alert areas page opens straight to (set from a map label).
    pub open_area: RwSignal<Option<u32>, LocalStorage>,
    /// Friend page shown in Friends tab; shared so map can open it.
    pub friend_detail: RwSignal<Option<String>, LocalStorage>,
    pub friends_page_back: StoredValue<Option<zgui::reactive::UnsyncCallback<()>>, LocalStorage>,
    pub friends_open: RwSignal<bool, LocalStorage>,
    /// Friend overlay, separate from trip layers. Saved in `show-friends` file.
    pub show_friends: RwSignal<bool, LocalStorage>,
    pub map_friends: RwSignal<Vec<(String, String)>, LocalStorage>,
    pub friend_markers: RwSignal<Vec<FriendMarker>, LocalStorage>,
    /// Invitation link opened from camera, link or image, awaiting user action.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub invite_link: RwSignal<String, LocalStorage>,
    /// Whose trips History and map show: empty is own only; else ("me" or peer ID, name),
    /// merged read-only.
    pub people: RwSignal<Vec<(String, String)>, LocalStorage>,
    /// Friend pictures by peer ID: file time and registered image.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    avatars: StoredValue<
        std::collections::HashMap<String, (std::time::SystemTime, zgui_image::ImageBytes)>,
        LocalStorage,
    >,
    /// Automatic offline map downloads: "off", "wifi" (unmetered only), or "any".
    /// Saved in `auto-map` file.
    pub auto_map_mode: RwSignal<String, LocalStorage>,
    /// Own profile picture URL, shown on own map dot and in Friends.
    pub own_avatar: RwSignal<Option<String>, LocalStorage>,
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    own_avatar_handle:
        StoredValue<Option<(std::time::SystemTime, zgui_image::ImageBytes)>, LocalStorage>,
    /// Map centers where an automatic map download was already tried (z10 tile).
    map_tried: StoredValue<std::collections::HashSet<(u32, u32)>, LocalStorage>,
    /// Friend (peer ID, name) whose trips are being viewed.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub friend_trips: RwSignal<Option<(String, String)>, LocalStorage>,
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
        let auto_map_mode = std::fs::read_to_string(directory.join("auto-map"))
            .ok()
            .filter(|v| matches!(v.as_str(), "off" | "wifi" | "any"))
            .unwrap_or_else(|| "wifi".into());
        let show_friends =
            std::fs::read_to_string(directory.join("show-friends")).map_or(true, |v| v != "off");
        Self {
            updates: super::update_state::Updater::new(&directory),
            basemap: Basemap::new(directory.join("basemap.pmtiles")),
            directory: StoredValue::new_local(directory),
            data: RwSignal::new_local(State::default()),
            sections: RwSignal::new_local(Vec::new()),
            selected: RwSignal::new_local("0".into()),
            draft: RwSignal::new_local(String::new()),
            dialog: RwSignal::new_local(false),
            busy: RwSignal::new_local(false),
            loaded: RwSignal::new_local(false),
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
            area_naming: RwSignal::new_local(false),
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
            map_offline: RwSignal::new_local(false),
            terrain_enabled: RwSignal::new_local(false),
            map_friend_selected: RwSignal::new_local(None),
            alert_draw_back_to_circle: RwSignal::new_local(false),
            alert_use_circle: StoredValue::new_local(None),
            drawing_alert: RwSignal::new_local(false),
            alert_polygon: RwSignal::new_local(Vec::new()),
            editing_circle: RwSignal::new_local(None),
            picking_place_center: RwSignal::new_local(false),
            place_peer: RwSignal::new_local(None),
            alert_areas_back: StoredValue::new_local(None),
            places_open: RwSignal::new_local(false),
            place_outline: RwSignal::new_local(Vec::new()),
            area_marks: RwSignal::new_local(Vec::new()),
            area_filters: RwSignal::new_local(Vec::new()),
            marks_friend: StoredValue::new_local(None),
            open_area: RwSignal::new_local(None),
            friend_detail: RwSignal::new_local(None),
            friends_page_back: StoredValue::new_local(None),
            friends_open: RwSignal::new_local(false),
            show_friends: RwSignal::new_local(show_friends),
            map_friends: RwSignal::new_local(Vec::new()),
            friend_markers: RwSignal::new_local(Vec::new()),
            invite_link: RwSignal::new_local(String::new()),
            friend_trips: RwSignal::new_local(None),
            people: RwSignal::new_local(Vec::new()),
            avatars: StoredValue::new_local(Default::default()),
            map_tried: StoredValue::new_local(Default::default()),
            own_avatar: RwSignal::new_local(None),
            auto_map_mode: RwSignal::new_local(auto_map_mode),
            own_avatar_handle: StoredValue::new_local(None),
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
            let people = self.people.get_untracked();
            let result = super::snapshot::load(directory, filter.clone(), map, people).await;
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
                    if !self.loaded.get_untracked() {
                        self.loaded.set(true);
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
                            self.coverage_filter.set(filter.clone());
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

    /// Moves map to `point` and stops following own position.
    pub fn center_on(self, point: Point) {
        let Some(center) = project(point.lon, point.lat) else {
            return;
        };
        self.follow.set(false);
        self.anchored.set(true);
        let zoom = self.camera.get_untracked().zoom.max(15.0);
        self.camera.set(Camera { center, zoom });
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
            let existing = self.directory.get_value().join("basemap.pmtiles");
            let result = blocking(move || download::plan(bounds, &existing)).await;
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
        self.settings_open.set(false);
        self.begin_download();
    }

    fn begin_download(self) {
        let mut plan = None;
        self.plan.update_value(|p| plan = p.take());
        let Some(plan) = plan else {
            return;
        };
        self.plan_ready.set(false);
        self.plan_info.set(String::new());
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

    /// Road coverage share; "<1%" so a little progress never reads as none.
    pub fn coverage_percent(self) -> Option<String> {
        let (total, traveled) = self.road_totals.get();
        (total > 0.0).then(|| {
            let percent = traveled / total * 100.0;
            if traveled > 0.0 && percent < 1.0 {
                "<1%".into()
            } else {
                format!("{percent:.0}%")
            }
        })
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

    pub fn set_auto_map(self, mode: &'static str) {
        self.auto_map_mode.set(mode.to_owned());
        // A new mode may allow areas skipped before (e.g. on mobile data).
        self.map_tried.set_value(Default::default());
        self.save_preference("auto-map", mode);
    }

    pub fn set_show_friends(self, visible: bool) {
        self.show_friends.set(visible);
        self.save_preference("show-friends", if visible { "on" } else { "off" });
    }

    /// Reads friend positions the sharing service published, and opened invitations.
    pub fn refresh_friends(self) {
        #[cfg(target_os = "android")]
        {
            if let Some(link) = crate::android::PENDING_INVITE
                .lock()
                .ok()
                .and_then(|mut l| l.take())
            {
                self.invite_link.set(link);
                self.friends_open.set(true);
            }
            self.reload_own_avatar();
            let directory = self.directory.get_value();
            let known: std::collections::HashMap<String, std::time::SystemTime> = self
                .avatars
                .with_value(|a| a.iter().map(|(k, v)| (k.clone(), v.0)).collect());
            zgui::reactive::spawn_detached(async move {
                let (markers, pictures, roster) = blocking(move || {
                    let published = crate::sharing::node::read_published(&directory);
                    let roster = crate::sharing::android_app::manager(&directory)
                        .and_then(|manager| manager.friends().list())
                        .ok()
                        .map(|friends| {
                            friends
                                .into_iter()
                                .filter(|friend| !friend.blocked)
                                .map(|friend| (friend.peer_id.to_string(), friend.display_name))
                                .collect::<Vec<_>>()
                        });
                    let now = now_ms() as i64;
                    let fresh =
                        now - published.updated_ms <= crate::sharing::node::HEARTBEAT_MS * 2;
                    let sharing = directory.join("sharing");
                    // Pictures: (peer, file time, new bytes when changed since last read).
                    let mut pictures = Vec::new();
                    for f in &published.friends {
                        let path =
                            crate::sharing::transport::friend_avatar_path(&sharing, f.peer_id);
                        let Ok(time) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
                            continue;
                        };
                        let id = f.peer_id.to_string();
                        let bytes = if known.get(&id) == Some(&time) {
                            None
                        } else {
                            std::fs::read(&path).ok()
                        };
                        pictures.push((id, time, bytes));
                    }
                    let names: std::collections::BTreeMap<_, _> = published
                        .friends
                        .iter()
                        .map(|f| (f.peer_id, f.name.clone()))
                        .collect();
                    let markers = published
                        .friends
                        .into_iter()
                        .filter_map(|f| {
                            let l = f.location?;
                            Some(FriendMarker {
                                id: f.peer_id.to_string(),
                                name: f.name,
                                via: f.via.and_then(|id| names.get(&id).cloned()),
                                avatar: None,
                                point: Point {
                                    lat: f64::from(l.latitude_e7) / 1e7,
                                    lon: f64::from(l.longitude_e7) / 1e7,
                                },
                                accuracy_m: f64::from(l.accuracy_cm) / 100.0,
                                timestamp_ms: l.timestamp_ms,
                                live: fresh && f.connected && f.sharing_active,
                            })
                        })
                        .collect::<Vec<_>>();
                    (markers, pictures, roster)
                })
                .await;
                if let Some(roster) = roster
                    && self
                        .map_friends
                        .with_untracked(|current| *current != roster)
                {
                    self.map_friends.set(roster);
                }
                self.avatars.update_value(|avatars| {
                    avatars.retain(|id, _| pictures.iter().any(|p| &p.0 == id));
                    for (id, time, bytes) in pictures {
                        if let Some(bytes) = bytes {
                            avatars.insert(id, (time, zgui_image::ImageBytes::new(bytes)));
                        }
                    }
                });
                let markers: Vec<_> = markers
                    .into_iter()
                    .map(|mut m| {
                        m.avatar = self.avatars.with_value(|a| a.get(&m.id).map(|v| v.1.url()));
                        m
                    })
                    .collect();
                if self.friend_markers.with_untracked(|m| m != &markers) {
                    self.friend_markers.set(markers);
                }
            });
        }
    }

    /// Re-registers own picture only when its file changed, so the image is not
    /// replaced (and reloaded) while it is showing.
    #[cfg(target_os = "android")]
    pub fn reload_own_avatar(self) {
        let path =
            crate::sharing::transport::own_avatar_path(&self.directory.get_value().join("sharing"));
        let time = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let known = self
            .own_avatar_handle
            .with_value(|h| h.as_ref().map(|h| h.0));
        if time == known {
            return;
        }
        let image = time.and_then(|time| {
            std::fs::read(&path)
                .ok()
                .map(|bytes| (time, zgui_image::ImageBytes::new(bytes)))
        });
        self.own_avatar.set(image.as_ref().map(|i| i.1.url()));
        self.own_avatar_handle.set_value(image);
    }

    /// Downloads offline map for the viewed area when it has none, so vector map and
    /// road coverage work without a manual download. Unmetered networks only on Android.
    pub fn auto_map(self) {
        if self.download.with_value(Option::is_some) || self.plan_ready.get_untracked() {
            return;
        }
        let mode = self.auto_map_mode.get_untracked();
        if mode == "off" {
            return;
        }
        #[cfg(target_os = "android")]
        {
            let usable = zgui::app::android::system::network()
                .ok()
                .flatten()
                .is_some_and(|n| n.validated && (mode == "any" || !n.metered));
            if !usable {
                return;
            }
        }
        // Web Mercator tile of a point at zoom z.
        let tile = |p: Point, z: u8| {
            let n = f64::from(1u32 << z);
            let x = ((p.lon + 180.0) / 360.0 * n).floor().clamp(0.0, n - 1.0) as u32;
            let y = ((1.0 - p.lat.to_radians().tan().asinh() / std::f64::consts::PI) / 2.0 * n)
                .floor()
                .clamp(0.0, n - 1.0) as u32;
            (x, y)
        };
        let archive = self.basemap.archive.get_untracked();
        // Offline map has data at `p` (deepest level, where roads come from).
        let covered = |p: Point| {
            archive.as_ref().is_some_and(|a| {
                let (x, y) = tile(p, a.max_zoom);
                a.entry(a.max_zoom, x, y).ok().flatten().is_some()
            })
        };
        // Tried areas: viewed (z10 tile) and history pieces (z12 tile, marked by top bit).
        let fresh = |key: (u32, u32)| !self.map_tried.with_value(|t| t.contains(&key));
        let camera = self.camera.get_untracked();
        let [lon, lat] = geographic(camera.center);
        let center = Point { lat, lon };
        let mut wanted = None;
        if camera.zoom >= 11.0 && fresh(tile(center, 10)) && !covered(center) {
            wanted = Some((tile(center, 10), center));
        }
        if wanted.is_none() {
            // Road coverage needs map under every trip shown (own or friends'):
            // fetch next missing ~10 km piece, one per call.
            let mut seen = std::collections::HashSet::new();
            self.data.with_untracked(|d| {
                for point in d.trips.iter().flat_map(|t| t.points.iter()) {
                    let p = point.fix.point;
                    let (x, y) = tile(p, 12);
                    let key = (x | 1 << 31, y);
                    // One archive lookup per area, not per GPS fix.
                    if !seen.insert(key) {
                        continue;
                    }
                    if fresh(key) && !covered(p) {
                        wanted = Some((key, p));
                        return;
                    }
                }
            });
        }
        let Some((key, point)) = wanted else {
            return;
        };
        self.map_tried.update_value(|t| {
            t.insert(key);
        });
        let bounds = download::square([point.lon, point.lat], 10.0);
        zgui::reactive::spawn_detached(async move {
            let existing = self.directory.get_value().join("basemap.pmtiles");
            if let Ok(plan) = blocking(move || download::plan(bounds, &existing)).await {
                if self.download.with_value(Option::is_some) {
                    return;
                }
                self.plan.set_value(Some(plan));
                self.begin_download();
            }
        });
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
        self.set_history_filter(TripFilter::Trip { trip_id: id });
        self.trips_open.set(false);
        self.fit(points);
    }

    /// Shows several people's trips together (miles, map, coverage), read-only; empty or
    /// only "me" is own history.
    pub fn set_people(self, mut people: Vec<(String, String)>) {
        if people.iter().all(|p| p.0 == "me") {
            people.clear();
        }
        if self.people.with_untracked(|old| *old == people) {
            return;
        }
        self.people.set(people);
        self.history_filter.set(TripFilter::All);
        self.data.set(State::default());
        self.sections.set(Vec::new());
        self.refresh(true);
    }

    /// "Megan, Mum" for viewed people.
    pub fn people_label(self) -> String {
        self.people.with(|people| {
            people
                .iter()
                .map(|p| p.1.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
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
            TripFilter::Trip { trip_id } => d
                .trips
                .iter()
                .find(|t| t.id == trip_id)
                .map_or_else(|| format!("Trip #{trip_id}"), |t| t.title()),
            TripFilter::Trips { trip_ids } => format!("{} selected trips", trip_ids.len()),
        })
    }

    /// Summary card: (title, trip count), e.g. ("All trips · 10.04 km", "4 trips").
    pub fn history_summary(self) -> (String, String) {
        let filter = self.history_filter.get();
        let (count, distance) = self.data.with(|d| {
            d.trips_for(filter).fold((0, 0.0), |(count, total), trip| {
                (count + 1, total + trip.distance_m)
            })
        });
        let whose = self.people.with(|people| {
            if people.is_empty() {
                String::new()
            } else {
                format!("{}: ", self.people_label())
            }
        });
        (
            format!(
                "{whose}{} · {}",
                self.history_label(),
                self.distance(distance)
            ),
            format!("{count} {}", if count == 1 { "trip" } else { "trips" }),
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
        match self.history_filter.get_untracked() {
            TripFilter::Trip { trip_id } if trip_id == id => {
                self.history_filter.set(TripFilter::All)
            }
            TripFilter::Trips { mut trip_ids } => {
                trip_ids.retain(|&trip_id| trip_id != id);
                self.history_filter.set(if trip_ids.is_empty() {
                    TripFilter::All
                } else {
                    TripFilter::Trips { trip_ids }
                });
            }
            _ => {}
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

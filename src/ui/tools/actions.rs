use super::{date, road_name};
use crate::ui::state::{Mark, Setup, UiState};
use crate::{
    area::Stats,
    coverage::Section,
    geo::{Point, circle_outline, local, segment_distance},
    model::TripFilter,
};
use serde_json::{Value, json};
use zgui::prelude::*;

impl UiState {
    /// Finished trips announce road length they traveled first. Needs all-trip coverage.
    pub(in crate::ui) fn announce_new_roads(self) {
        let mut ids = Vec::new();
        self.announce.update_value(|a| ids = std::mem::take(a));
        if ids.is_empty() || self.road_totals.get_untracked().0 == 0.0 {
            return;
        }
        let lines: Vec<String> = ids
            .iter()
            .map(|&id| {
                let meters = self.new_roads(id);
                format!(
                    "Trip #{id} finished · {} of new roads",
                    self.distance(meters)
                )
            })
            .collect();
        self.message.set(lines.join("\n"));
    }

    /// Road length this trip traveled before any earlier trip (all-trip coverage).
    pub fn new_roads(self, id: u64) -> f64 {
        self.sections.with_untracked(|sections| {
            sections
                .iter()
                .filter(|s| s.first_trip == Some(id))
                .map(|s| s.start.distance(s.end))
                .sum::<f64>()
                // Empty float sum is -0.0, which prints as "-0.00".
                .max(0.0)
        })
    }

    pub fn cancel_area(self) {
        self.area_drawing.set(false);
        self.area_points.set(Vec::new());
        self.area_id.set(None);
        self.area_stats.set(None);
        self.area_open.set(false);
    }

    fn map_center(self) -> Point {
        let [lon, lat] = crate::mapping::camera::geographic(self.camera.get_untracked().center);
        Point { lat, lon }
    }

    /// Map setup screen, shared by saved and alert areas: circle (given, or 200 m at map
    /// center) or, given corners and no circle, a drawn shape.
    pub fn open_setup(
        self,
        setup: Setup,
        name: String,
        circle: Option<(Point, f64)>,
        points: Vec<Point>,
    ) {
        self.cancel_area();
        self.marks_friend.set_value(None);
        self.area_marks.set(Vec::new());
        self.map_friend_selected.set(None);
        self.areas_open.set(false);
        self.places_open.set(false);
        self.friends_open.set(false);
        self.trips_open.set(false);
        self.settings_open.set(false);
        self.target.set(None);
        self.follow.set(false);
        self.message.set(String::new());
        self.area_name.set(name);
        self.area_history.set_value(Vec::new());
        self.setup_circle.set_value(None);
        if circle.is_none() && points.len() >= 3 {
            self.fit(points.iter().copied());
            self.area_points.set(points);
            self.area_drawing.set(true);
        } else {
            self.place_circle(circle.unwrap_or_else(|| (self.map_center(), 200.)));
        }
        self.setup.set(Some(setup));
    }

    fn place_circle(self, (center, radius): (Point, f64)) {
        self.area_drawing.set(false);
        self.editing_circle.set(Some((center, radius)));
        self.fit(circle_outline(center, radius));
        // Room around the circle for its edge handle.
        self.camera.update(|c| c.zoom = (c.zoom - 0.65).max(0.));
    }

    pub fn setup_use_circle(self) {
        if self.editing_circle.with_untracked(Option::is_none) {
            let circle = self.setup_circle.get_value();
            self.place_circle(circle.unwrap_or_else(|| (self.map_center(), 200.)));
        }
    }

    /// Corners drawn earlier in this setup stay.
    pub fn setup_use_drawing(self) {
        let Some(circle) = self.editing_circle.get_untracked() else {
            return;
        };
        self.setup_circle.set_value(Some(circle));
        self.editing_circle.set(None);
        self.area_history.set_value(Vec::new());
        self.area_drawing.set(true);
    }

    /// Setup's shape: circle, else drawn corners.
    pub fn setup_shape(self) -> (Option<(Point, f64)>, Vec<Point>) {
        match self.editing_circle.get_untracked() {
            Some(circle) => (Some(circle), Vec::new()),
            None => (None, self.area_points.get_untracked()),
        }
    }

    pub fn close_setup(self) {
        self.setup.set(None);
        self.editing_circle.set(None);
        self.area_drawing.set(false);
        self.area_points.set(Vec::new());
        self.area_history.set_value(Vec::new());
        self.message.set(String::new());
    }

    pub fn new_area(self) {
        self.open_setup(
            Setup {
                title: "New area",
                done: "Save area",
                finish: std::rc::Rc::new(move || self.save_setup_area(None)),
                cancel: std::rc::Rc::new(move || {
                    self.close_setup();
                    self.areas_open.set(true);
                }),
            },
            String::new(),
            None,
            Vec::new(),
        );
    }

    pub fn edit_area(self, id: u64) {
        let Some(area) = self
            .data
            .with_untracked(|d| d.areas.iter().find(|a| a.id == id).cloned())
        else {
            return;
        };
        self.open_setup(
            Setup {
                title: "Edit area",
                done: "Save area",
                finish: std::rc::Rc::new(move || self.save_setup_area(Some(id))),
                cancel: std::rc::Rc::new(move || {
                    self.close_setup();
                    self.show_area(id);
                }),
            },
            area.name,
            area.circle,
            area.points,
        );
    }

    /// Saves setup's shape as saved area `id` (new when none), then measures it.
    fn save_setup_area(self, id: Option<u64>) {
        let (circle, points) = self.setup_shape();
        let points = circle.map_or(points, |(center, radius)| circle_outline(center, radius));
        let name = self.area_name.get_untracked().trim().to_owned();
        let command = match id {
            Some(id) => {
                json!({"op":"update_area","area_id":id,"name":name,"points":points,"circle":circle})
            }
            None => json!({"op":"save_area","name":name,"points":points,"circle":circle}),
        };
        zgui::reactive::spawn_detached(async move {
            match self.request(command).await {
                Ok(_) => {
                    // New area takes next id, as journal assigns it.
                    let id = id.unwrap_or_else(|| {
                        self.data
                            .with_untracked(|d| d.areas.iter().map(|a| a.id).max().unwrap_or(0) + 1)
                    });
                    self.close_setup();
                    self.refresh(true);
                    self.area_id.set(Some(id));
                    self.fit(points.iter().copied());
                    self.area_points.set(points);
                    self.compute_area();
                }
                Err(error) => self.message.set(error),
            }
        });
    }

    /// Remembers corners before a change.
    pub fn push_area_history(self) {
        let points = self.area_points.get_untracked();
        self.area_history.update_value(|h| h.push(points));
    }

    pub fn undo_area(self) {
        let mut previous = None;
        self.area_history.update_value(|h| previous = h.pop());
        if let Some(points) = previous {
            self.area_points.set(points);
        }
    }

    /// Adds corner where it lengthens outline least, so tapping near an edge splits it.
    fn add_corner(self, p: Point) {
        self.push_area_history();
        self.area_points.update(|points| {
            if points.len() < 3 {
                points.push(p);
                return;
            }
            let cost = |i: usize| {
                let (a, b) = (points[i], points[(i + 1) % points.len()]);
                a.distance(p) + p.distance(b) - a.distance(b)
            };
            let best = (0..points.len())
                .min_by(|&i, &j| cost(i).total_cmp(&cost(j)))
                .unwrap_or(0);
            points.insert(best + 1, p);
        });
    }

    pub fn move_corner(self, index: usize, p: Point) {
        self.area_points.update(|points| {
            if let Some(corner) = points.get_mut(index) {
                *corner = p;
            }
        });
    }

    /// History was pushed when the press began.
    pub fn remove_corner(self, index: usize) {
        self.area_points.update(|points| {
            if index < points.len() {
                points.remove(index);
            }
        });
    }

    /// Loads all roads inside area and measures traveled share for current selection.
    pub fn compute_area(self) {
        let points = self.area_points.get_untracked();
        let filter = self.history_filter.get_untracked();
        self.area_busy.set(true);
        let directory = self.directory.get_value();
        zgui::reactive::spawn_detached(async move {
            // Decode on worker: large areas return thousands of sections.
            let result = blocking(move || {
                crate::backend::request(
                    directory,
                    json!({"op":"area","points":points,"trip_filter":filter}),
                )
                .and_then(|v| serde_json::from_value::<Stats>(v).map_err(|e| e.to_string()))
            })
            .await;
            self.area_busy.set(false);
            match result {
                Ok(stats) => {
                    if let Some(id) = self.area_id.get_untracked()
                        && stats.total_m > 0.0
                    {
                        let share = stats.traveled_m / stats.total_m * 100.0;
                        self.area_percent.update(|p| {
                            p.insert(id, share);
                        });
                    }
                    self.area_stats.set(Some(stats));
                    // Area may have loaded new road tiles.
                    self.refresh(true);
                }
                Err(error) => self.message.set(error),
            }
        });
    }

    pub fn show_area(self, id: u64) {
        let Some(points) = self.data.with_untracked(|d| {
            d.areas
                .iter()
                .find(|a| a.id == id)
                .map(|a| a.points.clone())
        }) else {
            return;
        };
        self.areas_open.set(false);
        self.marks_friend.set_value(None);
        self.area_marks.set(Vec::new());
        self.area_drawing.set(false);
        self.area_id.set(Some(id));
        self.area_points.set(points.clone());
        self.fit(points);
        self.compute_area();
    }

    /// Every saved area outlined with its name; none yet opens the Areas list instead.
    pub fn show_areas(self) {
        let marks: Vec<Mark> = self.data.with_untracked(|d| {
            d.areas
                .iter()
                .map(|a| Mark {
                    alert: false,
                    id: a.id,
                    name: a.name.clone(),
                    outline: a.points.clone(),
                })
                .collect()
        });
        if marks.is_empty() {
            self.areas_open.set(true);
            return;
        }
        self.show_marks(marks);
    }

    /// View-only map of `marks`: other area tools, friend card and pages closed.
    pub fn show_marks(self, marks: Vec<Mark>) {
        self.cancel_area();
        self.fit(marks.iter().flat_map(|m| m.outline.iter().copied()));
        self.marks_friend.set_value(None);
        self.area_marks.set(marks);
        self.map_friend_selected.set(None);
        self.message.set(String::new());
        self.areas_open.set(false);
        self.places_open.set(false);
        self.friends_open.set(false);
        self.trips_open.set(false);
        self.settings_open.set(false);
    }

    pub fn delete_area(self, id: u64) {
        self.send(json!({"op":"delete_area","area_id":id}), "Area deleted.");
    }

    /// Journal command, then refresh; `done` shows on success.
    pub fn send(self, command: Value, done: &'static str) {
        zgui::reactive::spawn_detached(async move {
            match self.request(command).await {
                Ok(_) => {
                    self.message.set(done.into());
                    self.refresh(true);
                }
                Err(error) => self.message.set(error),
            }
        });
    }

    /// Map tap: adds area corner while drawing, otherwise describes traveled road under it.
    pub fn tap(self, point: Point, meters_per_px: f64) {
        if self.area_drawing.get_untracked() {
            self.add_corner(point);
            return;
        }
        // Tap off a friend's pin drops its highlight and card; empty map also clears message.
        self.map_friend_selected.set(None);
        let reach = (24.0 * meters_per_px).max(8.0);
        let hit = self.sections.with_untracked(|sections| {
            sections
                .iter()
                .map(|s| {
                    let d =
                        segment_distance([0.0, 0.0], local(point, s.start), local(point, s.end));
                    (s, d)
                })
                .filter(|(_, d)| *d <= reach)
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(s, _)| s.clone())
        });
        let Some(section) = hit else {
            self.message.set(String::new());
            return;
        };
        let first = section.first_trip.and_then(|id| {
            self.data.with_untracked(|d| {
                d.trips
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| format!(" · first {} (trip #{id})", date(t.started_ms)))
            })
        });
        let trips = match section.visits {
            0 => "pieced together from several trips".to_string(),
            1 => "1 trip".into(),
            n => format!("{n} trips"),
        };
        self.message.set(format!(
            "{} · {trips}{}",
            road_name(&section.road_id),
            first.unwrap_or_default()
        ));
    }

    /// Finds closest untraveled road section to live position (or map center).
    pub fn nearest_road(self) {
        let from = self
            .position
            .get_untracked()
            .map(|p| p.0)
            .unwrap_or_else(|| {
                let [lon, lat] =
                    crate::mapping::camera::geographic(self.camera.get_untracked().center);
                Point { lat, lon }
            });
        let filter = self.history_filter.get_untracked();
        self.message.set("Finding nearest untraveled road…".into());
        zgui::reactive::spawn_detached(async move {
            let result = self
                .request(json!({"op":"nearest","lat":from.lat,"lon":from.lon,"trip_filter":filter}))
                .await;
            match result.and_then(|v| {
                Ok((
                    serde_json::from_value::<Section>(v["section"].clone())
                        .map_err(|e| e.to_string())?,
                    v["distance_m"].as_f64().unwrap_or(0.0),
                ))
            }) {
                Ok((section, meters)) => {
                    self.message.set(format!(
                        "Nearest untraveled: {} · {} away",
                        road_name(&section.road_id),
                        self.distance(meters)
                    ));
                    self.fit([from, section.start, section.end]);
                    // Section is ~20 m: stay at street level, not maximum zoom.
                    self.camera.update(|c| c.zoom = c.zoom.min(17.0));
                    self.target.set(Some(section));
                    self.refresh(true);
                }
                Err(error) => self.message.set(error),
            }
        });
    }

    pub fn backup(self) {
        self.settings_open.set(false);
        #[cfg(any(target_os = "android", target_os = "ios"))]
        if let Err(error) = crate::platform::backup(self.directory.get_value()) {
            self.message.set(error);
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        zgui::reactive::spawn_detached(async move {
            let path = self.directory.get_value().join("roamlog-backup.jsonl");
            let result = match self.request(json!({"op":"backup"})).await {
                Ok(v) => {
                    let journal = v["journal"].as_str().unwrap_or_default().to_owned();
                    let target = path.clone();
                    blocking(move || std::fs::write(target, journal))
                        .await
                        .map_err(|e| e.to_string())
                }
                Err(error) => Err(error),
            };
            self.message.set(match result {
                Ok(()) => format!("Backup saved to {}", path.display()),
                Err(error) => error,
            });
        });
    }

    /// Replaces all trips, categories and areas with picked backup.
    pub fn restore(self) {
        self.confirm_restore.set(false);
        self.settings_open.set(false);
        self.history_filter.set(TripFilter::All);
        #[cfg(any(target_os = "android", target_os = "ios"))]
        if let Err(error) = crate::platform::restore(self.directory.get_value()) {
            self.message.set(error);
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        zgui::reactive::spawn_detached(async move {
            let directory = self.directory.get_value();
            let copied = blocking(move || {
                let backup = directory.join("roamlog-backup.jsonl");
                let backup = if backup.exists() {
                    backup
                } else {
                    directory.join("traveler-backup.jsonl")
                };
                std::fs::copy(backup, directory.join("trips.jsonl.restore"))
            })
            .await;
            let result = match copied {
                Ok(_) => self.request(json!({"op":"restore"})).await,
                Err(error) => Err(format!("No roamlog-backup.jsonl in data folder: {error}")),
            };
            match result {
                Ok(v) => {
                    self.message.set(format!(
                        "Restored {} trips.",
                        v["trips"].as_u64().unwrap_or(0)
                    ));
                    self.refresh(true);
                }
                Err(error) => self.message.set(error),
            }
        });
    }

    pub fn edit_trip(self, id: u64) {
        self.confirm_delete.set(None);
        self.trips_open.set(false);
        self.editing.set(Some(id));
        self.edit_open.set(true);
        self.show_trip(id);
    }
}

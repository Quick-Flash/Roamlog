use super::{date, road_name};
use crate::ui::state::UiState;
use crate::{
    area::Stats,
    coverage::Section,
    geo::{Point, local, segment_distance},
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
                .sum()
        })
    }

    pub fn start_area(self) {
        self.area_points.set(Vec::new());
        self.area_id.set(None);
        self.begin_editing();
    }

    /// Edits corners of shown area; stats return after Done.
    pub fn edit_area(self) {
        self.begin_editing();
    }

    fn begin_editing(self) {
        self.area_history.set_value(Vec::new());
        self.area_stats.set(None);
        self.area_open.set(false);
        self.target.set(None);
        self.area_drawing.set(true);
        self.follow.set(false);
        self.message.set(
            "Tap map to add corners · drag dot to move · tap dot to remove. Drag map to pan."
                .into(),
        );
    }

    pub fn cancel_area(self) {
        self.area_drawing.set(false);
        self.area_points.set(Vec::new());
        self.area_id.set(None);
        self.area_stats.set(None);
        self.area_open.set(false);
        self.message.set(String::new());
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

    pub fn finish_area(self) {
        if self.area_points.with_untracked(Vec::len) < 3 {
            self.message.set("Area needs at least 3 corners.".into());
            return;
        }
        self.area_drawing.set(false);
        self.area_name.set(String::new());
        self.message.set(String::new());
        self.compute_area(true);
    }

    /// Loads all roads inside area and measures traveled share for current selection.
    pub fn compute_area(self, open: bool) {
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
                    if open {
                        self.area_open.set(true);
                    }
                    // Area may have loaded new road tiles.
                    self.refresh(true);
                }
                Err(error) => self.message.set(error),
            }
        });
    }

    pub fn save_area(self) {
        let name = self.area_name.get_untracked();
        let points = self.area_points.get_untracked();
        self.area_id.set(None);
        self.send(
            json!({"op":"save_area","name":name,"points":points}),
            "Area saved.",
        );
    }

    pub fn update_area(self) {
        let Some(id) = self.area_id.get_untracked() else {
            return;
        };
        let points = self.area_points.get_untracked();
        self.send(
            json!({"op":"update_area","area_id":id,"points":points}),
            "Area updated.",
        );
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
        self.area_drawing.set(false);
        self.area_id.set(Some(id));
        self.area_points.set(points.clone());
        self.fit(points);
        self.compute_area(true);
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
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::backup(self.directory.get_value()) {
            self.message.set(error);
        }
        #[cfg(not(target_os = "android"))]
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
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::restore(self.directory.get_value()) {
            self.message.set(error);
        }
        #[cfg(not(target_os = "android"))]
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
        self.trips_open.set(false);
        self.editing.set(Some(id));
        self.edit_open.set(true);
        self.show_trip(id);
    }
}

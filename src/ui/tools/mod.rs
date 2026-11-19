//! Area controls, statistics, trip editing, and journal actions.
mod actions;
mod area_bar;
mod area_dialog;
mod area_setup;
mod areas_sheet;
mod format;
mod stats;
mod trip_editor;
mod trip_editor_dialog;

pub use area_bar::AreaBarProps;
pub use area_dialog::AreaDialogProps;
pub use area_setup::{AreaSetupProps, shape_preview};
pub use areas_sheet::AreasSheetProps;
pub use format::{clock, date, road_name};
pub use stats::StatsTabProps;
pub use trip_editor_dialog::TripEditorDialogProps;

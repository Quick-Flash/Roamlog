//! Map control icons as filled outlines (24-unit square). Overlapping parts share winding;
//! ring counters wind opposite, so non-zero fill leaves them empty.
use zgui_ui_icons::prelude::IconData;

/// Settings: three slider bars with knobs.
pub const SLIDERS: IconData = IconData::new(
    "sliders",
    24.0,
    "M3 5h18v2H3z M5.5 6a2.5 2.5 0 1 1 5 0a2.5 2.5 0 1 1 -5 0z \
     M3 11h18v2H3z M13.5 12a2.5 2.5 0 1 1 5 0a2.5 2.5 0 1 1 -5 0z \
     M3 17h18v2H3z M7.5 18a2.5 2.5 0 1 1 5 0a2.5 2.5 0 1 1 -5 0z",
);

/// Follow location: ring, centre dot, four ticks.
pub const LOCATE: IconData = IconData::new(
    "locate",
    24.0,
    "M4 12a8 8 0 1 1 16 0a8 8 0 1 1 -16 0z M6 12a6 6 0 1 0 12 0a6 6 0 1 0 -12 0z \
     M9 12a3 3 0 1 1 6 0a3 3 0 1 1 -6 0z \
     M11 1h2v3h-2z M11 20h2v3h-2z M1 11h3v2H1z M20 11h3v2h-3z",
);

/// Nearest new road: signpost.
pub const EXPLORE: IconData = IconData::new(
    "explore",
    24.0,
    "M11 2h2v20h-2z M13 4h6l2.5 2.75L19 9.5h-6z M5 11h6v5.5H5l-2.5-2.75z",
);

/// North: solid north half, hollow south half; button rotates it with map.
pub const COMPASS: IconData = IconData::new(
    "compass",
    24.0,
    "M12 2L16.5 12H7.5z M7.5 12H16.5L12 22z M9.9 13.3L12 18.2L14.1 13.3z",
);

/// Fit: four frame corners.
pub const FIT: IconData = IconData::new(
    "fit",
    24.0,
    "M3 3h7v2H5v5H3z M14 3h7v7h-2V5h-5z M3 14h2v5h5v2H3z M19 14h2v7h-7v-2h5z",
);

/// Find friends: two heads and shoulders.
pub const PEOPLE: IconData = IconData::new(
    "people",
    24.0,
    "M5.5 8a3.5 3.5 0 1 1 7 0a3.5 3.5 0 1 1 -7 0z M1 20c0-4 3-6.5 8-6.5s8 2.5 8 6.5z \
     M14 5.2a3.2 3.2 0 1 1 3 5.6a5.5 5.5 0 0 0 -3 -5.6z M17.5 13.6c3.3.6 5.5 2.7 5.5 6.4h-4.2c0-2.6-.5-4.7-1.3-6.4z",
);

pub const HISTORY: IconData = IconData::new(
    "history",
    24.0,
    "M2 12a10 10 0 1 1 20 0a10 10 0 1 1 -20 0z M4 12a8 8 0 1 0 16 0a8 8 0 1 0 -16 0z M11 6h2v5l4 3-1 2-5-4z",
);

/// Two stacked map layers.
pub const LAYERS: IconData = IconData::new(
    "layers",
    24.0,
    "M12 2L2 8l10 6 10-6z M12 4.3L18.1 8 12 11.7 5.9 8z M2.5 12.5l9.5 5.7 9.5-5.7 1 1.7L12 20.5 1.5 14.2z",
);

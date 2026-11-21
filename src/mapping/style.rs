//! Basemap style for Protomaps v4 layers. Colors, road widths per zoom, and label colors
//! follow Organic Maps' default light/dark styles (data/styles/default, Apache-2.0); see NOTICE.
use crate::mvt::{Feature, Layer};

pub enum Paint {
    /// `alpha` stops (zoom, opacity); empty means opaque.
    Fill {
        min_zoom: f64,
        alpha: &'static [(u8, f64)],
    },
    /// Width stops (zoom, CSS px) from Organic Maps Roads.mapcss; hidden below first stop.
    Line(&'static [(u8, f64)]),
}

pub struct Style {
    light: u32,
    dark: u32,
    pub paint: Paint,
}

impl Style {
    /// Color at display zoom, with fill opacity stops applied.
    pub fn rgba(&self, dark: bool, zoom: u8) -> [u8; 4] {
        let mut c = rgba(if dark { self.dark } else { self.light });
        if let Paint::Fill { alpha, .. } = self.paint
            && let Some(&(_, a)) = alpha.iter().rev().find(|s| s.0 <= zoom)
        {
            c[3] = (a * 255.0) as u8;
        }
        c
    }

    /// Visible at integer display zoom?
    pub fn visible(&self, zoom: u8) -> bool {
        match self.paint {
            Paint::Fill { min_zoom, .. } => f64::from(zoom) >= min_zoom,
            Paint::Line(stops) => zoom >= stops[0].0,
        }
    }

    /// Line width at display zoom; stops past last keep last width.
    pub fn width(&self, zoom: u8) -> Option<f64> {
        let Paint::Line(stops) = self.paint else {
            return None;
        };
        stops.iter().rev().find(|s| s.0 <= zoom).map(|s| s.1)
    }
}

pub fn rgba(rgb: u32) -> [u8; 4] {
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255]
}

pub const BACKGROUND: Style = fill(0xE3E1D2, 0x0f0f0f, 0.0);

/// (text, halo) for light and dark: place names, road names.
pub const PLACE_LABEL: [(u32, u32); 2] = [(0x666666, 0xFFFFFF), (0x999999, 0x060600)];
pub const ROAD_LABEL: [(u32, u32); 2] = [(0x333333, 0xEDEBDB), (0x919191, 0x111100)];

const fn fill(light: u32, dark: u32, min_zoom: f64) -> Style {
    Style {
        light,
        dark,
        paint: Paint::Fill {
            min_zoom,
            alpha: &[],
        },
    }
}

const fn line(light: u32, dark: u32, stops: &'static [(u8, f64)]) -> Style {
    Style {
        light,
        dark,
        paint: Paint::Line(stops),
    }
}

const MOTORWAY: &[(u8, f64)] = &[
    (6, 0.8),
    (7, 0.9),
    (8, 1.1),
    (9, 1.2),
    (10, 1.5),
    (11, 1.7),
    (12, 1.9),
    (13, 2.8),
    (14, 3.8),
    (15, 4.4),
    (16, 6.0),
    (17, 8.2),
    (18, 11.0),
    (19, 13.5),
];
const PRIMARY: &[(u8, f64)] = &[
    (8, 0.7),
    (9, 0.8),
    (10, 1.2),
    (11, 1.5),
    (12, 1.7),
    (13, 2.3),
    (14, 3.2),
    (15, 3.8),
    (16, 5.0),
    (17, 6.6),
    (18, 9.4),
    (19, 11.6),
];
const SECONDARY: &[(u8, f64)] = &[
    (10, 1.2),
    (11, 1.5),
    (12, 1.7),
    (13, 2.3),
    (14, 3.2),
    (15, 3.8),
    (16, 5.0),
    (17, 6.6),
    (18, 9.4),
    (19, 11.6),
];
const TERTIARY: &[(u8, f64)] = &[
    (11, 0.8),
    (12, 1.3),
    (13, 1.8),
    (14, 2.7),
    (15, 3.8),
    (16, 5.0),
    (17, 6.6),
    (18, 9.4),
    (19, 11.6),
];
const RESIDENTIAL: &[(u8, f64)] = &[
    (12, 0.7),
    (13, 1.0),
    (14, 1.6),
    (15, 2.4),
    (16, 3.2),
    (17, 4.7),
    (18, 6.4),
    (19, 8.2),
];
const SERVICE: &[(u8, f64)] = &[(15, 1.2), (16, 1.6), (17, 2.0), (18, 3.0), (19, 4.0)];
const FOOTWAY: &[(u8, f64)] = &[(15, 1.5), (16, 2.0), (17, 2.4), (18, 3.2), (19, 4.2)];
const PATH: &[(u8, f64)] = &[
    (14, 0.9),
    (15, 1.1),
    (16, 1.5),
    (17, 2.0),
    (18, 2.8),
    (19, 3.7),
];
const RIVER: &[(u8, f64)] = &[(11, 0.8), (14, 1.6), (17, 3.0)];
const RAIL: &[(u8, f64)] = &[(12, 0.8), (14, 1.2), (17, 2.0)];
const BORDER: &[(u8, f64)] = &[(0, 1.0)];

/// First road-layer slot (path..motorway): satellite overlay draws these only.
pub const FIRST_ROAD: usize = 17;

/// Paint order: index 0 drawn first.
pub const STYLES: [Style; 26] = [
    fill(0xCFD5A8, 0x1f2310, 0.0),  // 0 grassland/scrub (green0)
    fill(0xAABF65, 0x363d19, 0.0),  // 1 forest
    fill(0xE4DEAD, 0x2e2b0f, 0.0),  // 2 farmland
    fill(0xD9D2BF, 0x302A2A, 0.0),  // 3 bare rock
    fill(0xFFFFFF, 0x111111, 0.0),  // 4 glacier
    fill(0xC6CF93, 0x262a13, 0.0),  // 5 park (green1)
    fill(0xE0D8BE, 0x262222, 12.0), // 6 pedestrian area
    fill(0xF8E4BA, 0x2e1f05, 12.0), // 7 university/school
    fill(0xF8D9D9, 0x2b0808, 12.0), // 8 hospital
    fill(0xE4E2EB, 0x17151e, 12.0), // 9 industrial/aerodrome
    fill(0xDDD9B0, 0x24220f, 13.0), // 10 sport
    fill(0xF2F2BC, 0x232306, 15.0), // 11 parking
    fill(0xFCEC7E, 0x28281A, 12.0), // 12 beach
    fill(0x8BD3E0, 0x003636, 0.0),  // 13 water
    line(0x8BD3E0, 0x003636, RIVER),
    Style {
        light: 0xC9C8BB,
        dark: 0x1F1F1F,
        paint: Paint::Fill {
            min_zoom: 14.0,
            alpha: &[(14, 0.5), (15, 0.6), (16, 0.8), (17, 1.0)],
        },
    }, // 15 building
    line(0x73726B, 0x666666, BORDER),
    line(0xA05012, 0x944A12, PATH),        // 17 path/track
    line(0xf2f2f2, 0x454545, FOOTWAY),     // 18 footway/cycleway
    line(0x969586, 0x44374A, RAIL),        // 19 rail
    line(0xf2f2f2, 0x454545, SERVICE),     // 20 service
    line(0xf2f2f2, 0x454545, RESIDENTIAL), // 21 residential
    line(0xf7f7f7, 0x4d4d4d, TERTIARY),    // 22 tertiary
    line(0xFFB226, 0x754b00, SECONDARY),   // 23 secondary
    line(0xFF8726, 0x753400, PRIMARY),     // 24 primary
    line(0xFF7326, 0x752900, MOTORWAY),    // 25 motorway/trunk
];

/// Style slot for feature, or None when not drawn.
pub fn classify(layer: &Layer, f: &Feature) -> Option<usize> {
    let kind = layer.str(f, "kind");
    let area = f.kind == 3;
    Some(match (layer.name.as_str(), area) {
        ("landcover", true) => match kind {
            "grassland" | "scrub" => 0,
            "forest" => 1,
            "farmland" => 2,
            "barren" => 3,
            "glacier" => 4,
            _ => return None,
        },
        ("landuse", true) => match kind {
            "grass" | "meadow" | "scrub" | "cemetery" | "grave_yard" => 0,
            "wood" | "forest" => 1,
            "farmland" | "farmyard" | "orchard" | "allotments" | "vineyard" => 2,
            "park" | "garden" | "golf_course" | "nature_reserve" | "national_park"
            | "recreation_ground" | "village_green" | "playground" | "dog_park" => 5,
            "pedestrian" | "platform" => 6,
            "school" | "university" | "college" | "kindergarten" => 7,
            "hospital" => 8,
            "industrial" | "railway" | "aerodrome" | "runway" | "taxiway" | "apron"
            | "military" | "quarry" => 9,
            "pitch" | "stadium" | "sports_centre" | "track" => 10,
            "parking" => 11,
            "beach" | "sand" => 12,
            _ => return None,
        },
        ("water", true) => 13,
        ("water", false) if f.kind == 2 => 14,
        ("buildings", true) => 15,
        ("boundaries", false) => 16,
        ("roads", false) => road(kind, layer.str(f, "kind_detail"))?,
        _ => return None,
    })
}

fn road(kind: &str, detail: &str) -> Option<usize> {
    let detail = detail.trim_end_matches("_link");
    Some(match (kind, detail) {
        ("rail", _) => 19,
        (_, "motorway" | "trunk") | ("highway", _) => 25,
        (_, "primary") => 24,
        (_, "secondary") => 23,
        (_, "tertiary" | "unclassified") | ("medium_road", _) => 22,
        (_, "residential" | "living_street" | "road") => 21,
        (_, "service") => 20,
        (
            _,
            "footway" | "pedestrian" | "steps" | "cycleway" | "corridor" | "sidewalk" | "crossing",
        ) => 18,
        (_, "path" | "track" | "bridleway") => 17,
        ("major_road", _) => 24,
        ("minor_road", _) => 21,
        ("path", _) => 18,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_follow_organic_maps_stops() {
        let primary = &STYLES[24];
        assert!(!primary.visible(7));
        assert_eq!(primary.width(14), Some(3.2));
        assert_eq!(primary.width(22), Some(11.6));
        assert_eq!(STYLES[13].width(14), None);
    }
}

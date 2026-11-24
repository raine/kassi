//! A store's indoor map from S-ryhmä's map server, a walking order through
//! it, and a drawing of that order.
//!
//! Map coordinates are longitude and latitude in a frame of the map
//! server's own; only distances within one store are meaningful. They are
//! converted to metres from the map's north-west corner, with y growing
//! southwards as in SVG.

use std::collections::HashMap;
use std::fmt::Write as _;

use anyhow::{Context, Result};
use serde_json::Value;

/// Point-of-interest types that the map server marks as where a route
/// starts and ends.
const ROUTE_START_POI: &str = "5ab9d2eb46e0fb000589808a";
const ROUTE_END_POI: &str = "5ab9d3125908010006d16e37";
/// Plain entrance and checkout points, used when a store has no route
/// start or end.
const ENTRANCE_POI: &str = "5ac47a15cff47e0006271d7e";
const CHECKOUT_POI: &str = "5ac479eccff47e0006271d7d";

/// Stops closer than this, in metres, are walked as one.
const SAME_STOP_METRES: f64 = 3.0;

const METRES_PER_DEGREE_LAT: f64 = 110_540.0;
const METRES_PER_DEGREE_LON_AT_EQUATOR: f64 = 111_320.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    /// Walking distance along a grid of aisles.
    fn walk(self, other: Point) -> f64 {
        (self.x - other.x).abs() + (self.y - other.y).abs()
    }
}

struct Area {
    name: String,
    rings: Vec<Vec<Point>>,
}

pub struct StoreMap {
    departments: Vec<Area>,
    shelves: Vec<Vec<Point>>,
    /// Centre of each shelf section by its code.
    sections: HashMap<String, Point>,
    entrance: Option<Point>,
    checkout: Option<Point>,
    width: f64,
    height: f64,
}

/// Something to pick up: the names of list items and the shelf sections
/// they are on.
pub struct Wanted {
    pub names: Vec<String>,
    pub sections: Vec<String>,
}

pub struct Stop {
    pub names: Vec<String>,
    pub point: Point,
    /// Walking distance from the previous stop, or from the entrance.
    pub metres_from_previous: f64,
}

pub struct Route {
    pub stops: Vec<Stop>,
    /// Items whose sections are not on the map.
    pub unplaced: Vec<String>,
    /// Walking distance from the entrance through every stop to the
    /// checkout.
    pub total_metres: f64,
}

impl StoreMap {
    /// Builds the map from the `departments`, `sections`, and `pois` layers.
    pub fn from_layers(departments: &Value, sections: &Value, pois: &Value) -> Result<Self> {
        let origin = Origin::of(departments).context("the store map has no departments")?;
        let departments = features(departments)
            .map(|feature| Area {
                name: feature["properties"]["name"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
                rings: rings(&feature["geometry"], &origin),
            })
            .collect();
        let mut shelves = Vec::new();
        let mut centres: HashMap<String, Vec<Point>> = HashMap::new();
        for feature in features(sections) {
            for ring in rings(&feature["geometry"], &origin) {
                if let Some(name) = feature["properties"]["name"].as_str() {
                    centres
                        .entry(name.to_owned())
                        .or_default()
                        .push(centre(&ring));
                }
                shelves.push(ring);
            }
        }
        let sections = centres
            .into_iter()
            .map(|(name, points)| (name, centre(&points)))
            .collect();
        let poi = |type_id: &str| {
            features(pois)
                .find(|feature| feature["properties"]["typeId"] == type_id)
                .and_then(|feature| point(&feature["geometry"]["coordinates"], &origin))
        };
        let mut map = Self {
            departments,
            shelves,
            sections,
            entrance: poi(ROUTE_START_POI).or_else(|| poi(ENTRANCE_POI)),
            checkout: poi(ROUTE_END_POI).or_else(|| poi(CHECKOUT_POI)),
            width: 0.0,
            height: 0.0,
        };
        let all = map
            .departments
            .iter()
            .flat_map(|area| area.rings.iter().flatten())
            .chain(map.shelves.iter().flatten());
        for p in all {
            map.width = map.width.max(p.x);
            map.height = map.height.max(p.y);
        }
        Ok(map)
    }

    /// Orders the wanted items into a walk from the entrance to the
    /// checkout: nearest stop first, then improved by reversing stretches
    /// of the walk while that shortens it.
    pub fn route(&self, wanted: &[Wanted]) -> Route {
        let mut stops: Vec<(Vec<String>, Point)> = Vec::new();
        let mut unplaced = Vec::new();
        for item in wanted {
            let points: Vec<Point> = item
                .sections
                .iter()
                .filter_map(|code| self.sections.get(code.trim()).copied())
                .collect();
            if points.is_empty() {
                unplaced.extend(item.names.iter().cloned());
                continue;
            }
            let at = centre(&points);
            match stops
                .iter_mut()
                .find(|(_, p)| p.walk(at) <= SAME_STOP_METRES)
            {
                Some((names, _)) => names.extend(item.names.iter().cloned()),
                None => stops.push((item.names.clone(), at)),
            }
        }

        let start = self.entrance.unwrap_or(Point { x: 0.0, y: 0.0 });
        let end = self.checkout.unwrap_or(start);
        let mut order = nearest_first(start, &stops);
        improve(start, end, &stops, &mut order);

        let mut previous = start;
        let mut total = 0.0;
        let stops: Vec<Stop> = order
            .into_iter()
            .map(|i| {
                let (names, point) = &stops[i];
                let metres = previous.walk(*point);
                total += metres;
                previous = *point;
                Stop {
                    names: names.clone(),
                    point: *point,
                    metres_from_previous: metres,
                }
            })
            .collect();
        total += previous.walk(end);
        Route {
            stops,
            unplaced,
            total_metres: total,
        }
    }

    /// Draws the floor plan with the route as SVG, in metres.
    pub fn svg(&self, route: &Route) -> String {
        const PAD: f64 = 4.0;
        let (w, h) = (self.width + 2.0 * PAD, self.height + 2.0 * PAD);
        let mut svg = String::new();
        let _ = write!(
            svg,
            r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{x} {y} {w:.1} {h:.1}" width="{pw:.0}" height="{ph:.0}" font-family="DejaVu Sans, Helvetica, Arial, sans-serif">"##,
            x = -PAD,
            y = -PAD,
            pw = 1400.0,
            ph = 1400.0 * h / w,
        );
        let _ = write!(
            svg,
            r##"<rect x="{x}" y="{y}" width="{w:.1}" height="{h:.1}" fill="#ffffff"/>"##,
            x = -PAD,
            y = -PAD
        );
        for area in &self.departments {
            let _ = write!(
                svg,
                r##"<path d="{}" fill="#f1f3f4" stroke="#b0b7bb" stroke-width="0.3"/>"##,
                path(&area.rings)
            );
        }
        let _ = write!(
            svg,
            r##"<path d="{}" fill="#d5dadd"/>"##,
            path(&self.shelves)
        );
        for area in &self.departments {
            let Some(at) = area.rings.first().map(|ring| centre(ring)) else {
                continue;
            };
            if area.name.is_empty() {
                continue;
            }
            let _ = write!(
                svg,
                r##"<text x="{:.1}" y="{:.1}" font-size="2.6" fill="#7a8489" text-anchor="middle">{}</text>"##,
                at.x,
                at.y,
                escape(&area.name)
            );
        }

        let start = self.entrance.unwrap_or(Point { x: 0.0, y: 0.0 });
        let end = self.checkout.unwrap_or(start);
        let mut line = format!("M{:.1} {:.1}", start.x, start.y);
        for stop in &route.stops {
            // Walk along the aisle grid: first across, then along.
            let _ = write!(line, " H{:.1} V{:.1}", stop.point.x, stop.point.y);
        }
        let _ = write!(line, " H{:.1} V{:.1}", end.x, end.y);
        let _ = write!(
            svg,
            r##"<path d="{line}" fill="none" stroke="#00a651" stroke-width="1.1" stroke-linejoin="round" stroke-linecap="round" opacity="0.85"/>"##
        );
        let mut marker = |p: Point, label: &str, colour: &str| {
            let _ = write!(
                svg,
                r##"<g><rect x="{:.1}" y="{:.1}" width="{:.1}" height="4" rx="1" fill="{colour}"/><text x="{:.1}" y="{:.1}" font-size="2.6" font-weight="bold" fill="#ffffff" text-anchor="middle">{label}</text></g>"##,
                p.x - label.len() as f64 * 0.9,
                p.y - 2.0,
                label.len() as f64 * 1.8,
                p.x,
                p.y + 0.9,
            );
        };
        marker(start, "SISÄÄN", "#005c2f");
        marker(end, "KASSA", "#005c2f");
        for (i, stop) in route.stops.iter().enumerate() {
            let _ = write!(
                svg,
                r##"<circle cx="{x:.1}" cy="{y:.1}" r="2.4" fill="#e4002b" stroke="#ffffff" stroke-width="0.5"/><text x="{x:.1}" y="{ty:.1}" font-size="2.8" font-weight="bold" fill="#ffffff" text-anchor="middle">{n}</text>"##,
                x = stop.point.x,
                y = stop.point.y,
                ty = stop.point.y + 1.0,
                n = i + 1,
            );
        }
        svg.push_str("</svg>");
        svg
    }
}

/// Renders an SVG drawing to PNG, at its own pixel size.
pub fn png(svg: &str) -> Result<Vec<u8>> {
    let mut options = resvg::usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let tree = resvg::usvg::Tree::from_str(svg, &options).context("drawing the map")?;
    let size = tree.size().to_int_size();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .context("the map drawing has no size")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::default(),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().context("encoding the map as PNG")
}

/// The map's north-west corner, from which positions are measured.
struct Origin {
    lon: f64,
    lat: f64,
    metres_per_degree_lon: f64,
}

impl Origin {
    fn of(layer: &Value) -> Option<Self> {
        let mut lon = f64::INFINITY;
        let mut lat = f64::NEG_INFINITY;
        for feature in features(layer) {
            for ring in feature["geometry"]["coordinates"]
                .as_array()
                .into_iter()
                .flatten()
            {
                for pair in ring.as_array().into_iter().flatten() {
                    if let (Some(x), Some(y)) = (pair[0].as_f64(), pair[1].as_f64()) {
                        lon = lon.min(x);
                        lat = lat.max(y);
                    }
                }
            }
        }
        lon.is_finite().then(|| Self {
            lon,
            lat,
            metres_per_degree_lon: METRES_PER_DEGREE_LON_AT_EQUATOR * lat.to_radians().cos(),
        })
    }

    fn project(&self, lon: f64, lat: f64) -> Point {
        Point {
            x: (lon - self.lon) * self.metres_per_degree_lon,
            y: (self.lat - lat) * METRES_PER_DEGREE_LAT,
        }
    }
}

fn features(layer: &Value) -> impl Iterator<Item = &Value> {
    layer["features"].as_array().into_iter().flatten()
}

fn point(pair: &Value, origin: &Origin) -> Option<Point> {
    Some(origin.project(pair[0].as_f64()?, pair[1].as_f64()?))
}

/// The rings of a Polygon or MultiPolygon geometry.
fn rings(geometry: &Value, origin: &Origin) -> Vec<Vec<Point>> {
    let ring = |ring: &Value| -> Vec<Point> {
        ring.as_array()
            .into_iter()
            .flatten()
            .filter_map(|pair| point(pair, origin))
            .collect()
    };
    let coordinates = geometry["coordinates"].as_array().into_iter().flatten();
    match geometry["type"].as_str() {
        Some("Polygon") => coordinates.map(ring).collect(),
        Some("MultiPolygon") => coordinates
            .flat_map(|polygon| polygon.as_array().into_iter().flatten())
            .map(ring)
            .collect(),
        _ => Vec::new(),
    }
}

fn centre(points: &[Point]) -> Point {
    let n = points.len().max(1) as f64;
    Point {
        x: points.iter().map(|p| p.x).sum::<f64>() / n,
        y: points.iter().map(|p| p.y).sum::<f64>() / n,
    }
}

fn path(rings: &[Vec<Point>]) -> String {
    let mut d = String::new();
    for ring in rings {
        for (i, p) in ring.iter().enumerate() {
            let _ = write!(d, "{}{:.1} {:.1}", if i == 0 { "M" } else { "L" }, p.x, p.y);
        }
        d.push('Z');
    }
    d
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Visits the stops by always walking to the nearest one not yet visited.
fn nearest_first(start: Point, stops: &[(Vec<String>, Point)]) -> Vec<usize> {
    let mut left: Vec<usize> = (0..stops.len()).collect();
    let mut order = Vec::with_capacity(stops.len());
    let mut at = start;
    while !left.is_empty() {
        let (index, _) = left
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| at.walk(stops[**a].1).total_cmp(&at.walk(stops[**b].1)))
            .expect("left is not empty");
        let next = left.swap_remove(index);
        at = stops[next].1;
        order.push(next);
    }
    order
}

/// Reverses stretches of the walk while doing so shortens it (2-opt).
fn improve(start: Point, end: Point, stops: &[(Vec<String>, Point)], order: &mut [usize]) {
    let length = |order: &[usize]| {
        let mut at = start;
        let mut total = 0.0;
        for &i in order {
            total += at.walk(stops[i].1);
            at = stops[i].1;
        }
        total + at.walk(end)
    };
    let mut best = length(order);
    let mut improved = true;
    while improved {
        improved = false;
        for i in 0..order.len() {
            for j in i + 1..order.len() {
                order[i..=j].reverse();
                let candidate = length(order);
                if candidate + 1e-9 < best {
                    best = candidate;
                    improved = true;
                } else {
                    order[i..=j].reverse();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn square(lon: f64, lat: f64, size: f64) -> Value {
        json!([[
            [lon, lat],
            [lon + size, lat],
            [lon + size, lat - size],
            [lon, lat - size],
            [lon, lat]
        ]])
    }

    fn map() -> StoreMap {
        let departments = json!({ "features": [{
            "properties": { "name": "PÄIVITTÄISTAVARA" },
            "geometry": { "type": "Polygon", "coordinates": square(25.0, 62.0, 0.002) }
        }]});
        let section = |name: &str, lon: f64| {
            json!({
                "properties": { "name": name },
                "geometry": { "type": "Polygon", "coordinates": square(lon, 61.9995, 0.0001) }
            })
        };
        let sections = json!({ "features": [
            section("A1", 25.0002), section("B1", 25.0010), section("C1", 25.0018)
        ]});
        let poi = |type_id: &str, lon: f64| {
            json!({
                "properties": { "typeId": type_id },
                "geometry": { "type": "Point", "coordinates": [lon, 61.9985] }
            })
        };
        let pois =
            json!({ "features": [poi(ROUTE_START_POI, 25.0001), poi(ROUTE_END_POI, 25.0019)] });
        StoreMap::from_layers(&departments, &sections, &pois).unwrap()
    }

    fn wanted(name: &str, section: &str) -> Wanted {
        Wanted {
            names: vec![name.to_owned()],
            sections: vec![section.to_owned()],
        }
    }

    #[test]
    fn route_walks_from_entrance_to_checkout_and_lists_unknown_sections() {
        let map = map();
        let route = map.route(&[
            wanted("kahvi", "C1"),
            wanted("maito", "A1"),
            wanted("leipä", "B1"),
            wanted("lahja", "ZZ9"),
        ]);
        let names: Vec<_> = route.stops.iter().map(|s| s.names[0].as_str()).collect();
        assert_eq!(names, ["maito", "leipä", "kahvi"]);
        assert_eq!(route.unplaced, ["lahja"]);
        assert!(route.total_metres > 100.0, "{}", route.total_metres);
    }

    #[test]
    fn items_on_the_same_shelf_share_a_stop() {
        let route = map().route(&[wanted("maito", "A1"), wanted("rahka", "A1")]);
        assert_eq!(route.stops.len(), 1);
        assert_eq!(route.stops[0].names, ["maito", "rahka"]);
    }

    /// Draws a real store map to `$KASSI_MAP_DIR/route.{svg,png}` from the
    /// layers saved there as `departments.json`, `sections.json`, and
    /// `pois.json`.
    #[test]
    #[ignore = "needs map layers downloaded to KASSI_MAP_DIR"]
    fn draw_saved_store_map() {
        let dir = std::path::PathBuf::from(std::env::var("KASSI_MAP_DIR").unwrap());
        let layer = |name: &str| -> Value {
            serde_json::from_slice(&std::fs::read(dir.join(format!("{name}.json"))).unwrap())
                .unwrap()
        };
        let map = StoreMap::from_layers(&layer("departments"), &layer("sections"), &layer("pois"))
            .unwrap();
        let wanted: Vec<Wanted> = std::env::var("KASSI_MAP_ITEMS")
            .unwrap()
            .split(';')
            .map(|item| {
                let (name, sections) = item.split_once('=').unwrap();
                Wanted {
                    names: vec![name.to_owned()],
                    sections: sections.split(',').map(str::to_owned).collect(),
                }
            })
            .collect();
        let route = map.route(&wanted);
        for stop in &route.stops {
            println!("{:?} {:.0} m", stop.names, stop.metres_from_previous);
        }
        println!(
            "unplaced {:?}, total {:.0} m",
            route.unplaced, route.total_metres
        );
        let svg = map.svg(&route);
        std::fs::write(dir.join("route.svg"), &svg).unwrap();
        std::fs::write(dir.join("route.png"), png(&svg).unwrap()).unwrap();
    }

    #[test]
    fn drawing_numbers_the_stops() {
        let map = map();
        let svg = map.svg(&map.route(&[wanted("maito", "A1"), wanted("kahvi", "C1")]));
        assert!(svg.starts_with("<svg"), "{svg}");
        assert!(svg.contains(">1</text>") && svg.contains(">2</text>"));
        assert!(svg.contains("PÄIVITTÄISTAVARA") && svg.contains("KASSA"));
    }
}

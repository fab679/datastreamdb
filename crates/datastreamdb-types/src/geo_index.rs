//! Geospatial index tokens. Port of `types/s2index.go`.
//!
//! Points index to parent cells `min..=max` plus the max-level cover cell;
//! polygons cover via [`RegionCoverer`] over [`Loop`]. Tokens carry `p/` and
//! `c/` prefixes so queries can address parents or cover independently.

#![warn(missing_docs)]

use s2::cellid::CellID;
use s2::latlng::LatLng;
use s2::point::Point as S2Point;
use s2::region::RegionCoverer;

use super::conversion::ConversionError;
use super::s2loop::Loop;

/// Smallest cell level (largest cells, ~250km × 380km).
pub const MIN_CELL_LEVEL: u8 = 5;
/// Largest cell level (smallest cells, ~120m × 180m).
pub const MAX_CELL_LEVEL: u8 = 16;
/// Maximum cover cells per region.
pub const MAX_CELLS: usize = 18;

/// Parent-cell token prefix.
pub const PARENT_PREFIX: &str = "p/";
/// Cover-cell token prefix.
pub const COVER_PREFIX: &str = "c/";

/// Index tokens for a geometry (parents + cover).
///
/// # Errors
///
/// Returns [`ConversionError::Geo`] for unsupported geometry or bad rings.
pub fn index_geo_tokens(geom: &geojson::GeometryValue) -> Result<Vec<String>, ConversionError> {
    let (parents, cover) = index_cells(geom)?;
    let mut tokens = Vec::with_capacity(parents.len() + cover.len());
    tokens.extend(create_tokens(&parents, PARENT_PREFIX));
    tokens.extend(create_tokens(&cover, COVER_PREFIX));
    debug_assert_eq!(tokens.len(), parents.len() + cover.len());
    Ok(tokens)
}

/// Parent cells (all levels down to min containing the geometry) and the
/// cover (smallest cells covering it).
///
/// # Errors
///
/// Returns [`ConversionError::Geo`] for unsupported geometry or bad rings.
pub fn index_cells(
    geom: &geojson::GeometryValue,
) -> Result<(Vec<CellID>, Vec<CellID>), ConversionError> {
    match geom {
        geojson::GeometryValue::Point {
            coordinates: position,
        } => {
            let (lng, lat) = ordinates(position)?;
            Ok(index_cells_for_point(
                lng,
                lat,
                MIN_CELL_LEVEL,
                MAX_CELL_LEVEL,
            ))
        }
        geojson::GeometryValue::Polygon { coordinates: rings } => {
            let l = loop_from_polygon(rings)?;
            let cover = cover_loop(&l, MIN_CELL_LEVEL, MAX_CELL_LEVEL, MAX_CELLS);
            let parents = parent_cells(&cover, MIN_CELL_LEVEL);
            Ok((parents, cover))
        }
        geojson::GeometryValue::MultiPolygon { coordinates: polys } => {
            let mut cover = Vec::new();
            for poly in polys {
                let l = loop_from_polygon(poly)?;
                cover.extend(cover_loop(&l, MIN_CELL_LEVEL, MAX_CELL_LEVEL, MAX_CELLS));
            }
            let parents = parent_cells(&cover, MIN_CELL_LEVEL);
            Ok((parents, cover))
        }
        other => Err(ConversionError::Geo(format!(
            "Cannot index geometry of type {other:?}"
        ))),
    }
}

/// Parent chain + max-level cover cell for a point.
///
/// # Panics
///
/// Panics when `max_level` is below `min_level`.
#[must_use]
pub fn index_cells_for_point(
    lng: f64,
    lat: f64,
    min_level: u8,
    max_level: u8,
) -> (Vec<CellID>, Vec<CellID>) {
    assert!(max_level >= min_level, "max level below min level");
    let cell = CellID::from(&S2Point::from(LatLng::from_degrees(lat, lng)));
    let parents = (min_level..=max_level)
        .map(|l| cell.parent(u64::from(l)))
        .collect();
    (parents, vec![cell.parent(u64::from(max_level))])
}

/// All ancestors down to `min_level` for every cover cell (sorted for
/// determinism; Go iterates a map in random order).
#[must_use]
pub fn parent_cells(cover: &[CellID], min_level: u8) -> Vec<CellID> {
    let mut parents = std::collections::BTreeSet::new();
    for cell in cover {
        let mut level = cell.level();
        while level >= u64::from(min_level) {
            parents.insert(cell.parent(level));
            if level == 0 {
                break;
            }
            level -= 1;
        }
    }
    parents.into_iter().collect()
}

/// Region cover for a loop.
#[must_use]
pub fn cover_loop(l: &Loop, min_level: u8, max_level: u8, max_cells: usize) -> Vec<CellID> {
    let coverer = RegionCoverer {
        min_level,
        max_level,
        level_mod: 0,
        max_cells,
    };
    coverer.covering(l).0
}

/// `prefix + cell token` strings.
#[must_use]
pub fn create_tokens(cells: &[CellID], prefix: &str) -> Vec<String> {
    cells
        .iter()
        .map(|c| format!("{prefix}{}", c.to_token()))
        .collect()
}

/// Converts an outer polygon ring to a loop (holes skipped, like Go).
/// Orientation follows the shoelace sign, flipped when the cap exceeds 90°.
///
/// # Errors
///
/// Returns [`ConversionError::Geo`] for short or unclosed rings.
#[allow(clippy::float_cmp, reason = "Go compares ring coordinates with ==")]
pub fn loop_from_polygon(rings: &[Vec<geojson::Position>]) -> Result<Loop, ConversionError> {
    let Some(outer) = rings.first() else {
        return Err(ConversionError::Geo(
            "Can't convert ring with less than 4 pts".to_owned(),
        ));
    };
    if outer.len() < 4 {
        return Err(ConversionError::Geo(
            "Can't convert ring with less than 4 pts".to_owned(),
        ));
    }
    let (first, last) = (outer[0].as_slice(), outer[outer.len() - 1].as_slice());
    if first.len() < 2 || last.len() < 2 || first[0] != last[0] || first[1] != last[1] {
        return Err(ConversionError::Geo(
            "Last coordinate not same as first for polygon".to_owned(),
        ));
    }
    let reverse = is_clockwise(outer);
    let mut points = Vec::with_capacity(outer.len() - 1);
    for i in 0..outer.len() - 1 {
        let coord = if reverse {
            &outer[outer.len() - 1 - i]
        } else {
            &outer[i]
        }
        .as_slice();
        points.push(S2Point::from(LatLng::from_degrees(coord[1], coord[0])));
    }
    let mut l = Loop::from_vertices(points);
    if l.cap_bound().radius().deg() > 90.0 {
        l.invert();
    }
    Ok(l)
}

/// Planar shoelace orientation (fast approximation, like Go).
fn is_clockwise(ring: &[geojson::Position]) -> bool {
    let n = ring.len();
    let mut area = 0.0;
    for i in 0..n {
        let (p1, p2) = (ring[i].as_slice(), ring[(i + 1) % n].as_slice());
        area += (p2[0] - p1[0]) * (p1[1] + p2[1]);
    }
    area > 0.0
}

/// S2 point from a `[lng, lat, ...]` position.
///
/// # Errors
///
/// Returns [`ConversionError::Geo`] when fewer than 2 ordinates exist.
pub fn point_from_position(position: &geojson::Position) -> Result<S2Point, ConversionError> {
    let (lng, lat) = ordinates(position)?;
    Ok(S2Point::from(LatLng::from_degrees(lat, lng)))
}

fn ordinates(position: &geojson::Position) -> Result<(f64, f64), ConversionError> {
    let ordinates = position.as_slice();
    if ordinates.len() < 2 {
        return Err(ConversionError::Geo(
            "position needs at least 2 ordinates".to_owned(),
        ));
    }
    Ok((ordinates[0], ordinates[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_cells_match_go_tokens() {
        // Port of TestIndexCellsPoint, including exact cell tokens.
        let (parents, cover) =
            index_cells_for_point(-122.082_506, 37.424_951_8, MIN_CELL_LEVEL, MAX_CELL_LEVEL);
        assert_eq!(
            parents.len(),
            (MAX_CELL_LEVEL - MIN_CELL_LEVEL + 1) as usize
        );
        assert_eq!(parents[0].level(), u64::from(MIN_CELL_LEVEL));
        assert_eq!(parents[0].to_token(), "808c");
        assert_eq!(
            parents[parents.len() - 1].level(),
            u64::from(MAX_CELL_LEVEL)
        );
        assert_eq!(parents[parents.len() - 1].to_token(), "808fb9f81");
        let mut level = parents[0].level();
        for cell in &parents[1..] {
            assert!(cell.level() > level);
            level = cell.level();
        }
        assert_eq!(cover.len(), 1);
        assert_eq!(cover[0].level(), u64::from(MAX_CELL_LEVEL));
        assert_eq!(cover[0].to_token(), "808fb9f81");
    }

    #[test]
    fn point_keys_count() {
        // Port of TestKeyGeneratorPoint (count only; tokens verified above).
        let geom = geojson::GeometryValue::Point {
            coordinates: geojson::Position::from(vec![-122.082_506, 37.424_951_8]),
        };
        let keys = index_geo_tokens(&geom).expect("tokens");
        assert_eq!(
            keys.len(),
            (MAX_CELL_LEVEL - MIN_CELL_LEVEL + 1) as usize + 1
        );
        assert!(
            keys.iter()
                .all(|k| k.starts_with("p/") || k.starts_with("c/"))
        );
    }

    #[test]
    fn polygon_cover_and_tokens() {
        // Ports of TestIndexCellsPolygon / TestKeyGeneratorPolygon.
        let data = std::fs::read_to_string(fixture("zip.json")).expect("fixture");
        let feature: geojson::Feature = serde_json::from_str(&data).expect("feature");
        let geom = feature.geometry.map(|g| g.value).expect("geometry");
        let (parents, cover) = index_cells(&geom).expect("cells");
        assert!(cover.len() <= MAX_CELLS, "cover {}", cover.len());
        for cell in &cover {
            assert!(
                (u64::from(MIN_CELL_LEVEL)..=u64::from(MAX_CELL_LEVEL)).contains(&cell.level())
            );
            assert!(parents.contains(cell));
        }
        assert!(parents.len() > cover.len());
        let keys = index_geo_tokens(&geom).expect("tokens");
        assert_eq!(keys.len(), 67);
    }

    #[test]
    fn polygon_ring_errors() {
        // Port of TestIndexCellsPolygonError.
        let ring: Vec<geojson::Position> = [
            (-122.0, 37.0),
            (-123.0, 37.0),
            (-123.0, 38.0),
            (-122.0, 38.0),
        ]
        .into_iter()
        .map(|(x, y)| geojson::Position::from(vec![x, y]))
        .collect();
        let bad = geojson::GeometryValue::Polygon {
            coordinates: vec![ring],
        };
        let err = index_cells(&bad).expect_err("open ring");
        assert!(
            err.to_string()
                .contains("Last coordinate not same as first"),
            "{err}"
        );
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name)
    }
}

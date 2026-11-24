//! Geometry codecs: `GeoJSON` text and WKB bytes over [`geojson::GeometryValue`].
//!
//! Replaces go-geom. Positions keep all ordinates (Z preserved); S2 indexing
//! uses x/y. WKB is hand-rolled for exact go-geom byte compatibility (XY/XYZ,
//! EWKB SRID tolerated on read, plain little-endian on write).

#![warn(missing_docs)]

use super::conversion::ConversionError;
use super::strconv::format_float_g;

/// Parses full `GeoJSON` text with single quotes tolerated (Go `Convert` path).
/// No shorthand forms, no ring validation.
///
/// # Errors
///
/// Returns [`ConversionError::Geo`] on malformed input.
pub fn geo_from_json_text(text: &str) -> Result<geojson::GeometryValue, ConversionError> {
    let normalized = text.replace('\'', "\"");
    parse_geojson_object(&normalized).map_err(|e| {
        ConversionError::Geo(format!(
            "Error while unmarshalling: [{text}] as geojson: {e}"
        ))
    })
}

/// Parses a geometry like Go's `convertToGeom`: full `GeoJSON` first, then
/// bracket shorthands (`[x,y]`, `[[[...]]]`, `[[[[...]]]]`), with closed-ring
/// validation for polygons.
///
/// # Errors
///
/// Returns [`ConversionError::Geo`] on malformed input or open rings.
pub fn convert_to_geom(text: &str) -> Result<geojson::GeometryValue, ConversionError> {
    if let Ok(geometry) = serde_json::from_str::<geojson::Geometry>(text) {
        return validate_rings(&geometry.value);
    }
    let flat: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if flat.len() < 5 {
        return Err(ConversionError::Geo("Invalid coordinates".to_owned()));
    }
    if flat.starts_with("[[[[") {
        let coords: Vec<Vec<Vec<Vec<f64>>>> = serde_json::from_str(&flat)
            .map_err(|e| ConversionError::Geo(format!("Invalid coordinates: {e}")))?;
        let coords = coords
            .into_iter()
            .map(|poly| {
                poly.into_iter()
                    .map(|ring| ring.into_iter().map(geojson::Position::from).collect())
                    .collect()
            })
            .collect();
        return validate_rings(&geojson::GeometryValue::MultiPolygon {
            coordinates: coords,
        });
    }
    if flat.starts_with("[[[") {
        let coords: Vec<Vec<Vec<f64>>> = serde_json::from_str(&flat)
            .map_err(|e| ConversionError::Geo(format!("Invalid coordinates: {e}")))?;
        let coords = coords
            .into_iter()
            .map(|ring| ring.into_iter().map(geojson::Position::from).collect())
            .collect();
        return validate_rings(&geojson::GeometryValue::Polygon {
            coordinates: coords,
        });
    }
    if flat.starts_with('[') {
        let coords: Vec<f64> = serde_json::from_str(&flat)
            .map_err(|e| ConversionError::Geo(format!("Invalid coordinates: {e}")))?;
        if coords.len() < 2 {
            return Err(ConversionError::Geo("Invalid coordinates".to_owned()));
        }
        return Ok(geojson::GeometryValue::Point {
            coordinates: geojson::Position::from(coords),
        });
    }
    Err(ConversionError::Geo("Invalid coordinates".to_owned()))
}

fn parse_geojson_object(text: &str) -> Result<geojson::GeometryValue, String> {
    let geometry: geojson::Geometry = serde_json::from_str(text).map_err(|e| e.to_string())?;
    Ok(geometry.value)
}

/// Rejects empty polygons and unclosed outer rings.
#[allow(clippy::float_cmp, reason = "Go compares ring coordinates with ==")]
fn validate_rings(
    value: &geojson::GeometryValue,
) -> Result<geojson::GeometryValue, ConversionError> {
    let check = |rings: &[Vec<geojson::Position>]| -> Result<(), ConversionError> {
        let Some(first) = rings.first() else {
            return Err(ConversionError::Geo("Got empty polygon.".to_owned()));
        };
        if first.len() < 2 {
            return Err(ConversionError::Geo(
                "Last coord not same as first".to_owned(),
            ));
        }
        let (a, b) = (first[0].as_slice(), first[first.len() - 1].as_slice());
        if a.len() < 2 || b.len() < 2 || a[0] != b[0] || a[1] != b[1] {
            return Err(ConversionError::Geo(
                "Last coord not same as first".to_owned(),
            ));
        }
        Ok(())
    };
    match value {
        geojson::GeometryValue::Polygon { coordinates: rings } => check(rings)?,
        geojson::GeometryValue::MultiPolygon { coordinates: polys } => {
            for poly in polys {
                check(poly)?;
            }
        }
        _ => {}
    }
    Ok(value.clone())
}

/// Renders canonical `GeoJSON` (`{"type":...,"coordinates":...}`, Go number
/// shapes, no spaces), matching go-geom's `geojson.Marshal`.
#[must_use]
pub fn geo_to_json(value: &geojson::GeometryValue) -> String {
    fn write_position(out: &mut String, position: &geojson::Position) {
        out.push('[');
        for (i, ordinate) in position.as_slice().iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&format_float_g(*ordinate));
        }
        out.push(']');
    }
    fn positions(out: &mut String, positions: &[geojson::Position]) {
        out.push('[');
        for (i, position) in positions.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            write_position(out, position);
        }
        out.push(']');
    }
    fn ring_set(out: &mut String, rings: &[Vec<geojson::Position>]) {
        out.push('[');
        for (i, ring) in rings.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            positions(out, ring);
        }
        out.push(']');
    }
    let mut body = String::new();
    let kind = match value {
        geojson::GeometryValue::Point { coordinates: p } => {
            write_position(&mut body, p);
            "Point"
        }
        geojson::GeometryValue::MultiPoint { coordinates: ps } => {
            positions(&mut body, ps);
            "MultiPoint"
        }
        geojson::GeometryValue::LineString { coordinates: ps } => {
            positions(&mut body, ps);
            "LineString"
        }
        geojson::GeometryValue::MultiLineString { coordinates: ls } => {
            ring_set(&mut body, ls);
            "MultiLineString"
        }
        geojson::GeometryValue::Polygon { coordinates: rings } => {
            ring_set(&mut body, rings);
            "Polygon"
        }
        geojson::GeometryValue::MultiPolygon { coordinates: ps } => {
            body.push('[');
            for (i, poly) in ps.iter().enumerate() {
                if i > 0 {
                    body.push(',');
                }
                ring_set(&mut body, poly);
            }
            body.push(']');
            "MultiPolygon"
        }
        geojson::GeometryValue::GeometryCollection { geometries } => {
            body.push('[');
            for (i, geometry) in geometries.iter().enumerate() {
                if i > 0 {
                    body.push(',');
                }
                body.push_str(&geo_to_json(&geometry.value));
            }
            body.push(']');
            return format!("{{\"type\":\"GeometryCollection\",\"geometries\":{body}}}");
        }
    };
    format!("{{\"type\":\"{kind}\",\"coordinates\":{body}}}")
}

/// Renders `GeoJSON` with `"` replaced by `'` (the upstream Go stored string form).
#[must_use]
pub fn geo_to_single_quoted_json(value: &geojson::GeometryValue) -> String {
    geo_to_json(value).replace('"', "'")
}

// --- WKB ---

const WKB_POINT: u32 = 1;
const WKB_LINESTRING: u32 = 2;
const WKB_POLYGON: u32 = 3;
const WKB_MULTIPOINT: u32 = 4;
const WKB_MULTILINESTRING: u32 = 5;
const WKB_MULTIPOLYGON: u32 = 6;
const WKB_GEOMETRYCOLLECTION: u32 = 7;
const WKB_SRID_FLAG: u32 = 0x8000_0000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Endian {
    Big,
    Little,
}

struct WkbReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> WkbReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], ConversionError> {
        if self.pos + n > self.data.len() {
            return Err(ConversionError::Geo("truncated WKB input".to_owned()));
        }
        let out = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn byte_order(&mut self) -> Result<Endian, ConversionError> {
        match self.take(1)?[0] {
            0 => Ok(Endian::Big),
            1 => Ok(Endian::Little),
            b => Err(ConversionError::Geo(format!("bad WKB byte order {b}"))),
        }
    }

    fn u32(&mut self, endian: Endian) -> Result<u32, ConversionError> {
        let bytes: [u8; 4] = self.take(4)?.try_into().expect("4 bytes");
        Ok(match endian {
            Endian::Big => u32::from_be_bytes(bytes),
            Endian::Little => u32::from_le_bytes(bytes),
        })
    }

    fn f64(&mut self, endian: Endian) -> Result<f64, ConversionError> {
        let bytes: [u8; 8] = self.take(8)?.try_into().expect("8 bytes");
        Ok(match endian {
            Endian::Big => f64::from_be_bytes(bytes),
            Endian::Little => f64::from_le_bytes(bytes),
        })
    }

    fn position(
        &mut self,
        endian: Endian,
        dims: usize,
    ) -> Result<geojson::Position, ConversionError> {
        let ordinates: Vec<f64> = (0..dims)
            .map(|_| self.f64(endian))
            .collect::<Result<_, _>>()?;
        Ok(geojson::Position::from(ordinates))
    }

    fn position_list(
        &mut self,
        endian: Endian,
        dims: usize,
    ) -> Result<Vec<geojson::Position>, ConversionError> {
        let n = self.u32(endian)? as usize;
        (0..n).map(|_| self.position(endian, dims)).collect()
    }

    fn ring_list(
        &mut self,
        endian: Endian,
        dims: usize,
    ) -> Result<Vec<Vec<geojson::Position>>, ConversionError> {
        let rings = self.u32(endian)? as usize;
        (0..rings)
            .map(|_| self.position_list(endian, dims))
            .collect()
    }

    fn geometry(&mut self) -> Result<geojson::GeometryValue, ConversionError> {
        let endian = self.byte_order()?;
        let mut type_id = self.u32(endian)?;
        if type_id & WKB_SRID_FLAG != 0 {
            type_id &= !WKB_SRID_FLAG;
            let _ = self.u32(endian)?; // SRID tolerated, ignored
        }
        let dims = match type_id / 1000 {
            0 => 2,
            1 => 3, // Z
            _ => {
                return Err(ConversionError::Geo(format!(
                    "unsupported WKB dimensions in type {type_id}"
                )));
            }
        };
        match type_id % 1000 {
            WKB_POINT => {
                let position = self.position(endian, dims)?;
                Ok(geojson::GeometryValue::Point {
                    coordinates: position,
                })
            }
            WKB_LINESTRING => Ok(geojson::GeometryValue::LineString {
                coordinates: self.position_list(endian, dims)?,
            }),
            WKB_POLYGON => Ok(geojson::GeometryValue::Polygon {
                coordinates: self.ring_list(endian, dims)?,
            }),
            WKB_MULTIPOINT => {
                let n = self.u32(endian)? as usize;
                let mut points = Vec::with_capacity(n);
                for _ in 0..n {
                    match self.geometry()? {
                        geojson::GeometryValue::Point { coordinates: p } => points.push(p),
                        _ => {
                            return Err(ConversionError::Geo(
                                "MultiPoint holds a non-point".to_owned(),
                            ));
                        }
                    }
                }
                Ok(geojson::GeometryValue::MultiPoint {
                    coordinates: points,
                })
            }
            WKB_MULTILINESTRING => {
                let n = self.u32(endian)? as usize;
                let mut lines = Vec::with_capacity(n);
                for _ in 0..n {
                    match self.geometry()? {
                        geojson::GeometryValue::LineString { coordinates: l } => lines.push(l),
                        _ => {
                            return Err(ConversionError::Geo(
                                "MultiLineString holds a non-linestring".to_owned(),
                            ));
                        }
                    }
                }
                Ok(geojson::GeometryValue::MultiLineString { coordinates: lines })
            }
            WKB_MULTIPOLYGON => {
                let n = self.u32(endian)? as usize;
                let mut polys = Vec::with_capacity(n);
                for _ in 0..n {
                    match self.geometry()? {
                        geojson::GeometryValue::Polygon { coordinates: p } => polys.push(p),
                        _ => {
                            return Err(ConversionError::Geo(
                                "MultiPolygon holds a non-polygon".to_owned(),
                            ));
                        }
                    }
                }
                Ok(geojson::GeometryValue::MultiPolygon { coordinates: polys })
            }
            WKB_GEOMETRYCOLLECTION => {
                let n = self.u32(endian)? as usize;
                let mut geometries = Vec::with_capacity(n);
                for _ in 0..n {
                    geometries.push(geojson::Geometry {
                        bbox: None,
                        value: self.geometry()?,
                        foreign_members: None,
                    });
                }
                Ok(geojson::GeometryValue::GeometryCollection { geometries })
            }
            other => Err(ConversionError::Geo(format!("unknown WKB type {other}"))),
        }
    }
}

/// Parses WKB bytes (either endianness; EWKB SRID tolerated).
///
/// # Errors
///
/// Returns [`ConversionError::Geo`] on malformed input.
pub fn geo_from_wkb(data: &[u8]) -> Result<geojson::GeometryValue, ConversionError> {
    WkbReader::new(data).geometry()
}

fn has_z(value: &geojson::GeometryValue) -> bool {
    fn long(position: &geojson::Position) -> bool {
        position.as_slice().len() > 2
    }
    match value {
        geojson::GeometryValue::Point { coordinates: p } => long(p),
        geojson::GeometryValue::MultiPoint { coordinates: ps }
        | geojson::GeometryValue::LineString { coordinates: ps } => ps.iter().any(long),
        geojson::GeometryValue::MultiLineString { coordinates: ls }
        | geojson::GeometryValue::Polygon { coordinates: ls } => ls.iter().flatten().any(long),
        geojson::GeometryValue::MultiPolygon { coordinates: ps } => {
            ps.iter().flatten().flatten().any(long)
        }
        geojson::GeometryValue::GeometryCollection { geometries: gs } => {
            gs.iter().any(|g| has_z(&g.value))
        }
    }
}

struct WkbWriter {
    out: Vec<u8>,
}

impl WkbWriter {
    fn new() -> Self {
        Self { out: Vec::new() }
    }

    fn u32(&mut self, v: u32) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn f64(&mut self, v: f64) {
        self.out.extend_from_slice(&v.to_le_bytes());
    }

    fn position(&mut self, position: &geojson::Position, with_z: bool) {
        let ordinates = position.as_slice();
        let (x, y) = (
            ordinates.first().copied().unwrap_or(0.0),
            ordinates.get(1).copied().unwrap_or(0.0),
        );
        self.f64(x);
        self.f64(y);
        if with_z {
            self.f64(ordinates.get(2).copied().unwrap_or(0.0));
        }
    }

    fn header(&mut self, type_id: u32, with_z: bool) {
        self.out.push(1);
        self.u32(type_id + u32::from(with_z) * 1000);
    }

    fn geometry(&mut self, value: &geojson::GeometryValue) {
        let with_z = has_z(value);
        match value {
            geojson::GeometryValue::Point { coordinates: p } => {
                self.header(WKB_POINT, with_z);
                self.position(p, with_z);
            }
            geojson::GeometryValue::LineString { coordinates: ps } => {
                self.header(WKB_LINESTRING, with_z);
                self.u32(ps.len() as u32);
                for p in ps {
                    self.position(p, with_z);
                }
            }
            geojson::GeometryValue::Polygon { coordinates: rings } => {
                self.header(WKB_POLYGON, with_z);
                self.u32(rings.len() as u32);
                for ring in rings {
                    self.u32(ring.len() as u32);
                    for p in ring {
                        self.position(p, with_z);
                    }
                }
            }
            geojson::GeometryValue::MultiPoint { coordinates: ps } => {
                self.header(WKB_MULTIPOINT, with_z);
                self.u32(ps.len() as u32);
                for p in ps {
                    self.geometry(&geojson::GeometryValue::Point {
                        coordinates: p.clone(),
                    });
                }
            }
            geojson::GeometryValue::MultiLineString { coordinates: lines } => {
                self.header(WKB_MULTILINESTRING, with_z);
                self.u32(lines.len() as u32);
                for line in lines {
                    self.geometry(&geojson::GeometryValue::LineString {
                        coordinates: line.clone(),
                    });
                }
            }
            geojson::GeometryValue::MultiPolygon { coordinates: polys } => {
                self.header(WKB_MULTIPOLYGON, with_z);
                self.u32(polys.len() as u32);
                for poly in polys {
                    self.geometry(&geojson::GeometryValue::Polygon {
                        coordinates: poly.clone(),
                    });
                }
            }
            geojson::GeometryValue::GeometryCollection { geometries } => {
                self.header(WKB_GEOMETRYCOLLECTION, with_z);
                self.u32(geometries.len() as u32);
                for geometry in geometries {
                    self.geometry(&geometry.value);
                }
            }
        }
    }
}

/// Encodes little-endian WKB (Z variant when positions carry depth),
/// byte-compatible with go-geom's writer.
#[must_use]
pub fn geo_to_wkb(value: &geojson::GeometryValue) -> Vec<u8> {
    let mut writer = WkbWriter::new();
    writer.geometry(value);
    writer.out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_quoted_geojson() {
        // Port of TestParse (structure; Z ordinates preserved).
        for text in [
            "{'type':'Point','coordinates':[1,2]}",
            "{'type':'MultiLineString','coordinates':[[[1,2,3],[4,5,6],[7,8,9],[1,2,3]]]}",
        ] {
            let geom = geo_from_json_text(text).expect("parse");
            let back = geo_to_single_quoted_json(&geom);
            assert_eq!(back, text, "roundtrip");
            let wkb = geo_to_wkb(&geom);
            assert_eq!(geo_from_wkb(&wkb).expect("wkb"), geom);
        }
    }

    #[test]
    fn rejects_bad_geojson() {
        // Port of TestParseGeoJsonErrors.
        for bad in [
            "{\"type\":\"Curve\",\"coordinates\":[1,2]}",
            "{\"type\":\"Feature\",\"geometry\":{\"type\":\"Point\",\"coordinates\":[125.6,10.1]}}",
            "{}",
            "thisisntjson",
        ] {
            assert!(geo_from_json_text(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn convert_to_geom_forms() {
        // Port of TestConvertToGeoJson_*.
        let point = convert_to_geom("[1.0, 2.0] ").expect("point");
        assert_eq!(
            point,
            geojson::GeometryValue::Point {
                coordinates: geojson::Position::from(vec![1.0, 2.0])
            }
        );
        let poly =
            convert_to_geom("[[[1.123, 2.543], [-3.23, 4.123], [4.43, -6.123], [1.123, 2.543]]]")
                .expect("poly");
        assert!(matches!(
            poly,
            geojson::GeometryValue::Polygon { coordinates: _ }
        ));
        assert!(convert_to_geom("[[1.76, -2.234], [3.543, 4.534], [4.54, 6.213]]").is_err());
        assert!(convert_to_geom("[[1.253, 2.2343], [-4.563, 6.1231], []]").is_err());
        assert!(convert_to_geom("[1,2]").is_ok());
        assert!(convert_to_geom("nope").is_err());
    }

    #[test]
    fn validates_closed_rings() {
        assert!(convert_to_geom("[[[0,0],[1,0],[1,1],[0,0]]]").is_ok());
        let err = convert_to_geom("[[[0,0],[1,0],[1,1],[0,1]]]").expect_err("open ring");
        assert!(
            err.to_string().contains("Last coord not same as first"),
            "{err}"
        );
    }

    #[test]
    fn wkb_matches_spec_bytes() {
        // POINT(1 2) little-endian: 01 | 01000000 | doubles.
        let geom = geojson::GeometryValue::Point {
            coordinates: geojson::Position::from(vec![1.0, 2.0]),
        };
        let wkb = geo_to_wkb(&geom);
        assert_eq!(
            wkb,
            [
                0x01, 0x01, 0, 0, 0, //
                0, 0, 0, 0, 0, 0, 0xF0, 0x3F, //
                0, 0, 0, 0, 0, 0, 0, 0x40,
            ]
        );
        assert_eq!(geo_from_wkb(&wkb).expect("read"), geom);
        // Big-endian input parses too.
        let mut be = vec![0x00, 0, 0, 0, 0x01];
        be.extend_from_slice(&1.0f64.to_be_bytes());
        be.extend_from_slice(&2.0f64.to_be_bytes());
        assert_eq!(geo_from_wkb(&be).expect("read"), geom);
    }
}

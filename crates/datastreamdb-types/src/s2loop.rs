//! Spherical polygon loops over the `s2` crate. Port of `types/s2.go` loop
//! helpers (`Contains`, `Intersects`) plus the [`Region`] implementation the
//! coverer needs (the `s2` crate ships no loop type).
//!
//! Point containment uses antipodal-ray parity with S2's exact edge-crossing
//! predicate.

#![warn(missing_docs)]

use s2::cap::Cap;
use s2::cell::Cell;
use s2::edge_crossings::{self, EdgeCrosser};
use s2::latlng::LatLng;
use s2::point::Point as S2Point;
use s2::rect::Rect;
use s2::rect_bounder::RectBounder;
use s2::region::Region;

/// A closed spherical loop (vertices without the repeated closer).
#[derive(Clone, Debug)]
pub struct Loop {
    vertices: Vec<S2Point>,
}

impl Loop {
    /// Loop over `vertices` (must hold ≥3 points; closure is implicit).
    #[must_use]
    pub fn from_vertices(vertices: Vec<S2Point>) -> Self {
        Self { vertices }
    }

    /// Edge count (== vertex count for a closed loop).
    #[must_use]
    pub fn num_edges(&self) -> usize {
        self.vertices.len()
    }

    /// Vertex `i`, wrapping (`vertex(n) == vertex(0)`, like Go).
    #[must_use]
    pub fn vertex(&self, i: usize) -> &S2Point {
        &self.vertices[i % self.vertices.len()]
    }

    /// All vertices.
    #[must_use]
    pub fn vertices(&self) -> &[S2Point] {
        &self.vertices
    }

    /// Latitude-longitude bounding rectangle.
    #[must_use]
    pub fn rect_bound(&self) -> Rect {
        let mut bounder = RectBounder::new();
        for vertex in &self.vertices {
            bounder.add_point(vertex);
        }
        bounder.get_bound()
    }

    /// Bounding cap centered on the first vertex (valid, not tight).
    #[must_use]
    pub fn cap_bound(&self) -> Cap {
        let center = &self.vertices[0];
        let mut radius = 0.0f64;
        for vertex in &self.vertices {
            radius = radius.max(center.distance(vertex).rad());
        }
        Cap::from_center_angle(center, &s2::s1::Angle::from(s2::s1::Rad(radius)))
    }

    /// Reverses the loop orientation.
    pub fn invert(&mut self) {
        self.vertices.reverse();
    }

    /// Whether the loop contains `point` (boundary counts as inside).
    #[must_use]
    pub fn contains_point(&self, point: &S2Point) -> bool {
        for vertex in &self.vertices {
            if vertex.approx_eq(point) {
                return true;
            }
        }
        let antipode = antipode(point);
        let mut inside = false;
        for i in 0..self.vertices.len() {
            let a = &self.vertices[i];
            let b = &self.vertices[(i + 1) % self.vertices.len()];
            if edge_crossings::edge_or_vertex_crossing(point, &antipode, a, b) {
                inside = !inside;
            }
        }
        inside
    }
}

/// Near-antipode of a point (for containment rays).
///
/// Exactly-antipodal edges are degenerate in S2 (every great circle through
/// the endpoints qualifies), so the ray target is perturbed slightly. Loops
/// must not contain the ray target; all indexed loops are far smaller than a
/// hemisphere, so this always holds.
fn antipode(point: &S2Point) -> S2Point {
    let mut lng = point.longitude().deg() + 180.0 + 1e-7;
    if lng > 180.0 {
        lng -= 360.0;
    }
    let lat = (-point.latitude().deg() + 0.7e-7).clamp(-90.0, 90.0);
    S2Point::from(LatLng::from_degrees(lat, lng))
}

impl Region for Loop {
    fn cap_bound(&self) -> Cap {
        self.cap_bound()
    }

    fn rect_bound(&self) -> Rect {
        self.rect_bound()
    }

    fn contains_cell(&self, cell: &Cell) -> bool {
        for vertex in cell.vertices() {
            if !self.contains_point(&vertex) {
                return false;
            }
        }
        !edges_cross_cell(self, cell)
    }

    fn intersects_cell(&self, cell: &Cell) -> bool {
        for vertex in cell.vertices() {
            if self.contains_point(&vertex) {
                return true;
            }
        }
        for vertex in &self.vertices {
            if cell.contains_point(vertex) {
                return true;
            }
        }
        edges_cross_cell(self, cell)
    }
}

/// Whether any loop edge crosses any cell edge.
fn edges_cross_cell(l: &Loop, cell: &Cell) -> bool {
    let corners = cell.vertices();
    for i in 0..4 {
        let (c0, c1) = (&corners[i], &corners[(i + 1) % 4]);
        for j in 0..l.vertices.len() {
            let (l0, l1) = (&l.vertices[j], &l.vertices[(j + 1) % l.vertices.len()]);
            if edge_crossings::edge_or_vertex_crossing(c0, c1, l0, l1) {
                return true;
            }
        }
    }
    false
}

/// Whether any edge of the point chain crosses the loop. Port of `edgesCrossPoints`.
fn edges_cross_points(l: &Loop, points: &[S2Point]) -> bool {
    let n = points.len();
    for i in 0..n {
        let mut crosser = EdgeCrosser::new_with_c(&points[i], &points[(i + 1) % n], l.vertex(0));
        for j in 1..=l.num_edges() {
            if crosser.chain_edge_or_vertex_crossing(l.vertex(j)) {
                return true;
            }
        }
    }
    false
}

/// Whether loop `a` contains loop `b`. Port of `Contains`.
#[must_use]
pub fn loop_contains(a: &Loop, b: &Loop) -> bool {
    if !a.rect_bound().contains(&b.rect_bound()) {
        return false;
    }
    if !a.contains_point(b.vertex(0)) && find_vertex(a, b.vertex(0)).is_none() {
        return false;
    }
    if edges_cross_points(a, b.vertices()) {
        return false;
    }
    if a.rect_bound().union(&b.rect_bound()).is_full()
        && b.contains_point(a.vertex(0))
        && find_vertex(b, a.vertex(0)).is_none()
    {
        return false;
    }
    true
}

fn find_vertex(a: &Loop, point: &S2Point) -> Option<usize> {
    a.vertices().iter().position(|v| v.approx_eq(point))
}

/// Whether two loops intersect. Port of `Intersects`.
#[must_use]
pub fn loops_intersect(l1: &Loop, l2: &Loop) -> bool {
    if l2.num_edges() > l1.num_edges() {
        intersects_inner(l2, l1)
    } else {
        intersects_inner(l1, l2)
    }
}

/// Whether loops intersect, using `l` for edge indexing. Port of `intersects`.
fn intersects_inner(l: &Loop, other: &Loop) -> bool {
    if !l.rect_bound().intersects(&other.rect_bound()) {
        return false;
    }
    if l.contains_point(other.vertex(0)) {
        return true;
    }
    if edges_cross_points(l, other.vertices()) {
        return true;
    }
    if other.rect_bound().contains(&l.rect_bound()) {
        return other.contains_point(l.vertex(0));
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use s2::latlng::LatLng;

    fn point(lng: f64, lat: f64) -> S2Point {
        S2Point::from(LatLng::from_degrees(lat, lng))
    }

    fn square(lng0: f64, lat0: f64, lng1: f64, lat1: f64) -> Loop {
        Loop::from_vertices(vec![
            point(lng0, lat0),
            point(lng1, lat0),
            point(lng1, lat1),
            point(lng0, lat1),
        ])
    }

    #[test]
    fn contains_points() {
        let l = square(-122.0, 37.0, -121.0, 38.0);
        assert!(l.contains_point(&point(-121.5, 37.5)));
        assert!(!l.contains_point(&point(-120.0, 37.5)));
        assert!(l.contains_point(&point(-122.0, 37.0))); // vertex counts
    }

    #[test]
    fn contains_and_intersects_loops() {
        let outer = square(-123.0, 37.0, -122.0, 38.0);
        let inner = square(-122.9, 37.1, -122.1, 37.9);
        let overlap = square(-122.5, 37.5, -121.5, 38.5);
        let far = square(-120.0, 35.0, -119.0, 36.0);
        assert!(loop_contains(&outer, &inner));
        assert!(!loop_contains(&inner, &outer));
        assert!(loops_intersect(&outer, &overlap));
        assert!(loops_intersect(&outer, &inner));
        assert!(!loops_intersect(&outer, &far));
    }
}

//! Spherical-earth helpers. Port of `types/earth.go`.

#![warn(missing_docs)]

/// Mean earth radius in meters (spherical model).
pub const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

/// Length on earth, in meters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Length(pub f64);

/// Area on earth, in square meters.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area(pub f64);

/// Converts an angle in radians to earth distance.
#[must_use]
pub fn earth_distance(angle_radians: f64) -> Length {
    Length(angle_radians * EARTH_RADIUS_METERS)
}

/// Converts earth distance in meters to an angle in radians.
#[must_use]
pub fn earth_angle(meters: f64) -> f64 {
    meters / EARTH_RADIUS_METERS
}

/// Converts unit-sphere area to earth area.
#[must_use]
pub fn earth_area(unit_area: f64) -> Area {
    Area(unit_area * EARTH_RADIUS_METERS * EARTH_RADIUS_METERS)
}

impl std::fmt::Display for Length {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0 > 1000.0 {
            write!(f, "{:.3} km", self.0 / 1000.0)
        } else if self.0 < 1.0 {
            write!(f, "{:.3} cm", self.0 * 100.0)
        } else {
            write!(f, "{:.3} m", self.0)
        }
    }
}

impl std::fmt::Display for Area {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        const KM2: f64 = 1_000_000.0;
        const CM2: f64 = 10_000.0;
        if self.0 > KM2 {
            write!(f, "{:.3} km^2", self.0 / KM2)
        } else if self.0 < 1.0 {
            write!(f, "{:.3} cm^2", self.0 * CM2)
        } else {
            write!(f, "{:.3} m^2", self.0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp, reason = "identical expressions must be bit-equal")]
    fn converts_and_formats() {
        assert_eq!(
            earth_distance(std::f64::consts::PI).0,
            std::f64::consts::PI * 6_371_000.0
        );
        assert_eq!(earth_angle(6_371_000.0), 1.0);
        assert_eq!(Length(1500.0).to_string(), "1.500 km");
        assert_eq!(Length(0.5).to_string(), "50.000 cm");
        assert_eq!(Length(5.0).to_string(), "5.000 m");
        assert_eq!(Area(2_000_000.0).to_string(), "2.000 km^2");
        assert_eq!(Area(0.5).to_string(), "5000.000 cm^2");
    }
}

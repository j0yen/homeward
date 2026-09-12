//! Great-circle distance for the `search_pets` radius filter.

/// Distance between two lat/lon points in kilometers (haversine formula).
///
/// Used only for the `search_pets` radius filter; inputs are already
/// coarse (rounded) coordinates from [`homeward_schema::ShelterLocation`],
/// so this never operates on precise/person-level location data.
#[must_use]
#[allow(clippy::float_arithmetic, clippy::suboptimal_flops)]
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6371.0;
    let d_lat = (lat2 - lat1).to_radians();
    let d_lon = (lon2 - lon1).to_radians();
    let a = (d_lat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (d_lon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().atan2((1.0 - a).sqrt());
    EARTH_RADIUS_KM * c
}

#[cfg(test)]
mod tests {
    use super::haversine_km;

    #[test]
    fn same_point_is_zero_distance() {
        assert!(haversine_km(30.27, -97.74, 30.27, -97.74) < 0.001);
    }

    #[test]
    fn austin_to_dallas_is_roughly_two_hundred_fifty_km() {
        // Austin, TX -> Dallas, TX is ~300km driving, ~260km great-circle.
        let d = haversine_km(30.2672, -97.7431, 32.7767, -96.7970);
        assert!((200.0..320.0).contains(&d), "unexpected distance: {d}");
    }
}

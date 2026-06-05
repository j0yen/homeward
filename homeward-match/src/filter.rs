//! Structured prefilter — applies species, availability, coarse-geo,
//! and intake-date window constraints before scoring.

use chrono::{DateTime, Duration, Utc};
use homeward_schema::{Availability, PetRecord, Species};
use ulid::Ulid;

use crate::MatchParams;

// ─── Geo helpers ─────────────────────────────────────────────────────────────

/// Haversine distance in kilometres between two lat/lon points.
///
/// # Panics
/// Does not panic.
#[must_use]
#[allow(
    clippy::float_arithmetic,
    clippy::suboptimal_flops,
    clippy::suspicious_operation_groupings
)]
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const R: f64 = 6371.0; // Earth radius km
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let sin_dlat_2 = (dlat / 2.0).sin();
    let sin_dlon_2 = (dlon / 2.0).sin();
    let a = sin_dlat_2 * sin_dlat_2
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (sin_dlon_2 * sin_dlon_2);
    let c = 2.0 * a.sqrt().asin();
    R * c
}

/// Compute distance (km) between a shelter record's location and a
/// report's last-seen location, if coordinates are available on both.
#[must_use]
pub fn record_report_distance_km(
    record_lat: Option<f64>,
    record_lon: Option<f64>,
    report_lat: Option<f64>,
    report_lon: Option<f64>,
) -> Option<f64> {
    match (record_lat, record_lon, report_lat, report_lon) {
        (Some(rlat), Some(rlon), Some(plat), Some(plon)) => {
            Some(haversine_km(rlat, rlon, plat, plon))
        }
        _ => None,
    }
}

// ─── PrefilterInput ──────────────────────────────────────────────────────────

/// A slim view of a shelter record used by the prefilter.
///
/// Callers materialise this from their store without loading the full
/// `PetRecord` bytes — only the fields needed for filtering.
#[derive(Debug, Clone)]
pub struct PrefilterInput {
    /// Stable ID for this record.
    pub canonical_id: Ulid,

    /// Species of the animal.
    pub species: Species,

    /// Current availability status.
    pub availability: Availability,

    /// Shelter latitude (coarse).
    pub lat: Option<f64>,

    /// Shelter longitude (coarse).
    pub lon: Option<f64>,

    /// Date the animal entered custody.
    pub intake_date: Option<DateTime<Utc>>,
}

impl From<&PetRecord> for PrefilterInput {
    fn from(r: &PetRecord) -> Self {
        Self {
            canonical_id: r.canonical_id,
            species: r.species,
            availability: r.availability,
            lat: r.location.as_ref().and_then(|l| l.lat),
            lon: r.location.as_ref().and_then(|l| l.lon),
            intake_date: r.intake_date,
        }
    }
}

// ─── Prefilter input bundle ───────────────────────────────────────────────────

/// Parameters passed to [`prefilter`] describing the lost report's location/time.
#[derive(Debug, Clone, Copy)]
pub struct ReportFilter {
    /// Species of the lost animal.
    pub species: Species,
    /// Last-seen latitude (coarse, may be absent).
    pub lat: Option<f64>,
    /// Last-seen longitude (coarse, may be absent).
    pub lon: Option<f64>,
    /// Date the report was created (used for intake-date window).
    pub date: DateTime<Utc>,
}

// ─── Prefilter ───────────────────────────────────────────────────────────────

/// Apply the structured prefilter, returning `canonical_id`s that pass.
///
/// Filtering rules (all must pass):
/// 1. `species` matches the report's species exactly.
/// 2. `availability` is [`Availability::InCustody`] or [`Availability::Adoptable`]
///    (i.e., **not** [`Availability::Departed`]).
/// 3. If both the record and the report have coordinates, the straight-line
///    distance is ≤ `params.radius_km`. Records without coordinates are kept
///    (geo-unknown records pass the filter to avoid silent data loss).
/// 4. If the record has an `intake_date`, it must fall within
///    `report_date ± params.date_window_days`. Records without `intake_date`
///    are kept.
///
/// Widening `radius_km` or `date_window_days` monotonically grows the
/// output set (AC2).
#[must_use]
pub fn prefilter(
    records: &[PrefilterInput],
    report: ReportFilter,
    params: &MatchParams,
) -> Vec<Ulid> {
    records
        .iter()
        .filter(|r| passes_filter(r, &report, params))
        .map(|r| r.canonical_id)
        .collect()
}

fn passes_filter(r: &PrefilterInput, report: &ReportFilter, params: &MatchParams) -> bool {
    // Rule 1: species
    if r.species != report.species {
        return false;
    }
    // Rule 2: availability (exclude Departed)
    if r.availability == Availability::Departed {
        return false;
    }
    // Rule 3: coarse geo (skip if no coords available)
    if let Some(dist) = record_report_distance_km(r.lat, r.lon, report.lat, report.lon) {
        if dist > params.radius_km {
            return false;
        }
    }
    // Rule 4: intake date window (skip if no intake_date)
    if let Some(intake) = r.intake_date {
        let window = Duration::days(params.date_window_days);
        let earliest = report.date - window;
        let latest = report.date + window;
        if intake < earliest || intake > latest {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use homeward_schema::Species;

    fn make_record(
        id_str: &str,
        species: Species,
        avail: Availability,
        lat: Option<f64>,
        lon: Option<f64>,
        intake_offset_days: Option<i64>,
        base_date: DateTime<Utc>,
    ) -> PrefilterInput {
        let intake_date = intake_offset_days.map(|d| base_date + Duration::days(d));
        PrefilterInput {
            canonical_id: ulid_from_str(id_str),
            species,
            availability: avail,
            lat,
            lon,
            intake_date,
        }
    }

    fn ulid_from_str(s: &str) -> Ulid {
        let padded = format!("{:0>26}", s);
        padded.parse().unwrap_or(Ulid::nil())
    }

    fn base_date() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap()
    }

    fn report(species: Species, lat: Option<f64>, lon: Option<f64>) -> ReportFilter {
        ReportFilter {
            species,
            lat,
            lon,
            date: base_date(),
        }
    }

    #[test]
    fn species_mismatch_excluded() {
        let records = vec![make_record(
            "1",
            Species::Cat,
            Availability::InCustody,
            None,
            None,
            None,
            base_date(),
        )];
        let params = MatchParams::default();
        let result = prefilter(&records, report(Species::Dog, None, None), &params);
        assert!(result.is_empty(), "cat record should not appear for dog report");
    }

    #[test]
    fn departed_excluded() {
        let records = vec![make_record(
            "2",
            Species::Dog,
            Availability::Departed,
            None,
            None,
            None,
            base_date(),
        )];
        let params = MatchParams::default();
        let result = prefilter(&records, report(Species::Dog, None, None), &params);
        assert!(result.is_empty(), "Departed record should not appear");
    }

    #[test]
    fn in_custody_and_adoptable_both_pass() {
        let records = vec![
            make_record(
                "3",
                Species::Dog,
                Availability::InCustody,
                None,
                None,
                None,
                base_date(),
            ),
            make_record(
                "4",
                Species::Dog,
                Availability::Adoptable,
                None,
                None,
                None,
                base_date(),
            ),
        ];
        let params = MatchParams::default();
        let result = prefilter(&records, report(Species::Dog, None, None), &params);
        assert_eq!(result.len(), 2, "both InCustody and Adoptable should pass");
    }

    #[test]
    fn geo_radius_filters_distant() {
        let nyc_lat = 40.71;
        let nyc_lon = -74.01;
        let la_lat = 34.05;
        let la_lon = -118.24;
        let records = vec![
            make_record(
                "5",
                Species::Dog,
                Availability::InCustody,
                Some(nyc_lat),
                Some(nyc_lon),
                None,
                base_date(),
            ),
            make_record(
                "6",
                Species::Dog,
                Availability::InCustody,
                Some(la_lat),
                Some(la_lon),
                None,
                base_date(),
            ),
        ];
        let params = MatchParams {
            radius_km: 100.0,
            ..MatchParams::default()
        };
        let result = prefilter(
            &records,
            report(Species::Dog, Some(nyc_lat), Some(nyc_lon)),
            &params,
        );
        assert_eq!(result.len(), 1, "only nearby record should pass");
    }

    #[test]
    fn wider_radius_grows_candidate_set() {
        let nyc_lat = 40.71_f64;
        let nyc_lon = -74.01_f64;
        let near_lat = 41.6_f64; // roughly 100 km north
        let records = vec![
            make_record(
                "7",
                Species::Dog,
                Availability::InCustody,
                Some(near_lat),
                Some(nyc_lon),
                None,
                base_date(),
            ),
        ];
        let narrow = MatchParams {
            radius_km: 50.0,
            ..MatchParams::default()
        };
        let wide = MatchParams {
            radius_km: 200.0,
            ..MatchParams::default()
        };
        let r_narrow = prefilter(
            &records,
            report(Species::Dog, Some(nyc_lat), Some(nyc_lon)),
            &narrow,
        );
        let r_wide = prefilter(
            &records,
            report(Species::Dog, Some(nyc_lat), Some(nyc_lon)),
            &wide,
        );
        assert!(
            r_wide.len() >= r_narrow.len(),
            "wider radius must produce >= candidates: wide={} narrow={}",
            r_wide.len(),
            r_narrow.len()
        );
    }

    #[test]
    fn intake_date_window_filters_old() {
        let records = vec![make_record(
            "8",
            Species::Dog,
            Availability::InCustody,
            None,
            None,
            Some(-60),
            base_date(),
        )];
        let params = MatchParams {
            date_window_days: 30,
            ..MatchParams::default()
        };
        let result = prefilter(&records, report(Species::Dog, None, None), &params);
        assert!(
            result.is_empty(),
            "record 60 days before report (window=30) should be excluded"
        );
    }

    #[test]
    fn wider_date_window_grows_candidate_set() {
        let records = vec![make_record(
            "9",
            Species::Dog,
            Availability::InCustody,
            None,
            None,
            Some(-45),
            base_date(),
        )];
        let narrow = MatchParams {
            date_window_days: 30,
            ..MatchParams::default()
        };
        let wide = MatchParams {
            date_window_days: 60,
            ..MatchParams::default()
        };
        let r_narrow = prefilter(&records, report(Species::Dog, None, None), &narrow);
        let r_wide = prefilter(&records, report(Species::Dog, None, None), &wide);
        assert!(
            r_wide.len() >= r_narrow.len(),
            "wider window must produce >= candidates"
        );
    }

    #[test]
    fn no_coords_on_record_passes_geo() {
        let records = vec![make_record(
            "10",
            Species::Dog,
            Availability::InCustody,
            None,
            None,
            None,
            base_date(),
        )];
        let params = MatchParams {
            radius_km: 1.0,
            ..MatchParams::default()
        };
        let result = prefilter(
            &records,
            report(Species::Dog, Some(40.71), Some(-74.01)),
            &params,
        );
        assert_eq!(
            result.len(),
            1,
            "record with no coords should pass geo filter"
        );
    }
}

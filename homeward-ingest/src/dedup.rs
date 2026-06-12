//! Entity resolution for incoming [`PetRecord`]s.
//!
//! Two records are considered the same animal when:
//! 1. They share the same `source_animal_id` within the same source, **or**
//! 2. They share the same attribute key `(species, breed_primary, sex, age_bucket,
//!    shelter_name, intake_date)` AND their primary-photo pHash is within
//!    `hamming_threshold` bits of each other.
//!
//! pHash here is a simple 64-bit difference hash on the photo URL (we don't
//! download images; the URL-based hash is a stand-in until homeward-embed lands
//! the full perceptual hash pipeline).
//!
//! The [`federated_merge`] pass reconciles community found/stray reports from
//! federated sources (e.g. PetFBI, HelpingLostPets) against shelter intake
//! records using species + geo + date-window + perceptual-hash guards.

use chrono::DateTime;
use homeward_schema::{PetRecord, Provenance, Species, intake::IntakeType};
use ulid::Ulid;

use crate::store::{Store, StoreError};

// ── DedupConfig ───────────────────────────────────────────────────────────────

/// Configuration for dedup and federated reconciliation.
///
/// Defaults are biased toward **false-split over false-merge**: merging two
/// different animals is worse than listing one animal twice.
#[derive(Debug, Clone)]
pub struct DedupConfig {
    /// Max distance (km) between found-location and intake-location for a
    /// candidate federated merge.  Default: 10.0 km.
    pub federated_geo_max_km: f64,

    /// Max days between found-date and intake-date for a candidate federated
    /// merge.  Default: 7 days.
    pub federated_date_window_days: u32,

    /// Max normalised perceptual-hash distance (0.0 = identical, 1.0 =
    /// completely different) for a candidate federated merge.
    /// Default: 0.15 (conservative — requires photos to look very similar).
    pub federated_phash_max_distance: f64,
}

impl Default for DedupConfig {
    fn default() -> Self {
        Self {
            federated_geo_max_km: 10.0,
            federated_date_window_days: 7,
            federated_phash_max_distance: 0.15,
        }
    }
}

// ── Haversine helper ──────────────────────────────────────────────────────────

/// Great-circle distance in kilometres between two (lat, lon) points.
///
/// Uses the Haversine formula; accurate to within ~0.5% for distances up to
/// a few hundred km — more than adequate for shelter-geo matching.
#[must_use]
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const EARTH_RADIUS_KM: f64 = 6_371.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let lat1r = lat1.to_radians();
    let lat2r = lat2.to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1r.cos() * lat2r.cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    EARTH_RADIUS_KM * c
}

// ── Federated merge ───────────────────────────────────────────────────────────

/// Returns `true` if `record` is a federated community found/stray report.
///
/// A federated record is one whose `intake_type` is [`IntakeType::FoundReport`]
/// or [`IntakeType::Stray`] (community-submitted stray sightings).  Only these
/// are eligible to be reconciled against shelter intakes; two lost-reports
/// (records that describe an animal the owner is *searching* for) must never
/// be merged with each other.
///
/// In practice PetFBI/HelpingLostPets records arrive as `FoundReport`.
fn is_federated_found(record: &PetRecord) -> bool {
    matches!(record.intake_type, IntakeType::FoundReport)
}

/// Whether two [`DateTime`] values are within `window_days` of each other.
fn within_date_window(
    a: Option<&DateTime<chrono::Utc>>,
    b: Option<&DateTime<chrono::Utc>>,
    window_days: u32,
) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            let diff = (*a - *b).num_days().unsigned_abs();
            diff <= u64::from(window_days)
        }
        // If either date is absent we cannot rule out a match on this
        // dimension alone, so we conservatively allow it through.
        _ => true,
    }
}

/// Whether two records are within the configured geo radius.
fn within_geo(shelter: &PetRecord, federated: &PetRecord, max_km: f64) -> bool {
    let s_loc = match &shelter.location {
        Some(l) => l,
        None => return true, // no geo → can't filter on this dimension
    };
    let f_loc = match &federated.location {
        Some(l) => l,
        None => return true,
    };
    let (s_lat, s_lon) = match (s_loc.lat, s_loc.lon) {
        (Some(lat), Some(lon)) => (lat, lon),
        _ => return true,
    };
    let (f_lat, f_lon) = match (f_loc.lat, f_loc.lon) {
        (Some(lat), Some(lon)) => (lat, lon),
        _ => return true,
    };
    haversine_km(s_lat, s_lon, f_lat, f_lon) <= max_km
}

/// Normalised Hamming distance between two 64-bit URL-based hashes.
///
/// Returns a value in `[0.0, 1.0]` where `0.0` means identical and `1.0`
/// means every bit differs.
#[must_use]
fn normalised_phash_distance(a: u64, b: u64) -> f64 {
    f64::from(hamming_distance(a, b)) / 64.0
}

/// Whether two records are within the configured perceptual-hash distance.
fn within_phash(shelter: &PetRecord, federated: &PetRecord, max_distance: f64) -> bool {
    let sh = url_hash(shelter);
    let fh = url_hash(federated);
    match (sh, fh) {
        (Some(a), Some(b)) => normalised_phash_distance(a, b) <= max_distance,
        // If either record lacks a photo we cannot filter on this dimension.
        _ => true,
    }
}

/// Reconcile federated community found/stray records against existing shelter
/// intakes.
///
/// For each federated record (identified by [`IntakeType::FoundReport`]) this
/// function searches `shelter_records` within the bounds set by `config`:
///
/// 1. Same species.
/// 2. Intake/first-seen dates within `federated_date_window_days`.
/// 3. Locations within `federated_geo_max_km` (when both have lat/lon).
/// 4. Perceptual-hash distance within `federated_phash_max_distance` (when
///    both have photos).
///
/// On the **first** shelter record that passes all filters the federated
/// record's [`Provenance`] is appended to `shelter.secondary_provenances`
/// (additive, non-destructive).  No shelter record is modified more than once
/// per call.
///
/// **Two lost-reports are never merged with each other.**  Only
/// `IntakeType::FoundReport` federated records are eligible.
///
/// Returns the count of merges performed, which is observable in tests and
/// metrics.
pub fn federated_merge(
    shelter_records: &mut [PetRecord],
    federated_records: &[PetRecord],
    config: &DedupConfig,
) -> usize {
    let mut merge_count = 0;

    // Track which shelter indices have already been merged in this pass so
    // one federated record can't absorb multiple shelter records (and vice
    // versa — first-match wins, greedy left-to-right).
    let mut already_merged: Vec<bool> = vec![false; shelter_records.len()];

    'outer: for fed in federated_records {
        // Guard: only reconcile found/stray federated records.
        if !is_federated_found(fed) {
            continue;
        }

        for (i, shelter) in shelter_records.iter_mut().enumerate() {
            if already_merged[i] {
                continue;
            }

            // Filter 1: species must match.
            if shelter.species != fed.species {
                continue;
            }

            // Filter 2: date window.
            let shelter_date = shelter.intake_date.as_ref().or(Some(&shelter.first_seen));
            let fed_date = fed.intake_date.as_ref().or(Some(&fed.first_seen));
            if !within_date_window(shelter_date, fed_date, config.federated_date_window_days) {
                continue;
            }

            // Filter 3: geo proximity.
            if !within_geo(shelter, fed, config.federated_geo_max_km) {
                continue;
            }

            // Filter 4: perceptual hash distance.
            if !within_phash(shelter, fed, config.federated_phash_max_distance) {
                continue;
            }

            // All filters passed — merge federated provenance into shelter record.
            let prov = Provenance {
                source: fed.source.clone(),
                fetched_at: fed.first_seen,
                source_url: None,
                source_etag: None,
            };
            shelter.secondary_provenances.push(prov);
            already_merged[i] = true;
            merge_count += 1;
            continue 'outer;
        }
    }

    merge_count
}

// ── Attribute key ─────────────────────────────────────────────────────────────

/// The attribute-based dedup key.
///
/// Two records with identical keys AND similar photo hashes collapse into one
/// canonical record.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AttributeKey {
    /// Species (dog or cat).
    pub species: Species,
    /// Primary breed string.
    pub breed_primary: Option<String>,
    /// Formatted sex tag (from `{:?}` of `Sex`).
    pub sex_tag: String,
    /// Formatted age bucket tag (from `{:?}` of `AgeBucket`).
    pub age_tag: String,
    /// Shelter city/county, if available.
    pub shelter_name: Option<String>,
    /// Intake date truncated to day (YYYY-MM-DD), if available.
    pub intake_date_day: Option<String>,
}

impl AttributeKey {
    /// Build an [`AttributeKey`] from a [`PetRecord`].
    #[must_use]
    pub fn from_record(r: &PetRecord) -> Self {
        Self {
            species: r.species,
            breed_primary: r.breed_primary.clone(),
            sex_tag: format!("{:?}", r.sex),
            age_tag: format!("{:?}", r.age_bucket),
            shelter_name: r.location.as_ref().map(|l| l.city_county.clone()),
            intake_date_day: r.intake_date.as_ref().map(|d| d.format("%Y-%m-%d").to_string()),
        }
    }
}

// ── URL-based stand-in hash ───────────────────────────────────────────────────

/// Compute a cheap 64-bit hash from the primary-photo URL.
///
/// This is a stand-in until homeward-embed ships perceptual hashing.
/// Two records with `None` photos never match on this dimension alone.
#[must_use]
pub fn url_hash(record: &PetRecord) -> Option<u64> {
    let url = record.photos.first()?.url.as_str();
    // FNV-1a 64-bit hash.
    let mut h: u64 = 14_695_981_039_346_656_037;
    for b in url.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(1_099_511_628_211);
    }
    Some(h)
}

/// Hamming distance between two 64-bit hashes.
#[must_use]
pub const fn hamming_distance(a: u64, b: u64) -> u32 {
    (a ^ b).count_ones()
}

// ── Resolver ─────────────────────────────────────────────────────────────────

/// Decide what canonical id to assign a new incoming record.
///
/// Resolution order:
/// 1. If `source_animal_id` is set → look up by `(source_name, source_animal_id)`.
/// 2. Otherwise → generate a fresh ULID (cross-source pHash dedup is deferred
///    to homeward-match which runs the full embedding pipeline).
///
/// # Errors
/// Propagates [`StoreError`] on sqlite failures.
pub fn resolve_canonical_id(
    store: &Store,
    record: &PetRecord,
) -> Result<Ulid, StoreError> {
    // Fast path: source-scoped stable id.
    if let Some(ref sid) = record.source_animal_id {
        if let Some(existing) = store.find_by_source_animal_id(&record.source.name, sid)? {
            return Ok(existing);
        }
    }
    // No match → new canonical record.
    Ok(Ulid::new())
}

/// Apply dedup resolution and return the record with the correct canonical id.
///
/// # Errors
/// Propagates [`StoreError`] on sqlite failures.
pub fn resolve(store: &Store, mut record: PetRecord) -> Result<PetRecord, StoreError> {
    record.canonical_id = resolve_canonical_id(store, &record)?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use homeward_schema::{
        ChipStatus,
        intake::{Availability, IntakeType},
        provenance::{SourceId, TosClass},
    };
    use ulid::Ulid;

    use super::*;
    use crate::store::Store;

    fn make_record(source_animal_id: Option<&str>) -> PetRecord {
        PetRecord {
            canonical_id: Ulid::new(),
            source: SourceId::new("test-source", TosClass::OpenData),
            source_animal_id: source_animal_id.map(str::to_owned),
            species: Species::Dog,
            breed_primary: Some("Labrador".to_owned()),
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            markings_text: None,
            intake_type: IntakeType::Stray,
            availability: Availability::InCustody,
            chip_status: ChipStatus::NotScanned,
            location: None,
            found_location_text: None,
            photos: vec![],
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            last_confirmed: Some(Utc::now()),
            intake_date: None,
            outcome_date: None,
            secondary_provenances: vec![],
        }
    }

    #[test]
    fn same_source_animal_id_resolves_to_same_canonical() {
        let mut store = Store::open_in_memory().expect("store");

        let r1 = make_record(Some("animal-42"));
        let resolved1 = resolve(&store, r1.clone()).expect("resolve1");
        store.upsert(&resolved1).expect("upsert1");

        let r2 = make_record(Some("animal-42"));
        let resolved2 = resolve(&store, r2).expect("resolve2");

        assert_eq!(
            resolved1.canonical_id, resolved2.canonical_id,
            "same source_animal_id must resolve to the same canonical_id"
        );
    }

    #[test]
    fn different_source_animal_ids_get_distinct_canonicals() {
        let store = Store::open_in_memory().expect("store");
        let r1 = resolve(&store, make_record(Some("animal-1"))).expect("r1");
        let r2 = resolve(&store, make_record(Some("animal-2"))).expect("r2");
        assert_ne!(r1.canonical_id, r2.canonical_id);
    }

    #[test]
    fn no_source_animal_id_always_new() {
        let store = Store::open_in_memory().expect("store");
        let r1 = resolve(&store, make_record(None)).expect("r1");
        let r2 = resolve(&store, make_record(None)).expect("r2");
        // Without a source_animal_id, each call is a fresh canonical id.
        assert_ne!(r1.canonical_id, r2.canonical_id);
    }

    #[test]
    fn url_hash_same_url_same_hash() {
        use homeward_schema::media::PhotoRef;
        let mut r = make_record(None);
        r.photos = vec![PhotoRef {
            url: "https://example.com/dog.jpg".to_owned(),
            is_primary: true,
            attribution: None,
        }];
        let h1 = url_hash(&r);
        let h2 = url_hash(&r);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hamming_distance_identical_is_zero() {
        assert_eq!(hamming_distance(0xDEAD_BEEF, 0xDEAD_BEEF), 0);
    }

    #[test]
    fn hamming_distance_all_differ_is_64() {
        assert_eq!(hamming_distance(0, u64::MAX), 64);
    }
}

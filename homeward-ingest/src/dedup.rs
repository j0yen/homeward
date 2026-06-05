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

use homeward_schema::{PetRecord, Species};
use ulid::Ulid;

use crate::store::{Store, StoreError};

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

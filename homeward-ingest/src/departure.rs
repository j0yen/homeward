//! Departure detection — marks animals as departed via four strategies:
//!
//! 1. **Explicit** — source sets `Availability::Departed` or `outcome_date`.
//! 2. **Two-strikes absence** — missing from two consecutive full syncs.
//! 3. **404 on canonical URL** (delegated to connector; connector sets `Departed`).
//! 4. **TTL backstop** — not re-confirmed within `N × source_cadence`.
//!
//! `run_departure_detection` is always called for a single `source_name`, and
//! every strategy it drives (two-strikes absence via `increment_absent`, TTL
//! via `expire_stale`) must only ever touch records belonging to that
//! source — see `Store::expire_stale`'s doc comment for the 2026-08-13
//! incident where an unscoped TTL query departed ~147k unrelated records.

use chrono::{Duration, Utc};
use homeward_schema::Availability;
use ulid::Ulid;

use crate::store::{Store, StoreError};

/// Configuration for the departure detector.
#[derive(Debug, Clone)]
pub struct DepartureConfig {
    /// Number of consecutive absent syncs before marking departed.
    /// PRD mandates 2 (two-strikes).
    pub absent_strikes: u32,
    /// TTL backstop: expire if not re-confirmed within this duration.
    pub ttl: Duration,
}

impl Default for DepartureConfig {
    fn default() -> Self {
        Self {
            absent_strikes: 2,
            ttl: Duration::days(7),
        }
    }
}

/// Run departure detection after a full sync of `source_name`.
///
/// - Increments absence counters for records NOT in `seen_ids`.
/// - Marks records that hit `config.absent_strikes` as departed.
/// - Runs TTL expiry (records older than `config.ttl` since `last_confirmed`).
///
/// Returns the list of departed canonical ids.
///
/// # Errors
/// Propagates [`StoreError`] on sqlite or JSON errors.
pub fn run_departure_detection(
    store: &mut Store,
    source_name: &str,
    seen_ids: &[Ulid],
    config: &DepartureConfig,
) -> Result<Vec<Ulid>, StoreError> {
    let mut departed: Vec<Ulid> = Vec::new();
    let now = Utc::now();

    // Reset absent counters for all records observed in this sync.
    for id in seen_ids {
        store.reset_absent(*id, source_name)?;
    }

    // Two-strikes absence — only for records NOT in seen_ids.
    let struck = store.increment_absent(source_name, seen_ids, config.absent_strikes)?;
    for id in &struck {
        store.mark_departed(*id, now)?;
        departed.push(*id);
    }

    // TTL backstop — scoped to `source_name` (see `Store::expire_stale` doc
    // comment: an unscoped version of this call was the 2026-08-13 mass
    // departure incident).
    let cutoff = now - config.ttl;
    let expired = store.expire_stale(source_name, cutoff)?;
    for id in expired {
        if !departed.contains(&id) {
            departed.push(id);
        }
    }

    Ok(departed)
}

/// Check whether a single record's source reports explicit departure.
///
/// Returns `true` when the caller should immediately mark the record
/// departed without waiting for two-strikes.
#[must_use]
pub fn is_explicitly_departed(record: &homeward_schema::PetRecord) -> bool {
    record.availability == Availability::Departed || record.outcome_date.is_some()
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use homeward_schema::{
        ChipStatus, Species,
        intake::{Availability, IntakeType},
        provenance::{SourceId, TosClass},
    };
    use ulid::Ulid;

    use super::*;
    use crate::store::Store;

    fn insert_active(store: &mut Store, source: &str, animal_id: &str) -> Ulid {
        let id = Ulid::new();
        let record = homeward_schema::PetRecord {
            canonical_id: id,
            source: SourceId::new(source, TosClass::OpenData),
            source_animal_id: Some(animal_id.to_owned()),
            species: Species::Dog,
            breed_primary: None,
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
        };
        store.upsert(&record).expect("upsert");
        id
    }

    #[test]
    fn one_absence_does_not_depart() {
        let mut store = Store::open_in_memory().expect("store");
        let id = insert_active(&mut store, "src", "a1");
        let config = DepartureConfig {
            absent_strikes: 2,
            ttl: chrono::Duration::days(30),
        };
        // First sync: id is absent.
        let departed = run_departure_detection(&mut store, "src", &[], &config).expect("run");
        assert!(
            !departed.contains(&id),
            "one absence must not trigger departure (two-strikes required)"
        );
    }

    #[test]
    fn two_absences_departs() {
        let mut store = Store::open_in_memory().expect("store");
        let id = insert_active(&mut store, "src", "a2");
        let config = DepartureConfig {
            absent_strikes: 2,
            ttl: chrono::Duration::days(30),
        };
        run_departure_detection(&mut store, "src", &[], &config).expect("run1");
        let departed = run_departure_detection(&mut store, "src", &[], &config).expect("run2");
        assert!(
            departed.contains(&id),
            "two consecutive absences must trigger departure"
        );
        let record = store.get(id).expect("get");
        assert_eq!(
            record.availability,
            Availability::Departed,
            "departed flag must be persisted"
        );
    }

    #[test]
    fn seen_resets_absence() {
        let mut store = Store::open_in_memory().expect("store");
        let id = insert_active(&mut store, "src", "a3");
        let config = DepartureConfig {
            absent_strikes: 2,
            ttl: chrono::Duration::days(30),
        };
        // First sync: absent.
        run_departure_detection(&mut store, "src", &[], &config).expect("run1");
        // Second sync: present → reset.
        run_departure_detection(&mut store, "src", &[id], &config).expect("run2");
        // Third sync: absent again → only one strike.
        let departed = run_departure_detection(&mut store, "src", &[], &config).expect("run3");
        assert!(
            !departed.contains(&id),
            "after a re-observation, one absence must NOT trigger departure"
        );
    }

    #[test]
    fn ttl_backstop_expires_old_records() {
        let mut store = Store::open_in_memory().expect("store");
        // Insert with a very old last_confirmed.
        let id = Ulid::new();
        let old_time = Utc::now() - chrono::Duration::days(100);
        let record = homeward_schema::PetRecord {
            canonical_id: id,
            source: SourceId::new("src", TosClass::OpenData),
            source_animal_id: Some("a4".to_owned()),
            species: Species::Cat,
            breed_primary: None,
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
            first_seen: old_time,
            last_seen: old_time,
            last_confirmed: Some(old_time),
            intake_date: None,
            outcome_date: None,
            secondary_provenances: vec![],
        };
        store.upsert(&record).expect("upsert");

        let config = DepartureConfig {
            absent_strikes: 2,
            ttl: chrono::Duration::days(7),
        };
        // Run with the record "seen" so absence doesn't trigger — TTL should fire.
        let departed = run_departure_detection(&mut store, "src", &[id], &config).expect("run");
        assert!(
            departed.contains(&id),
            "TTL backstop must expire record not confirmed in >7 days"
        );
    }

    /// Regression for the 2026-08-13 mass-departure incident: a *different*
    /// source's genuine first full poll must never depart another source's
    /// records, no matter how stale they are. Production shape: "austin" had
    /// ~147k records with `last_confirmed` well past the TTL (their delta
    /// polls never re-run departure detection — see
    /// `orchestrator::tick`'s `is_full_sync` gate); "rescuegroups" ran its
    /// first-ever successful (full-sync) poll and its departure-detection
    /// call's TTL backstop was NOT scoped to "rescuegroups", so it swept up
    /// every stale "austin" record too.
    #[test]
    fn full_sync_of_one_source_never_departs_another_sources_stale_records() {
        use homeward_schema::{ChipStatus, intake::{Availability, IntakeType}, provenance::{SourceId, TosClass}};

        let mut store = Store::open_in_memory().expect("store");

        // Source A ("austin"): old records, stale well past any reasonable TTL.
        let old_time = Utc::now() - chrono::Duration::days(100);
        let mut austin_ids = Vec::new();
        for i in 0..5 {
            let id = Ulid::new();
            let record = homeward_schema::PetRecord {
                canonical_id: id,
                source: SourceId::new("austin", TosClass::OpenData),
                source_animal_id: Some(format!("austin-{i}")),
                species: Species::Dog,
                breed_primary: None,
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
                first_seen: old_time,
                last_seen: old_time,
                last_confirmed: Some(old_time),
                intake_date: None,
                outcome_date: None,
                secondary_provenances: vec![],
            };
            store.upsert(&record).expect("upsert austin");
            austin_ids.push(id);
        }

        // Source B ("rescuegroups"): brand-new records from its first-ever
        // (genuine full-sync) successful poll.
        let now = Utc::now();
        let mut rg_ids = Vec::new();
        for i in 0..3 {
            let id = Ulid::new();
            let record = homeward_schema::PetRecord {
                canonical_id: id,
                source: SourceId::new("rescuegroups", TosClass::Api),
                source_animal_id: Some(format!("rg-{i}")),
                species: Species::Dog,
                breed_primary: None,
                breed_secondary: None,
                sex: None,
                age_bucket: None,
                size: None,
                colors: vec![],
                markings_text: None,
                intake_type: IntakeType::Adoptable,
                availability: Availability::Adoptable,
                chip_status: ChipStatus::NotScanned,
                location: None,
                found_location_text: None,
                photos: vec![],
                first_seen: now,
                last_seen: now,
                last_confirmed: Some(now),
                intake_date: None,
                outcome_date: None,
                secondary_provenances: vec![],
            };
            store.upsert(&record).expect("upsert rescuegroups");
            rg_ids.push(id);
        }

        // A short TTL so austin's 100-day-old records WOULD be TTL-eligible
        // if the sweep were (still, buggily) unscoped.
        let config = DepartureConfig {
            absent_strikes: 2,
            ttl: chrono::Duration::days(7),
        };

        // Genuine full poll of "rescuegroups" only — seen_ids is exactly its
        // own brand-new records, matching what the orchestrator passes on a
        // real bootstrap tick.
        let departed = run_departure_detection(&mut store, "rescuegroups", &rg_ids, &config)
            .expect("run departure detection for rescuegroups");

        assert!(
            departed.is_empty(),
            "rescuegroups' own first full poll must not depart anything of its own \
             (all its records were just seen) or anyone else's"
        );

        for id in &austin_ids {
            let record = store.get(*id).expect("austin record must still exist");
            assert_ne!(
                record.availability,
                Availability::Departed,
                "austin record must NOT be departed as a side effect of rescuegroups' full sync"
            );
        }
    }

    #[test]
    fn is_explicitly_departed_with_outcome_date() {
        use homeward_schema::{ChipStatus, intake::{Availability, IntakeType}, provenance::{SourceId, TosClass}};
        let record = homeward_schema::PetRecord {
            canonical_id: Ulid::new(),
            source: SourceId::new("src", TosClass::OpenData),
            source_animal_id: None,
            species: Species::Dog,
            breed_primary: None,
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            markings_text: None,
            intake_type: IntakeType::Adoptable,
            availability: Availability::InCustody,
            chip_status: ChipStatus::NotScanned,
            location: None,
            found_location_text: None,
            photos: vec![],
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            last_confirmed: Some(Utc::now()),
            intake_date: None,
            outcome_date: Some(Utc::now()),
            secondary_provenances: vec![],
        };
        assert!(is_explicitly_departed(&record));
    }

    #[test]
    fn delete_org_removes_records() {
        let mut store = Store::open_in_memory().expect("store");
        insert_active(&mut store, "org-a", "x1");
        insert_active(&mut store, "org-a", "x2");
        insert_active(&mut store, "org-b", "y1");

        let deleted = store.delete_org("org-a").expect("delete");
        assert_eq!(deleted, 2, "both org-a records must be removed");

        let remaining = store.list_all().expect("list");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].source.name, "org-b");
    }
}

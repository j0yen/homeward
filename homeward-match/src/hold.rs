//! Stray-hold flagging — identifies strays within their legal hold window.
//!
//! A stray within its hold period is the *most important* candidate to surface
//! to an owner: they have a limited window to reclaim the animal before it
//! enters adoption. This module computes the hold deadline and flags such
//! candidates so the match pipeline can sort them with priority.

use chrono::{DateTime, Duration, Utc};
use homeward_schema::{Availability, IntakeType, PetRecord};

/// Default stray hold duration used when no jurisdiction-specific table
/// is provided. US shelters commonly require 3–5 days minimum; we use 5
/// as a conservative default.
pub const DEFAULT_HOLD_DAYS: i64 = 5;

/// Compute the stray-hold deadline for a record, if applicable.
///
/// Returns `Some(deadline)` when:
/// - `record.intake_type == IntakeType::Stray`, AND
/// - `record.availability == Availability::InCustody` (still in hold), AND
/// - `record.intake_date` is set (needed to compute the deadline).
///
/// Returns `None` in all other cases (not a stray, already adopted, or
/// intake date unknown).
///
/// `hold_days` is the jurisdiction's required hold period. Pass
/// `DEFAULT_HOLD_DAYS` when no jurisdiction-specific value is available.
///
/// # Panics
/// Does not panic.
#[must_use]
pub fn stray_hold_deadline(record: &PetRecord, hold_days: i64) -> Option<DateTime<Utc>> {
    if record.intake_type != IntakeType::Stray {
        return None;
    }
    if record.availability != Availability::InCustody {
        return None;
    }
    let intake = record.intake_date?;
    Some(intake + Duration::days(hold_days))
}

/// Returns `true` if the record is a stray currently within its hold window.
///
/// Uses `now` as the reference time so tests can inject a fixed clock.
#[must_use]
pub fn is_in_stray_hold(record: &PetRecord, hold_days: i64, now: DateTime<Utc>) -> bool {
    stray_hold_deadline(record, hold_days).is_some_and(|deadline| now < deadline)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use homeward_schema::{Availability, ChipStatus, IntakeType, Species};
    use homeward_schema::provenance::{SourceId, TosClass};

    fn base_date() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 0, 0, 0).unwrap()
    }

    fn make_record(intake_type: IntakeType, avail: Availability, intake_offset_days: Option<i64>) -> PetRecord {
        let intake_date = intake_offset_days.map(|d| base_date() + Duration::days(d));
        PetRecord {
            canonical_id: ulid::Ulid::nil(),
            source: SourceId::new("test", TosClass::OpenData),
            source_animal_id: None,
            species: Species::Dog,
            breed_primary: None,
            breed_secondary: None,
            sex: None,
            age_bucket: None,
            size: None,
            colors: vec![],
            markings_text: None,
            intake_type,
            availability: avail,
            chip_status: ChipStatus::NotScanned,
            location: None,
            found_location_text: None,
            photos: vec![],
            first_seen: base_date(),
            last_seen: base_date(),
            last_confirmed: None,
            intake_date,
            outcome_date: None,
            secondary_provenances: vec![],
        }
    }

    #[test]
    fn stray_in_custody_has_deadline() {
        let record = make_record(IntakeType::Stray, Availability::InCustody, Some(0));
        let deadline = stray_hold_deadline(&record, DEFAULT_HOLD_DAYS);
        assert!(deadline.is_some(), "stray in custody should have a deadline");
        assert_eq!(
            deadline.unwrap(),
            base_date() + Duration::days(DEFAULT_HOLD_DAYS)
        );
    }

    #[test]
    fn non_stray_has_no_deadline() {
        let record = make_record(IntakeType::OwnerSurrender, Availability::InCustody, Some(0));
        assert!(
            stray_hold_deadline(&record, DEFAULT_HOLD_DAYS).is_none(),
            "owner surrender should have no deadline"
        );
    }

    #[test]
    fn stray_adoptable_has_no_deadline() {
        // Once availability flips to Adoptable, hold period ended.
        let record = make_record(IntakeType::Stray, Availability::Adoptable, Some(-10));
        assert!(
            stray_hold_deadline(&record, DEFAULT_HOLD_DAYS).is_none(),
            "adoptable stray should have no hold deadline"
        );
    }

    #[test]
    fn stray_no_intake_date_has_no_deadline() {
        let record = make_record(IntakeType::Stray, Availability::InCustody, None);
        assert!(
            stray_hold_deadline(&record, DEFAULT_HOLD_DAYS).is_none(),
            "stray without intake_date cannot compute deadline"
        );
    }

    #[test]
    fn is_in_stray_hold_within_window() {
        let record = make_record(IntakeType::Stray, Availability::InCustody, Some(0));
        // 2 days after intake — within 5-day hold
        let now = base_date() + Duration::days(2);
        assert!(is_in_stray_hold(&record, DEFAULT_HOLD_DAYS, now));
    }

    #[test]
    fn is_in_stray_hold_after_window() {
        let record = make_record(IntakeType::Stray, Availability::InCustody, Some(0));
        // 6 days after intake — past 5-day hold
        let now = base_date() + Duration::days(6);
        assert!(!is_in_stray_hold(&record, DEFAULT_HOLD_DAYS, now));
    }
}

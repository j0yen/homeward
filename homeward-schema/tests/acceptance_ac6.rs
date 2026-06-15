//! AC6: All public types round-trip through JSON; optional fields default
//! correctly when absent.

use chrono::Utc;
use homeward_schema::{
    chip::ChipStatus,
    geo::ShelterLocation,
    intake::{Availability, IntakeType},
    lost::{BrokeredContactToken, CoarseLocation, LostReport, LostStatus},
    media::PhotoRef,
    provenance::{Provenance, SourceId, TosClass},
    PetRecord, Species,
};
use ulid::Ulid;

fn make_full_pet() -> PetRecord {
    PetRecord {
        canonical_id: Ulid::new(),
        source: SourceId::new("rescuegroups", TosClass::Api),
        source_animal_id: Some("rg-12345".into()),
        species: Species::Dog,
        breed_primary: Some("Labrador".into()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec!["black".into(), "white".into()],
        markings_text: Some("white patch on chest".into()),
        intake_type: IntakeType::Transfer,
        availability: Availability::Adoptable,
        chip_status: ChipStatus::Scanned {
            chip: Some("123456789012345".into()),
        },
        location: Some(ShelterLocation::new(
            Some(47.6_f64),
            Some(-122.3_f64),
            2,
            "Seattle".into(),
            Some("WA".into()),
        )),
        found_location_text: None,
        photos: vec![PhotoRef::new("https://example.com/dog.jpg".into())],
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        last_confirmed: None,
        intake_date: None,
        outcome_date: None,
        secondary_provenances: vec![],
    }
}

#[test]
fn ac6_pet_record_json_roundtrip() {
    let pet = make_full_pet();
    let json = serde_json::to_string(&pet).expect("serialize PetRecord");
    let back: PetRecord = serde_json::from_str(&json).expect("deserialize PetRecord");
    assert_eq!(back.canonical_id, pet.canonical_id);
    assert_eq!(back.species, pet.species);
    assert_eq!(back.breed_primary, pet.breed_primary);
    assert_eq!(back.colors, pet.colors);
}

#[test]
fn ac6_pet_record_missing_optionals_use_defaults() {
    // Minimal JSON — only required fields present.
    let now = Utc::now();
    let id = Ulid::new();
    let json = serde_json::json!({
        "canonical_id": id.to_string(),
        "source": { "name": "test", "tos_class": "api" },
        "species": "dog",
        "intake_type": "stray",
        "availability": "in_custody",
        "chip_status": { "status": "not_scanned" },
        "first_seen": now,
        "last_seen": now
    });
    let pet: PetRecord =
        serde_json::from_value(json).expect("deserialize minimal PetRecord");
    // Optional fields should default.
    assert!(pet.source_animal_id.is_none());
    assert!(pet.breed_primary.is_none());
    assert!(pet.photos.is_empty());
    assert!(pet.colors.is_empty());
    assert!(pet.location.is_none());
}

#[test]
fn ac6_provenance_json_roundtrip() {
    let prov = Provenance {
        source: SourceId::new("seattle_socrata", TosClass::OpenData),
        fetched_at: Utc::now(),
        source_url: Some("https://data.seattle.gov/animals/123".into()),
        source_etag: Some("etag-abc".into()),
    };
    let json = serde_json::to_string(&prov).expect("serialize Provenance");
    let back: Provenance = serde_json::from_str(&json).expect("deserialize Provenance");
    assert_eq!(back.source.name, prov.source.name);
    assert_eq!(back.source_url, prov.source_url);
}

#[test]
fn ac6_lost_report_json_roundtrip() {
    let report = LostReport {
        report_id: Ulid::new().to_string(),
        species: Species::Cat,
        breed_primary: Some("Tabby".into()),
        breed_secondary: None,
        sex: None,
        age_bucket: None,
        size: None,
        colors: vec!["orange".into()],
        description: Some("striped tabby".into()),
        photos: vec![],
        last_seen: CoarseLocation {
            zip_code: Some("98101".into()),
            city: Some("Seattle".into()),
            state: Some("WA".into()),
            radius_miles: None,
        },
        contact: BrokeredContactToken::new("relay-xyz".into()),
        created: Utc::now(),
        expires: Utc::now(),
        status: LostStatus::Active,
        notify_url: None,
    };
    let json = serde_json::to_string(&report).expect("serialize LostReport");
    let back: LostReport = serde_json::from_str(&json).expect("deserialize LostReport");
    assert_eq!(back.report_id, report.report_id);
    assert_eq!(back.species, report.species);
    assert_eq!(back.status, report.status);
}

#[test]
fn ac6_lost_report_missing_optionals_default() {
    let now = Utc::now();
    let json = serde_json::json!({
        "report_id": Ulid::new().to_string(),
        "species": "cat",
        "last_seen": {
            "zip_code": "98101"
        },
        "contact": "relay-abc",
        "created": now,
        "expires": now,
        "status": "active"
    });
    let report: LostReport =
        serde_json::from_value(json).expect("deserialize minimal LostReport");
    assert!(report.breed_primary.is_none());
    assert!(report.photos.is_empty());
    assert!(report.colors.is_empty());
}

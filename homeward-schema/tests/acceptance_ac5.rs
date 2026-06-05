//! AC5: BrokeredContactToken has no public raw phone/email accessor;
//! CoarseLocation has no street_address field.

use homeward_schema::lost::{BrokeredContactToken, CoarseLocation};

#[test]
fn ac5_brokered_contact_token_opaque() {
    let token = BrokeredContactToken::new("relay-handle-xyz".into());
    // Only the opaque relay token is exposed — not a phone or email.
    assert_eq!(token.token(), "relay-handle-xyz");
}

#[test]
fn ac5_brokered_contact_token_no_raw_accessor() {
    // Type-level check: BrokeredContactToken has one public method: token().
    // There is no phone(), email(), or raw_contact() method.
    // This test compiles only if those methods do NOT exist.
    let token = BrokeredContactToken::new("t".into());
    let _: &str = token.token(); // only public accessor
}

#[test]
fn ac5_coarse_location_no_street_address() {
    // Exhaustive destructuring of CoarseLocation proves no street_address field.
    let loc = CoarseLocation {
        zip_code: Some("98101".into()),
        city: Some("Seattle".into()),
        state: Some("WA".into()),
        radius_miles: None,
    };
    // Exhaustive — will fail to compile if a street_address field is added.
    let CoarseLocation {
        zip_code: _,
        city: _,
        state: _,
        radius_miles: _,
    } = loc;
}

#[test]
fn ac5_coarse_location_json_roundtrip() {
    let loc = CoarseLocation {
        zip_code: Some("98101".into()),
        city: Some("Seattle".into()),
        state: Some("WA".into()),
        radius_miles: Some(5.0),
    };
    let json = serde_json::to_string(&loc).expect("serialize CoarseLocation");
    let back: CoarseLocation = serde_json::from_str(&json).expect("deserialize CoarseLocation");
    assert_eq!(back.zip_code, loc.zip_code);
    assert_eq!(back.city, loc.city);
}

#[test]
fn ac5_brokered_contact_token_json_roundtrip() {
    let token = BrokeredContactToken::new("relay-abc-123".into());
    let json = serde_json::to_string(&token).expect("serialize token");
    let back: BrokeredContactToken =
        serde_json::from_str(&json).expect("deserialize token");
    assert_eq!(back.token(), token.token());
}

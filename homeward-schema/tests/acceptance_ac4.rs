//! AC4: PhotoRef has no bytes/data field (hotlink-only copyright posture).

use homeward_schema::media::PhotoRef;

#[test]
fn ac4_photo_ref_has_url_field() {
    let p = PhotoRef::new("https://example.com/photo.jpg".into());
    assert_eq!(p.url, "https://example.com/photo.jpg");
}

#[test]
fn ac4_photo_ref_with_attribution() {
    let p = PhotoRef::with_attribution(
        "https://example.com/photo.jpg".into(),
        "© Seattle Animal Shelter".into(),
    );
    assert_eq!(
        p.attribution.as_deref(),
        Some("© Seattle Animal Shelter")
    );
}

#[test]
fn ac4_photo_ref_json_roundtrip() {
    let p = PhotoRef {
        url: "https://example.com/photo.jpg".into(),
        attribution: Some("© Test".into()),
        is_primary: true,
    };
    let json = serde_json::to_string(&p).expect("serialize PhotoRef");
    let back: PhotoRef = serde_json::from_str(&json).expect("deserialize PhotoRef");
    assert_eq!(back.url, p.url);
    assert_eq!(back.attribution, p.attribution);
    assert_eq!(back.is_primary, p.is_primary);
}

#[test]
fn ac4_photo_ref_no_bytes_field() {
    // Type-level guarantee: PhotoRef has no bytes/data field.
    // This test documents the invariant and will fail to compile if someone
    // adds a bytes field that is accessible.
    //
    // The struct fields are: url, attribution, is_primary.
    // We verify by exhaustive struct pattern (no `..`).
    let PhotoRef {
        url: _,
        attribution: _,
        is_primary: _,
    } = PhotoRef::new("https://example.com/x.jpg".into());
    // If someone adds a `bytes` field, this exhaustive destructuring will
    // fail to compile, catching the violation at the type level.
}

//! Deterministic plain-text flyer renderer for lost-pet reports.
//!
//! Takes a [`LostReport`] directly and renders a human-postable flyer.
//! Same input always produces byte-identical output.

use homeward_schema::LostReport;

/// Generates a deterministic, human-postable text flyer for a [`LostReport`].
///
/// Same input → byte-identical output. No network calls, no randomness.
///
/// # Format
///
/// ```text
/// LOST: Buddy (dog / Labrador)
/// Last seen: Austin, TX 78701 on 2026-06-11
/// Description: black and white, friendly dog
/// Photo: https://example.com/photo.jpg
/// Contact: https://homeward.example/relay/tok_abc123
/// ---
/// Report filed via homeward. Please post to local groups with this flyer.
/// ```
#[must_use]
pub fn render_flyer(report: &LostReport) -> String {
    // Name: use description first word heuristic or "Unknown"
    // (LostReport has no name field — use species + breed as identity)
    let species_str = format!("{:?}", report.species).to_lowercase();
    let breed_str = report
        .breed_primary
        .as_deref()
        .unwrap_or("mixed");

    let header = format!(
        "LOST: Unknown ({species_str} / {breed_str})"
    );

    // Coarse location — no street address
    let loc = &report.last_seen;
    let mut loc_parts: Vec<String> = Vec::new();
    if let Some(city) = &loc.city {
        loc_parts.push(city.clone());
    }
    if let Some(state) = &loc.state {
        loc_parts.push(state.clone());
    }
    if let Some(zip) = &loc.zip_code {
        loc_parts.push(zip.clone());
    }
    let location_str = if loc_parts.is_empty() {
        "location not specified".to_owned()
    } else {
        loc_parts.join(", ")
    };

    let date_str = report.created.format("%Y-%m-%d").to_string();
    let last_seen_line = format!("Last seen: {location_str} on {date_str}");

    // Color + description (truncated to 100 chars)
    let color_str = if report.colors.is_empty() {
        None
    } else {
        Some(report.colors.join(", "))
    };
    let desc_raw = match (&color_str, &report.description) {
        (Some(c), Some(d)) => format!("{c}, {d}"),
        (Some(c), None) => c.clone(),
        (None, Some(d)) => d.clone(),
        (None, None) => String::new(),
    };
    let desc_truncated: String = desc_raw.chars().take(100).collect();
    let description_line = if desc_truncated.is_empty() {
        "Description: (none)".to_owned()
    } else {
        format!("Description: {desc_truncated}")
    };

    // Photo — first primary photo URL or "none"
    let photo_url = report
        .photos
        .iter()
        .find(|p| p.is_primary)
        .or_else(|| report.photos.first())
        .map(|p| p.url.as_str())
        .unwrap_or("none");
    let photo_line = format!("Photo: {photo_url}");

    // Contact — relay URL constructed from token (never raw PII)
    let contact_url = format!(
        "https://homeward.example/relay/{}",
        report.contact.token()
    );
    let contact_line = format!("Contact: {contact_url}");

    let mut out = String::new();
    out.push_str(&header);
    out.push('\n');
    out.push_str(&last_seen_line);
    out.push('\n');
    out.push_str(&description_line);
    out.push('\n');
    out.push_str(&photo_line);
    out.push('\n');
    out.push_str(&contact_line);
    out.push('\n');
    out.push_str("---\n");
    out.push_str("Report filed via homeward. Please post to local groups with this flyer.\n");
    out
}

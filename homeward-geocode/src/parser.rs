//! Parser/normalizer for `found_location_text` strings.
//!
//! Recognises common forms:
//!
//! - 5-digit ZIP: `"78701"`
//! - `"City, ST"`: `"Austin, TX"`
//! - `"City ST"` (no comma): `"Austin TX"`
//! - Prefix with suffix city+state: `"Near downtown, Austin, TX"`
//!
//! Returns `None` rather than guessing when the text does not match a known pattern.

use crate::gazetteer::Gazetteer;
use homeward_schema::ShelterLocation;

/// Result of parsing a `found_location_text` string.
#[derive(Debug, Clone, PartialEq)]
pub enum ParsedLocation {
    /// A resolved coarse [`ShelterLocation`].
    Resolved(ShelterLocation),
    /// Text was recognised as a pattern but not found in the gazetteer.
    NotInGazetteer,
    /// Text did not match any recognised pattern.
    Unrecognised,
}

/// Attempt to parse `text` into a [`ShelterLocation`] using `gazetteer`.
///
/// Strategy (tried in order, first match wins):
/// 1. 5-digit ZIP
/// 2. `"City, ST"` — last two comma-separated tokens
/// 3. `"City ST"` — last two whitespace-separated tokens where the last is a 2-letter US state
/// 4. Returns [`ParsedLocation::Unrecognised`] on no match
///
/// Never panics. Prefers `None` over a low-confidence guess.
#[must_use]
pub fn parse_found_location(text: &str, gazetteer: &Gazetteer) -> ParsedLocation {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return ParsedLocation::Unrecognised;
    }

    // --- Strategy 1: bare 5-digit ZIP -----------------------------------
    if let Some(loc) = try_zip(trimmed, gazetteer) {
        return ParsedLocation::Resolved(loc);
    }

    // --- Strategy 2: "..., City, ST" (last two comma tokens) -----------
    if let Some(loc) = try_city_state_comma(trimmed, gazetteer) {
        return ParsedLocation::Resolved(loc);
    }

    // --- Strategy 3: "... City ST" (last two space tokens, 2-letter ST) -
    if let Some(loc) = try_city_state_space(trimmed, gazetteer) {
        return ParsedLocation::Resolved(loc);
    }

    // Text matched no pattern.
    ParsedLocation::Unrecognised
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Try to match the text as a bare 5-digit ZIP code.
fn try_zip(text: &str, gz: &Gazetteer) -> Option<ShelterLocation> {
    // Accept exactly 5 ASCII digits, possibly surrounded by whitespace (already
    // trimmed). Also accept a ZIP that appears as the last 5-digit token.
    let candidate = last_five_digit_token(text)?;
    gz.lookup_zip(candidate)
        .map(|e| e.to_shelter_location())
}

/// Return the last (or only) token that is exactly 5 ASCII digits,
/// or `None` if none exists.
fn last_five_digit_token(text: &str) -> Option<&str> {
    text.split_whitespace()
        .rev()
        .find(|tok| tok.len() == 5 && tok.chars().all(|c| c.is_ascii_digit()))
}

/// Try to match `"..., City, ST"` — extract the last two comma-delimited tokens.
fn try_city_state_comma(text: &str, gz: &Gazetteer) -> Option<ShelterLocation> {
    let parts: Vec<&str> = text.split(',').map(str::trim).collect();
    if parts.len() < 2 {
        return None;
    }
    let state_tok = *parts.last()?;
    let city_tok = parts.get(parts.len().wrapping_sub(2))?;
    if !is_state_abbr(state_tok) {
        return None;
    }
    gz.lookup_name_state(city_tok, state_tok)
        .map(|e| e.to_shelter_location())
}

/// Try to match `"City ST"` — last two space-separated tokens, last is a 2-letter state.
fn try_city_state_space(text: &str, gz: &Gazetteer) -> Option<ShelterLocation> {
    // Do NOT attempt if the text has commas (handled by comma strategy above).
    if text.contains(',') {
        return None;
    }
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() < 2 {
        return None;
    }
    let state_tok = *tokens.last()?;
    if !is_state_abbr(state_tok) {
        return None;
    }
    // Treat everything before the state token as the city name.
    // len >= 2 verified above, so saturating_sub is always safe.
    let prefix_len = tokens.len().saturating_sub(1);
    let city = tokens.iter().take(prefix_len).copied().collect::<Vec<_>>().join(" ");
    gz.lookup_name_state(&city, state_tok)
        .map(|e| e.to_shelter_location())
}

/// Return `true` if `s` is a valid 2-letter US state/territory abbreviation.
#[must_use]
fn is_state_abbr(s: &str) -> bool {
    matches!(
        s.to_uppercase().as_str(),
        "AL" | "AK" | "AZ" | "AR" | "CA" | "CO" | "CT" | "DE" | "FL" | "GA"
        | "HI" | "ID" | "IL" | "IN" | "IA" | "KS" | "KY" | "LA" | "ME" | "MD"
        | "MA" | "MI" | "MN" | "MS" | "MO" | "MT" | "NE" | "NV" | "NH" | "NJ"
        | "NM" | "NY" | "NC" | "ND" | "OH" | "OK" | "OR" | "PA" | "RI" | "SC"
        | "SD" | "TN" | "TX" | "UT" | "VT" | "VA" | "WA" | "WV" | "WI" | "WY"
        | "DC" | "AS" | "GU" | "MP" | "PR" | "VI"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gazetteer::Gazetteer;

    fn load() -> Gazetteer {
        Gazetteer::load_default().expect("load")
    }

    #[test]
    fn parse_zip_78701() {
        let gz = load();
        let result = parse_found_location("78701", &gz);
        match result {
            ParsedLocation::Resolved(loc) => {
                // Austin TX — lat roughly 30.27
                let lat = loc.lat.expect("lat");
                assert!((lat - 30.27_f64).abs() < 0.01, "lat={lat}");
                assert_eq!(loc.state.as_deref(), Some("TX"));
            }
            other => panic!("expected Resolved, got {other:?}"),
        }
    }

    #[test]
    fn parse_city_state_comma_austin() {
        let gz = load();
        let result = parse_found_location("Austin, TX", &gz);
        assert!(matches!(result, ParsedLocation::Resolved(_)));
    }

    #[test]
    fn parse_city_state_comma_dallas() {
        let gz = load();
        let result = parse_found_location("Dallas, TX", &gz);
        assert!(matches!(result, ParsedLocation::Resolved(_)));
    }

    #[test]
    fn parse_city_state_comma_long_beach() {
        let gz = load();
        let result = parse_found_location("Long Beach, CA", &gz);
        assert!(matches!(result, ParsedLocation::Resolved(_)));
    }

    #[test]
    fn parse_city_state_space_austin() {
        let gz = load();
        let result = parse_found_location("Austin TX", &gz);
        assert!(matches!(result, ParsedLocation::Resolved(_)));
    }

    #[test]
    fn parse_prefix_with_city_state() {
        let gz = load();
        let result = parse_found_location("Near downtown, Austin, TX", &gz);
        assert!(matches!(result, ParsedLocation::Resolved(_)));
    }

    #[test]
    fn garbage_returns_unrecognised() {
        let gz = load();
        let result = parse_found_location("xyzzy frobnitz 99999", &gz);
        // 99999 is not in the gazetteer → Unrecognised (no ZIP match, no city/state)
        assert!(
            matches!(
                result,
                ParsedLocation::Unrecognised | ParsedLocation::NotInGazetteer
            ),
            "expected unrecognised/not-in-gz, got {result:?}"
        );
    }

    #[test]
    fn empty_string_returns_unrecognised() {
        let gz = load();
        assert_eq!(parse_found_location("", &gz), ParsedLocation::Unrecognised);
    }

    #[test]
    fn unknown_city_returns_unrecognised_or_not_in_gz() {
        let gz = load();
        let result = parse_found_location("Xanadu, TX", &gz);
        // Xanadu,TX is not in the gazetteer
        assert!(
            matches!(
                result,
                ParsedLocation::Unrecognised | ParsedLocation::NotInGazetteer
            ),
            "expected unrecognised/not-in-gz, got {result:?}"
        );
    }
}

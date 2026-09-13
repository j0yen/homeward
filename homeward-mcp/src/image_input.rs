//! Caller-supplied image validation for `match_photo`.
//!
//! Runs before any network call to the embed sidecar (AC5: a non-image or
//! oversized payload must yield a typed validation error, never reach the
//! sidecar, and never crash the server).
//!
//! `image_url` gets a scheme allowlist (SSRF surface -- the PRD's own open
//! question, "URL-fetch of caller images: size cap and allowed schemes").
//! Rejecting every scheme but `http`/`https` here is the Rust-side half of
//! that mitigation; the actual fetch happens in the Python embed sidecar
//! (see `crates/homeward-embed-client`'s doc comment), so deeper
//! network-level guards (blocking private/link-local/loopback destinations)
//! belong to that fetch path and are out of scope for this crate.
//!
//! `image_b64` is decoded and checked against a size cap plus a JPEG/PNG
//! magic-byte sniff, so garbage or absurdly large payloads are rejected
//! before ever leaving this process.

/// Hard cap on a decoded `image_b64` payload: generous for a phone photo,
/// small enough that a malicious/broken caller can't push this process
/// into holding tens of megabytes per call.
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// Validate a caller-supplied `image_url`: must parse, and must use
/// `http`/`https` (never `file:`, `ftp:`, `gopher:`, etc).
///
/// # Errors
/// Returns a caller-facing message (never a panic) describing why the URL
/// was rejected.
pub fn validate_image_url(raw: &str) -> Result<(), String> {
    let parsed = url::Url::parse(raw).map_err(|e| format!("invalid image_url: {e}"))?;
    match parsed.scheme() {
        "http" | "https" => Ok(()),
        other => Err(format!(
            "image_url scheme '{other}' is not allowed (only http/https)"
        )),
    }
}

/// Decode a base64 `image_b64` payload and verify it is a plausible
/// JPEG/PNG under [`MAX_IMAGE_BYTES`]. Returns the decoded bytes on
/// success; the caller is responsible for not persisting them (AC3).
///
/// # Errors
/// Returns a caller-facing message (never a panic) for invalid base64, an
/// empty/oversized payload, or a payload that isn't a recognized image.
pub fn validate_image_b64(b64: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| format!("image_b64 is not valid base64: {e}"))?;
    if bytes.is_empty() {
        return Err("image_b64 decoded to zero bytes".to_owned());
    }
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(format!(
            "image exceeds the {MAX_IMAGE_BYTES}-byte limit ({} bytes submitted)",
            bytes.len()
        ));
    }
    if !is_jpeg(&bytes) && !is_png(&bytes) {
        return Err("image_b64 is not a recognized JPEG or PNG image".to_owned());
    }
    Ok(bytes)
}

fn is_jpeg(bytes: &[u8]) -> bool {
    bytes.get(0..3) == Some([0xFF, 0xD8, 0xFF].as_slice())
}

fn is_png(bytes: &[u8]) -> bool {
    bytes.get(0..8) == Some([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A].as_slice())
}

#[cfg(test)]
mod tests {
    use super::{validate_image_b64, validate_image_url};
    use base64::Engine as _;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn http_and_https_urls_are_allowed() {
        assert!(validate_image_url("http://example.org/dog.jpg").is_ok());
        assert!(validate_image_url("https://example.org/dog.jpg").is_ok());
    }

    #[test]
    fn file_scheme_is_rejected() {
        let err = validate_image_url("file:///etc/passwd").expect_err("must reject file://");
        assert!(err.contains("file"), "error should name the rejected scheme: {err}");
    }

    #[test]
    fn ftp_scheme_is_rejected() {
        assert!(validate_image_url("ftp://example.org/dog.jpg").is_err());
    }

    #[test]
    fn unparseable_url_is_rejected() {
        assert!(validate_image_url("not a url").is_err());
    }

    #[test]
    fn valid_jpeg_magic_bytes_decode_ok() {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0];
        bytes.extend_from_slice(&[0u8; 16]);
        let decoded = validate_image_b64(&b64(&bytes)).expect("valid jpeg header should decode");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn valid_png_magic_bytes_decode_ok() {
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&[0u8; 16]);
        let decoded = validate_image_b64(&b64(&bytes)).expect("valid png header should decode");
        assert_eq!(decoded, bytes);
    }

    #[test]
    fn non_image_payload_is_rejected() {
        let bytes = b"this is definitely not an image, just text".to_vec();
        let err = validate_image_b64(&b64(&bytes)).expect_err("must reject non-image payload");
        assert!(err.contains("not a recognized"), "unexpected message: {err}");
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let mut bytes = vec![0xFF, 0xD8, 0xFF];
        bytes.extend(std::iter::repeat_n(0u8, super::MAX_IMAGE_BYTES + 1));
        let err = validate_image_b64(&b64(&bytes)).expect_err("must reject oversized payload");
        assert!(err.contains("exceeds"), "unexpected message: {err}");
    }

    #[test]
    fn invalid_base64_is_rejected() {
        assert!(validate_image_b64("!!!not base64!!!").is_err());
    }

    #[test]
    fn empty_payload_is_rejected() {
        assert!(validate_image_b64(&b64(&[])).is_err());
    }
}

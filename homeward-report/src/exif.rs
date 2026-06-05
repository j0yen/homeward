//! EXIF stripping for uploaded photos.
//!
//! A JPEG file embeds metadata in APP1 markers (0xFFE1) that may contain GPS
//! coordinates via Exif. This module removes every APP1 marker (and all other
//! APP markers except APP0/JFIF) so that the stored/forwarded image carries no
//! EXIF metadata at all — in particular no home-GPS coordinates (AC1).
//!
//! We do not depend on an external EXIF library. The stripping is structural:
//! we walk the JPEG marker stream and drop any marker whose type falls in the
//! APP range (0xFFE0–0xFFEF) except 0xFFE0 (JFIF/APP0), which is harmless
//! and required by some decoders. Non-JPEG input is returned unchanged.

/// Strip EXIF and all APP markers (except APP0/JFIF) from `bytes`.
///
/// - If `bytes` is not a valid JPEG (does not start with SOI 0xFFD8), returns
///   a copy of the input unchanged.
/// - Otherwise returns a new buffer with every APP1–APP15 marker removed.
#[must_use]
pub fn strip_exif(bytes: &[u8]) -> Vec<u8> {
    // JPEG SOI marker — need at least 2 bytes with 0xFF 0xD8.
    let (Some(&0xFF), Some(&0xD8)) = (bytes.first(), bytes.get(1)) else {
        return bytes.to_vec();
    };

    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    // Write SOI
    out.extend_from_slice(&[0xFF, 0xD8]);

    let mut pos = 2usize;
    while pos + 1 < bytes.len() {
        // Each marker starts with 0xFF — use get() to avoid indexing lint.
        let (Some(&0xFF), Some(&marker_type)) = (bytes.get(pos), bytes.get(pos + 1)) else {
            // Not a valid marker byte; copy rest as-is and stop stripping.
            if let Some(rest) = bytes.get(pos..) {
                out.extend_from_slice(rest);
            }
            break;
        };

        // Stand-alone markers (no length field): SOI, EOI, RST0-RST7, TEM
        let standalone = matches!(marker_type, 0xD8 | 0xD9 | 0xD0..=0xD7 | 0x01);
        if standalone {
            out.push(0xFF);
            out.push(marker_type);
            pos += 2;
            // If EOI, we're done
            if marker_type == 0xD9 {
                break;
            }
            continue;
        }

        // All other markers have a 2-byte length field (inclusive of itself,
        // exclusive of the 0xFF marker byte).
        let (Some(&len_hi), Some(&len_lo)) = (bytes.get(pos + 2), bytes.get(pos + 3)) else {
            // Truncated — copy what's left
            if let Some(rest) = bytes.get(pos..) {
                out.extend_from_slice(rest);
            }
            break;
        };
        let seg_len = usize::from(u16::from_be_bytes([len_hi, len_lo]));
        let seg_end = pos + 2 + seg_len; // pos+2 is the length field start

        // APP markers: 0xFFE0 (APP0/JFIF) is kept; APP1–APP15 are dropped.
        let is_app = (0xE0..=0xEF).contains(&marker_type);
        let keep = !is_app || marker_type == 0xE0;

        if keep {
            let end = seg_end.min(bytes.len());
            if let Some(seg) = bytes.get(pos..end) {
                out.extend_from_slice(seg);
            }
        }
        // Advance past marker + segment
        pos = seg_end.min(bytes.len());
    }

    out
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal JPEG with an embedded APP1 (EXIF) marker.
    fn jpeg_with_exif() -> Vec<u8> {
        let mut buf = vec![
            0xFF, 0xD8, // SOI
        ];
        // APP1 marker (EXIF stub, 12 bytes payload incl length field)
        let app1_payload = b"ExifFakeGPS!";
        let app1_len = (2 + app1_payload.len()) as u16;
        buf.push(0xFF);
        buf.push(0xE1); // APP1
        buf.extend_from_slice(&app1_len.to_be_bytes());
        buf.extend_from_slice(app1_payload);
        // A minimal SOF0 segment header to make it look like valid JPEG
        // (stripped by tests, but present to test that non-APP markers pass through)
        buf.push(0xFF);
        buf.push(0xE0); // APP0 — should be kept
        let app0_payload = b"JFIFstub";
        let app0_len = (2 + app0_payload.len()) as u16;
        buf.extend_from_slice(&app0_len.to_be_bytes());
        buf.extend_from_slice(app0_payload);
        // EOI
        buf.push(0xFF);
        buf.push(0xD9);
        buf
    }

    #[test]
    fn strips_app1_exif_marker() {
        let input = jpeg_with_exif();
        let output = strip_exif(&input);

        // Output must still be a JPEG (SOI present)
        assert_eq!(output[0], 0xFF);
        assert_eq!(output[1], 0xD8);

        // APP1 marker (0xFFE1) must be absent
        let has_app1 = output
            .windows(2)
            .any(|w| w[0] == 0xFF && w[1] == 0xE1);
        assert!(!has_app1, "APP1/EXIF marker should have been stripped");
    }

    #[test]
    fn keeps_app0_jfif_marker() {
        let input = jpeg_with_exif();
        let output = strip_exif(&input);

        // APP0 marker (0xFFE0) must still be present
        let has_app0 = output
            .windows(2)
            .any(|w| w[0] == 0xFF && w[1] == 0xE0);
        assert!(has_app0, "APP0/JFIF marker should have been kept");
    }

    #[test]
    fn non_jpeg_returned_unchanged() {
        let input = b"PNG\x89not-a-jpeg".to_vec();
        let output = strip_exif(&input);
        assert_eq!(output, input);
    }

    #[test]
    fn jpeg_without_exif_unchanged_structure() {
        // A bare JPEG with no APP markers at all
        let input = vec![0xFF, 0xD8, 0xFF, 0xD9]; // SOI + EOI
        let output = strip_exif(&input);
        assert_eq!(output[0], 0xFF);
        assert_eq!(output[1], 0xD8);
        // EOI should still be present
        assert!(output
            .windows(2)
            .any(|w| w[0] == 0xFF && w[1] == 0xD9));
    }
}

//! Small helpers shared by more than one protocol adapter. Kept private to
//! the crate: none of this is part of the public API surface.

use std::time::{SystemTime, UNIX_EPOCH};

/// Best-effort MIME type for an image referenced by URL, based on its file
/// extension. Used when a wire format gives us a bare URL with no
/// content-type metadata (e.g. OpenAI `image_url` parts, Anthropic `url`
/// image sources) and `ImageBlock::mime` still needs *some* value.
pub(crate) fn guess_mime_from_extension(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".png") {
        "image/png".to_string()
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg".to_string()
    } else if lower.ends_with(".gif") {
        "image/gif".to_string()
    } else if lower.ends_with(".webp") {
        "image/webp".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// Best-effort MIME type for a *base64-encoded* image payload, sniffed from
/// the leading characters of the base64 text itself (which map 1:1 onto the
/// leading magic bytes of the decoded image). Ollama's `/api/chat` wire
/// format carries raw base64 strings in an `images` array with no
/// accompanying content-type, so this is the only signal available without
/// pulling in a base64-decoding dependency just to peek at four bytes.
pub(crate) fn sniff_base64_image_mime(data: &str) -> String {
    let trimmed = data.trim_start();
    if trimmed.starts_with("iVBORw0KGgo") {
        "image/png".to_string()
    } else if trimmed.starts_with("/9j/") {
        "image/jpeg".to_string()
    } else if trimmed.starts_with("R0lGOD") {
        "image/gif".to_string()
    } else if trimmed.starts_with("UklGR") {
        "image/webp".to_string()
    } else {
        "application/octet-stream".to_string()
    }
}

/// Parses a `data:` URL (`data:<mime>;base64,<payload>`) into its mime type
/// and base64 payload. Returns `None` for anything else (a plain http(s)
/// URL, or a malformed data URL), letting the caller fall back to treating
/// the string as a remote URL.
pub(crate) fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let mime = meta.split(';').next().unwrap_or("application/octet-stream");
    Some((mime.to_string(), data.to_string()))
}

/// Current time formatted as RFC 3339 (UTC, nanosecond precision), matching
/// the `created_at` shape Ollama's `/api/chat` responses use. Implemented
/// with plain arithmetic (Howard Hinnant's `civil_from_days` algorithm)
/// rather than pulling in a datetime crate just for one timestamp field.
pub(crate) fn rfc3339_now() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = dur.as_secs();
    let nanos = dur.subsec_nanos();
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{nanos:09}Z")
}

/// Converts a count of days since the Unix epoch into a proleptic Gregorian
/// (year, month, day) triple. See Howard Hinnant's "chrono-Compatible
/// Low-Level Date Algorithms" for a derivation; this is a direct transcription.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_mime_from_common_extensions() {
        assert_eq!(guess_mime_from_extension("https://x/a.PNG"), "image/png");
        assert_eq!(
            guess_mime_from_extension("https://x/a.jpeg?w=1"),
            "image/jpeg"
        );
        assert_eq!(
            guess_mime_from_extension("https://x/a.bin"),
            "application/octet-stream"
        );
    }

    #[test]
    fn sniffs_png_and_jpeg_signatures() {
        assert_eq!(sniff_base64_image_mime("iVBORw0KGgoAAAA"), "image/png");
        assert_eq!(sniff_base64_image_mime("/9j/4AAQSkZJRg"), "image/jpeg");
        assert_eq!(
            sniff_base64_image_mime("not-a-real-image"),
            "application/octet-stream"
        );
    }

    #[test]
    fn parses_data_url() {
        let (mime, data) = parse_data_url("data:image/png;base64,aGVsbG8=").unwrap();
        assert_eq!(mime, "image/png");
        assert_eq!(data, "aGVsbG8=");
        assert!(parse_data_url("https://example.com/a.png").is_none());
    }

    #[test]
    fn rfc3339_now_is_well_formed() {
        let ts = rfc3339_now();
        // Spot check shape rather than exact value: YYYY-MM-DDTHH:MM:SS.NNNNNNNNNZ
        assert_eq!(ts.len(), "2024-01-01T00:00:00.000000000Z".len());
        assert!(ts.starts_with("20"));
        assert!(ts.ends_with('Z'));
    }

    #[test]
    fn civil_from_days_matches_known_epoch_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_723), (2024, 1, 1));
    }
}

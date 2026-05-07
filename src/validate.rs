// Input validation helpers. Reject malformed input at the boundary before
// it reaches SQLite and silently produces empty result sets.

/// Validate ISO-ish date strings. Accepts: YYYY, YYYY-MM, YYYY-MM-DD,
/// YYYY-MM-DD HH:MM:SS, empty string, None.
/// Rejects natural-language dates ("yesterday", "March 2026") and garbage.
#[allow(clippy::needless_lifetimes)]
pub fn sanitize_iso_date<'a>(val: Option<&'a str>) -> anyhow::Result<Option<&'a str>> {
    let s = match val {
        None | Some("") => return Ok(None),
        Some(s) => s.trim(),
    };
    if s.is_empty() {
        return Ok(None);
    }

    if s.len() < 4 || !s[..4].chars().all(|c| c.is_ascii_digit()) {
        return Err(anyhow::anyhow!(
            "Invalid date: '{s}' — expected ISO (YYYY, YYYY-MM, YYYY-MM-DD, YYYY-MM-DD HH:MM:SS)"
        ));
    }

    let parts: Vec<&str> = s.split(&[' ', '-', ':', 'T'][..]).collect();
    if parts.is_empty() {
        return Err(anyhow::anyhow!("Invalid date: '{s}'"));
    }

    let year: i32 = parts[0]
        .parse()
        .map_err(|_| anyhow::anyhow!("Invalid year in date: '{s}'"))?;
    if !(1970..=2099).contains(&year) {
        return Err(anyhow::anyhow!("Year out of range (1970–2099): '{s}'"));
    }

    if parts.len() >= 2 {
        let month: u32 = parts[1]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid month in date: '{s}'"))?;
        if !(1..=12).contains(&month) {
            return Err(anyhow::anyhow!("Month out of range (1–12): '{s}'"));
        }
    }

    if parts.len() >= 3 {
        let day: u32 = parts[2]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid day in date: '{s}'"))?;
        if !(1..=31).contains(&day) {
            return Err(anyhow::anyhow!("Day out of range (1–31): '{s}'"));
        }
    }

    if parts.len() >= 4 {
        let hour: u32 = parts[3]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid hour in date: '{s}'"))?;
        if hour > 23 {
            return Err(anyhow::anyhow!("Hour out of range (0–23): '{s}'"));
        }
    }

    if parts.len() >= 5 {
        let min: u32 = parts[4]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid minute in date: '{s}'"))?;
        if min > 59 {
            return Err(anyhow::anyhow!("Minute out of range (0–59): '{s}'"));
        }
    }

    if parts.len() >= 6 {
        let sec: u32 = parts[5]
            .parse()
            .map_err(|_| anyhow::anyhow!("Invalid second in date: '{s}'"))?;
        if sec > 59 {
            return Err(anyhow::anyhow!("Second out of range (0–59): '{s}'"));
        }
    }

    if parts.len() > 6 {
        return Err(anyhow::anyhow!(
            "Too many date components in: '{s}'"
        ));
    }

    Ok(Some(s))
}

/// Validate a wing/room/agent name. Rejects null bytes, control chars,
/// and strings that are too long.
#[allow(clippy::needless_lifetimes)]
pub fn sanitize_name<'a>(
    val: Option<&'a str>,
    field_name: &str,
) -> anyhow::Result<Option<&'a str>> {
    let s = match val {
        None | Some("") => return Ok(None),
        Some(s) => s,
    };
    validate_name_str(s, field_name)?;
    Ok(Some(s))
}

/// Validate a required name (not optional — empty string is rejected).
#[allow(clippy::needless_lifetimes)]
pub fn sanitize_name_required<'a>(
    val: &'a str,
    field_name: &str,
) -> anyhow::Result<&'a str> {
    if val.is_empty() {
        return Err(anyhow::anyhow!("{field_name} must not be empty"));
    }
    validate_name_str(val, field_name)?;
    Ok(val)
}

fn validate_name_str(s: &str, field_name: &str) -> anyhow::Result<()> {
    if s.len() > 256 {
        return Err(anyhow::anyhow!(
            "{field_name} exceeds 256 character limit"
        ));
    }
    if s.contains('\0') {
        return Err(anyhow::anyhow!("{field_name} contains null byte"));
    }
    for (i, c) in s.char_indices() {
        if c.is_control() && c != ' ' {
            return Err(anyhow::anyhow!(
                "{field_name} contains control character U+{:04X} at position {i}",
                c as u32
            ));
        }
    }
    Ok(())
}

/// Validate content string. Rejects null bytes and over-size content.
#[allow(clippy::needless_lifetimes)]
pub fn sanitize_content<'a>(val: &'a str) -> anyhow::Result<&'a str> {
    if val.contains('\0') {
        return Err(anyhow::anyhow!("content contains null byte"));
    }
    if val.len() > 100_000 {
        return Err(anyhow::anyhow!(
            "content exceeds 100,000 character limit ({})",
            val.len()
        ));
    }
    if val.len() > 10_000 {
        crate::log::log!(
            "warn",
            "content is large ({} chars) — embedding quality may degrade",
            val.len()
        );
    }
    Ok(val)
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accepts_full_iso() {
        assert!(sanitize_iso_date(Some("2026-05-06 18:30:00")).is_ok());
        assert!(sanitize_iso_date(Some("2026-05-06T18:30:00")).is_ok());
    }

    #[test]
    fn test_accepts_date_only() {
        assert!(sanitize_iso_date(Some("2026-05-06")).is_ok());
    }

    #[test]
    fn test_accepts_month_only() {
        assert!(sanitize_iso_date(Some("2026-05")).is_ok());
    }

    #[test]
    fn test_accepts_year_only() {
        assert!(sanitize_iso_date(Some("2026")).is_ok());
    }

    #[test]
    fn test_accepts_empty() {
        assert!(sanitize_iso_date(None).unwrap().is_none());
        assert!(sanitize_iso_date(Some("")).unwrap().is_none());
        assert!(sanitize_iso_date(Some("  ")).unwrap().is_none());
    }

    #[test]
    fn test_rejects_natural_language() {
        assert!(sanitize_iso_date(Some("yesterday")).is_err());
        assert!(sanitize_iso_date(Some("March 2026")).is_err());
    }

    #[test]
    fn test_rejects_garbage() {
        assert!(sanitize_iso_date(Some("not a date")).is_err());
    }

    #[test]
    fn test_rejects_out_of_range_month() {
        assert!(sanitize_iso_date(Some("2026-13-01")).is_err());
    }

    #[test]
    fn test_rejects_out_of_range_day() {
        assert!(sanitize_iso_date(Some("2026-01-32")).is_err());
    }

    #[test]
    fn test_rejects_year_out_of_range() {
        assert!(sanitize_iso_date(Some("1800-01-01")).is_err());
    }

    #[test]
    fn test_rejects_null_byte_in_name() {
        assert!(sanitize_name(Some("hello\0world"), "wing").is_err());
    }

    #[test]
    fn test_rejects_empty_required() {
        assert!(sanitize_name_required("", "wing").is_err());
    }

    #[test]
    fn test_accepts_empty_optional() {
        assert!(sanitize_name(None, "wing").unwrap().is_none());
        assert!(sanitize_name(Some(""), "wing").unwrap().is_none());
    }

    #[test]
    fn test_rejects_over_256_chars() {
        let long = "a".repeat(257);
        assert!(sanitize_name(Some(&long), "wing").is_err());
    }

    #[test]
    fn test_accepts_256_chars() {
        let max = "a".repeat(256);
        assert!(sanitize_name(Some(&max), "wing").is_ok());
    }

    #[test]
    fn test_rejects_control_chars() {
        assert!(sanitize_name(Some("hello\nworld"), "wing").is_err());
        assert!(sanitize_name(Some("hello\x07world"), "agent").is_err());
    }

    #[test]
    fn test_accepts_unicode() {
        assert!(sanitize_name(Some("café"), "wing").is_ok());
        assert!(sanitize_name(Some("東京"), "room").is_ok());
    }

    #[test]
    fn test_rejects_null_byte_in_content() {
        assert!(sanitize_content("hi\0bye").is_err());
    }

    #[test]
    fn test_accepts_normal_content() {
        assert!(sanitize_content("hello world").is_ok());
    }

    #[test]
    fn test_rejects_oversize_content() {
        let big = "x".repeat(100_001);
        assert!(sanitize_content(&big).is_err());
    }

    #[test]
    fn test_accepts_100k_content() {
        let max = "x".repeat(100_000);
        assert!(sanitize_content(&max).is_ok());
    }
}

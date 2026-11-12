//! Source timestamps keep the pinned UTC/naive distinction and microsecond format.
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, SecondsFormat, Utc};
use serde_json::Value;

pub fn iso(value: &Value) -> Option<String> {
    fn epoch(seconds: f64) -> Option<String> {
        if !seconds.is_finite() {
            return None;
        }
        let micros = (seconds * 1_000_000.0).round_ties_even();
        if micros < i64::MIN as f64 || micros > i64::MAX as f64 {
            return None;
        }
        let micros = micros as i64;
        let date = DateTime::<Utc>::from_timestamp(
            micros.div_euclid(1_000_000),
            (micros.rem_euclid(1_000_000) * 1000) as u32,
        )?;
        if !(1..=9999).contains(&date.year()) {
            return None;
        }
        Some(date.to_rfc3339_opts(
            if date.timestamp_subsec_micros() == 0 {
                SecondsFormat::Secs
            } else {
                SecondsFormat::Micros
            },
            false,
        ))
    }
    match value {
        Value::Null => None,
        Value::Bool(b) => epoch(if *b { 1.0 } else { 0.0 }),
        Value::Number(n) => epoch(n.as_f64()?),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let numeric = s.strip_prefix('-').unwrap_or(s);
            if !numeric.is_empty() && numeric.bytes().all(|b| b.is_ascii_digit()) {
                return epoch(s.parse::<f64>().ok()?);
            }
            if let Ok(date) = DateTime::parse_from_rfc3339(s) {
                return Some(date.to_rfc3339_opts(
                    if date.timestamp_subsec_micros() == 0 {
                        SecondsFormat::Secs
                    } else {
                        SecondsFormat::Micros
                    },
                    false,
                ));
            }
            for format in [
                "%Y-%m-%dT%H:%M:%S%.f",
                "%Y-%m-%d %H:%M:%S%.f",
                "%Y-%m-%dT%H:%M",
                "%Y-%m-%d %H:%M",
            ] {
                if let Ok(date) = NaiveDateTime::parse_from_str(s, format) {
                    return Some(
                        date.format(if date.and_utc().timestamp_subsec_micros() == 0 {
                            "%Y-%m-%dT%H:%M:%S"
                        } else {
                            "%Y-%m-%dT%H:%M:%S%.6f"
                        })
                        .to_string(),
                    );
                }
            }
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .ok()
                .map(|date| format!("{date}T00:00:00"))
        }
        _ => None,
    }
}

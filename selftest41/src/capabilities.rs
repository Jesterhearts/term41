use std::fmt::Write as _;

#[derive(Clone, Debug, Default)]
pub struct CapabilityReport {
    pub raw_reply: Option<String>,
    pub level: Option<u16>,
    pub features: Vec<u16>,
    pub query_ok: bool,
}

pub fn parse_da1_reply(bytes: &[u8]) -> Option<CapabilityReport> {
    let payload = da1_payload(bytes)?;
    let payload = std::str::from_utf8(payload).ok()?;
    let mut parts = payload.split(';');
    let level = parts.next()?.parse().ok()?;
    let mut features = Vec::new();
    for part in parts {
        if let Ok(code) = part.parse() {
            features.push(code);
        }
    }
    Some(CapabilityReport {
        raw_reply: Some(format!("\u{1b}[?{payload}c")),
        level: Some(level),
        features,
        query_ok: true,
    })
}

fn da1_payload(bytes: &[u8]) -> Option<&[u8]> {
    let mut i = 0;
    while i < bytes.len() {
        let start = if bytes[i] == 0x9b {
            i + 1
        } else if bytes[i..].starts_with(b"\x1b[") {
            i + 2
        } else {
            i += 1;
            continue;
        };
        let rest = bytes.get(start..)?;
        if !rest.starts_with(b"?") {
            i += 1;
            continue;
        }
        let payload_start = start + 1;
        let payload_end = bytes
            .get(payload_start..)?
            .iter()
            .position(|&b| b == b'c')?;
        let payload = &bytes[payload_start..payload_start + payload_end];
        if payload.iter().all(|b| b.is_ascii_digit() || *b == b';') {
            return Some(payload);
        }
        i = payload_start + payload_end + 1;
    }
    None
}

pub fn fallback_report() -> CapabilityReport {
    CapabilityReport {
        raw_reply: None,
        level: None,
        features: Vec::new(),
        query_ok: false,
    }
}

pub fn format_status(
    report: &CapabilityReport,
    width: usize,
) -> String {
    if width == 0 {
        return String::new();
    }

    if !report.query_ok {
        return truncate("DA1: no reply", width);
    }

    let mut labels = Vec::new();
    if let Some(level) = report.level
        && let Some(label) = level_label(level)
    {
        labels.push(label);
    }

    let mut feature_labels: Vec<_> = report
        .features
        .iter()
        .filter_map(|code| feature_label(*code))
        .collect();
    feature_labels.sort_by_key(|label| label.sort_key);

    let mut out = String::from("DA1 ");
    for label in &labels {
        if push_segment(&mut out, label.short, width) {
            return out;
        }
    }

    for label in &feature_labels {
        if push_segment(&mut out, label.short, width) {
            return out;
        }
    }

    if out.len() <= width {
        return out;
    }

    let mut partial = String::from("DA1 ");
    let mut reversed = feature_labels;
    reversed.sort_by_key(|label| std::cmp::Reverse(label.sort_key));
    for label in reversed {
        if push_segment(&mut partial, label.short, width) {
            return partial;
        }
    }
    truncate(&partial, width)
}

pub fn describe(report: &CapabilityReport) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(raw) = &report.raw_reply {
        lines.push(format!("Raw DA1 reply: {}", raw.escape_default()));
    } else {
        lines.push(String::from("Raw DA1 reply: unavailable"));
    }

    if let Some(level) = report.level {
        if let Some(label) = level_label(level) {
            lines.push(format!(
                "Reported operating level: {} ({})",
                label.long, level
            ));
        } else {
            lines.push(format!("Reported operating level: {}", level));
        }
    } else {
        lines.push(String::from("Reported operating level: unknown"));
    }

    if report.features.is_empty() {
        lines.push(String::from("Feature bits: none reported"));
    } else {
        let mut feature_text = String::from("Feature bits:");
        for code in &report.features {
            let label = feature_label(*code)
                .map(|label| label.long.to_string())
                .unwrap_or_else(|| unknown_feature_label(*code));
            let _ = write!(feature_text, " {}", label);
        }
        lines.push(feature_text);
    }

    lines
}

#[derive(Clone, Copy)]
struct FeatureLabel {
    short: &'static str,
    long: &'static str,
    sort_key: u16,
}

#[derive(Clone, Copy)]
struct LevelLabel {
    short: &'static str,
    long: &'static str,
}

fn level_label(level: u16) -> Option<LevelLabel> {
    match level {
        61 => Some(LevelLabel {
            short: "VT100-L1",
            long: "VT100 family / level 1",
        }),
        62 => Some(LevelLabel {
            short: "VT200-L2",
            long: "VT200 family / level 2",
        }),
        63 => Some(LevelLabel {
            short: "VT300-L3",
            long: "VT300 family / level 3",
        }),
        64 => Some(LevelLabel {
            short: "VT400-L4",
            long: "VT400 family / level 4",
        }),
        _ => None,
    }
}

fn feature_label(code: u16) -> Option<FeatureLabel> {
    match code {
        7 => Some(FeatureLabel {
            short: "DRCS",
            long: "DRCS / soft chars (7)",
            sort_key: code,
        }),
        21 => Some(FeatureLabel {
            short: "HScroll",
            long: "Horizontal scrolling (21)",
            sort_key: code,
        }),
        22 => Some(FeatureLabel {
            short: "Color",
            long: "ANSI color (22)",
            sort_key: code,
        }),
        28 => Some(FeatureLabel {
            short: "Rect",
            long: "Rectangular ops (28)",
            sort_key: code,
        }),
        29 => Some(FeatureLabel {
            short: "Locator",
            long: "ANSI text locator (29)",
            sort_key: code,
        }),
        32 => Some(FeatureLabel {
            short: "Macros",
            long: "Macros (32)",
            sort_key: code,
        }),
        _ => None,
    }
}

fn unknown_feature_label(code: u16) -> String {
    format!("feature-{}", code)
}

fn push_segment(
    out: &mut String,
    segment: &str,
    width: usize,
) -> bool {
    let candidate = if out.ends_with(' ') {
        format!("{}{}", out, segment)
    } else {
        format!("{} | {}", out, segment)
    };
    if candidate.chars().count() > width {
        return true;
    }
    *out = candidate;
    false
}

fn truncate(
    text: &str,
    width: usize,
) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width <= 1 {
        return "…".to_string();
    }
    text.chars().take(width - 1).chain(['…']).collect()
}

#[cfg(test)]
mod tests {
    use super::parse_da1_reply;

    #[test]
    fn parses_seven_bit_da1_reply() {
        let report = parse_da1_reply(b"\x1b[?64;7;21;22;28;29c").unwrap();
        assert_eq!(report.level, Some(64));
        assert_eq!(report.features, vec![7, 21, 22, 28, 29]);
    }

    #[test]
    fn parses_eight_bit_da1_reply() {
        let report = parse_da1_reply(b"\x9b?63;7;21;22;28;29;32c").unwrap();
        assert_eq!(report.level, Some(63));
        assert_eq!(report.features, vec![7, 21, 22, 28, 29, 32]);
    }
}

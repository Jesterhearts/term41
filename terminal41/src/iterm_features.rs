//! iTerm2 terminal feature reporting.
//!
//! The feature string is used in both `TERM_FEATURES` and
//! `OSC 1337;Capabilities` replies. Keep it coarse and policy-filtered: this
//! is a fingerprinting surface, and disabled local integrations should not be
//! advertised.

use config41::FeaturePermissions;
use config41::PermissionPolicy;

/// Build the iTerm2 Terminal Feature Reporting feature string for the current
/// protocol policy.
pub fn term_features(feature_permissions: &FeaturePermissions) -> String {
    let mut out = String::new();

    // 24BIT: compatibility `;` RGB and full `:` RGB syntaxes.
    out.push_str("T3");

    if feature_permissions.clipboard.write != PermissionPolicy::Deny {
        out.push_str("Cw");
    }

    out.push_str("Lr");
    out.push('M');
    // DECSCUSR: 1-4 plus 5-6 plus mode 0 reset.
    out.push_str("Sc7");
    out.push('U');
    out.push_str("Uw");
    out.push_str(&unicode_width::UNICODE_VERSION.0.to_string());
    // TITLES: title stack plus title setting.
    out.push_str("Ts3");
    out.push('B');
    // The current iTerm2 table assigns `F` to both FOCUS_REPORTING and FILE.
    // One `F` advertises the shared code without duplicating it.
    out.push('F');
    out.push_str("Gs");
    out.push_str("Go");
    out.push_str("Sy");
    out.push('H');
    out.push_str("Sx");

    out
}

#[cfg(test)]
mod tests {
    use config41::ClipboardPermissions;

    use super::*;

    #[test]
    fn term_features_include_clipboard_when_writes_are_not_denied() {
        let features = term_features(&FeaturePermissions::default());
        assert!(features.contains("Cw"));
        assert!(features.contains("T3"));
        assert!(features.contains("Sc7"));
        assert!(features.contains("Uw17"));
        assert!(features.contains("Ts3"));
    }

    #[test]
    fn term_features_hide_clipboard_when_writes_are_denied() {
        let features = term_features(&FeaturePermissions {
            clipboard: ClipboardPermissions {
                write: PermissionPolicy::Deny,
                ..ClipboardPermissions::default()
            },
            ..FeaturePermissions::default()
        });

        assert!(!features.contains("Cw"));
        assert!(features.contains("T3"));
    }
}

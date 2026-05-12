use std::collections::BTreeMap;

use serde::Deserialize;

use crate::MAX_DECUDK_PAYLOAD_BYTES;
use crate::MAX_DRCS_PAYLOAD_BYTES;
use crate::MAX_DRCS_TOTAL_STORAGE_BYTES;
use crate::MAX_MACRO_BYTES;
use crate::MAX_MACRO_INVOCATION_DEPTH;
use crate::MAX_UDK_BYTES;
use crate::deserialize::clipboard_permission_opt;
use crate::deserialize::permission_policy_opt;
use crate::deserialize::program_allowlist_opt;
use crate::deserialize::usize_opt;

/// Permission gates for terminal features that can execute stored data or
/// otherwise need explicit host approval.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeaturePermissions {
    /// Permission gate for VT420 programmable macros.
    pub macros: ProgramAllowlist,
    /// Permission gate for DEC user-defined keys and related keyboard controls.
    pub udks: ProgramAllowlist,
    /// Permission gates for host-driven OSC 52 clipboard access.
    pub clipboard: ClipboardPermissions,
    /// Permission gate for host-driven kitty graphics file reads.
    pub kitty_graphics_files: PermissionPolicy,
}

/// Runtime resource limits for terminal-owned protocol state.
///
/// These are deliberately grouped separately from feature permissions:
/// permissions answer "may this feature run?", while limits answer "how much
/// state may this terminal retain or process for enabled features?".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalLimits {
    /// Maximum decoded bytes retained across all VT macro definitions.
    pub macro_storage_bytes: usize,
    /// Maximum nested macro expansion depth.
    pub macro_invocation_depth: usize,
    /// Maximum decoded bytes retained across all DEC user-defined keys.
    pub udk_storage_bytes: usize,
    /// Maximum bytes accumulated for one DECUDK DCS payload.
    pub decudk_payload_bytes: usize,
    /// Maximum bytes accumulated for one DRCS DCS payload.
    pub drcs_payload_bytes: usize,
    /// Maximum bytes accumulated for one XTGETTCAP capability query payload.
    pub xtgettcap_payload_bytes: usize,
    /// Maximum decoded DRCS glyph storage retained by the terminal.
    pub drcs_storage_bytes: usize,
    /// Maximum base64 payload bytes accepted for one kitty graphics command.
    pub kitty_graphics_payload_bytes: usize,
    /// Maximum decoded kitty image bytes retained for reusable images.
    pub kitty_graphics_storage_bytes: usize,
}

impl Default for TerminalLimits {
    fn default() -> Self {
        Self {
            macro_storage_bytes: MAX_MACRO_BYTES,
            macro_invocation_depth: MAX_MACRO_INVOCATION_DEPTH,
            udk_storage_bytes: MAX_UDK_BYTES,
            decudk_payload_bytes: MAX_DECUDK_PAYLOAD_BYTES,
            drcs_payload_bytes: MAX_DRCS_PAYLOAD_BYTES,
            xtgettcap_payload_bytes: 4096,
            drcs_storage_bytes: MAX_DRCS_TOTAL_STORAGE_BYTES,
            kitty_graphics_payload_bytes: 32 * 1024 * 1024,
            kitty_graphics_storage_bytes: 128 * 1024 * 1024,
        }
    }
}

/// Coarse allow/deny gate for a protocol feature.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub enum ProgramAllowlist {
    /// Deny all requests for this feature.
    #[default]
    #[serde(alias = "none", alias = "deny")]
    DenyAll,
    /// Allow all requests for this feature.
    #[serde(alias = "*", alias = "all")]
    AllowAll,
}

impl ProgramAllowlist {
    /// Whether this gate allows the protected feature.
    pub fn allow(&self) -> bool {
        match self {
            Self::DenyAll => false,
            Self::AllowAll => true,
        }
    }
}

/// Read/write permission gates for host-driven clipboard access.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClipboardPermissions {
    /// Whether host programs may read local clipboard contents.
    pub read: PermissionPolicy,
    /// Whether host programs may write local clipboard contents.
    pub write: PermissionPolicy,
}

/// Permission policy for one host-mediated local resource access direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PermissionPolicy {
    /// Ask the user for this request.
    #[default]
    #[serde(alias = "request")]
    Ask,
    /// Allow every request without prompting.
    #[serde(alias = "*", alias = "all")]
    Allow,
    /// Deny every request without prompting.
    #[serde(alias = "no", alias = "none")]
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
pub struct ScriptPermissions {
    #[serde(default)]
    pub filesystem: bool,
    #[serde(default)]
    pub shell: bool,
    #[serde(default)]
    pub process_info: bool,
    #[serde(default)]
    pub resource_usage: bool,
}

#[derive(Deserialize, Default)]
pub(crate) struct SecuritySettings {
    #[serde(default)]
    features: Option<AllowFeaturesConfig>,
    #[serde(default)]
    clipboard: Option<ClipboardPermissionsConfig>,
    #[serde(default)]
    kitty_graphics: Option<KittyGraphicsPermissionsConfig>,
    #[serde(default)]
    limits: Option<LimitSettings>,
    #[serde(default)]
    scripts: Option<BTreeMap<String, ScriptPermissions>>,
}

#[derive(Debug)]
pub(crate) struct BuiltSecurity {
    pub feature_permissions: FeaturePermissions,
    pub limits: TerminalLimits,
    pub script_permissions: BTreeMap<String, ScriptPermissions>,
}

#[derive(Deserialize, Default)]
struct AllowFeaturesConfig {
    #[serde(deserialize_with = "program_allowlist_opt")]
    #[serde(default)]
    macros: Option<ProgramAllowlist>,
    #[serde(deserialize_with = "program_allowlist_opt")]
    #[serde(default)]
    udks: Option<ProgramAllowlist>,
}

#[derive(Deserialize, Default)]
struct ClipboardPermissionsConfig {
    #[serde(deserialize_with = "clipboard_permission_opt")]
    #[serde(default)]
    read: Option<PermissionPolicy>,
    #[serde(deserialize_with = "clipboard_permission_opt")]
    #[serde(default)]
    write: Option<PermissionPolicy>,
}

#[derive(Deserialize, Default)]
struct KittyGraphicsPermissionsConfig {
    #[serde(deserialize_with = "permission_policy_opt")]
    #[serde(default)]
    files: Option<PermissionPolicy>,
}

#[derive(Deserialize, Default)]
struct LimitSettings {
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    macro_storage_bytes: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    macro_invocation_depth: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    udk_storage_bytes: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    decudk_payload_bytes: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    drcs_payload_bytes: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    xtgettcap_payload_bytes: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    drcs_storage_bytes: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    kitty_graphics_payload_bytes: Option<usize>,
    #[serde(deserialize_with = "usize_opt")]
    #[serde(default)]
    kitty_graphics_storage_bytes: Option<usize>,
}

pub(crate) fn build_security(raw: Option<SecuritySettings>) -> BuiltSecurity {
    let SecuritySettings {
        features,
        clipboard,
        kitty_graphics,
        limits,
        scripts,
    } = raw.unwrap_or_default();
    let features = features.unwrap_or_default();
    let clipboard = clipboard.unwrap_or_default();
    let kitty_graphics = kitty_graphics.unwrap_or_default();
    BuiltSecurity {
        feature_permissions: FeaturePermissions {
            macros: features.macros.unwrap_or_default(),
            udks: features.udks.unwrap_or_default(),
            clipboard: ClipboardPermissions {
                read: clipboard.read.unwrap_or_default(),
                write: clipboard.write.unwrap_or_default(),
            },
            kitty_graphics_files: kitty_graphics.files.unwrap_or_default(),
        },
        limits: build_limits(limits),
        script_permissions: scripts.unwrap_or_default(),
    }
}

fn build_limits(raw: Option<LimitSettings>) -> TerminalLimits {
    let settings = raw.unwrap_or_default();
    let defaults = TerminalLimits::default();
    TerminalLimits {
        macro_storage_bytes: settings
            .macro_storage_bytes
            .unwrap_or(defaults.macro_storage_bytes),
        macro_invocation_depth: settings
            .macro_invocation_depth
            .unwrap_or(defaults.macro_invocation_depth),
        udk_storage_bytes: settings
            .udk_storage_bytes
            .unwrap_or(defaults.udk_storage_bytes),
        decudk_payload_bytes: settings
            .decudk_payload_bytes
            .unwrap_or(defaults.decudk_payload_bytes),
        drcs_payload_bytes: settings
            .drcs_payload_bytes
            .unwrap_or(defaults.drcs_payload_bytes),
        xtgettcap_payload_bytes: settings
            .xtgettcap_payload_bytes
            .unwrap_or(defaults.xtgettcap_payload_bytes),
        drcs_storage_bytes: settings
            .drcs_storage_bytes
            .unwrap_or(defaults.drcs_storage_bytes),
        kitty_graphics_payload_bytes: settings
            .kitty_graphics_payload_bytes
            .unwrap_or(defaults.kitty_graphics_payload_bytes),
        kitty_graphics_storage_bytes: settings
            .kitty_graphics_storage_bytes
            .unwrap_or(defaults.kitty_graphics_storage_bytes),
    }
}

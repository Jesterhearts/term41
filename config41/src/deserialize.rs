use serde::Deserialize;
use smol_str::SmolStr;

use crate::BellMode;
use crate::CursorShape;
use crate::PowerPreference;
use crate::StatusLineMode;
use crate::VSync;
use crate::compatibility::EmojiCompatibilityMode;
use crate::keybindings::KeybindingConfig;
use crate::security::PermissionPolicy;
use crate::security::ProgramAllowlist;

pub(crate) fn smolstr_opt<'de, D>(deserializer: D) -> Result<Option<SmolStr>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<SmolStr>::deserialize(deserializer) {
        Ok(opt) => {
            if let Some(s) = opt {
                return Ok(Some(s));
            }
            Ok(None)
        }
        Err(e) => {
            warn!("failed to parse char in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn float_opt_clamp_0_1<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<f32>::deserialize(deserializer) {
        Ok(opt) => {
            if let Some(f) = opt {
                if !(0.0..=1.0).contains(&f) {
                    warn!("float value {f} out of range [0.0, 1.0]; clamping");
                }
                Ok(Some(f.clamp(0.0, 1.0)))
            } else {
                Ok(None)
            }
        }
        Err(e) => {
            warn!("failed to parse float in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn float_opt<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<f32>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse float in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn u32_opt<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<u32>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse integer in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn usize_opt<'de, D>(deserializer: D) -> Result<Option<usize>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<usize>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse byte/depth limit in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn cursor_shape_opt<'de, D>(deserializer: D) -> Result<Option<CursorShape>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<CursorShape>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse cursor shape in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn cursor_blink_opt<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<bool>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse cursor blink in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn keybindings_opt<'de, D>(
    deserializer: D
) -> Result<Option<Vec<KeybindingConfig>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<Vec<KeybindingConfig>>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse keybindings in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn bell_mode_opt<'de, D>(deserializer: D) -> Result<Option<BellMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<BellMode>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse bell mode in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn gutter_opt<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<bool>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse gutter setting in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn status_line_mode_opt<'de, D>(
    deserializer: D
) -> Result<Option<StatusLineMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<StatusLineMode>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse status_line mode in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn program_allowlist_opt<'de, D>(
    deserializer: D
) -> Result<Option<ProgramAllowlist>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<ProgramAllowlist>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse feature allowlist in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn clipboard_permission_opt<'de, D>(
    deserializer: D
) -> Result<Option<PermissionPolicy>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<PermissionPolicy>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse clipboard permission in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn permission_policy_opt<'de, D>(
    deserializer: D
) -> Result<Option<PermissionPolicy>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<PermissionPolicy>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse permission policy in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn power_preference_opt<'de, D>(
    deserializer: D
) -> Result<Option<PowerPreference>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<PowerPreference>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse power preference in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn vsync_opt<'de, D>(deserializer: D) -> Result<Option<VSync>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<VSync>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse vsync setting in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn emoji_compatibility_mode_opt<'de, D>(
    deserializer: D
) -> Result<Option<EmojiCompatibilityMode>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<EmojiCompatibilityMode>::deserialize(deserializer) {
        Ok(opt) => Ok(opt),
        Err(e) => {
            warn!("failed to parse emoji compatibility mode in config: {e}");
            Ok(None)
        }
    }
}

pub(crate) fn u32_opt_clamp_1_16<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    match Option::<u32>::deserialize(deserializer) {
        Ok(opt) => {
            if let Some(v) = opt {
                if !(1..=16).contains(&v) {
                    warn!("integer value {v} out of range [1, 16]; clamping");
                }
                Ok(Some(v.clamp(1, 16)))
            } else {
                Ok(None)
            }
        }
        Err(e) => {
            warn!("failed to parse integer in config: {e}");
            Ok(None)
        }
    }
}

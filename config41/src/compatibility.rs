use serde::Deserialize;

use crate::deserialize::emoji_compatibility_mode_opt;

/// How term41 should handle legacy shell emoji editing compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EmojiCompatibilityMode {
    /// Enable only in a shell-integration command-editing phase.
    #[default]
    Auto,
    /// Always use normal terminal grapheme handling.
    Off,
    /// Always use legacy scalar emoji handling.
    On,
}

impl EmojiCompatibilityMode {
    /// Cycle through the modes in the order used by the UI hotkey.
    pub fn next(self) -> Self {
        match self {
            Self::Auto => Self::Off,
            Self::Off => Self::On,
            Self::On => Self::Auto,
        }
    }

    /// Human-readable lowercase label for logs/UI.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Off => "off",
            Self::On => "on",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompatibilityConfig {
    pub emoji: EmojiCompatibilityMode,
}

impl Default for CompatibilityConfig {
    fn default() -> Self {
        Self {
            emoji: EmojiCompatibilityMode::Auto,
        }
    }
}

#[derive(Deserialize, Default)]
pub(crate) struct CompatibilitySettings {
    /// Legacy shell emoji editing compatibility: `auto`, `off`, or `on`.
    #[serde(deserialize_with = "emoji_compatibility_mode_opt")]
    #[serde(default)]
    emoji: Option<EmojiCompatibilityMode>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShellIntegrationConfig {
    /// Opt in to shell startup hooks that emit OSC 133 lifecycle markers.
    pub hooks: bool,
}

#[derive(Deserialize, Default)]
pub(crate) struct ShellIntegrationSettings {
    /// Install ephemeral shell hooks in spawned default shells.
    #[serde(default)]
    hooks: Option<bool>,
}

pub(crate) fn build_compatibility(raw: Option<CompatibilitySettings>) -> CompatibilityConfig {
    let settings = raw.unwrap_or_default();
    CompatibilityConfig {
        emoji: settings.emoji.unwrap_or_default(),
    }
}

pub(crate) fn build_shell_integration(
    raw: Option<ShellIntegrationSettings>
) -> ShellIntegrationConfig {
    let settings = raw.unwrap_or_default();
    let defaults = ShellIntegrationConfig::default();
    ShellIntegrationConfig {
        hooks: settings.hooks.unwrap_or(defaults.hooks),
    }
}

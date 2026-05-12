use std::fmt::Display;
use std::path::PathBuf;

use serde::Deserialize;
use smol_str::SmolStr;
use smol_str::ToSmolStr;

use crate::BellMode;
use crate::Config;
use crate::CursorShape;
use crate::CursorStyle;
use crate::DEFAULT_SCROLLBACK;
use crate::PowerPreference;
use crate::StatusLineMode;
use crate::VSync;
use crate::command_editor::CommandEditorSettings;
use crate::command_editor::build_command_editor;
use crate::compatibility::CompatibilitySettings;
use crate::compatibility::ShellIntegrationSettings;
use crate::compatibility::build_compatibility;
use crate::compatibility::build_shell_integration;
use crate::deserialize::bell_mode_opt;
use crate::deserialize::cursor_blink_opt;
use crate::deserialize::cursor_shape_opt;
use crate::deserialize::float_opt;
use crate::deserialize::float_opt_clamp_0_1;
use crate::deserialize::gutter_opt;
use crate::deserialize::keybindings_opt;
use crate::deserialize::power_preference_opt;
use crate::deserialize::smolstr_opt;
use crate::deserialize::status_line_mode_opt;
use crate::deserialize::u32_opt;
use crate::deserialize::u32_opt_clamp_1_16;
use crate::deserialize::vsync_opt;
use crate::keybindings::Keybinding;
use crate::keybindings::KeybindingConfig;
use crate::keybindings::Keybindings;
use crate::palette::ColorsConfig;
use crate::palette::build_palette;
use crate::runtime::expand_path;
use crate::security::SecuritySettings;
use crate::security::build_security;

#[derive(Deserialize)]
pub(crate) struct ConfigFile {
    #[serde(deserialize_with = "float_opt_clamp_0_1")]
    #[serde(default)]
    opacity: Option<f32>,
    #[serde(default)]
    fonts: Option<String>,
    #[serde(deserialize_with = "float_opt")]
    #[serde(default)]
    font_size: Option<f32>,
    #[serde(deserialize_with = "u32_opt")]
    #[serde(default)]
    scrollback_lines: Option<u32>,
    /// Default DEC status-line mode on startup and after hard reset.
    /// `off` hides it; `indicator` shows the emulator-owned indicator line.
    #[serde(deserialize_with = "status_line_mode_opt")]
    #[serde(default)]
    status_line: Option<StatusLineMode>,
    /// Cursor shape: `block`, `underline`, or `beam`.
    #[serde(deserialize_with = "cursor_shape_opt")]
    #[serde(default)]
    cursor_shape: Option<CursorShape>,
    /// Whether the cursor blinks. Defaults to true.
    #[serde(deserialize_with = "cursor_blink_opt")]
    #[serde(default)]
    cursor_blink: Option<bool>,
    /// Replace the default keybindings entirely. Setting an empty array
    /// disables all bindings, which is useful for debugging conflicts.
    #[serde(deserialize_with = "keybindings_opt")]
    #[serde(default)]
    keybindings: Option<Vec<KeybindingConfig>>,
    /// Bell behaviour: `off`, `visual`, or `urgent`.
    #[serde(deserialize_with = "bell_mode_opt")]
    #[serde(default)]
    bell: Option<BellMode>,
    /// Show the shell-integration gutter on the left edge - a thin strip
    /// where OSC 133 prompt rows get a coloured dot marking the last
    /// command's exit status. Defaults to on; disable for a pure
    /// terminal-text view or when the shell doesn't emit OSC 133 at all.
    #[serde(deserialize_with = "gutter_opt")]
    #[serde(default)]
    gutter: Option<bool>,
    /// Preferred power mode for the GPU. See wgpu::PowerPreference docs for
    /// details.
    #[serde(deserialize_with = "power_preference_opt")]
    #[serde(default)]
    power_preference: Option<PowerPreference>,
    /// Whether to enable vsync.
    #[serde(deserialize_with = "vsync_opt")]
    #[serde(default)]
    vsync: Option<VSync>,
    /// Override the monitor's DPI scale factor. When absent, the system
    /// scale factor is used automatically. Set to `1.0` to disable DPI
    /// scaling entirely.
    #[serde(deserialize_with = "float_opt")]
    #[serde(default)]
    dpi_scale: Option<f32>,
    /// Path to an image file to draw behind the terminal cells. PNG is
    /// always supported; GIF (including animated) requires the `ffmpeg`
    /// cargo feature. Cells with the default background colour become
    /// transparent over the image so it shows through; cells with an
    /// explicit SGR background still paint over the image.
    #[serde(default)]
    background_image: Option<PathBuf>,
    /// Multiplier applied to the background image's RGB. `1.0` paints the
    /// image at full brightness; `0.0` makes it invisible. Useful for
    /// dimming a busy wallpaper enough that text remains readable. The
    /// image's own alpha channel is preserved either way.
    #[serde(deserialize_with = "float_opt_clamp_0_1")]
    #[serde(default)]
    background_opacity: Option<f32>,

    #[serde(deserialize_with = "smolstr_opt")]
    #[serde(default)]
    new_tab_text: Option<SmolStr>,

    /// Supersampling factor for font rasterization. Higher values produce
    /// smoother results at the cost of increased CPU usage and memory
    /// consumption. Default is 4.
    #[serde(deserialize_with = "u32_opt_clamp_1_16")]
    #[serde(default)]
    font_supersampling: Option<u32>,

    /// Color palette in Rio format.
    #[serde(default)]
    colors: Option<ColorsConfig>,
    /// Security-sensitive settings.
    #[serde(default)]
    security: Option<SecuritySettings>,
    #[serde(default)]
    compatibility: Option<CompatibilitySettings>,
    #[serde(default)]
    shell_integration: Option<ShellIntegrationSettings>,
    #[serde(default)]
    command_editor: Option<CommandEditorSettings>,
}

/// Parse a config TOML string into a [`Config`]. Split out from runtime file
/// loading so tests can exercise the mapping logic without touching the
/// filesystem.
pub(crate) fn parse_config(
    contents: &str,
    source: &dyn Display,
) -> Config {
    let (file, ignored_keys) = match parse_config_file(contents) {
        Ok(parsed) => parsed,
        Err(e) => {
            warn!("failed to parse {source}: {e}");
            return Config::default();
        }
    };
    for key in ignored_keys {
        warn!("ignored unknown config key: {key}");
    }

    let cursor_style = build_cursor_style(file.cursor_shape, file.cursor_blink);
    let keybindings = build_keybindings(file.keybindings, source);
    let palette = build_palette(file.colors);
    let security = build_security(file.security);
    let compatibility = build_compatibility(file.compatibility);
    let shell_integration = build_shell_integration(file.shell_integration);
    let command_editor = build_command_editor(file.command_editor);
    let new_tab_text = file.new_tab_text.unwrap_or('⮒'.to_smolstr());

    Config {
        opacity: file.opacity.unwrap_or(1.0),
        fonts: file.fonts,
        font_size: file.font_size.unwrap_or(24.0).max(1.0),
        scrollback_lines: file.scrollback_lines.unwrap_or(DEFAULT_SCROLLBACK),
        status_line: file.status_line.unwrap_or_default(),
        cursor_style,
        keybindings,
        bell: file.bell.unwrap_or_default(),
        gutter: file.gutter.unwrap_or(true),
        power_preference: file.power_preference.unwrap_or_default(),
        vsync: file.vsync.unwrap_or(VSync::Auto),
        dpi_scale: file.dpi_scale.map(|v| v.max(0.25)),
        background_image: file.background_image.map(expand_path),
        background_opacity: file.background_opacity.unwrap_or(1.0),
        font_supersampling: file.font_supersampling.unwrap_or(4),
        palette,
        feature_permissions: security.feature_permissions,
        limits: security.limits,
        script_permissions: security.script_permissions,
        compatibility,
        shell_integration,
        command_editor,
        new_tab_text,
    }
}

pub(crate) fn parse_config_file(
    contents: &str
) -> Result<(ConfigFile, Vec<String>), toml::de::Error> {
    let mut ignored = vec![];
    let deserializer = toml::Deserializer::parse(contents)?;
    let file = serde_ignored::deserialize(deserializer, |path| {
        ignored.push(normalize_ignored_path(&path.to_string()));
    })?;
    Ok((file, ignored))
}

fn normalize_ignored_path(path: &str) -> String {
    path.split('.')
        .filter(|segment| *segment != "?")
        .collect::<Vec<_>>()
        .join(".")
}

/// Map the optional `keybindings = [...]` toml field onto a
/// [`Keybindings`]. Returns [`Keybindings::defaults`] when the key is
/// absent; an empty array (`keybindings = []`) is honoured as "no
/// bindings" so users can disable them all to debug a conflict.
fn build_keybindings(
    raw: Option<Vec<KeybindingConfig>>,
    path: &dyn Display,
) -> Keybindings {
    let Some(entries) = raw else {
        return Keybindings::defaults();
    };
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        match Keybinding::from_config_entry(entry) {
            Ok(b) => out.push(b),
            Err(e) => warn!("invalid keybinding in {path}: {e}"),
        }
    }
    Keybindings::from_config(out)
}

/// Map the optional shape + blink toml fields onto a [`CursorStyle`]. Falls
/// back to [`CursorStyle::default`] when both are absent. Unknown shape names
/// log a warning and default to block.
fn build_cursor_style(
    shape: Option<CursorShape>,
    blink: Option<bool>,
) -> CursorStyle {
    let mut style = CursorStyle::default();
    if let Some(s) = shape {
        style.shape = s;
    }
    if let Some(b) = blink {
        style.blink = b;
    }
    style
}

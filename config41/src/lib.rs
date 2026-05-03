use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

mod command_editor;
pub mod keybindings;

pub use command_editor::CommandEditorConfig;
use palette::Srgb;
use parking_lot::Mutex;
use serde::Deserialize;
use smol_str::SmolStr;
use smol_str::ToSmolStr;
use utils41::blend_colors;

use crate::command_editor::CommandEditorSettings;
use crate::command_editor::build_command_editor;
use crate::keybindings::Keybinding;
use crate::keybindings::KeybindingConfig;
use crate::keybindings::Keybindings;

#[macro_use]
extern crate log;

pub const DEFAULT_SCROLLBACK: u32 = 10_000;
pub const MAX_MACRO_BYTES: usize = 6 * 1024;
pub const MAX_MACRO_INVOCATION_DEPTH: usize = 32;
pub const MAX_UDK_BYTES: usize = 256;
pub const MAX_DECUDK_PAYLOAD_BYTES: usize = 2048;
pub const MAX_DRCS_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_DRCS_TOTAL_STORAGE_BYTES: usize = 256 * 1024;

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

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PowerPreference {
    #[default]
    #[serde(alias = "none")]
    Auto,
    LowPower,
    HighPerformance,
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

pub const fn default_fg() -> Srgb<u8> {
    Srgb::new(204, 204, 204)
}

pub const fn default_bg() -> Srgb<u8> {
    Srgb::new(0, 0, 0)
}

/// Runtime color palette. Stores the 16 ANSI colors, default fg/bg,
/// cursor colors, and selection colors. Built from the `[colors]` config
/// section (Rio palette format), falling back to the hardcoded defaults
/// for any value not overridden.
#[derive(Debug, Clone)]
pub struct ColorPalette {
    /// Default foreground (SGR 39 / row clear).
    pub fg: Srgb<u8>,
    /// Default background (SGR 49 / row clear / wallpaper transparency).
    pub bg: Srgb<u8>,
    /// Default foreground for the DEC status line.
    pub status_line_fg: Srgb<u8>,
    /// Default background for the DEC status line.
    pub status_line_bg: Srgb<u8>,
    /// Cursor color. `None` = use cell foreground (current behavior).
    pub cursor: Option<Srgb<u8>>,
    /// Text color used under a block cursor. `None` = invert against the
    /// cell background (current behavior).
    pub cursor_text: Option<Srgb<u8>>,
    /// Selection background. `None` = invert (current behavior).
    pub selection_bg: Option<Srgb<u8>>,
    /// Selection text color. `None` = invert (current behavior).
    pub selection_fg: Option<Srgb<u8>>,
    /// The 16 ANSI colors: indices 0–7 are normal, 8–15 are bright.
    pub ansi: [Srgb<u8>; 16],
}

impl Default for ColorPalette {
    fn default() -> Self {
        let fg = default_fg();
        let bg = default_bg();
        Self {
            fg,
            bg,
            status_line_fg: fg,
            status_line_bg: blend_colors(bg, fg, 0.25),
            cursor: None,
            cursor_text: None,
            selection_bg: None,
            selection_fg: None,
            ansi: [
                Srgb::new(0, 0, 0),       // 0  black           rgb(0, 0, 0)
                Srgb::new(205, 0, 0),     // 1  red             rgb(205, 0, 0)
                Srgb::new(0, 205, 0),     // 2  green           rgb(0, 205, 0)
                Srgb::new(205, 205, 0),   // 3  yellow          rgb(205, 205, 0)
                Srgb::new(0, 0, 238),     // 4  blue            rgb(0, 0, 238)
                Srgb::new(205, 0, 205),   // 5  magenta         rgb(205, 0, 205)
                Srgb::new(0, 205, 205),   // 6  cyan            rgb(0, 205, 205)
                Srgb::new(229, 229, 229), // 7  white           rgb(229, 229, 229)
                Srgb::new(127, 127, 127), // 8  bright black    rgb(127, 127, 127)
                Srgb::new(255, 0, 0),     // 9  bright red      rgb(255, 0, 0)
                Srgb::new(0, 255, 0),     // 10 bright green    rgb(0, 255, 0)
                Srgb::new(255, 255, 0),   // 11 bright yellow   rgb(255, 255, 0)
                Srgb::new(92, 92, 255),   // 12 bright blue     rgb(92, 92, 255)
                Srgb::new(255, 0, 255),   // 13 bright magenta  rgb(255, 0, 255)
                Srgb::new(0, 255, 255),   // 14 bright cyan     rgb(0, 255, 255)
                Srgb::new(255, 255, 255), // 15 bright white    rgb(255, 255, 255)
            ],
        }
    }
}

/// Geometry of the cursor overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CursorShape {
    /// Full-cell block. The glyph beneath inverts so the character stays
    /// readable.
    #[default]
    Block,
    /// Thin horizontal bar at the bottom of the cell.
    #[serde(alias = "underscore")]
    Underline,
    /// Thin vertical bar at the left edge of the cell.
    #[serde(alias = "bar")]
    #[serde(alias = "ibeam")]
    Beam,
}

/// Combined shape + blink state. `Default` matches the long-standing xterm
/// default of a blinking block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorStyle {
    /// Cursor overlay geometry.
    pub shape: CursorShape,
    /// Whether the renderer should blink the cursor.
    pub blink: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blink: true,
        }
    }
}

/// How term41 should handle legacy shell emoji editing compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Deserialize)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StatusLineMode {
    #[default]
    Off,
    Indicator,
}

/// VSync mode for frame presentation. See the `vsync` config key and the
/// `Config::vsync` field for details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum VSync {
    /// Let the OS decide when to present frames. This is the default and
    /// usually means "sync to the display's refresh rate", but some
    /// platforms may choose a different strategy.
    #[default]
    Auto,
    /// Try using fast-vsync or similar techniques to present frames immediately
    /// when they're ready, without screen tearing.
    Fast,
    /// Present frames as soon as they're ready, even if that means
    /// tearing.
    Off,
    /// Wait for the next vertical blanking interval before presenting each
    /// frame. Eliminates tearing at the cost of increased latency and
    /// potential stuttering if the render time exceeds the display's refresh
    /// period.
    On,
}

/// What to do when the foreground app rings the bell (BEL / `\x07`).
///
/// Default is [`BellMode::Off`] because shells like bash ring the bell on
/// completion-ambiguity by default — most users find that surprising
/// rather than useful out of the box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BellMode {
    /// Ignore the bell entirely.
    #[default]
    #[serde(alias = "none", alias = "false")]
    Off,
    /// Briefly flash the screen.
    #[serde(alias = "flash")]
    Visual,
    /// Ask the compositor to mark the window as needing attention
    /// (taskbar bobbing on macOS, urgency hint on X11/Wayland). Quiet
    /// when the window is focused; eye-catching when it isn't.
    #[serde(alias = "attention")]
    Urgent,
}

// ---------------------------------------------------------------------------
// [colors] — Rio/Alacritty-format palette config
// ---------------------------------------------------------------------------

/// Top-level `[colors]` table in the config file.
#[derive(Deserialize, Default)]
struct ColorsConfig {
    /// Cursor color override, either `cursor = "#009fff"` or
    /// `[colors.cursor] cursor = "#009fff" text = "#000000"`.
    cursor: Option<CursorColorsConfig>,
    /// `[colors.primary]` — default foreground / background.
    primary: Option<PrimaryColors>,
    /// `[colors.selection]` — selection highlight colors.
    selection: Option<SelectionColors>,
    /// `[colors.status_line]` — DEC status line default colors.
    status_line: Option<StatusLineColors>,
    /// `[colors.normal]` — the 8 standard ANSI colors.
    normal: Option<AnsiColors>,
    /// `[colors.bright]` — the 8 bright ANSI colors.
    bright: Option<AnsiColors>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CursorColorsConfig {
    Color(String),
    Table(CursorColors),
}

#[derive(Deserialize, Default)]
struct CursorColors {
    cursor: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct PrimaryColors {
    foreground: Option<String>,
    background: Option<String>,
}

#[derive(Deserialize, Default)]
struct SelectionColors {
    background: Option<String>,
    text: Option<String>,
}

#[derive(Deserialize, Default)]
struct StatusLineColors {
    foreground: Option<String>,
    background: Option<String>,
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
struct SecuritySettings {
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
struct CompatibilitySettings {
    /// Legacy shell emoji editing compatibility: `auto`, `off`, or `on`.
    #[serde(deserialize_with = "emoji_compatibility_mode_opt")]
    #[serde(default)]
    emoji: Option<EmojiCompatibilityMode>,
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
struct AnsiColors {
    black: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    cyan: Option<String>,
    white: Option<String>,
}

/// Try to parse a hex color, logging a warning on failure and returning the
/// provided fallback.
fn parse_color_or_default(
    s: &Option<String>,
    fallback: Srgb<u8>,
    label: &str,
) -> Srgb<u8> {
    match s {
        Some(string) => match palette::Srgb::from_str(string)
            .ok()
            .or_else(|| palette::named::from_str(string))
        {
            Some(c) => c,
            None => {
                warn!("invalid color for {label}: {string:?}; using default");
                fallback
            }
        },
        None => fallback,
    }
}

fn parse_color_optional(
    s: &str,
    label: &str,
) -> Option<Srgb<u8>> {
    Srgb::from_str(s)
        .ok()
        .or_else(|| palette::named::from_str(s))
        .or_else(|| {
            warn!("invalid color for {label}: {s:?}; ignoring");
            None
        })
}

/// Build a [`ColorPalette`] from the deserialized `[colors]` config,
/// falling back to hardcoded defaults for any value not specified.
fn build_palette(colors: Option<ColorsConfig>) -> ColorPalette {
    let mut pal = ColorPalette::default();
    let Some(c) = colors else {
        return pal;
    };

    if let Some(ref p) = c.primary {
        pal.fg = parse_color_or_default(&p.foreground, pal.fg, "colors.primary.foreground");
        pal.bg = parse_color_or_default(&p.background, pal.bg, "colors.primary.background");
    }

    if let Some(ref status) = c.status_line {
        pal.status_line_fg =
            parse_color_or_default(&status.foreground, pal.fg, "colors.status_line.foreground");
        pal.status_line_bg = parse_color_or_default(
            &status.background,
            blend_colors(pal.bg, pal.status_line_fg, 0.25),
            "colors.status_line.background",
        );
    } else {
        pal.status_line_fg = pal.fg;
        pal.status_line_bg = blend_colors(pal.bg, pal.status_line_fg, 0.25);
    }

    if let Some(ref cursor) = c.cursor {
        match cursor {
            CursorColorsConfig::Color(s) => {
                pal.cursor = parse_color_optional(s, "colors.cursor");
            }
            CursorColorsConfig::Table(table) => {
                pal.cursor = table
                    .cursor
                    .as_ref()
                    .and_then(|s| parse_color_optional(s, "colors.cursor.cursor"));
                pal.cursor_text = table
                    .text
                    .as_ref()
                    .and_then(|s| parse_color_optional(s, "colors.cursor.text"));
            }
        }
    }

    if let Some(ref sel) = c.selection {
        pal.selection_bg = sel.background.as_ref().and_then(|s| {
            Srgb::from_str(s)
                .ok()
                .or_else(|| palette::named::from_str(s))
                .or_else(|| {
                    warn!("invalid hex color for colors.selection.background: {s:?}; ignoring");
                    None
                })
        });
        pal.selection_fg = sel.text.as_ref().and_then(|s| {
            Srgb::from_str(s)
                .ok()
                .or_else(|| palette::named::from_str(s))
                .or_else(|| {
                    warn!("invalid hex color for colors.selection.text: {s:?}; ignoring");
                    None
                })
        });
    }

    if let Some(ref n) = c.normal {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let fields = [
            &n.black, &n.red, &n.green, &n.yellow, &n.blue, &n.magenta, &n.cyan, &n.white,
        ];
        for (i, (field, name)) in fields.iter().zip(names.iter()).enumerate() {
            pal.ansi[i] =
                parse_color_or_default(field, pal.ansi[i], &format!("colors.normal.{name}"));
        }
    }

    if let Some(ref b) = c.bright {
        let names = [
            "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
        ];
        let fields = [
            &b.black, &b.red, &b.green, &b.yellow, &b.blue, &b.magenta, &b.cyan, &b.white,
        ];
        for (i, (field, name)) in fields.iter().zip(names.iter()).enumerate() {
            pal.ansi[8 + i] =
                parse_color_or_default(field, pal.ansi[8 + i], &format!("colors.bright.{name}"));
        }
    }

    pal
}

#[derive(Deserialize)]
struct ConfigFile {
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
    /// disables all bindings — useful for debugging conflicts.
    #[serde(deserialize_with = "keybindings_opt")]
    #[serde(default)]
    keybindings: Option<Vec<KeybindingConfig>>,
    /// Bell behaviour: `off`, `visual`, or `urgent`.
    #[serde(deserialize_with = "bell_mode_opt")]
    #[serde(default)]
    bell: Option<BellMode>,
    /// Show the shell-integration gutter on the left edge — a thin strip
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
    command_editor: Option<CommandEditorSettings>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub opacity: f32,
    pub fonts: Option<String>,
    pub font_size: f32,
    pub scrollback_lines: u32,
    pub status_line: StatusLineMode,
    pub cursor_style: CursorStyle,
    pub keybindings: Keybindings,
    pub bell: BellMode,
    pub gutter: bool,
    pub power_preference: PowerPreference,
    pub vsync: VSync,
    /// Override the monitor's DPI scale factor. `None` = automatic (use the
    /// system scale factor). `Some(x)` = use `x` regardless of monitor.
    pub dpi_scale: Option<f32>,
    /// Optional wallpaper image painted behind terminal cells.
    pub background_image: Option<PathBuf>,
    /// RGB multiplier applied to the background image. Always in `[0.0, 1.0]`.
    pub background_opacity: f32,
    /// Supersampling factor for font rasterization. Higher values produce
    /// smoother results at the cost of increased CPU usage and memory
    /// consumption. Default is 4.
    pub font_supersampling: u32,
    pub new_tab_text: SmolStr,
    /// Color palette (ANSI 16 colors, default fg/bg, cursor, selection).
    pub palette: ColorPalette,
    pub feature_permissions: FeaturePermissions,
    pub limits: TerminalLimits,
    pub script_permissions: BTreeMap<String, ScriptPermissions>,
    pub compatibility: CompatibilityConfig,
    pub command_editor: CommandEditorConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            fonts: None,
            font_size: 24.0,
            scrollback_lines: DEFAULT_SCROLLBACK,
            status_line: StatusLineMode::Off,
            cursor_style: CursorStyle::default(),
            keybindings: Keybindings::defaults(),
            bell: BellMode::default(),
            gutter: true,
            power_preference: PowerPreference::default(),
            vsync: VSync::Auto,
            dpi_scale: None,
            background_image: None,
            background_opacity: 1.0,
            font_supersampling: 4,
            palette: ColorPalette::default(),
            feature_permissions: FeaturePermissions::default(),
            new_tab_text: '⮒'.to_smolstr(),
            limits: TerminalLimits::default(),
            script_permissions: BTreeMap::new(),
            compatibility: CompatibilityConfig::default(),
            command_editor: CommandEditorConfig::default(),
        }
    }
}

/// Read and parse the config at `path`, falling back to defaults on any
/// I/O or parse failure. Used both by the startup loader and the
/// live-reload watcher (which already knows the path it's watching).
fn load_from(path: &std::path::Path) -> Config {
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return Config::default(),
    };
    parse_config(&contents, &path.display())
}

/// Parse a config TOML string into a [`Config`]. Split out from
/// [`load_from`] so tests can exercise the mapping logic without touching
/// the filesystem.
fn parse_config(
    contents: &str,
    source: &dyn std::fmt::Display,
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
    let SecuritySettings {
        features,
        clipboard,
        kitty_graphics,
        limits,
        scripts,
    } = file.security.unwrap_or_default();
    let features = features.unwrap_or_default();
    let clipboard = clipboard.unwrap_or_default();
    let kitty_graphics = kitty_graphics.unwrap_or_default();
    let limits = build_limits(limits);
    let compatibility = build_compatibility(file.compatibility);
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
        feature_permissions: FeaturePermissions {
            macros: features.macros.unwrap_or_default(),
            udks: features.udks.unwrap_or_default(),
            clipboard: ClipboardPermissions {
                read: clipboard.read.unwrap_or_default(),
                write: clipboard.write.unwrap_or_default(),
            },
            kitty_graphics_files: kitty_graphics.files.unwrap_or_default(),
        },
        limits,
        script_permissions: scripts.unwrap_or_default(),
        compatibility,
        command_editor,
        new_tab_text,
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

fn build_compatibility(raw: Option<CompatibilitySettings>) -> CompatibilityConfig {
    let settings = raw.unwrap_or_default();
    CompatibilityConfig {
        emoji: settings.emoji.unwrap_or_default(),
    }
}

pub(crate) fn dedupe_paths(paths: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in paths {
        if !path.as_os_str().is_empty() && !out.iter().any(|existing| existing == &path) {
            out.push(path);
        }
    }
    out
}

fn parse_config_file(contents: &str) -> Result<(ConfigFile, Vec<String>), toml::de::Error> {
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

/// Resolve `~` and `$VAR` / `${VAR}` references in a config-supplied
/// path. Without this, `background_image = "~/foo.png"` is opened
/// literally and fails with ENOENT, since Rust's `PathBuf` (unlike a
/// shell) doesn't expand `~`. `shellexpand::full` also accepts
/// `${XDG_CONFIG_HOME}/term41/wall.png` and similar — handy because
/// terminals are exactly where users expect shell-style paths to work.
///
/// On a lookup failure (referenced env var unset), we log the error and
/// fall back to the literal path so the downstream loader reports a
/// clean "no such file" diagnostic against what the user actually wrote.
pub(crate) fn expand_path(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    match shellexpand::full(&raw) {
        Ok(expanded) => PathBuf::from(expanded.as_ref()),
        Err(e) => {
            warn!("path: failed to expand {raw:?}: {e}");
            path
        }
    }
}

pub fn scripts_dir_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("scripts"))
}

static CONFIG: Mutex<Option<Config>> = Mutex::new(None);

pub fn init_config(
    config_reload: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<std::thread::Thread>>,
) -> Config {
    if let Some(config) = CONFIG.lock().clone() {
        warn!("Init config called twice");
        return config;
    }

    let Some(config_path) = config_path() else {
        error!("Failed to initialize config watcher");
        *CONFIG.lock() = Some(Config::default());
        return Config::default();
    };

    let config = load_from(&config_path);
    *CONFIG.lock() = Some(config);

    spawn_config_watcher(config_path, config_reload, render_thread_handle);

    CONFIG.lock().clone().unwrap()
}

pub fn config() -> Config {
    CONFIG.lock().clone().unwrap_or_default()
}

/// Map the optional `keybindings = [...]` toml field onto a
/// [`Keybindings`]. Returns [`Keybindings::defaults`] when the key is
/// absent; an empty array (`keybindings = []`) is honoured as "no
/// bindings" so users can disable them all to debug a conflict.
fn build_keybindings(
    raw: Option<Vec<KeybindingConfig>>,
    path: &dyn std::fmt::Display,
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

fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("term41").join("config.toml"))
}

fn spawn_config_watcher(
    config_path: PathBuf,
    config_reload: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<std::thread::Thread>>,
) {
    use notify::EventKind;
    use notify::RecursiveMode;
    use notify::Watcher;

    let Some(dir) = config_path.parent().map(PathBuf::from) else {
        return;
    };

    std::thread::Builder::new()
        .name("config-watcher".into())
        .spawn(move || {
            let target = config_path.clone();
            let scripts_dir = dir.join("scripts");
            let config_reload_for_handler = config_reload.clone();
            let mut watcher = match notify::recommended_watcher(move |res| {
                let event: notify::Event = match res {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("config watcher error: {e}");
                        return;
                    }
                };
                let touches_config_or_script = event
                    .paths
                    .iter()
                    .any(|p| p == &target || p.starts_with(&scripts_dir));
                if !touches_config_or_script {
                    return;
                }
                if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    return;
                }

                *CONFIG.lock() = Some(load_from(&config_path));

                config_reload_for_handler.store(true, Ordering::Release);
                if let Some(thread) = render_thread_handle.get() {
                    thread.unpark();
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    warn!("failed to create config watcher: {e}");
                    return;
                }
            };

            if let Err(e) = watcher.watch(&dir, RecursiveMode::Recursive) {
                warn!("failed to watch config dir {}: {e}", dir.display());
                return;
            }
            std::thread::park();
        })
        .expect("spawn config watcher");
}

fn smolstr_opt<'de, D>(deserializer: D) -> Result<Option<SmolStr>, D::Error>
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

fn float_opt_clamp_0_1<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
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

fn float_opt<'de, D>(deserializer: D) -> Result<Option<f32>, D::Error>
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

fn u32_opt<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
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

fn cursor_shape_opt<'de, D>(deserializer: D) -> Result<Option<CursorShape>, D::Error>
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

fn cursor_blink_opt<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
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

fn keybindings_opt<'de, D>(deserializer: D) -> Result<Option<Vec<KeybindingConfig>>, D::Error>
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

fn bell_mode_opt<'de, D>(deserializer: D) -> Result<Option<BellMode>, D::Error>
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

fn gutter_opt<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
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

fn status_line_mode_opt<'de, D>(deserializer: D) -> Result<Option<StatusLineMode>, D::Error>
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

fn program_allowlist_opt<'de, D>(deserializer: D) -> Result<Option<ProgramAllowlist>, D::Error>
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

fn clipboard_permission_opt<'de, D>(deserializer: D) -> Result<Option<PermissionPolicy>, D::Error>
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

fn permission_policy_opt<'de, D>(deserializer: D) -> Result<Option<PermissionPolicy>, D::Error>
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

fn power_preference_opt<'de, D>(deserializer: D) -> Result<Option<PowerPreference>, D::Error>
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

fn vsync_opt<'de, D>(deserializer: D) -> Result<Option<VSync>, D::Error>
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

fn emoji_compatibility_mode_opt<'de, D>(
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

fn u32_opt_clamp_1_16<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Config {
        parse_config(s, &"<test>")
    }

    fn ignored_keys(s: &str) -> Vec<String> {
        parse_config_file(s).expect("config parses").1
    }

    #[test]
    fn gutter_defaults_to_enabled_when_absent() {
        assert!(parse("").gutter);
    }

    #[test]
    fn gutter_honours_explicit_false() {
        assert!(!parse("gutter = false").gutter);
    }

    #[test]
    fn gutter_honours_explicit_true() {
        assert!(parse("gutter = true").gutter);
    }

    #[test]
    fn status_line_defaults_to_off() {
        assert_eq!(parse("").status_line, StatusLineMode::Off);
    }

    #[test]
    fn status_line_accepts_indicator() {
        assert_eq!(
            parse("status_line = \"indicator\"").status_line,
            StatusLineMode::Indicator
        );
    }

    #[test]
    fn macros_allowlist_defaults_to_deny_all() {
        assert_eq!(
            parse("").feature_permissions.macros,
            ProgramAllowlist::DenyAll
        );
    }

    #[test]
    fn macros_allowlist_accepts_all_string() {
        assert_eq!(
            parse("[security.features]\nmacros = \"all\"\n")
                .feature_permissions
                .macros,
            ProgramAllowlist::AllowAll
        );
        assert_eq!(
            parse("[security.features]\nmacros = \"*\"\n")
                .feature_permissions
                .macros,
            ProgramAllowlist::AllowAll
        );
    }

    #[test]
    fn udks_allowlist_defaults_to_deny_all() {
        assert_eq!(
            parse("").feature_permissions.udks,
            ProgramAllowlist::DenyAll
        );
    }

    #[test]
    fn udks_allowlist_accepts_all_string() {
        assert_eq!(
            parse("[security.features]\nudks = \"all\"\n")
                .feature_permissions
                .udks,
            ProgramAllowlist::AllowAll
        );
    }

    #[test]
    fn clipboard_permissions_default_to_ask() {
        let permissions = parse("").feature_permissions.clipboard;
        assert_eq!(permissions.read, PermissionPolicy::Ask);
        assert_eq!(permissions.write, PermissionPolicy::Ask);
    }

    #[test]
    fn clipboard_permissions_accept_read_and_write_modes() {
        let allow = parse("[security.clipboard]\nread = \"all\"\nwrite = \"allow\"\n")
            .feature_permissions
            .clipboard;
        assert_eq!(allow.read, PermissionPolicy::Allow);
        assert_eq!(allow.write, PermissionPolicy::Allow);

        let wildcard = parse("[security.clipboard]\nread = \"*\"\n")
            .feature_permissions
            .clipboard;
        assert_eq!(wildcard.read, PermissionPolicy::Allow);
        assert_eq!(wildcard.write, PermissionPolicy::Ask);

        let deny = parse("[security.clipboard]\nread = \"deny\"\nwrite = \"no\"\n")
            .feature_permissions
            .clipboard;
        assert_eq!(deny.read, PermissionPolicy::Deny);
        assert_eq!(deny.write, PermissionPolicy::Deny);

        let none = parse("[security.clipboard]\nread = \"none\"\n")
            .feature_permissions
            .clipboard;
        assert_eq!(none.read, PermissionPolicy::Deny);
        assert_eq!(none.write, PermissionPolicy::Ask);
    }

    #[test]
    fn kitty_graphics_file_permission_defaults_to_ask() {
        assert_eq!(
            parse("").feature_permissions.kitty_graphics_files,
            PermissionPolicy::Ask
        );
    }

    #[test]
    fn kitty_graphics_file_permission_accepts_modes() {
        assert_eq!(
            parse("[security.kitty_graphics]\nfiles = \"allow\"\n")
                .feature_permissions
                .kitty_graphics_files,
            PermissionPolicy::Allow
        );
        assert_eq!(
            parse("[security.kitty_graphics]\nfiles = \"*\"\n")
                .feature_permissions
                .kitty_graphics_files,
            PermissionPolicy::Allow
        );
        assert_eq!(
            parse("[security.kitty_graphics]\nfiles = \"deny\"\n")
                .feature_permissions
                .kitty_graphics_files,
            PermissionPolicy::Deny
        );
    }

    #[test]
    fn limits_default_to_terminal_defaults() {
        assert_eq!(parse("").limits, TerminalLimits::default());
    }

    #[test]
    fn limits_accept_individual_overrides() {
        let cfg = parse(
            r#"
[security.limits]
macro_storage_bytes = 8192
macro_invocation_depth = 12
udk_storage_bytes = 1024
decudk_payload_bytes = 4096
drcs_payload_bytes = 131072
xtgettcap_payload_bytes = 2048
drcs_storage_bytes = 524288
kitty_graphics_payload_bytes = 65536
kitty_graphics_storage_bytes = 1048576
"#,
        );
        assert_eq!(cfg.limits.macro_storage_bytes, 8192);
        assert_eq!(cfg.limits.macro_invocation_depth, 12);
        assert_eq!(cfg.limits.udk_storage_bytes, 1024);
        assert_eq!(cfg.limits.decudk_payload_bytes, 4096);
        assert_eq!(cfg.limits.drcs_payload_bytes, 131072);
        assert_eq!(cfg.limits.xtgettcap_payload_bytes, 2048);
        assert_eq!(cfg.limits.drcs_storage_bytes, 524288);
        assert_eq!(cfg.limits.kitty_graphics_payload_bytes, 65536);
        assert_eq!(cfg.limits.kitty_graphics_storage_bytes, 1048576);
    }

    #[test]
    fn script_permissions_default_to_empty_policy_map() {
        assert!(parse("").script_permissions.is_empty());
    }

    #[test]
    fn script_permissions_parse_under_security() {
        let cfg = parse(
            r#"
[security.scripts.status]
filesystem = true
resource_usage = true

[security.scripts.title]
shell = true
process_info = true
"#,
        );
        assert_eq!(
            cfg.script_permissions.get("status"),
            Some(&ScriptPermissions {
                filesystem: true,
                resource_usage: true,
                ..ScriptPermissions::default()
            })
        );
        assert_eq!(
            cfg.script_permissions.get("title"),
            Some(&ScriptPermissions {
                shell: true,
                process_info: true,
                ..ScriptPermissions::default()
            })
        );
    }

    #[test]
    fn invalid_clipboard_permission_falls_back_to_ask() {
        let permissions = parse("[security.clipboard]\nread = \"sometimes\"\n")
            .feature_permissions
            .clipboard;
        assert_eq!(permissions.read, PermissionPolicy::Ask);
        assert_eq!(permissions.write, PermissionPolicy::Ask);
    }

    #[test]
    fn compatibility_emoji_defaults_to_auto() {
        assert_eq!(parse("").compatibility.emoji, EmojiCompatibilityMode::Auto);
    }

    #[test]
    fn compatibility_emoji_accepts_modes() {
        assert_eq!(
            parse("[compatibility]\nemoji = \"off\"\n")
                .compatibility
                .emoji,
            EmojiCompatibilityMode::Off
        );
        assert_eq!(
            parse("[compatibility]\nemoji = \"on\"\n")
                .compatibility
                .emoji,
            EmojiCompatibilityMode::On
        );
        assert_eq!(
            parse("[compatibility]\nemoji = \"auto\"\n")
                .compatibility
                .emoji,
            EmojiCompatibilityMode::Auto
        );
    }

    #[test]
    fn command_editor_defaults_disabled() {
        let cfg = parse("");
        assert!(!cfg.command_editor.enabled);
        assert!(!cfg.command_editor.vim_mode);
        assert!(cfg.command_editor.completions.is_empty());
        assert_eq!(
            cfg.command_editor.binary_dirs,
            CommandEditorConfig::default().binary_dirs
        );
        assert!(cfg.command_editor.merge_extra_dirs);
        assert!(!cfg.command_editor.deep_history_integration);
        assert_eq!(cfg.command_editor.max_history, 200);
        assert_eq!(cfg.command_editor.max_persistent_history_per_dir, 200);
    }

    #[test]
    fn command_editor_parses_settings() {
        let cfg = parse(
            r#"
[command_editor]
enabled = true
vim_mode = true
completions = ["cargo", "git"]
binary_dirs = ["~/custom-bin"]
merge_extra_dirs = false
deep_history_integration = true
max_history = 25
max_persistent_history_per_dir = 75
"#,
        );
        assert!(cfg.command_editor.enabled);
        assert!(cfg.command_editor.vim_mode);
        assert_eq!(cfg.command_editor.completions, ["cargo", "git"]);
        assert_eq!(
            cfg.command_editor.binary_dirs,
            [expand_path(PathBuf::from("~/custom-bin"))]
        );
        assert!(!cfg.command_editor.merge_extra_dirs);
        assert!(cfg.command_editor.deep_history_integration);
        assert_eq!(cfg.command_editor.max_history, 25);
        assert_eq!(cfg.command_editor.max_persistent_history_per_dir, 75);
    }

    #[test]
    fn invalid_compatibility_emoji_falls_back_to_auto() {
        assert_eq!(
            parse("[compatibility]\nemoji = \"sometimes\"\n")
                .compatibility
                .emoji,
            EmojiCompatibilityMode::Auto
        );
    }

    #[test]
    fn malformed_toml_falls_back_to_defaults_with_gutter_on() {
        // A typo shouldn't silently leave gutter off; the whole config
        // resets to defaults, and the default gutter state is on.
        let cfg = parse("gutter = \"yes-please\"");
        assert!(cfg.gutter);
    }

    #[test]
    fn unknown_top_level_keys_are_reported() {
        assert_eq!(
            ignored_keys("allow_feature = true\n"),
            vec!["allow_feature"]
        );
    }

    #[test]
    fn unknown_nested_keys_are_reported() {
        assert_eq!(
            ignored_keys("[features]\nmacros = \"all\"\n"),
            vec!["features"]
        );
    }

    #[test]
    fn background_image_defaults_to_none() {
        let cfg = parse("");
        assert!(cfg.background_image.is_none());
        // 1.0 is the no-op multiplier — image painted at full brightness
        // when set, but doesn't affect anything when unset.
        assert_eq!(cfg.background_opacity, 1.0);
    }

    #[test]
    fn background_image_and_opacity_round_trip() {
        let cfg = parse("background_image = \"/tmp/wallpaper.png\"\nbackground_opacity = 0.6\n");
        assert_eq!(
            cfg.background_image.as_deref(),
            Some(std::path::Path::new("/tmp/wallpaper.png"))
        );
        assert_eq!(cfg.background_opacity, 0.6);
    }

    #[test]
    fn background_opacity_clamps_to_unit_range() {
        let cfg = parse("background_opacity = 1.7");
        assert_eq!(cfg.background_opacity, 1.0);
        let cfg = parse("background_opacity = -0.3");
        assert_eq!(cfg.background_opacity, 0.0);
    }

    #[test]
    fn background_image_path_expands_tilde() {
        let cfg = parse("background_image = \"~/wallpapers/forest.png\"");
        let bg = cfg.background_image.expect("path parsed");
        // dirs::home_dir() can return None in some sandboxed test envs;
        // when it does, shellexpand leaves `~` literal and we just check
        // the path round-tripped untouched.
        if let Some(home) = dirs::home_dir() {
            assert_eq!(bg, home.join("wallpapers/forest.png"));
        } else {
            assert_eq!(bg, std::path::Path::new("~/wallpapers/forest.png"));
        }
    }

    #[test]
    fn background_image_path_expands_env_var() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };

        let cfg = parse("background_image = \"$HOME/term41-wallpaper.png\"");
        let expected = std::path::Path::new(&home).join("term41-wallpaper.png");
        assert_eq!(cfg.background_image.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn background_image_absolute_path_passes_through() {
        let cfg = parse("background_image = \"/srv/share/wall.png\"");
        assert_eq!(
            cfg.background_image.as_deref(),
            Some(std::path::Path::new("/srv/share/wall.png"))
        );
    }

    // ---- color palette ----

    #[test]
    fn empty_config_uses_default_palette() {
        let cfg = parse("");
        assert_eq!(cfg.palette.fg, Srgb::new(204, 204, 204));
        assert_eq!(cfg.palette.bg, Srgb::new(0, 0, 0));
        assert_eq!(cfg.palette.status_line_fg, cfg.palette.fg);
        assert_eq!(
            cfg.palette.status_line_bg,
            blend_colors(cfg.palette.bg, cfg.palette.fg, 0.25)
        );
        assert!(cfg.palette.cursor.is_none());
        assert!(cfg.palette.cursor_text.is_none());
        assert!(cfg.palette.selection_bg.is_none());
        // Default ANSI red
        assert_eq!(cfg.palette.ansi[1], Srgb::new(205, 0, 0));
    }

    #[test]
    fn status_line_palette_defaults_to_blended_background() {
        let cfg = parse(
            r##"
[colors.primary]
foreground = "#f0f0f0"
background = "#101820"
"##,
        );
        assert_eq!(cfg.palette.status_line_fg, cfg.palette.fg);
        assert_eq!(
            cfg.palette.status_line_bg,
            blend_colors(cfg.palette.bg, cfg.palette.status_line_fg, 0.25)
        );
    }

    #[test]
    fn status_line_palette_overrides_parse() {
        let cfg = parse(
            r##"
[colors.status_line]
foreground = "#123456"
background = "#654321"
"##,
        );
        assert_eq!(cfg.palette.status_line_fg, Srgb::new(0x12, 0x34, 0x56));
        assert_eq!(cfg.palette.status_line_bg, Srgb::new(0x65, 0x43, 0x21));
    }

    #[test]
    fn full_rio_palette_parses() {
        let cfg = parse(
            r##"
[colors]
cursor = "#009fff"

[colors.primary]
foreground = "#fbfbfb"
background = "#070707"

[colors.selection]
background = "#fbfbfb"
text = "#070707"

[colors.normal]
black = "#141415"
red = "#ff2e3f"
green = "#0dbe4e"
yellow = "#ffca00"
blue = "#009fff"
magenta = "#c635e4"
cyan = "#08c0ef"
white = "#c6c6c8"

[colors.bright]
black = "#6c6c71"
red = "#ff6762"
green = "#5ecc71"
yellow = "#ffd452"
blue = "#69b1ff"
magenta = "#d568ea"
cyan = "#68cdf2"
white = "#fbfbfb"
"##,
        );
        assert_eq!(cfg.palette.fg, Srgb::new(0xfb, 0xfb, 0xfb));
        assert_eq!(cfg.palette.bg, Srgb::new(0x07, 0x07, 0x07));
        assert_eq!(cfg.palette.cursor, Some(Srgb::new(0x00, 0x9f, 0xff)));
        assert!(cfg.palette.cursor_text.is_none());
        assert_eq!(cfg.palette.selection_bg, Some(Srgb::new(0xfb, 0xfb, 0xfb)));
        assert_eq!(cfg.palette.selection_fg, Some(Srgb::new(0x07, 0x07, 0x07)));
        // Spot-check a few ANSI colors
        assert_eq!(cfg.palette.ansi[0], Srgb::new(0x14, 0x14, 0x15)); // normal black
        assert_eq!(cfg.palette.ansi[1], Srgb::new(0xff, 0x2e, 0x3f)); // normal red
        assert_eq!(cfg.palette.ansi[8], Srgb::new(0x6c, 0x6c, 0x71)); // bright black
        assert_eq!(cfg.palette.ansi[15], Srgb::new(0xfb, 0xfb, 0xfb)); // bright white
    }

    #[test]
    fn cursor_table_palette_parses_cursor_and_text_colors() {
        let cfg = parse(
            r##"
[colors.cursor]
cursor = "#009fff"
text = "#070707"
"##,
        );
        assert_eq!(cfg.palette.cursor, Some(Srgb::new(0x00, 0x9f, 0xff)));
        assert_eq!(cfg.palette.cursor_text, Some(Srgb::new(0x07, 0x07, 0x07)));
    }

    #[test]
    fn partial_palette_keeps_defaults_for_unspecified() {
        let cfg = parse(
            r##"
[colors.primary]
foreground = "#ff0000"
"##,
        );
        assert_eq!(cfg.palette.fg, Srgb::new(255, 0, 0));
        // Background stays default
        assert_eq!(cfg.palette.bg, Srgb::new(0, 0, 0));
        // ANSI colors stay default
        assert_eq!(cfg.palette.ansi[1], Srgb::new(205, 0, 0));
    }

    #[test]
    fn invalid_hex_color_falls_back_to_default() {
        let cfg = parse(
            r##"
[colors.primary]
foreground = "not-a-color"
"##,
        );
        // Should fall back to default fg
        assert_eq!(cfg.palette.fg, Srgb::new(204, 204, 204));
    }
}

#[macro_use]
extern crate log;
extern crate palette as palette_crate;

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Deserialize;
use smol_str::SmolStr;
use smol_str::ToSmolStr;

mod command_editor;
mod compatibility;
mod deserialize;
pub mod keybindings;
mod palette;
mod runtime;
mod schema;
mod security;

pub use command_editor::CommandEditorConfig;
pub use compatibility::CompatibilityConfig;
pub use compatibility::EmojiCompatibilityMode;
pub use compatibility::ShellIntegrationConfig;
pub(crate) use deserialize::usize_opt;
pub use palette::ColorPalette;
pub use palette::default_bg;
pub use palette::default_fg;
pub use runtime::config;
pub(crate) use runtime::dedupe_paths;
pub(crate) use runtime::expand_path;
pub use runtime::init_config;
pub use runtime::scripts_dir_path;
pub use security::ClipboardPermissions;
pub use security::FeaturePermissions;
pub use security::PermissionPolicy;
pub use security::ProgramAllowlist;
pub use security::ScriptPermissions;
pub use security::TerminalLimits;

pub const DEFAULT_SCROLLBACK: u32 = 10_000;
pub const MAX_MACRO_BYTES: usize = 6 * 1024;
pub const MAX_MACRO_INVOCATION_DEPTH: usize = 32;
pub const MAX_UDK_BYTES: usize = 256;
pub const MAX_DECUDK_PAYLOAD_BYTES: usize = 2048;
pub const MAX_DRCS_PAYLOAD_BYTES: usize = 64 * 1024;
pub const MAX_DRCS_TOTAL_STORAGE_BYTES: usize = 256 * 1024;

#[derive(Debug, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PowerPreference {
    #[default]
    #[serde(alias = "none")]
    Auto,
    LowPower,
    HighPerformance,
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
    /// Present frames as soon as they're ready, even if that means tearing.
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
/// completion-ambiguity by default; most users find that surprising rather
/// than useful out of the box.
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

#[derive(Debug, Clone)]
pub struct Config {
    pub opacity: f32,
    pub fonts: Option<String>,
    pub font_size: f32,
    pub scrollback_lines: u32,
    pub status_line: StatusLineMode,
    pub cursor_style: CursorStyle,
    pub keybindings: keybindings::Keybindings,
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
    pub shell_integration: ShellIntegrationConfig,
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
            keybindings: keybindings::Keybindings::defaults(),
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
            shell_integration: ShellIntegrationConfig::default(),
            command_editor: CommandEditorConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use utils41::blend_colors;

    use super::*;
    use crate::palette_crate::Srgb;

    fn parse(s: &str) -> Config {
        crate::schema::parse_config(s, &"<test>")
    }

    fn ignored_keys(s: &str) -> Vec<String> {
        crate::schema::parse_config_file(s)
            .expect("config parses")
            .1
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
    fn shell_integration_hooks_default_to_off() {
        assert!(!parse("").shell_integration.hooks);
    }

    #[test]
    fn shell_integration_hooks_are_opt_in() {
        assert!(
            parse("[shell_integration]\nhooks = true\n")
                .shell_integration
                .hooks
        );
    }

    #[test]
    fn command_editor_defaults_disabled() {
        let cfg = parse("");
        assert!(!cfg.command_editor.enabled);
        assert!(!cfg.command_editor.vim_mode);
        assert!(cfg.command_editor.completions.is_empty());
        assert!(cfg.command_editor.completion_files.is_empty());
        assert!(cfg.command_editor.command_completions.is_empty());
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
completion_files = ["~/completions/cargo.json"]
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
            cfg.command_editor.completion_files,
            [expand_path(PathBuf::from("~/completions/cargo.json"))]
        );
        assert!(cfg.command_editor.command_completions.is_empty());
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
        // 1.0 is the no-op multiplier: image painted at full brightness
        // when set, but doesn't affect anything when unset.
        assert_eq!(cfg.background_opacity, 1.0);
    }

    #[test]
    fn background_image_and_opacity_round_trip() {
        let cfg = parse("background_image = \"/tmp/wallpaper.png\"\nbackground_opacity = 0.6\n");
        assert_eq!(
            cfg.background_image.as_deref(),
            Some(Path::new("/tmp/wallpaper.png"))
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
            assert_eq!(bg, Path::new("~/wallpapers/forest.png"));
        }
    }

    #[test]
    fn background_image_path_expands_env_var() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };

        let cfg = parse("background_image = \"$HOME/term41-wallpaper.png\"");
        let expected = Path::new(&home).join("term41-wallpaper.png");
        assert_eq!(cfg.background_image.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn background_image_absolute_path_passes_through() {
        let cfg = parse("background_image = \"/srv/share/wall.png\"");
        assert_eq!(
            cfg.background_image.as_deref(),
            Some(Path::new("/srv/share/wall.png"))
        );
    }

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
        assert_eq!(cfg.palette.ansi[0], Srgb::new(0x14, 0x14, 0x15));
        assert_eq!(cfg.palette.ansi[1], Srgb::new(0xff, 0x2e, 0x3f));
        assert_eq!(cfg.palette.ansi[8], Srgb::new(0x6c, 0x6c, 0x71));
        assert_eq!(cfg.palette.ansi[15], Srgb::new(0xfb, 0xfb, 0xfb));
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
        assert_eq!(cfg.palette.bg, Srgb::new(0, 0, 0));
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
        assert_eq!(cfg.palette.fg, Srgb::new(204, 204, 204));
    }
}

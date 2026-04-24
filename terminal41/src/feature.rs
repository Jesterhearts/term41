use serde::Deserialize;

use crate::ColorPalette;
use crate::DrcsStore;
use crate::Screen;
use crate::StatusDisplayKind;
use crate::Viewport;
use crate::dec::r#macro::MAX_MACRO_BYTES;
use crate::dec::r#macro::MAX_MACRO_INVOCATION_DEPTH;
use crate::dec::r#macro::MacroEncoding;
use crate::dec::r#macro::MacroStore;
use crate::dec::udk::DecModifierKey;
use crate::dec::udk::LocalFunctionKeyControl;
use crate::dec::udk::MAX_DECUDK_PAYLOAD_BYTES;
use crate::dec::udk::MAX_UDK_BYTES;
use crate::dec::udk::ModifierKeyControl;
use crate::dec::udk::UdkState;
use crate::drcs::MAX_DRCS_PAYLOAD_BYTES;
use crate::drcs::MAX_DRCS_TOTAL_STORAGE_BYTES;
use crate::screen;

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
    pub read: ClipboardPermission,
    /// Whether host programs may write local clipboard contents.
    pub write: ClipboardPermission,
}

/// Permission policy for one clipboard access direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClipboardPermission {
    /// Ask the user for this request.
    #[default]
    Ask,
    /// Allow every request without prompting.
    #[serde(alias = "*", alias = "all")]
    Allow,
    /// Deny every request without prompting.
    #[serde(alias = "no", alias = "none")]
    Deny,
}

pub(crate) fn macro_feature_enabled(permissions: &FeaturePermissions) -> bool {
    permissions.macros.allow()
}

pub(crate) fn udk_feature_enabled(permissions: &FeaturePermissions) -> bool {
    permissions.udks.allow()
}

pub(crate) fn define_macro(
    enabled: bool,
    macros: &mut MacroStore,
    params: vtepp::Params,
    payload: &[u8],
    limits: TerminalLimits,
) {
    if !enabled {
        return;
    }
    let Some(id) = params
        .iter()
        .next()
        .and_then(|group| group.first().copied())
    else {
        return;
    };
    let delete_existing = matches!(
        params
            .iter()
            .nth(1)
            .and_then(|group| group.first().copied()),
        Some(0 | 1)
    );
    let Some(encoding) = params
        .iter()
        .nth(2)
        .and_then(|group| group.first().copied())
        .and_then(MacroEncoding::from_param)
    else {
        return;
    };
    macros.define(
        id,
        delete_existing,
        encoding,
        payload,
        limits.macro_storage_bytes,
    );
}

pub(crate) fn invoke_macro(
    enabled: bool,
    macros: &MacroStore,
    macro_invocation_depth: usize,
    id: u16,
    limits: TerminalLimits,
) -> Option<Vec<u8>> {
    if !enabled || macro_invocation_depth >= limits.macro_invocation_depth {
        return None;
    }
    macros.get(id).map(ToOwned::to_owned)
}

pub(crate) fn drcs_render_glyphs(drcs: &DrcsStore) -> font41::DrcsGlyphMap {
    drcs.render_glyphs()
}

pub(crate) fn define_udk(
    enabled: bool,
    udks: &mut UdkState,
    params: vtepp::Params,
    payload: &[u8],
    limits: TerminalLimits,
) {
    if enabled {
        udks.define(params, payload, limits.udk_storage_bytes);
    }
}

pub(crate) fn lookup_udk(
    enabled: bool,
    udks: &UdkState,
    selector: u16,
) -> Option<Vec<u8>> {
    enabled
        .then(|| udks.definition(selector).map(ToOwned::to_owned))
        .flatten()
}

pub(crate) fn local_function_key_control(
    enabled: bool,
    udks: &UdkState,
    selector: u16,
) -> Option<LocalFunctionKeyControl> {
    enabled.then(|| udks.local_function_key(selector)).flatten()
}

pub(crate) fn modifier_key_control(
    enabled: bool,
    udks: &UdkState,
    key: DecModifierKey,
) -> ModifierKeyControl {
    if enabled {
        udks.modifier_key(key)
    } else {
        ModifierKeyControl::Modifier
    }
}

pub(crate) fn apply_status_display_mode(
    screen: &mut Screen,
    total_rows: u32,
    cols: u32,
    status_display: StatusDisplayKind,
    palette: &ColorPalette,
) -> u32 {
    let old_rows = total_rows.saturating_sub(screen::status_line_rows(screen));
    screen::set_status_display(
        screen,
        cols,
        status_display,
        palette.status_line_fg,
        palette.status_line_bg,
    );
    let new_rows = total_rows.saturating_sub(screen::status_line_rows(screen));
    if new_rows != old_rows {
        screen::resize_screen(screen, cols, old_rows, cols, new_rows);
        if screen::page_memory_active(screen)
            && let Some(page_rows) = screen::page_rows(screen)
        {
            screen::resize_page_memory(
                screen,
                &Viewport {
                    rows: new_rows,
                    cols,
                    top: 0,
                },
                page_rows,
            );
        }
    }
    new_rows
}

pub(crate) fn apply_scrollback_limit(
    screen: &mut Screen,
    viewport: &Viewport,
    limit: u32,
) {
    screen.grid.scrollback_limit = limit;

    let max_rows = viewport.rows as usize + limit as usize;
    let grid = &mut screen.grid;
    let popped_before = grid.rows.len();
    while grid.rows.len() > max_rows {
        grid.rows.pop_front();
        grid.total_popped += 1;
    }
    let popped = popped_before - grid.rows.len();
    if popped > 0 {
        screen.images.retain(|_, img| img.row >= popped);
        for img in screen.images.values_mut() {
            img.row -= popped;
        }
    }
}

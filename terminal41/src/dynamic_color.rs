use config41::ColorPalette;
use palette::Srgb;

use crate::DecColorState;
use crate::Screen;
use crate::dec::color::effective_palette;
use crate::dec::color::rebase_all_indexed_entries;
use crate::dec::color::rebase_default_entries;
use crate::dec::color::rebase_indexed_entry;
use crate::screen::palette_sync::apply_screen_palette;
use crate::screen::palette_sync::sync_screen_erase_defaults;

#[derive(Debug, Clone)]
pub struct RuntimeColorOverrides {
    indexed: [Option<Srgb<u8>>; 256],
    foreground: Option<Srgb<u8>>,
    background: Option<Srgb<u8>>,
    cursor: CursorOverride,
}

#[derive(Debug, Clone, Copy)]
enum CursorOverride {
    Inherit,
    Color(Srgb<u8>),
    Configured,
}

impl Default for RuntimeColorOverrides {
    fn default() -> Self {
        Self {
            indexed: [None; 256],
            foreground: None,
            background: None,
            cursor: CursorOverride::Inherit,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DynamicColorTarget {
    Foreground,
    Background,
    Cursor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ColorMutation {
    SetIndexed {
        index: u8,
        color: Srgb<u8>,
    },
    ResetIndexed(u8),
    ResetAllIndexed,
    SetDynamic {
        target: DynamicColorTarget,
        color: Srgb<u8>,
    },
    ResetDynamic(DynamicColorTarget),
}

pub(crate) fn apply_mutations(
    active: &mut Screen,
    stash: &mut Screen,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    overrides: &mut RuntimeColorOverrides,
    mutations: impl IntoIterator<Item = ColorMutation>,
) {
    let old_runtime_palette = runtime_palette(base_palette, overrides);
    let mut changed_indices = [false; 256];
    let mut changed = false;
    for mutation in mutations {
        match mutation {
            ColorMutation::SetIndexed { index, .. } | ColorMutation::ResetIndexed(index) => {
                changed_indices[index as usize] = true;
            }
            ColorMutation::ResetAllIndexed => changed_indices.fill(true),
            ColorMutation::SetDynamic { .. } | ColorMutation::ResetDynamic(_) => {}
        }
        mutate(overrides, mutation);
        changed = true;
    }
    if !changed {
        return;
    }
    let new_runtime_palette = runtime_palette(base_palette, overrides);
    if changed_indices.iter().all(|changed| *changed) {
        rebase_all_indexed_entries(dec_color, &old_runtime_palette, &new_runtime_palette);
    } else {
        for (index, changed) in changed_indices.into_iter().enumerate() {
            if changed {
                rebase_indexed_entry(
                    dec_color,
                    &old_runtime_palette,
                    &new_runtime_palette,
                    index as u8,
                );
            }
        }
    }
    rebase_default_entries(dec_color, &old_runtime_palette, &new_runtime_palette);

    *palette = effective_palette(&new_runtime_palette, dec_color);
    for screen in [active, stash] {
        apply_screen_palette(screen, palette, dec_color);
        sync_screen_erase_defaults(screen, dec_color);
    }
}

pub(crate) fn runtime_palette(
    base_palette: &ColorPalette,
    overrides: &RuntimeColorOverrides,
) -> ColorPalette {
    let mut palette = base_palette.clone();
    for (index, color) in overrides.indexed.iter().enumerate() {
        if let Some(color) = color {
            palette.set_indexed_color(index as u8, *color);
        }
    }
    if let Some(color) = overrides.foreground {
        palette.fg = color;
    }
    if let Some(color) = overrides.background {
        palette.bg = color;
    }
    match overrides.cursor {
        CursorOverride::Inherit => {}
        CursorOverride::Color(color) => palette.cursor = Some(color),
        CursorOverride::Configured => {
            palette.cursor = Some(base_palette.cursor.unwrap_or(base_palette.fg));
        }
    }
    palette
}

pub(crate) fn effective_runtime_palette(
    base_palette: &ColorPalette,
    overrides: &RuntimeColorOverrides,
    dec_color: &DecColorState,
) -> ColorPalette {
    effective_palette(&runtime_palette(base_palette, overrides), dec_color)
}

pub(crate) fn clear_indexed(overrides: &mut RuntimeColorOverrides) {
    overrides.indexed.fill(None);
}

fn mutate(
    overrides: &mut RuntimeColorOverrides,
    mutation: ColorMutation,
) {
    match mutation {
        ColorMutation::SetIndexed { index, color } => {
            overrides.indexed[index as usize] = Some(color);
        }
        ColorMutation::ResetIndexed(index) => overrides.indexed[index as usize] = None,
        ColorMutation::ResetAllIndexed => overrides.indexed.fill(None),
        ColorMutation::SetDynamic { target, color } => match target {
            DynamicColorTarget::Foreground => overrides.foreground = Some(color),
            DynamicColorTarget::Background => overrides.background = Some(color),
            DynamicColorTarget::Cursor => overrides.cursor = CursorOverride::Color(color),
        },
        ColorMutation::ResetDynamic(target) => match target {
            DynamicColorTarget::Foreground => overrides.foreground = None,
            DynamicColorTarget::Background => overrides.background = None,
            DynamicColorTarget::Cursor => overrides.cursor = CursorOverride::Configured,
        },
    }
}

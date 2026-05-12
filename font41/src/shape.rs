use std::sync::Arc;

use harfrust::Direction;
use harfrust::Script;
use harfrust::UnicodeBuffer;
use icu_properties::props::BinaryProperty;
use icu_properties::props::EmojiComponent;
use icu_properties::props::EnumeratedProperty;
use icu_properties::props::GeneralCategory;
use log::trace;
use smol_str::SmolStr;

use crate::FontSystem;
use crate::ShapedGlyph;
use crate::attrs::CellAttrs;
use crate::drcs;
use crate::families;
use crate::families::FamilyVariants;
use crate::legacy;
use crate::loader;
use crate::loader::LazyFontFace;
use crate::loader::LoadedFont;
use crate::loader::load_font_candidate;
use crate::loader::loaded_font_ref;
use crate::rasterize::outline_glyph_bounds;

/// Key for the ShapePlan cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PlanKey {
    font_index: usize,
    direction: Direction,
    script: Script,
}

pub(crate) fn shape_row(
    font_system: &mut FontSystem,
    cells: &[SmolStr],
    attrs: &[CellAttrs],
) -> Vec<ShapedGlyph> {
    font_system.sync_font_generation();

    if cells.is_empty() {
        return vec![];
    }

    let font_faces = loader::font_faces();
    let family_variants = loader::family_variants();
    let final_font_index = font_faces.len();

    trace!(
        "shaping row: cells={:?}",
        if cells.iter().all(|c| c.is_empty() || c == " ") {
            vec!["<all empty>"]
        } else {
            cells.iter().map(|c| c.as_str()).collect::<Vec<_>>()
        }
    );

    let (row_text, col_map) = row_text_and_col_map(cells);
    let wants_color: Vec<bool> = cells
        .iter()
        .map(|c| families::cluster_prefers_color(c))
        .collect();
    let wants_nerd_symbol: Vec<bool> = cells
        .iter()
        .map(|c| families::cluster_prefers_nerd_symbol(c))
        .collect();
    let nerd_symbol_font_index = font_faces.iter().position(|font| font.is_nerd_symbol_hint);
    let preferred = preferred_fonts_for_cells(
        cells,
        attrs,
        &family_variants,
        final_font_index,
        &wants_color,
        &wants_nerd_symbol,
        nerd_symbol_font_index,
    );

    let mut has_glyph = vec![false; cells.len()];
    let mut result: Vec<ShapedGlyph> = Vec::with_capacity(cells.len());
    add_synthetic_glyphs(cells, &mut has_glyph, &mut result);

    if all_visible_cells_covered(cells, &has_glyph) {
        return result;
    }

    shape_with_font_fallback(
        font_system,
        ShapeFallbackInputs {
            cells,
            row_text: &row_text,
            col_map: &col_map,
            font_faces: &font_faces,
            preferred: &preferred,
            wants_color: &wants_color,
            wants_nerd_symbol: &wants_nerd_symbol,
        },
        ShapeFallbackState {
            has_glyph: &mut has_glyph,
            result: &mut result,
        },
    );

    result
}

struct ShapeFallbackInputs<'a> {
    cells: &'a [SmolStr],
    row_text: &'a str,
    col_map: &'a [u16],
    font_faces: &'a [Arc<LazyFontFace>],
    preferred: &'a [Option<usize>],
    wants_color: &'a [bool],
    wants_nerd_symbol: &'a [bool],
}

struct ShapeFallbackState<'a> {
    has_glyph: &'a mut [bool],
    result: &'a mut Vec<ShapedGlyph>,
}

struct ApplyShapedInputs<'a> {
    cells: &'a [SmolStr],
    col_map: &'a [u16],
    font_idx: usize,
    loaded: &'a LoadedFont,
    font_ref: &'a read_fonts::FontRef<'a>,
    output: &'a harfrust::GlyphBuffer,
    preferred: &'a [Option<usize>],
    wants_nerd_symbol: &'a [bool],
    pass: usize,
}

struct ApplyShapedState<'a> {
    has_glyph: &'a mut [bool],
    result: &'a mut Vec<ShapedGlyph>,
}

fn row_text_and_col_map(cells: &[SmolStr]) -> (String, Vec<u16>) {
    let mut row_text = String::new();
    let mut col_map: Vec<u16> = Vec::new();

    for (col, cell) in cells.iter().enumerate() {
        let start = row_text.len();
        let mut chars = cell.chars();
        if let Some(ch) = chars.next()
            && is_orphaned_emoji_component(ch)
            && chars.next().is_none()
        {
            // Orphaned emoji components need their own cell broken from the previous one.
            row_text.push('\u{200C}');
        }
        row_text.push_str(cell);
        let added = row_text.len() - start;
        for _ in 0..added {
            col_map.push(col as u16);
        }
    }

    (row_text, col_map)
}

fn is_orphaned_emoji_component(ch: char) -> bool {
    matches!(
        GeneralCategory::for_char(ch),
        GeneralCategory::ModifierSymbol | GeneralCategory::ModifierLetter
    ) && EmojiComponent::for_char(ch)
}

fn preferred_fonts_for_cells(
    cells: &[SmolStr],
    attrs: &[CellAttrs],
    family_variants: &[FamilyVariants],
    final_font_index: usize,
    wants_color: &[bool],
    wants_nerd_symbol: &[bool],
    nerd_symbol_font_index: Option<usize>,
) -> Vec<Option<usize>> {
    cells
        .iter()
        .enumerate()
        .map(|(col, _)| {
            families::preferred_font_for_cell(
                family_variants,
                final_font_index,
                attrs[col],
                wants_color[col],
                wants_nerd_symbol[col],
                nerd_symbol_font_index,
            )
        })
        .collect()
}

fn add_synthetic_glyphs(
    cells: &[SmolStr],
    has_glyph: &mut [bool],
    result: &mut Vec<ShapedGlyph>,
) {
    for (col, cell) in cells.iter().enumerate() {
        if let Some(glyph_id) = legacy::encode_single(cell) {
            result.push(ShapedGlyph {
                glyph_id,
                font_index: legacy::FONT_INDEX,
                col: col as u16,
                cells_wide: 1,
                x_offset: 0.0,
                y_offset: 0.0,
            });
            has_glyph[col] = true;
        } else if let Some(glyph_id) = drcs::encode_single(cell) {
            result.push(ShapedGlyph {
                glyph_id,
                font_index: drcs::FONT_INDEX,
                col: col as u16,
                cells_wide: 1,
                x_offset: 0.0,
                y_offset: 0.0,
            });
            has_glyph[col] = true;
        }
    }
}

fn shape_with_font_fallback(
    font_system: &mut FontSystem,
    inputs: ShapeFallbackInputs<'_>,
    state: ShapeFallbackState<'_>,
) {
    let final_font_index = inputs.font_faces.len();
    for pass in 0..2 {
        for font_idx in 0..=final_font_index {
            if pass == 0
                && !font_is_useful_for_preferred_pass(
                    font_idx,
                    inputs.font_faces,
                    &font_system.final_font,
                    inputs.preferred,
                    inputs.wants_color,
                    state.has_glyph,
                    inputs.cells,
                )
            {
                continue;
            }

            let Some(font_candidate) =
                load_font_candidate(font_idx, inputs.font_faces, &font_system.final_font)
            else {
                continue;
            };
            let loaded = font_candidate.as_loaded();

            let font_ref = match loaded_font_ref(loaded) {
                Ok(f) => f,
                Err(_) => continue,
            };

            let mut buffer = UnicodeBuffer::new();
            buffer.push_str(inputs.row_text);
            buffer.guess_segment_properties();

            let direction = buffer.direction();
            let script = buffer.script();
            let key = PlanKey {
                font_index: font_idx,
                direction,
                script,
            };

            font_system.plan_cache.entry(key).or_insert_with(|| {
                let shaper = loaded.shaper_data.shaper(&font_ref).build();
                harfrust::ShapePlan::new(
                    &shaper,
                    direction,
                    Some(script),
                    buffer.language().as_ref(),
                    &[],
                )
            });
            let plan = &font_system.plan_cache[&key];

            let shaper = loaded.shaper_data.shaper(&font_ref).build();
            let output = shaper.shape_with_plan(plan, buffer, &[]);

            apply_shaped_output(
                font_system,
                ApplyShapedInputs {
                    cells: inputs.cells,
                    col_map: inputs.col_map,
                    font_idx,
                    loaded,
                    font_ref: &font_ref,
                    output: &output,
                    preferred: inputs.preferred,
                    wants_nerd_symbol: inputs.wants_nerd_symbol,
                    pass,
                },
                ApplyShapedState {
                    has_glyph: state.has_glyph,
                    result: state.result,
                },
            );

            if all_visible_cells_covered(inputs.cells, state.has_glyph) {
                return;
            }
        }
    }
}

fn apply_shaped_output(
    font_system: &FontSystem,
    inputs: ApplyShapedInputs<'_>,
    state: ApplyShapedState<'_>,
) {
    let infos = inputs.output.glyph_infos();
    let positions = inputs.output.glyph_positions();
    let scale = font_system.font_size / inputs.loaded.units_per_em;

    for (i, (info, pos)) in infos.iter().zip(positions.iter()).enumerate() {
        let cluster = info.cluster as usize;
        if cluster >= inputs.col_map.len() {
            continue;
        }
        let col = inputs.col_map[cluster];

        if state.has_glyph[col as usize] {
            continue;
        }

        let glyph_id = info.glyph_id as u16;
        if glyph_id == 0 {
            continue;
        }

        if inputs.pass == 0 {
            match inputs.preferred[col as usize] {
                Some(pref_idx) => {
                    if inputs.font_idx != pref_idx {
                        continue;
                    }
                }
                None => {
                    if !inputs.loaded.is_color {
                        continue;
                    }
                }
            }
        }

        let max_col =
            mark_consumed_columns(cluster, i, infos, inputs.col_map, state.has_glyph, col);
        let y_offset = pos.y_offset as f32 * scale
            + symbol_cell_y_offset(
                inputs.wants_nerd_symbol[col as usize],
                inputs.font_ref,
                glyph_id,
                scale,
                font_system.cell_height,
                font_system.ascent,
            );

        state.result.push(ShapedGlyph {
            glyph_id,
            font_index: inputs.font_idx,
            col,
            cells_wide: glyph_cells_wide(inputs.cells, col, max_col),
            x_offset: pos.x_offset as f32 * scale,
            y_offset,
        });
    }
}

fn mark_consumed_columns(
    cluster: usize,
    glyph_index: usize,
    infos: &[harfrust::GlyphInfo],
    col_map: &[u16],
    has_glyph: &mut [bool],
    col: u16,
) -> u16 {
    let end_byte = if glyph_index + 1 < infos.len() {
        (infos[glyph_index + 1].cluster as usize).min(col_map.len())
    } else {
        col_map.len()
    };
    let mut max_col = col;
    for byte in cluster..end_byte {
        if byte < col_map.len() {
            let consumed_col = col_map[byte];
            has_glyph[consumed_col as usize] = true;
            max_col = max_col.max(consumed_col);
        }
    }
    max_col
}

fn all_visible_cells_covered(
    cells: &[SmolStr],
    has_glyph: &[bool],
) -> bool {
    has_glyph
        .iter()
        .enumerate()
        .all(|(i, &has)| has || cells[i] == " " || cells[i].is_empty())
}

fn font_is_useful_for_preferred_pass(
    font_idx: usize,
    font_faces: &[Arc<LazyFontFace>],
    final_font: &LoadedFont,
    preferred: &[Option<usize>],
    wants_color: &[bool],
    has_glyph: &[bool],
    cells: &[SmolStr],
) -> bool {
    preferred.iter().enumerate().any(|(col, preferred)| {
        if has_glyph[col] || cells[col] == " " || cells[col].is_empty() {
            return false;
        }

        match preferred {
            Some(preferred_idx) => *preferred_idx == font_idx,
            None if wants_color[col] => font_is_color_candidate(font_idx, font_faces, final_font),
            None => false,
        }
    })
}

fn font_is_color_candidate(
    font_idx: usize,
    font_faces: &[Arc<LazyFontFace>],
    final_font: &LoadedFont,
) -> bool {
    font_faces
        .get(font_idx)
        .map(|font| font.is_color_hint)
        .unwrap_or(final_font.is_color)
}

pub(crate) fn glyph_cells_wide(
    cells: &[SmolStr],
    start_col: u16,
    max_consumed_col: u16,
) -> u8 {
    let start = start_col as usize;
    let mut end_excl = max_consumed_col as usize + 1;

    while end_excl < cells.len() && cells[end_excl].is_empty() {
        end_excl += 1;
    }

    end_excl.saturating_sub(start).clamp(1, u8::MAX as usize) as u8
}

fn symbol_cell_y_offset(
    is_symbol_cell: bool,
    font: &read_fonts::FontRef<'_>,
    glyph_id: u16,
    scale: f32,
    cell_height: u32,
    baseline: f32,
) -> f32 {
    if !is_symbol_cell {
        return 0.0;
    }

    let Some([_, y_min, _, y_max]) = outline_glyph_bounds(font, glyph_id, scale) else {
        return 0.0;
    };

    cell_centering_y_offset(cell_height as f32, baseline, y_min, y_max)
}

pub(crate) fn cell_centering_y_offset(
    cell_height: f32,
    baseline: f32,
    y_min: f32,
    y_max: f32,
) -> f32 {
    let glyph_height = y_max - y_min;
    if glyph_height <= 0.0 {
        return 0.0;
    }

    let current_top = baseline - y_max;
    let desired_top = ((cell_height - glyph_height) * 0.5).max(0.0);
    current_top - desired_top
}

#[cfg(test)]
mod emoji_component_tests {
    use super::*;

    #[test]
    fn orphaned_emoji_component_requires_component_and_modifier_category() {
        assert!(is_orphaned_emoji_component('\u{1F3FB}'));
        assert!(!is_orphaned_emoji_component('7'));
        assert!(!is_orphaned_emoji_component('A'));
    }
}

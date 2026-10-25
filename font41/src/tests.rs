use super::*;
use crate::families::FamilyVariants;
use crate::families::NERD_SYMBOL_FAMILY;
use crate::families::NERD_SYMBOL_FAMILY_MONO;
use crate::families::cluster_prefers_color;
use crate::families::cluster_prefers_nerd_symbol;
use crate::families::nerd_symbol_family_priority;
use crate::families::preferred_font_for_cell;
use crate::metrics::ScaledCellMetrics;
use crate::metrics::aggregate_cell_metrics;
use crate::shape::cell_centering_y_offset;
use crate::shape::glyph_cells_wide;

fn face_info(
    families: &[&str],
    weight: fontdb::Weight,
    style: fontdb::Style,
) -> fontdb::FaceInfo {
    fontdb::FaceInfo {
        id: fontdb::ID::dummy(),
        source: fontdb::Source::Binary(Arc::new(Vec::<u8>::new())),
        index: 0,
        families: families
            .iter()
            .map(|family| (family.to_string(), fontdb::Language::English_UnitedStates))
            .collect(),
        post_script_name: families.first().copied().unwrap_or("TestFont").to_string(),
        style,
        weight,
        stretch: fontdb::Stretch::Normal,
        monospaced: true,
    }
}

/// All shaped glyphs for visible characters must rasterize to non-empty
/// bitmaps, including ligature replacement glyphs that may be nested
/// composites (composite referencing another composite).
#[test]
fn shaped_glyphs_rasterize() {
    let mut fs = FontSystem::new(None, 18.0, 4);

    for text in [":: ", "a::b ", "Hello "] {
        let cells: Vec<SmolStr> = text
            .chars()
            .map(|c| {
                let mut buf = [0u8; 4];
                SmolStr::new_inline(c.encode_utf8(&mut buf))
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = fs.shape_row(&cells, &attrs);
        for sg in &shaped {
            let cell = &cells[sg.col as usize];
            if cell == " " {
                continue;
            }
            let raster = fs.rasterize_glyph(sg.font_index, sg.glyph_id, sg.cells_wide as u32);
            assert!(
                raster.width > 0 && raster.height > 0,
                "glyph {} for {cell:?} at col {} in {text:?} must rasterize, got {}x{}",
                sg.glyph_id,
                sg.col,
                raster.width,
                raster.height,
            );
        }
    }
}

#[test]
fn ready_fonts_wait_for_sync_and_do_not_change_other_instances() {
    let mut first = FontSystem::new(None, 18.0, 1);
    let mut second = FontSystem::new(None, 18.0, 1);
    let pending = Arc::new(std::sync::OnceLock::new());
    first.pending_fonts = Some(pending.clone());
    second.pending_fonts = None;
    let cells = [SmolStr::new("A")];
    let attrs = [CellAttrs::default()];
    let original = first.shape_row(&cells, &attrs).remove(0);
    let raster = first.rasterize_glyph(original.font_index, original.glyph_id, 1);
    let dimensions = (first.cell_width, first.cell_height);
    let generation = first.font_generation();

    let fonts = Arc::new(loader::FontSet {
        faces: vec![Arc::new(loader::LazyFontFace::new(
            fontdb::Source::Binary(Arc::new(FAIRFAX_HD)),
            0,
            false,
            false,
            true,
            false,
        ))],
        families: vec![FamilyVariants {
            regular: 0,
            bold: None,
            italic: None,
            bold_italic: None,
        }],
    });
    assert!(pending.set(fonts).is_ok());

    let before_sync = first.shape_row(&cells, &attrs).remove(0);
    assert_eq!(before_sync.font_index, original.font_index);
    assert_eq!(first.font_generation(), generation);
    assert_eq!(
        first
            .rasterize_glyph(original.font_index, original.glyph_id, 1)
            .bitmap,
        raster.bitmap
    );
    assert!(!first.font_is_bold(0));

    assert!(first.sync_loaded_fonts());
    assert_ne!(first.font_generation(), generation);
    assert_eq!((first.cell_width, first.cell_height), dimensions);
    let after_sync = first.shape_row(&cells, &attrs).remove(0);
    assert_eq!(after_sync.font_index, 0);
    assert!(first.font_is_bold(after_sync.font_index));
    assert!(!first.sync_loaded_fonts());
    assert_eq!(second.font_generation(), generation);
    assert!(!second.font_is_bold(0));
    assert_eq!(
        second.shape_row(&cells, &attrs)[0].font_index,
        original.font_index
    );
}

#[test]
fn fork_shares_font_loading_but_installs_at_independent_boundaries() {
    let mut first = FontSystem::new(None, 18.0, 1);
    let pending = Arc::new(std::sync::OnceLock::new());
    first.pending_fonts = Some(pending.clone());
    let mut second = first.fork();
    assert!(Arc::ptr_eq(
        first.pending_fonts.as_ref().unwrap(),
        second.pending_fonts.as_ref().unwrap()
    ));
    assert!(Arc::ptr_eq(&first.final_font, &second.final_font));
    let loaded = Arc::new(loader::FontSet::default());
    assert!(pending.set(loaded.clone()).is_ok());
    assert!(first.sync_loaded_fonts());
    assert_ne!(first.font_generation(), second.font_generation());
    assert!(second.sync_loaded_fonts());
    assert_eq!(first.font_generation(), second.font_generation());
    assert!(Arc::ptr_eq(&first.fonts, &second.fonts));
    assert!(Arc::ptr_eq(&loaded, &second.fonts));

    let original_metrics = (first.cell_width, first.cell_height);
    second.set_scale_factor(2.0);
    assert_eq!((first.cell_width, first.cell_height), original_metrics);
    assert_ne!((second.cell_width, second.cell_height), original_metrics);
}

#[test]
fn drcs_contexts_are_isolated_between_calls() {
    let mut fonts = FontSystem::new(None, 18.0, 1);
    let geometry = DrcsGeometryClass::Col80Line24;
    let glyph_map = |pixels| {
        Arc::new(HashMap::from([(
            (geometry, 0),
            DrcsGlyphDef {
                glyph_id: 0,
                width: 2,
                height: 2,
                pixels,
                full_cell: true,
            },
        )]))
    };
    let first = glyph_map(vec![255, 0, 0, 0]);
    let second = glyph_map(vec![0, 0, 0, 255]);
    let cells = [SmolStr::new(encode_drcs_char(0).unwrap().to_string())];
    let attrs = [CellAttrs::default()];
    let render = |fonts: &mut FontSystem, glyphs| {
        let context = Some(DrcsContext { geometry, glyphs });
        let glyph = fonts.shape_row_with_drcs(&cells, &attrs, context).remove(0);
        fonts
            .rasterize_glyph_with_drcs(glyph.font_index, glyph.glyph_id, 1, context)
            .bitmap
    };
    let first_pixels = render(&mut fonts, &first);
    let second_pixels = render(&mut fonts, &second);
    assert_ne!(first_pixels, second_pixels);
    assert_eq!(render(&mut fonts, &first), first_pixels);
    assert!(
        fonts
            .rasterize_glyph_with_drcs(drcs::FONT_INDEX, 0, 1, None)
            .bitmap
            .is_empty()
    );
}

#[test]
fn glyph_cells_wide_uses_consumed_columns_and_continuations() {
    let trailing = vec![
        SmolStr::new("👩\u{200D}💻"),
        SmolStr::default(),
        SmolStr::new(" "),
    ];
    assert_eq!(glyph_cells_wide(&trailing, 0, 0), 2);

    let followed = vec![
        SmolStr::new("👩\u{200D}💻"),
        SmolStr::default(),
        SmolStr::new("X"),
    ];
    assert_eq!(glyph_cells_wide(&followed, 0, 0), 2);

    let ligature = vec![SmolStr::new("🇺"), SmolStr::new("🇸"), SmolStr::new(" ")];
    assert_eq!(glyph_cells_wide(&ligature, 0, 1), 2);
}

#[test]
fn nerd_font_private_use_symbols_stay_on_text_fallback_path() {
    assert!(!cluster_prefers_color("\u{e0b0}"));
    assert!(!cluster_prefers_color("\u{f000}"));
    assert!(!cluster_prefers_color("\u{f0001}"));
    assert!(cluster_prefers_nerd_symbol("\u{e0b0}"));
    assert!(cluster_prefers_nerd_symbol("\u{f000}"));
    assert!(cluster_prefers_nerd_symbol("\u{f0001}"));
    assert!(!cluster_prefers_nerd_symbol("H"));
}

#[test]
fn color_preference_uses_emoji_presentation_property() {
    assert!(cluster_prefers_color("😀"));
    assert!(!cluster_prefers_color("♻"));
    assert!(cluster_prefers_color("♻\u{FE0F}"));
}

#[test]
fn private_use_cells_prefer_symbol_font_before_primary_text_font() {
    let families = [FamilyVariants {
        regular: 1,
        bold: Some(2),
        italic: None,
        bold_italic: None,
    }];

    assert_eq!(
        preferred_font_for_cell(&families, 99, CellAttrs::default(), false, true, Some(8),),
        Some(8)
    );
    assert_eq!(
        preferred_font_for_cell(&families, 99, CellAttrs::default(), false, true, None),
        Some(1)
    );
    assert_eq!(
        preferred_font_for_cell(&families, 99, CellAttrs::default(), true, true, Some(8)),
        None
    );
}

#[test]
fn aggregate_cell_metrics_keeps_largest_width_and_vertical_extents() {
    let metrics = aggregate_cell_metrics([
        ScaledCellMetrics {
            cell_width: 8,
            cell_height: 17,
            ascent: 12.0,
        },
        ScaledCellMetrics {
            cell_width: 11,
            cell_height: 16,
            ascent: 9.0,
        },
        ScaledCellMetrics {
            cell_width: 9,
            cell_height: 19,
            ascent: 14.0,
        },
    ]);

    assert_eq!(metrics.cell_width, 11);
    assert_eq!(metrics.cell_height, 21);
    assert_eq!(metrics.ascent, 14.0);
}

#[test]
fn cell_centering_y_offset_moves_high_symbol_down() {
    let offset = cell_centering_y_offset(20.0, 14.0, -3.0, 18.0);

    assert_eq!(offset, -4.0);
}

#[test]
fn nerd_symbol_family_priority_prefers_mono_symbol_face() {
    let plain = face_info(
        &[NERD_SYMBOL_FAMILY],
        fontdb::Weight::NORMAL,
        fontdb::Style::Normal,
    );
    let mono = face_info(
        &[NERD_SYMBOL_FAMILY_MONO],
        fontdb::Weight::NORMAL,
        fontdb::Style::Normal,
    );
    let patched = face_info(
        &["JetBrainsMono Nerd Font"],
        fontdb::Weight::NORMAL,
        fontdb::Style::Normal,
    );

    assert_eq!(nerd_symbol_family_priority(&plain), Some(1));
    assert_eq!(nerd_symbol_family_priority(&mono), Some(2));
    assert_eq!(nerd_symbol_family_priority(&patched), Some(1));
}

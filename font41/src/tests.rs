use super::*;

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

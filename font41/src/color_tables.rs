use read_fonts::TableProvider;

#[derive(Clone, Copy, Default)]
pub(super) struct ColorTables {
    colr: bool,
    svg: bool,
    sbix: bool,
    cbdt: bool,
}

impl ColorTables {
    pub(super) fn any(self) -> bool {
        self.colr || self.svg || self.sbix || self.cbdt
    }

    fn score(self) -> u8 {
        // Prefer scalable color outlines when duplicate faces share the same
        // family/style/weight. This keeps a locally installed COLR/SVG Noto
        // Color Emoji ahead of distro bitmap-only CBDT builds.
        (self.colr as u8) * 8 + (self.svg as u8) * 4 + (self.sbix as u8) * 2 + self.cbdt as u8
    }
}

pub(super) fn color_tables(rf: &read_fonts::FontRef<'_>) -> ColorTables {
    ColorTables {
        colr: rf.colr().is_ok(),
        svg: rf.svg().is_ok(),
        sbix: rf.sbix().is_ok(),
        cbdt: rf.cbdt().is_ok(),
    }
}

pub(super) fn color_table_score(
    data: &[u8],
    face_index: u32,
) -> u8 {
    read_fonts::FontRef::from_index(data, face_index)
        .map(|rf| color_tables(&rf).score())
        .unwrap_or_default()
}

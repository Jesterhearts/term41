use std::collections::HashSet;
use std::sync::Arc;

use log::debug;
use log::info;

use crate::attrs::CellAttrs;
use crate::charmap::charmap_lookup;
use crate::color_tables::color_table_score;
use crate::loader;
use crate::loader::LazyFontFace;

pub(crate) const NERD_SYMBOL_FAMILY: &str = "nerd font";
pub(crate) const NERD_SYMBOL_FAMILY_MONO: &str = "nerd font mono";
const NERD_SYMBOL_SAMPLE: &[char] = &[
    '\u{e0a0}',  // Powerline branch.
    '\u{e0b0}',  // Powerline separator.
    '\u{e200}',  // Font Awesome Extension.
    '\u{e300}',  // Weather icons.
    '\u{e5fa}',  // Seti UI + custom icons.
    '\u{e700}',  // Devicons.
    '\u{f000}',  // Font Awesome.
    '\u{f400}',  // Octicons.
    '\u{f0001}', // Material Design Icons.
];

/// The set of weight/style variants loaded for one user-requested family.
/// `regular` is always present -- if the regular weight/style can't be found,
/// the family is dropped from the list outright. The other three variants
/// are optional; a missing variant means the renderer falls back to the
/// closest available face and synthesizes the missing style when possible.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FamilyVariants {
    pub(crate) regular: usize,
    pub(crate) bold: Option<usize>,
    pub(crate) italic: Option<usize>,
    pub(crate) bold_italic: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct VariantStyle {
    pub(crate) is_bold: bool,
    pub(crate) is_italic: bool,
}

/// Look up a face in `db` that matches `family` with *exactly* the requested
/// weight and style. fontdb's `query()` will fuzzy-match (a BOLD query falls
/// back to a NORMAL face when no bold is available); we reject those fuzzy
/// hits so a missing variant stays missing and the caller can record it as
/// `None` in the family table.
pub(crate) fn load_family_variant(
    db: &fontdb::Database,
    fonts: &mut Vec<Arc<LazyFontFace>>,
    loaded_face_ids: &mut HashSet<fontdb::ID>,
    family: &fontdb::Family<'_>,
    weight: fontdb::Weight,
    style: fontdb::Style,
    variant_style: VariantStyle,
) -> Option<usize> {
    if let fontdb::Family::Name(name) = family {
        let (id, is_color_hint, is_nerd_symbol_hint) =
            pick_named_family_face(db, name, weight, style)?;
        let font_face = loader::lazy_font_face(
            db,
            id,
            is_color_hint,
            is_nerd_symbol_hint,
            variant_style.is_bold,
            variant_style.is_italic,
        )?;
        let idx = fonts.len();
        fonts.push(font_face);
        loaded_face_ids.insert(id);
        return Some(idx);
    }

    let query = fontdb::Query {
        families: std::slice::from_ref(family),
        weight,
        stretch: fontdb::Stretch::Normal,
        style,
    };
    let id = db.query(&query)?;
    let face = db.face(id)?;
    if face.weight != weight || face.style != style {
        return None;
    }
    let is_color_hint = db.with_face_data(id, color_table_score).unwrap_or_default() > 0;
    let is_nerd_symbol_hint = nerd_symbol_family_priority(face).is_some();
    let font_face = loader::lazy_font_face(
        db,
        id,
        is_color_hint,
        is_nerd_symbol_hint,
        variant_style.is_bold,
        variant_style.is_italic,
    )?;
    let idx = fonts.len();
    fonts.push(font_face);
    loaded_face_ids.insert(id);
    Some(idx)
}

pub(crate) fn append_nerd_symbol_fallback(
    db: &fontdb::Database,
    fonts: &mut Vec<Arc<LazyFontFace>>,
    loaded_face_ids: &mut HashSet<fontdb::ID>,
) {
    let Some((id, family_name)) = pick_nerd_symbol_face(db, loaded_face_ids) else {
        return;
    };
    let is_color_hint = db.with_face_data(id, color_table_score).unwrap_or_default() > 0;
    let Some(font_face) = loader::lazy_font_face(db, id, is_color_hint, true, false, false) else {
        return;
    };

    fonts.push(font_face);
    loaded_face_ids.insert(id);
    info!("loaded Nerd Font symbol fallback: {family_name}");
}

fn pick_nerd_symbol_face(
    db: &fontdb::Database,
    loaded_face_ids: &HashSet<fontdb::ID>,
) -> Option<(fontdb::ID, String)> {
    db.faces()
        .filter(|face| !loaded_face_ids.contains(&face.id))
        .filter_map(|face| {
            let family_priority = nerd_symbol_family_priority(face)?;
            let glyph_score = db
                .with_face_data(face.id, nerd_symbol_coverage_score)
                .unwrap_or_default();
            if glyph_score == 0 {
                return None;
            }

            let regular_style = face.weight == fontdb::Weight::NORMAL
                && face.style == fontdb::Style::Normal
                && face.stretch == fontdb::Stretch::Normal;
            let rank = (glyph_score, family_priority, regular_style, face.monospaced);
            let family_name = face
                .families
                .iter()
                .find_map(|(name, _)| is_nerd_symbol_family(name).then(|| name.clone()))
                .unwrap_or_else(|| face.post_script_name.clone());
            Some((rank, face.id, family_name))
        })
        .max_by_key(|(rank, _, _)| *rank)
        .map(|(_, id, family_name)| (id, family_name))
}

pub(crate) fn nerd_symbol_family_priority(face: &fontdb::FaceInfo) -> Option<u8> {
    face.families
        .iter()
        .filter_map(|(name, _)| {
            debug!("checking family {name} for Nerd Font symbol fallback");
            let name = name.to_lowercase();
            if name.contains(NERD_SYMBOL_FAMILY_MONO) {
                Some(2)
            } else if name.contains(NERD_SYMBOL_FAMILY) {
                Some(1)
            } else {
                None
            }
        })
        .max()
}

fn is_nerd_symbol_family(name: &str) -> bool {
    name.to_lowercase().contains(NERD_SYMBOL_FAMILY)
}

fn nerd_symbol_coverage_score(
    data: &[u8],
    face_index: u32,
) -> u8 {
    let Ok(font) = read_fonts::FontRef::from_index(data, face_index) else {
        return 0;
    };
    NERD_SYMBOL_SAMPLE
        .iter()
        .filter(|&&ch| charmap_lookup(&font, ch) != 0)
        .count()
        .try_into()
        .unwrap_or(u8::MAX)
}

fn pick_named_family_face(
    db: &fontdb::Database,
    name: &str,
    weight: fontdb::Weight,
    style: fontdb::Style,
) -> Option<(fontdb::ID, bool, bool)> {
    db.faces()
        .filter(|face| {
            face.weight == weight
                && face.style == style
                && face.stretch == fontdb::Stretch::Normal
                && face.families.iter().any(|family| family.0 == name)
        })
        .max_by_key(|face| {
            db.with_face_data(face.id, color_table_score)
                .unwrap_or_default()
        })
        .map(|face| {
            let is_color_hint = db
                .with_face_data(face.id, color_table_score)
                .unwrap_or_default()
                > 0;
            let is_nerd_symbol_hint = nerd_symbol_family_priority(face).is_some();
            (face.id, is_color_hint, is_nerd_symbol_hint)
        })
}

/// Pick the face that should shape a cell with the given attributes, walking
/// `families` in user-declared order. Returns `(font_index, synth_bold,
/// synth_italic)` -- synth flags are set when the chosen face doesn't natively
/// cover the requested style and the renderer should fake it on top.
///
/// Degradation prefers preserving the rarer style: for BOLD|ITALIC with only
/// a bold face available, keep bold and synthesize italic via vertex shear;
/// with only an italic face, keep italic and synthesize bold (COLR only).
pub(crate) fn pick_variant(
    families: &[FamilyVariants],
    final_fallback: usize,
    want_bold: bool,
    want_italic: bool,
) -> (usize, bool, bool) {
    let final_fallback = FamilyVariants {
        regular: final_fallback,
        bold: None,
        italic: None,
        bold_italic: None,
    };
    let family = families.first().unwrap_or(&final_fallback);
    let exact = match (want_bold, want_italic) {
        (false, false) => Some(family.regular),
        (true, false) => family.bold,
        (false, true) => family.italic,
        (true, true) => family.bold_italic,
    };
    if let Some(idx) = exact {
        return (idx, false, false);
    }
    match (want_bold, want_italic) {
        (true, true) => {
            if let Some(idx) = family.bold {
                return (idx, false, true);
            }
            if let Some(idx) = family.italic {
                return (idx, true, false);
            }
            (family.regular, true, true)
        }
        (true, false) => (family.regular, true, false),
        (false, true) => (family.regular, false, true),
        (false, false) => (family.regular, false, false),
    }
}

pub(crate) fn preferred_font_for_cell(
    families: &[FamilyVariants],
    final_font_index: usize,
    attrs: CellAttrs,
    wants_color: bool,
    wants_nerd_symbol: bool,
    nerd_symbol_font_index: Option<usize>,
) -> Option<usize> {
    if wants_color {
        return None;
    }

    if wants_nerd_symbol && let Some(font_index) = nerd_symbol_font_index {
        return Some(font_index);
    }

    let (font_index, _, _) = pick_variant(
        families,
        final_font_index,
        attrs.contains(CellAttrs::BOLD),
        attrs.contains(CellAttrs::ITALIC),
    );
    Some(font_index)
}

pub(crate) fn cluster_prefers_nerd_symbol(cell: &str) -> bool {
    cell.chars()
        .any(|ch| is_private_use_codepoint(ch) && ch != '\u{FE0F}')
}

fn is_private_use_codepoint(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

/// Heuristic: true when a cluster is likely meant to render as a colour
/// emoji. Covers the two common routes: explicit `VS16` selector, and
/// default-emoji-presentation codepoints in the main emoji blocks. Keeps
/// CJK and ordinary symbols (which `unicode-width` also reports as wide)
/// out of the colour path.
pub(crate) fn cluster_prefers_color(cell: &str) -> bool {
    if cell.ends_with('\u{FE0F}') {
        return true;
    }
    cell.chars().any(is_default_emoji_codepoint)
}

fn is_default_emoji_codepoint(c: char) -> bool {
    let cp = c as u32;
    matches!(
        cp,
        0x1F300..=0x1F5FF
            | 0x1F600..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F700..=0x1F77F
            | 0x1F780..=0x1F7FF
            | 0x1F800..=0x1F8FF
            | 0x1F900..=0x1F9FF
            | 0x1FA00..=0x1FA6F
            | 0x1FA70..=0x1FAFF
            | 0x2600..=0x26FF
            | 0x2700..=0x27BF
            | 0x1F000..=0x1F0FF
            | 0x1F100..=0x1F1FF
            | 0x1F200..=0x1F2FF
    )
}

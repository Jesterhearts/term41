use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::thread;

use harfrust::FontRef;
use harfrust::ShaperData;
use log::info;
use log::warn;
use read_fonts::TableProvider;

use crate::color_tables::color_tables;
use crate::families;
use crate::families::FamilyVariants;
use crate::metrics::FontEmMetrics;
use crate::metrics::font_em_metrics;

pub(crate) type SharedFontData = Arc<dyn AsRef<[u8]> + Send + Sync>;

/// A loaded font with its shaping data and raw bytes.
pub(crate) struct LoadedFont {
    pub(crate) data: SharedFontData,
    pub(crate) face_index: u32,
    pub(crate) shaper_data: ShaperData,
    pub(crate) units_per_em: f32,
    pub(crate) metrics: FontEmMetrics,
    /// True if the font carries colour glyph tables (COLR, CBDT, sbix, or
    /// SVG). Used by shape_row to prefer colour fonts for emoji clusters
    /// over text fonts that might also have a monochrome outline for the
    /// same codepoint.
    pub(crate) is_color: bool,
    /// True when the face was loaded as a bold weight variant (fontdb
    /// weight >= 600). Combined with `is_italic` at render time to decide
    /// whether a cell's BOLD attribute still needs synthesis on top.
    pub(crate) is_bold: bool,
    /// True when the face was loaded as an italic/oblique variant.
    pub(crate) is_italic: bool,
}

pub(crate) fn font_bytes(data: &SharedFontData) -> &[u8] {
    data.as_ref().as_ref()
}

pub(crate) fn loaded_font_ref(loaded: &LoadedFont) -> Result<FontRef<'_>, read_fonts::ReadError> {
    FontRef::from_index(font_bytes(&loaded.data), loaded.face_index)
}

/// A configured font face whose bytes are loaded only when a row needs it.
/// The descriptor is cheap to keep resident: it stores only the selected
/// source, face index, and style/color hints, while the expensive file
/// byte buffer lives behind `loaded`.
pub(crate) struct LazyFontFace {
    source: fontdb::Source,
    face_index: u32,
    pub(crate) is_color_hint: bool,
    pub(crate) is_nerd_symbol_hint: bool,
    pub(crate) is_bold: bool,
    pub(crate) is_italic: bool,
    loaded: Mutex<Option<Arc<LoadedFont>>>,
}

impl LazyFontFace {
    pub(crate) fn new(
        source: fontdb::Source,
        face_index: u32,
        is_color_hint: bool,
        is_nerd_symbol_hint: bool,
        is_bold: bool,
        is_italic: bool,
    ) -> Self {
        Self {
            source,
            face_index,
            is_color_hint,
            is_nerd_symbol_hint,
            is_bold,
            is_italic,
            loaded: Mutex::new(None),
        }
    }

    pub(crate) fn load(&self) -> Option<Arc<LoadedFont>> {
        let mut loaded = self.loaded.lock().unwrap();
        if let Some(font) = loaded.as_ref() {
            return Some(font.clone());
        }

        let data = shared_font_data_from_source(&self.source)?;
        let font = load_font(data, self.face_index, self.is_bold, self.is_italic)?;
        let font = Arc::new(font);
        *loaded = Some(font.clone());
        Some(font)
    }

    pub(crate) fn is_color(&self) -> bool {
        self.loaded
            .lock()
            .unwrap()
            .as_ref()
            .map(|font| font.is_color)
            .unwrap_or(self.is_color_hint)
    }
}

/// Faces and their family indices are published and retained as one snapshot.
#[derive(Default)]
pub(crate) struct FontSet {
    pub(crate) faces: Vec<Arc<LazyFontFace>>,
    pub(crate) families: Vec<FamilyVariants>,
}

pub(crate) type PendingFonts = Arc<OnceLock<Arc<FontSet>>>;

pub(crate) fn load_fonts(fonts_config: Option<String>) -> Arc<FontSet> {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    let (faces, families) = load_fonts_from_database(&db, fonts_config.as_deref());
    Arc::new(FontSet { faces, families })
}

fn load_fonts_from_database(
    db: &fontdb::Database,
    fonts_config: Option<&str>,
) -> (Vec<Arc<LazyFontFace>>, Vec<FamilyVariants>) {
    let mut fonts: Vec<Arc<LazyFontFace>> = Vec::new();
    let mut family_variants: Vec<FamilyVariants> = Vec::new();
    let mut loaded_face_ids: HashSet<fontdb::ID> = HashSet::new();

    if let Some(families_str) = fonts_config {
        for family_name in families_str.split(',').map(|s| s.trim()) {
            if family_name.is_empty() {
                continue;
            }

            let family = match family_name.to_lowercase().as_str() {
                "monospace" => fontdb::Family::Monospace,
                "serif" => fontdb::Family::Serif,
                "sans-serif" | "sans serif" => fontdb::Family::SansSerif,
                _ => fontdb::Family::Name(family_name),
            };

            let Some(regular) = families::load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::NORMAL,
                fontdb::Style::Normal,
                families::VariantStyle {
                    is_bold: false,
                    is_italic: false,
                },
            ) else {
                warn!("font not found: {family_name}");
                continue;
            };
            let bold = families::load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::BOLD,
                fontdb::Style::Normal,
                families::VariantStyle {
                    is_bold: true,
                    is_italic: false,
                },
            );
            let italic = families::load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::NORMAL,
                fontdb::Style::Italic,
                families::VariantStyle {
                    is_bold: false,
                    is_italic: true,
                },
            );
            let bold_italic = families::load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::BOLD,
                fontdb::Style::Italic,
                families::VariantStyle {
                    is_bold: true,
                    is_italic: true,
                },
            );
            info!(
                "loaded font family: {family_name} (bold={} italic={} bold_italic={})",
                bold.is_some(),
                italic.is_some(),
                bold_italic.is_some()
            );
            family_variants.push(FamilyVariants {
                regular,
                bold,
                italic,
                bold_italic,
            });
        }
    }

    families::append_nerd_symbol_fallback(db, &mut fonts, &mut loaded_face_ids);

    (fonts, family_variants)
}

pub(crate) fn start_background_font_load(fonts_config: Option<String>) -> PendingFonts {
    let pending = Arc::new(OnceLock::new());
    let output = pending.clone();
    let spawn_result = thread::Builder::new()
        .name("font-loader".into())
        .spawn(move || {
            let _ = output.set(load_fonts(fonts_config));
        });
    if let Err(err) = spawn_result {
        warn!("failed to start font-loader thread: {err}");
    }
    pending
}

pub(crate) enum LoadedFontCandidate<'a> {
    Lazy(Arc<LoadedFont>),
    Final(&'a LoadedFont),
}

impl<'a> LoadedFontCandidate<'a> {
    pub(crate) fn as_loaded(&'a self) -> &'a LoadedFont {
        match self {
            Self::Lazy(font) => font,
            Self::Final(font) => font,
        }
    }
}

pub(crate) fn load_font_candidate<'a>(
    font_idx: usize,
    font_faces: &[Arc<LazyFontFace>],
    final_font: &'a LoadedFont,
) -> Option<LoadedFontCandidate<'a>> {
    if let Some(font_face) = font_faces.get(font_idx) {
        return font_face.load().map(LoadedFontCandidate::Lazy);
    }

    Some(LoadedFontCandidate::Final(final_font))
}

pub(crate) fn load_font(
    data: SharedFontData,
    face_index: u32,
    is_bold: bool,
    is_italic: bool,
) -> Option<LoadedFont> {
    let bytes = font_bytes(&data);
    let font_ref = FontRef::from_index(bytes, face_index).ok()?;
    let shaper_data = ShaperData::new(&font_ref);
    let rf = read_fonts::FontRef::from_index(bytes, face_index).ok()?;
    let head = rf.head().ok()?;
    let units_per_em = head.units_per_em() as f32;
    let metrics = font_em_metrics(&rf, head);
    let is_color = color_tables(&rf).any();

    Some(LoadedFont {
        data,
        face_index,
        shaper_data,
        units_per_em,
        metrics,
        is_color,
        is_bold,
        is_italic,
    })
}

fn shared_font_data_from_source(source: &fontdb::Source) -> Option<SharedFontData> {
    match source {
        fontdb::Source::Binary(data) => Some(data.clone()),
        fontdb::Source::File(path) => {
            // Own the bytes so font updates cannot mutate a live shaping
            // snapshot.
            let data = std::fs::read(path).ok()?;
            Some(Arc::new(data))
        }
        fontdb::Source::SharedFile(path, _) => {
            let data = std::fs::read(path).ok()?;
            Some(Arc::new(data))
        }
    }
}

pub(crate) fn lazy_font_face(
    db: &fontdb::Database,
    id: fontdb::ID,
    is_color_hint: bool,
    is_nerd_symbol_hint: bool,
    is_bold: bool,
    is_italic: bool,
) -> Option<Arc<LazyFontFace>> {
    let (source, face_index) = db.face_source(id)?;
    Some(Arc::new(LazyFontFace::new(
        source,
        face_index,
        is_color_hint,
        is_nerd_symbol_hint,
        is_bold,
        is_italic,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loaded_font_bytes_survive_file_truncation_and_removal() {
        let path =
            std::env::temp_dir().join(format!("term41-owned-font-{}.ttf", std::process::id()));
        std::fs::write(&path, crate::FAIRFAX_HD).unwrap();
        let face = LazyFontFace::new(
            fontdb::Source::File(path.clone()),
            0,
            false,
            false,
            false,
            false,
        );
        let loaded = face.load().unwrap();
        std::fs::write(&path, []).unwrap();
        assert_eq!(font_bytes(&loaded.data), crate::FAIRFAX_HD);
        assert!(loaded_font_ref(&loaded).is_ok());
        std::fs::remove_file(&path).unwrap();
        assert!(Arc::ptr_eq(&loaded, &face.load().unwrap()));
        assert_eq!(font_bytes(&loaded.data), crate::FAIRFAX_HD);
    }
}

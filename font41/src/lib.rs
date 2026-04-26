#![allow(clippy::too_many_arguments)]

//! Font discovery, shaping, fallback, and glyph rasterization for `term41`.
//!
//! The renderer asks this crate to shape terminal rows into positioned glyphs
//! and to rasterize individual glyph ids into RGBA bitmaps suitable for the
//! GPU atlas. Font fallback is ordered by user configuration, with embedded
//! Fairfax HD as the final fallback.

/// Per-cell text attribute flags.
pub mod attrs;
mod bitmap;
mod colr;
mod drcs;
mod legacy;
mod svg;

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::thread;

use harfrust::Direction;
use harfrust::FontRef;
use harfrust::Script;
use harfrust::ShapePlan;
use harfrust::ShaperData;
use harfrust::UnicodeBuffer;
use raqote::DrawOptions;
use raqote::DrawTarget;
use raqote::PathBuilder;
use raqote::SolidSource;
use raqote::Source;
use read_fonts::TableProvider;
use read_fonts::tables::glyf::Anchor;
use read_fonts::tables::glyf::CompositeGlyph;
use read_fonts::tables::glyf::CurvePoint;
use read_fonts::tables::glyf::Glyph;
use read_fonts::tables::glyf::SimpleGlyph;
use read_fonts::tables::loca::Loca;
use read_fonts::types::GlyphId;
use smol_str::SmolStr;
use unicode_properties::GeneralCategory;
use unicode_properties::UnicodeEmoji;
use unicode_properties::UnicodeGeneralCategory;

use crate::attrs::CellAttrs;

#[macro_use]
extern crate log;

/// The embedded Fairfax HD font (ultimate fallback).
static FAIRFAX_HD: &[u8] = include_bytes!("../resources/fonts/FairfaxHD.ttf");

/// Rasterized glyph data ready for upload to a texture atlas. The bitmap is
/// always RGBA8 (4 bytes per pixel). Outline glyphs encode their coverage
/// into the alpha channel with `rgb = 0`; color glyphs (COLR, emoji bitmaps)
/// encode full colour and set `is_color = true` so the shader samples the
/// atlas directly instead of tinting by the fg colour.
#[derive(Debug, Clone)]
pub struct RasterizedGlyph {
    /// Row-major RGBA8 bitmap.
    pub bitmap: Vec<u8>,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal bearing from the cell origin to the bitmap origin.
    pub bearing_x: i32,
    /// Vertical bearing from the baseline to the bitmap origin.
    pub bearing_y: i32,
    /// Whether RGB channels carry color and should not be foreground-tinted.
    pub is_color: bool,
}

pub use self::drcs::GLYPHS_PER_SET as DRCS_GLYPHS_PER_SET;
pub use self::drcs::GeometryClass as DrcsGeometryClass;
pub use self::drcs::GeometryGuard as DrcsGeometryGuard;
pub use self::drcs::GlyphDef as DrcsGlyphDef;
pub use self::drcs::GlyphMap as DrcsGlyphMap;
pub use self::drcs::encode_char as encode_drcs_char;
pub use self::drcs::set_context as set_drcs_context;

/// A shaped glyph with its position info, ready for rendering.
pub struct ShapedGlyph {
    /// Font-specific glyph id.
    pub glyph_id: u16,
    /// Index into the loaded font list, or a sentinel for synthetic fonts.
    pub font_index: usize,
    /// Terminal column where this glyph's cluster starts.
    pub col: u16,
    /// Number of terminal cells this glyph occupies horizontally — derived
    /// from the column gap to the next shaped glyph (or the row end). For a
    /// ZWJ ligature the font collapses into a single glyph, this counts all
    /// the cells the ligated cluster claims, so colour rasterisers can scale
    /// the emoji to fit its visual footprint instead of squashing it into a
    /// single cell.
    pub cells_wide: u8,
    /// HarfBuzz horizontal positioning adjustment, in pixels.
    pub x_offset: f32,
    /// HarfBuzz vertical positioning adjustment, in pixels.
    pub y_offset: f32,
}

/// A loaded font with its shaping data and raw bytes.
struct LoadedFont {
    data: SharedFontData,
    face_index: u32,
    shaper_data: ShaperData,
    units_per_em: f32,
    /// True if the font carries colour glyph tables (COLR, CBDT, sbix, or
    /// SVG). Used by shape_row to prefer colour fonts for emoji clusters
    /// over text fonts that might also have a monochrome outline for the
    /// same codepoint.
    is_color: bool,
    /// True when the face was loaded as a bold weight variant (fontdb
    /// weight ≥ 600). Combined with `is_italic` at render time to decide
    /// whether a cell's BOLD attribute still needs synthesis on top.
    is_bold: bool,
    /// True when the face was loaded as an italic/oblique variant.
    is_italic: bool,
}

type SharedFontData = Arc<dyn AsRef<[u8]> + Send + Sync>;

fn font_bytes(data: &SharedFontData) -> &[u8] {
    data.as_ref().as_ref()
}

fn loaded_font_ref(loaded: &LoadedFont) -> Result<FontRef<'_>, read_fonts::ReadError> {
    FontRef::from_index(font_bytes(&loaded.data), loaded.face_index)
}

/// A configured font face whose bytes are mapped only when a row needs it.
/// The descriptor is cheap to keep resident: it stores only the selected
/// source, face index, and style/color hints, while the expensive file
/// mapping lives behind `loaded`.
struct LazyFontFace {
    source: fontdb::Source,
    face_index: u32,
    is_color_hint: bool,
    is_nerd_symbol_hint: bool,
    is_bold: bool,
    is_italic: bool,
    loaded: Mutex<Option<Arc<LoadedFont>>>,
}

impl LazyFontFace {
    fn new(
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

    fn load(&self) -> Option<Arc<LoadedFont>> {
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

    fn is_color(&self) -> bool {
        self.loaded
            .lock()
            .unwrap()
            .as_ref()
            .map(|font| font.is_color)
            .unwrap_or(self.is_color_hint)
    }
}

/// The set of weight/style variants loaded for one user-requested family.
/// `regular` is always present — if the regular weight/style can't be found,
/// the family is dropped from the list outright. The other three variants
/// are optional; a missing variant means the renderer falls back to the
/// closest available face and synthesizes the missing style when possible.
#[derive(Debug, Clone, Copy)]
struct FamilyVariants {
    regular: usize,
    bold: Option<usize>,
    italic: Option<usize>,
    bold_italic: Option<usize>,
}

/// Key for the ShapePlan cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct PlanKey {
    font_index: usize,
    direction: Direction,
    script: Script,
}

static FONTS: RwLock<Vec<Arc<LazyFontFace>>> = RwLock::new(Vec::new());
static FAMILIES: RwLock<Vec<FamilyVariants>> = RwLock::new(Vec::new());
static FONT_LOAD_STATE: Mutex<FontLoadState> = Mutex::new(FontLoadState::NotStarted);
static FONT_LOAD_EPOCH: AtomicU64 = AtomicU64::new(0);
static INSTALLED_FONT_GENERATION: AtomicU64 = AtomicU64::new(0);
const NERD_SYMBOL_FAMILY: &str = " nerd font";
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontLoadState {
    NotStarted,
    Loading,
    Loaded,
}

fn load_fonts(fonts_config: Option<String>) -> (Vec<Arc<LazyFontFace>>, Vec<FamilyVariants>) {
    let mut db = fontdb::Database::new();
    db.load_system_fonts();
    load_fonts_from_database(&db, fonts_config.as_deref())
}

fn load_fonts_from_database(
    db: &fontdb::Database,
    fonts_config: Option<&str>,
) -> (Vec<Arc<LazyFontFace>>, Vec<FamilyVariants>) {
    let mut fonts: Vec<Arc<LazyFontFace>> = Vec::new();
    let mut families: Vec<FamilyVariants> = Vec::new();
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

            let Some(regular) = load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::NORMAL,
                fontdb::Style::Normal,
                false,
                false,
            ) else {
                warn!("font not found: {family_name}");
                continue;
            };
            let bold = load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::BOLD,
                fontdb::Style::Normal,
                true,
                false,
            );
            let italic = load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::NORMAL,
                fontdb::Style::Italic,
                false,
                true,
            );
            let bold_italic = load_family_variant(
                db,
                &mut fonts,
                &mut loaded_face_ids,
                &family,
                fontdb::Weight::BOLD,
                fontdb::Style::Italic,
                true,
                true,
            );
            info!(
                "loaded font family: {family_name} (bold={} italic={} bold_italic={})",
                bold.is_some(),
                italic.is_some(),
                bold_italic.is_some()
            );
            families.push(FamilyVariants {
                regular,
                bold,
                italic,
                bold_italic,
            });
        }
    }

    append_nerd_symbol_fallback(db, &mut fonts, &mut loaded_face_ids);

    (fonts, families)
}

fn install_fonts(
    fonts: Vec<Arc<LazyFontFace>>,
    families: Vec<FamilyVariants>,
) {
    *FONTS.write().unwrap() = fonts;
    *FAMILIES.write().unwrap() = families;
    INSTALLED_FONT_GENERATION.fetch_add(1, Ordering::AcqRel);
}

fn start_background_font_load(fonts_config: Option<String>) {
    let mut state = FONT_LOAD_STATE.lock().unwrap();
    if *state != FontLoadState::NotStarted {
        return;
    }

    *state = FontLoadState::Loading;
    let epoch = FONT_LOAD_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    let spawn_result = thread::Builder::new()
        .name("font-loader".into())
        .spawn(move || finish_background_font_load(epoch, fonts_config));
    if let Err(err) = spawn_result {
        warn!("failed to start font-loader thread: {err}");
        *state = FontLoadState::NotStarted;
    }
}

fn finish_background_font_load(
    epoch: u64,
    fonts_config: Option<String>,
) {
    let (fonts, families) = load_fonts(fonts_config);
    if FONT_LOAD_EPOCH.load(Ordering::Acquire) == epoch {
        install_fonts(fonts, families);
        *FONT_LOAD_STATE.lock().unwrap() = FontLoadState::Loaded;
    }
}

fn reload_fonts(fonts_config: Option<String>) {
    let epoch = FONT_LOAD_EPOCH.fetch_add(1, Ordering::AcqRel) + 1;
    let (fonts, families) = load_fonts(fonts_config);
    if FONT_LOAD_EPOCH.load(Ordering::Acquire) == epoch {
        install_fonts(fonts, families);
        *FONT_LOAD_STATE.lock().unwrap() = FontLoadState::Loaded;
    }
}

/// Font system: manages an ordered list of fonts with fallback and plan
/// caching. Families are kept in user-declared order — the first entry is
/// the primary text face; later entries provide fallback glyphs for cells
/// the primary can't cover. Each family carries up to four (weight, style)
/// variants; missing variants degrade to `regular` with synthesis at
/// render time.
pub struct FontSystem {
    final_font: LoadedFont,

    plan_cache: HashMap<PlanKey, ShapePlan>,
    font_generation: u64,
    /// Current terminal cell width in physical pixels.
    pub cell_width: u32,
    /// Current terminal cell height in physical pixels.
    pub cell_height: u32,
    /// Supersampling factor used while rasterizing outline glyphs.
    pub supersample: u32,
    /// Effective font size after DPI scaling.
    pub font_size: f32,
    ascent: f32,
    /// The user-configured font size before DPI scaling.
    base_font_size: f32,
}

impl FontSystem {
    /// Create a font system and start asynchronous system-font loading.
    ///
    /// Until loading completes, shaping falls through to the embedded
    /// Fairfax HD fallback.
    pub fn new(
        fonts_config: Option<String>,
        font_size: f32,
        supersample: u32,
    ) -> Self {
        // Kick off font loading in the background so the window appears
        // immediately. FONTS/FAMILIES start empty; shape_row falls through
        // to the embedded fallback until they're populated.
        start_background_font_load(fonts_config);

        // Always append embedded Fairfax HD as ultimate fallback. It ships
        // only a regular face; cells that want bold/italic from the fallback
        // route get the regular glyph plus any synthesis the renderer can
        // still apply on top.
        let final_font =
            load_font(Arc::new(FAIRFAX_HD), 0, false, false).expect("Failed to load embedded font");

        let mut fs = Self {
            final_font,
            plan_cache: HashMap::new(),
            font_generation: INSTALLED_FONT_GENERATION.load(Ordering::Acquire),
            cell_width: 0,
            cell_height: 0,
            font_size,
            ascent: 0.0,
            base_font_size: font_size,
            supersample,
        };
        fs.recompute_metrics();
        fs
    }

    /// Reload fonts, font size, and supersampling factor. Scans system fonts
    /// synchronously (appropriate for config-file hot-reload), then
    /// recomputes cell metrics. The caller must clear the glyph atlas and
    /// recalculate the grid size after calling this.
    pub fn reload(
        &mut self,
        fonts_config: Option<String>,
        font_size: f32,
        supersample: u32,
    ) {
        reload_fonts(fonts_config);
        let scale = self.font_size / self.base_font_size;
        self.base_font_size = font_size;
        self.font_size = font_size * scale;
        self.supersample = supersample;
        self.plan_cache.clear();
        self.font_generation = INSTALLED_FONT_GENERATION.load(Ordering::Acquire);
        self.recompute_metrics();
    }

    fn sync_font_generation(&mut self) {
        let generation = INSTALLED_FONT_GENERATION.load(Ordering::Acquire);
        if self.font_generation != generation {
            self.plan_cache.clear();
            self.font_generation = generation;
        }
    }

    pub fn font_generation(&self) -> u64 {
        INSTALLED_FONT_GENERATION.load(Ordering::Acquire)
    }

    /// Recompute cell_width, cell_height, ascent, and font_size from the
    /// fallback font's metrics and the current base_font_size. Shared by
    /// `new()`, `reload()`, and `set_scale_factor()`.
    fn recompute_metrics(&mut self) {
        let rf = loaded_font_ref(&self.final_font).expect("parse font");
        let hhea = rf.hhea().expect("hhea table");
        let hmtx = rf.hmtx().expect("hmtx table");

        let s = self.font_size / self.final_font.units_per_em;
        let ascent = hhea.ascender().to_i16() as f32 * s;
        let descent = hhea.descender().to_i16() as f32 * s;
        let line_gap = hhea.line_gap().to_i16() as f32 * s;

        self.cell_height = (ascent - descent + line_gap).ceil() as u32;
        let m_advance = hmtx
            .advance(GlyphId::new(charmap_lookup(&rf, 'M')))
            .unwrap_or(0) as f32
            * s;
        self.cell_width = m_advance.ceil() as u32;
        self.ascent = ascent;
    }

    /// Whether `font_index`'s face was loaded as a bold weight. The renderer
    /// combines this with a cell's BOLD attribute to decide if the COLR
    /// synthesis path should kick in on top of the rasterized glyph.
    pub fn font_is_bold(
        &self,
        font_index: usize,
    ) -> bool {
        if font_index == legacy::FONT_INDEX {
            return false;
        }
        FONTS
            .read()
            .unwrap()
            .get(font_index)
            .map(|font| font.is_bold)
            .unwrap_or(self.final_font.is_bold)
    }

    /// Whether `font_index`'s face was loaded as an italic/oblique variant.
    /// When false and the cell wants italic, the renderer synthesizes
    /// italic by shearing the glyph quad.
    pub fn font_is_italic(
        &self,
        font_index: usize,
    ) -> bool {
        if font_index == legacy::FONT_INDEX {
            return false;
        }
        FONTS
            .read()
            .unwrap()
            .get(font_index)
            .map(|font| font.is_italic)
            .unwrap_or(self.final_font.is_italic)
    }

    /// Whether `font_index` is a colour-glyph font (COLR/CBDT/sbix/SVG).
    /// Synthetic bold is only safe to apply here — outline fonts would
    /// smear stems unpleasantly under a blind bitmap dilation.
    pub fn font_is_color(
        &self,
        font_index: usize,
    ) -> bool {
        if font_index == legacy::FONT_INDEX {
            return false;
        }
        FONTS
            .read()
            .unwrap()
            .get(font_index)
            .map(|font| font.is_color())
            .unwrap_or(self.final_font.is_color)
    }

    /// Convert a pixel viewport into terminal grid dimensions.
    pub fn grid_dimensions(
        &self,
        pixel_width: u32,
        pixel_height: u32,
    ) -> (u32, u32) {
        let cols = (pixel_width / self.cell_width).max(1);
        let rows = (pixel_height / self.cell_height).max(1);
        (cols, rows)
    }

    /// Baseline offset from the top of a cell, in pixels.
    pub fn baseline_offset(&self) -> f32 {
        self.ascent
    }

    /// Apply a new DPI scale factor, recalculating cell metrics and effective
    /// font size. The glyph atlas must be cleared separately after calling
    /// this — cached rasters are at the old resolution.
    pub fn set_scale_factor(
        &mut self,
        scale: f32,
    ) {
        let effective = self.base_font_size * scale;
        if (effective - self.font_size).abs() < f32::EPSILON {
            return;
        }
        self.font_size = effective;
        self.recompute_metrics();
    }

    /// Shape an entire terminal row with font fallback and plan caching.
    /// Takes `&[SmolStr]` directly from the terminal's SoA storage — each
    /// cell is one grapheme cluster — plus parallel `&[CellAttrs]` so each
    /// cell can pick the bold/italic variant of the primary family.
    ///
    /// Emoji clusters (ending in VS16 or whose first codepoint falls in a
    /// known emoji-presentation range) are shaped in a dedicated first pass
    /// that only accepts glyphs from colour fonts. A text font with a
    /// monochrome outline of `U+2764 HEAVY BLACK HEART` would otherwise win
    /// the race against a colour-emoji font and render a narrow lifeless
    /// glyph in place of the emoji the user pasted.
    ///
    /// Text cells pick a preferred face from the first family's variant
    /// table (bold/italic/bold_italic, falling back to regular with
    /// `synthetic_*` flags set when the variant is missing). Pass 0 only
    /// accepts the cell's preferred face, so a bold cell takes its bold
    /// glyph even when a fallback family also carries the same codepoint.
    pub fn shape_row(
        &mut self,
        cells: &[SmolStr],
        attrs: &[CellAttrs],
    ) -> Vec<ShapedGlyph> {
        self.sync_font_generation();

        if cells.is_empty() {
            return vec![];
        }

        let font_faces = FONTS.read().unwrap().clone();
        let families = FAMILIES.read().unwrap().clone();
        let final_font_index = font_faces.len();

        trace!(
            "shaping row: cells={:?}",
            if cells.iter().all(|c| c.is_empty() || c == " ") {
                vec!["<all empty>"]
            } else {
                cells.iter().map(|c| c.as_str()).collect::<Vec<_>>()
            }
        );

        // Build the row string and byte-offset → column mapping.
        let mut row_text = String::new();
        let mut col_map: Vec<u16> = Vec::new();
        for (col, cell) in cells.iter().enumerate() {
            let start = row_text.len();
            let mut cs = cell.chars();
            if let Some(ch) = cs.next()
                && matches!(
                    ch.general_category(),
                    GeneralCategory::ModifierSymbol | GeneralCategory::ModifierLetter
                )
                && ch.is_emoji_component()
                && cs.next().is_none()
            {
                // Orphaned emoji components (e.g. a lone skin-tone modifier) need their own
                // cell broken from the previous one.
                row_text.push('\u{200C}');
            }
            row_text.push_str(cell);
            let added = row_text.len() - start;
            for _ in 0..added {
                col_map.push(col as u16);
            }
        }

        let wants_color: Vec<bool> = cells.iter().map(|c| cluster_prefers_color(c)).collect();
        let wants_nerd_symbol: Vec<bool> = cells
            .iter()
            .map(|c| cluster_prefers_nerd_symbol(c))
            .collect();
        let nerd_symbol_font_index = font_faces.iter().position(|font| font.is_nerd_symbol_hint);

        // Preferred face per cell on pass 0. `Some(idx)` pins the cell to
        // that exact face (text cells lock the variant that matches their
        // attributes). `None` lets pass 0 accept any colour font — colour
        // fonts rarely ship weight/style variants, so emoji preference is
        // still resolved by `loaded.is_color` like the original logic.
        let preferred: Vec<Option<usize>> = cells
            .iter()
            .enumerate()
            .map(|(col, _)| {
                preferred_font_for_cell(
                    &families,
                    final_font_index,
                    attrs[col],
                    wants_color[col],
                    wants_nerd_symbol[col],
                    nerd_symbol_font_index,
                )
            })
            .collect();

        // Track which columns still need a glyph (for fallback).
        let mut has_glyph = vec![false; cells.len()];
        let mut result: Vec<ShapedGlyph> = Vec::with_capacity(cells.len());

        // Pre-pass: intercept block elements, braille, sextants, and SFLC
        // 1/8 blocks so they render through our own "fill the exact tile"
        // rasteriser instead of a font glyph with gappy edges. Marking the
        // cell as covered keeps subsequent shaping passes from overriding
        // the synthetic glyph.
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

        // If the pre-pass covered every non-blank cell we can skip font
        // shaping entirely — saves shaper setup for rows that are pure
        // pixel-art.
        let all_covered = has_glyph
            .iter()
            .enumerate()
            .all(|(i, &has)| has || cells[i] == " " || cells[i].is_empty());
        if all_covered {
            return result;
        }

        // Two passes: first only accept a glyph from the cell's preferred
        // face (variant-matched, or colour-matched for emoji). Second pass
        // lets any remaining uncovered cell take whichever font has a glyph.
        for pass in 0..2 {
            for font_idx in 0..=final_font_index {
                if pass == 0
                    && !font_is_useful_for_preferred_pass(
                        font_idx,
                        &font_faces,
                        &self.final_font,
                        &preferred,
                        &wants_color,
                        &has_glyph,
                        cells,
                    )
                {
                    continue;
                }

                let Some(font_candidate) =
                    load_font_candidate(font_idx, &font_faces, &self.final_font)
                else {
                    continue;
                };
                let loaded = font_candidate.as_loaded();

                let font_ref = match loaded_font_ref(loaded) {
                    Ok(f) => f,
                    Err(_) => continue,
                };

                let mut buffer = UnicodeBuffer::new();
                buffer.push_str(&row_text);
                buffer.guess_segment_properties();

                let direction = buffer.direction();
                let script = buffer.script();

                let key = PlanKey {
                    font_index: font_idx,
                    direction,
                    script,
                };

                // Get or create cached plan.
                self.plan_cache.entry(key).or_insert_with(|| {
                    let shaper = loaded.shaper_data.shaper(&font_ref).build();

                    ShapePlan::new(
                        &shaper,
                        direction,
                        Some(script),
                        buffer.language().as_ref(),
                        &[],
                    )
                });
                let plan = &self.plan_cache[&key];

                let shaper = loaded.shaper_data.shaper(&font_ref).build();
                let output = shaper.shape_with_plan(plan, buffer, &[]);

                let infos = output.glyph_infos();
                let positions = output.glyph_positions();
                let scale = self.font_size / loaded.units_per_em;

                for (i, (info, pos)) in infos.iter().zip(positions.iter()).enumerate() {
                    let cluster = info.cluster as usize;
                    if cluster >= col_map.len() {
                        continue;
                    }
                    let col = col_map[cluster];

                    if has_glyph[col as usize] {
                        continue;
                    }

                    let glyph_id = info.glyph_id as u16;
                    if glyph_id == 0 {
                        continue;
                    }

                    // Pass 0: text cells pin to their preferred variant;
                    // emoji clusters accept any colour font.
                    if pass == 0 {
                        match preferred[col as usize] {
                            Some(pref_idx) => {
                                if font_idx != pref_idx {
                                    continue;
                                }
                            }
                            None => {
                                if !loaded.is_color {
                                    continue;
                                }
                            }
                        }
                    }

                    // Mark all columns consumed by this glyph (handles
                    // ligatures and multi-codepoint clusters).
                    let end_byte = if i + 1 < infos.len() {
                        (infos[i + 1].cluster as usize).min(col_map.len())
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

                    result.push(ShapedGlyph {
                        glyph_id,
                        font_index: font_idx,
                        col,
                        cells_wide: glyph_cells_wide(cells, col, max_col),
                        x_offset: pos.x_offset as f32 * scale,
                        y_offset: pos.y_offset as f32 * scale,
                    });
                }

                let all_covered = has_glyph
                    .iter()
                    .enumerate()
                    .all(|(i, &has)| has || cells[i] == " " || cells[i].is_empty());
                if all_covered {
                    return result;
                }
            }
        }

        result
    }

    /// Rasterize a glyph from a specific font in the chain.
    ///
    /// Probes color-glyph tables in scalable-first order — COLR → SVG →
    /// sbix → CBDT — then falls back to `glyf` outlines.
    /// Each color rasteriser derives its own scaling from the emoji font's
    /// metrics, so a Noto Color Emoji glyph (1024 upem) fits the cell
    /// regardless of the primary monospace font's unit system.
    pub fn rasterize_glyph(
        &self,
        font_index: usize,
        glyph_index: u16,
        cells_wide: u32,
    ) -> RasterizedGlyph {
        // Legacy shapes (block elements, braille, sextants, SFLC 1/8 blocks)
        // bypass the font entirely — they render as cell-filling solid tiles
        // so "pixel art" built from them tiles seamlessly across neighbouring
        // cells. See `legacy` for the full codepoint list.
        if font_index == legacy::FONT_INDEX {
            debug!("rasterizing legacy glyph {glyph_index} for cell span {cells_wide}");
            return legacy::rasterize(
                glyph_index,
                self.cell_width,
                self.cell_height,
                self.ascent,
                self.supersample,
            );
        }
        if font_index == drcs::FONT_INDEX {
            debug!("rasterizing DRCS glyph {glyph_index} for cell span {cells_wide}");
            return drcs::rasterize(glyph_index, self.cell_width, self.cell_height);
        }

        let font_faces = FONTS.read().unwrap().clone();
        let Some(font_candidate) = load_font_candidate(font_index, &font_faces, &self.final_font)
        else {
            return empty_glyph();
        };
        let loaded = font_candidate.as_loaded();
        let scale = self.font_size / loaded.units_per_em;

        let Ok(font) = loaded_font_ref(loaded) else {
            return empty_glyph();
        };

        // Colour rasterisers receive the cell box scaled to the cluster's
        // visual span. The outline path doesn't read `cell_width` at all —
        // glyf positioning derives entirely from the glyph's own bounding
        // box and `font_size` — so it sees no behavioural change when a
        // cluster covers more than one cell.
        let target_w = self.cell_width * cells_wide.max(1);

        if let Some(glyph) = colr::rasterize_colr(&font, glyph_index, target_w, self.cell_height) {
            debug!("rasterized COLR glyph {glyph_index} for cell span {cells_wide}");
            return glyph;
        }
        if let Some(glyph) = svg::rasterize_svg(&font, glyph_index, target_w, self.cell_height) {
            debug!("rasterized SVG glyph {glyph_index} for cell span {cells_wide}");
            return glyph;
        }
        if let Some(glyph) = bitmap::rasterize_sbix(&font, glyph_index, target_w, self.cell_height)
        {
            debug!("rasterized sbix glyph {glyph_index} for cell span {cells_wide}");
            return glyph;
        }
        if let Some(glyph) = bitmap::rasterize_cbdt(&font, glyph_index, target_w, self.cell_height)
        {
            debug!("rasterized CBDT glyph {glyph_index} for cell span {cells_wide}");
            return glyph;
        }

        let loca = match font.loca(None) {
            Ok(l) => l,
            Err(_) => return empty_glyph(),
        };
        let glyf = match font.glyf() {
            Ok(g) => g,
            Err(_) => return empty_glyph(),
        };

        let gid = GlyphId::new(glyph_index as u32);
        match loca.get_glyf(gid, &glyf) {
            Ok(Some(Glyph::Simple(simple))) => {
                debug!("rasterizing outline glyph {glyph_index} for cell span {cells_wide}");
                rasterize_simple_glyph(&simple, scale, self.supersample)
            }
            Ok(Some(Glyph::Composite(composite))) => {
                debug!("rasterizing composite glyph {glyph_index} for cell span {cells_wide}");
                rasterize_composite_glyph(&composite, &loca, &glyf, scale, self.supersample)
            }
            _ => empty_glyph(),
        }
    }
}

enum LoadedFontCandidate<'a> {
    Lazy(Arc<LoadedFont>),
    Final(&'a LoadedFont),
}

impl<'a> LoadedFontCandidate<'a> {
    fn as_loaded(&'a self) -> &'a LoadedFont {
        match self {
            Self::Lazy(font) => font,
            Self::Final(font) => font,
        }
    }
}

fn load_font_candidate<'a>(
    font_idx: usize,
    font_faces: &[Arc<LazyFontFace>],
    final_font: &'a LoadedFont,
) -> Option<LoadedFontCandidate<'a>> {
    if let Some(font_face) = font_faces.get(font_idx) {
        return font_face.load().map(LoadedFontCandidate::Lazy);
    }

    Some(LoadedFontCandidate::Final(final_font))
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

fn preferred_font_for_cell(
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

fn cluster_prefers_nerd_symbol(cell: &str) -> bool {
    cell.chars()
        .any(|ch| is_private_use_codepoint(ch) && ch != '\u{FE0F}')
}

fn is_private_use_codepoint(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

fn glyph_cells_wide(
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

/// Heuristic: true when a cluster is likely meant to render as a colour
/// emoji. Covers the two common routes: explicit `VS16` selector, and
/// default-emoji-presentation codepoints in the main emoji blocks. Keeps
/// CJK and ordinary symbols (which `unicode-width` also reports as wide)
/// out of the colour path.
fn cluster_prefers_color(cell: &str) -> bool {
    if cell.ends_with('\u{FE0F}') {
        return true;
    }
    cell.chars().any(is_default_emoji_codepoint)
}

fn is_default_emoji_codepoint(c: char) -> bool {
    let cp = c as u32;
    // Misc Symbols / Dingbats / Transport blocks that are all emoji-by-default.
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

fn empty_glyph() -> RasterizedGlyph {
    RasterizedGlyph {
        bitmap: vec![],
        width: 0,
        height: 0,
        bearing_x: 0,
        bearing_y: 0,
        is_color: false,
    }
}

fn load_font(
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
    // Probe for colour glyph tables. If any is present, we treat this as a
    // colour font and let it win the font-selection race for emoji clusters.
    let color_tables = color_tables(&rf);
    let is_color = color_tables.any();

    Some(LoadedFont {
        data,
        face_index,
        shaper_data,
        units_per_em,
        is_color,
        is_bold,
        is_italic,
    })
}

fn shared_font_data_from_source(source: &fontdb::Source) -> Option<SharedFontData> {
    match source {
        fontdb::Source::Binary(data) => Some(data.clone()),
        fontdb::Source::File(path) => {
            let file = std::fs::File::open(path).ok()?;
            // SAFETY: term41 only reads font files through the returned
            // mapping. The usual mmap caveat applies: external mutation of
            // installed font files while the terminal is running can expose
            // changed bytes to this process.
            let data = unsafe { memmap2::MmapOptions::new().map(&file).ok()? };
            Some(Arc::new(data) as SharedFontData)
        }
        fontdb::Source::SharedFile(_, data) => Some(data.clone()),
    }
}

/// Look up a face in `db` that matches `family` with *exactly* the requested
/// weight and style. fontdb's `query()` will fuzzy-match (a BOLD query falls
/// back to a NORMAL face when no bold is available); we reject those fuzzy
/// hits so a missing variant stays missing and the caller can record it as
/// `None` in the family table.
fn load_family_variant(
    db: &fontdb::Database,
    fonts: &mut Vec<Arc<LazyFontFace>>,
    loaded_face_ids: &mut HashSet<fontdb::ID>,
    family: &fontdb::Family,
    weight: fontdb::Weight,
    style: fontdb::Style,
    is_bold: bool,
    is_italic: bool,
) -> Option<usize> {
    if let fontdb::Family::Name(name) = family {
        let (id, is_color_hint, is_nerd_symbol_hint) =
            pick_named_family_face(db, name, weight, style)?;
        let font_face = lazy_font_face(
            db,
            id,
            is_color_hint,
            is_nerd_symbol_hint,
            is_bold,
            is_italic,
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
    let font_face = lazy_font_face(
        db,
        id,
        is_color_hint,
        is_nerd_symbol_hint,
        is_bold,
        is_italic,
    )?;
    let idx = fonts.len();
    fonts.push(font_face);
    loaded_face_ids.insert(id);
    Some(idx)
}

fn lazy_font_face(
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

fn append_nerd_symbol_fallback(
    db: &fontdb::Database,
    fonts: &mut Vec<Arc<LazyFontFace>>,
    loaded_face_ids: &mut HashSet<fontdb::ID>,
) {
    let Some((id, family_name)) = pick_nerd_symbol_face(db, loaded_face_ids) else {
        return;
    };
    let is_color_hint = db.with_face_data(id, color_table_score).unwrap_or_default() > 0;
    let Some(font_face) = lazy_font_face(db, id, is_color_hint, true, false, false) else {
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

fn nerd_symbol_family_priority(face: &fontdb::FaceInfo) -> Option<u8> {
    face.families
        .iter()
        .filter_map(|(name, _)| {
            debug!("checking family {name} for Nerd Font symbol fallback");
            if name.to_lowercase().contains(NERD_SYMBOL_FAMILY) {
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

#[derive(Clone, Copy, Default)]
struct ColorTables {
    colr: bool,
    svg: bool,
    sbix: bool,
    cbdt: bool,
}

impl ColorTables {
    fn any(self) -> bool {
        self.colr || self.svg || self.sbix || self.cbdt
    }

    fn score(self) -> u8 {
        // Prefer scalable color outlines when duplicate faces share the same
        // family/style/weight. This keeps a locally installed COLR/SVG Noto
        // Color Emoji ahead of distro bitmap-only CBDT builds.
        (self.colr as u8) * 8 + (self.svg as u8) * 4 + (self.sbix as u8) * 2 + self.cbdt as u8
    }
}

fn color_tables(rf: &read_fonts::FontRef<'_>) -> ColorTables {
    ColorTables {
        colr: rf.colr().is_ok(),
        svg: rf.svg().is_ok(),
        sbix: rf.sbix().is_ok(),
        cbdt: rf.cbdt().is_ok(),
    }
}

fn color_table_score(
    data: &[u8],
    face_index: u32,
) -> u8 {
    read_fonts::FontRef::from_index(data, face_index)
        .map(|rf| color_tables(&rf).score())
        .unwrap_or_default()
}

/// Pick the face that should shape a cell with the given attributes, walking
/// `families` in user-declared order. Returns `(font_index, synth_bold,
/// synth_italic)` — synth flags are set when the chosen face doesn't natively
/// cover the requested style and the renderer should fake it on top.
///
/// Degradation prefers preserving the rarer style: for BOLD|ITALIC with only
/// a bold face available, keep bold and synthesize italic via vertex shear;
/// with only an italic face, keep italic and synthesize bold (COLR only).
fn pick_variant(
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

fn charmap_lookup(
    font: &read_fonts::FontRef,
    ch: char,
) -> u32 {
    let cmap = match font.cmap() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for record in cmap.encoding_records() {
        if let Ok(subtable) = record.subtable(cmap.offset_data())
            && let Some(gid) = subtable.map_codepoint(ch)
        {
            return gid.to_u32();
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Rasterization via raqote
// ---------------------------------------------------------------------------

fn rasterize_simple_glyph(
    simple: &SimpleGlyph,
    scale: f32,
    supersample: u32,
) -> RasterizedGlyph {
    // 1× bounds — these define the output size and bearing so the glyph
    // lands at the same position it would without supersampling.
    let x_min = (simple.x_min() as f32 * scale).floor();
    let y_max = (simple.y_max() as f32 * scale).ceil();
    let width = ((simple.x_max() as f32 * scale).ceil() - x_min) as i32 + 2;
    let height = (y_max - (simple.y_min() as f32 * scale).floor()) as i32 + 2;

    if width <= 0 || height <= 0 {
        return empty_glyph();
    }

    // Rasterize at SS× resolution then downsample.
    let ss = supersample as i32;
    let ss_scale = scale * ss as f32;
    let ss_w = width * ss;
    let ss_h = height * ss;
    let path = build_path(
        simple,
        ss_scale,
        x_min * ss as f32,
        y_max * ss as f32,
        STEM_DARKEN_SS_PX,
    );
    let mut dt = DrawTarget::new(ss_w, ss_h);
    dt.fill(
        &path,
        &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
        &DrawOptions::default(),
    );

    RasterizedGlyph {
        bitmap: downsample_alpha(dt.get_data(), ss_w, ss_h, width, height, ss),
        width: width as u32,
        height: height as u32,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
        is_color: false,
    }
}

/// Box-filter downsample a supersampled raqote alpha buffer into an RGBA8
/// bitmap whose colour channels are zero. Each output pixel averages the
/// `ss × ss` block of source pixels that map to it, giving the glyph
/// `ss²` levels of sub-pixel coverage — noticeably smoother stems and
/// curves than the binary on/off coverage at 1×.
///
/// `pixels` is the raqote `DrawTarget::get_data()` buffer — premultiplied
/// ARGB in platform byte order, `ss_w × ss_h` pixels. Output is
/// `out_w × out_h` RGBA8 with `rgb = 0`.
fn downsample_alpha(
    pixels: &[u32],
    ss_w: i32,
    ss_h: i32,
    out_w: i32,
    out_h: i32,
    ss: i32,
) -> Vec<u8> {
    let pixels_u8: Vec<u8> = pixels.iter().flat_map(|&p| p.to_le_bytes()).collect();
    downsample_alpha_u8(&pixels_u8, ss_w, ss_h, out_w, out_h, ss)
}

fn downsample_alpha_u8(
    pixels: &[u8],
    ss_w: i32,
    ss_h: i32,
    out_w: i32,
    out_h: i32,
    ss: i32,
) -> Vec<u8> {
    let mut bitmap = vec![0u8; (out_w * out_h * 4) as usize];
    let area = (ss * ss) as u32;
    for y in 0..out_h {
        for x in 0..out_w {
            let mut alpha_sum = 0u32;
            for dy in 0..ss {
                for dx in 0..ss {
                    let sx = x * ss + dx;
                    let sy = y * ss + dy;
                    if sx < ss_w && sy < ss_h {
                        let idx = (sy * ss_w + sx) as usize * 4;
                        alpha_sum += pixels[idx + 3] as u32;
                    }
                }
            }
            let idx = (y * out_w + x) as usize * 4;
            bitmap[idx + 3] = (alpha_sum / area) as u8;
        }
    }
    bitmap
}

/// Recursively collect the bounding box of a composite glyph, flattening
/// nested composites so that leaf simple-glyph components at any depth are
/// included.
fn composite_bounds(
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
    parent_dx: f32,
    parent_dy: f32,
    bounds: &mut [f32; 4], // [x_min, y_min, x_max, y_max]
) {
    for comp in composite.components() {
        let gid = GlyphId::new(comp.glyph.to_u32());
        let glyph = match loca.get_glyf(gid, glyf) {
            Ok(Some(g)) => g,
            _ => continue,
        };

        let (dx, dy) = match comp.anchor {
            Anchor::Offset { x, y } => (x as f32 * scale + parent_dx, y as f32 * scale + parent_dy),
            _ => (parent_dx, parent_dy),
        };

        match glyph {
            Glyph::Simple(simple) => {
                bounds[0] = bounds[0].min(simple.x_min() as f32 * scale + dx);
                bounds[1] = bounds[1].min(simple.y_min() as f32 * scale + dy);
                bounds[2] = bounds[2].max(simple.x_max() as f32 * scale + dx);
                bounds[3] = bounds[3].max(simple.y_max() as f32 * scale + dy);
            }
            Glyph::Composite(inner) => {
                composite_bounds(&inner, loca, glyf, scale, dx, dy, bounds);
            }
        }
    }
}

/// Recursively add all simple-glyph leaf components of a composite to a path,
/// accumulating offsets through any nesting depth.
fn composite_to_path(
    pb: &mut PathBuilder,
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
    x_min: f32,
    y_max: f32,
    parent_dx: f32,
    parent_dy: f32,
    embolden: f32,
) {
    for comp in composite.components() {
        let gid = GlyphId::new(comp.glyph.to_u32());
        let glyph = match loca.get_glyf(gid, glyf) {
            Ok(Some(g)) => g,
            _ => continue,
        };

        let (dx, dy) = match comp.anchor {
            Anchor::Offset { x, y } => (x as f32 * scale + parent_dx, y as f32 * scale + parent_dy),
            _ => (parent_dx, parent_dy),
        };

        match glyph {
            Glyph::Simple(simple) => {
                add_simple_glyph_to_path(pb, &simple, scale, x_min, y_max, dx, dy, embolden);
            }
            Glyph::Composite(inner) => {
                composite_to_path(
                    pb, &inner, loca, glyf, scale, x_min, y_max, dx, dy, embolden,
                );
            }
        }
    }
}

fn rasterize_composite_glyph(
    composite: &CompositeGlyph,
    loca: &Loca,
    glyf: &read_fonts::tables::glyf::Glyf,
    scale: f32,
    supersample: u32,
) -> RasterizedGlyph {
    // 1× bounds for output.
    let mut bounds = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    composite_bounds(composite, loca, glyf, scale, 0.0, 0.0, &mut bounds);
    let [x_min_all, y_min_all, x_max_all, y_max_all] = bounds;

    if x_min_all >= x_max_all || y_min_all >= y_max_all {
        return empty_glyph();
    }

    let x_min = x_min_all.floor();
    let y_max = y_max_all.ceil();
    let width = (x_max_all.ceil() - x_min) as i32 + 2;
    let height = (y_max - y_min_all.floor()) as i32 + 2;

    if width <= 0 || height <= 0 {
        return empty_glyph();
    }

    // Rasterize at SS× resolution then downsample.
    let ss = supersample as i32;
    let ss_scale = scale * ss as f32;
    let ss_w = width * ss;
    let ss_h = height * ss;

    let mut pb = PathBuilder::new();
    composite_to_path(
        &mut pb,
        composite,
        loca,
        glyf,
        ss_scale,
        x_min * ss as f32,
        y_max * ss as f32,
        0.0,
        0.0,
        STEM_DARKEN_SS_PX,
    );

    let path = pb.finish();
    let mut dt = DrawTarget::new(ss_w, ss_h);
    dt.fill(
        &path,
        &Source::Solid(SolidSource::from_unpremultiplied_argb(255, 255, 255, 255)),
        &DrawOptions::default(),
    );

    RasterizedGlyph {
        bitmap: downsample_alpha(dt.get_data(), ss_w, ss_h, width, height, ss),
        width: width as u32,
        height: height as u32,
        bearing_x: x_min as i32,
        bearing_y: y_max as i32,
        is_color: false,
    }
}

fn add_simple_glyph_to_path(
    pb: &mut PathBuilder,
    simple: &SimpleGlyph,
    scale: f32,
    x_min: f32,
    y_max: f32,
    dx: f32,
    dy: f32,
    embolden: f32,
) {
    let points: Vec<CurvePoint> = simple.points().collect();
    let contour_ends: Vec<usize> = simple
        .end_pts_of_contours()
        .iter()
        .map(|e| e.get() as usize)
        .collect();

    let mut contour_start = 0;
    for &contour_end in &contour_ends {
        let contour = &points[contour_start..=contour_end];
        add_contour_to_path_with_offset(pb, contour, scale, x_min, y_max, dx, dy, embolden);
        contour_start = contour_end + 1;
    }
}

fn build_path(
    simple: &SimpleGlyph,
    scale: f32,
    x_min: f32,
    y_max: f32,
    embolden: f32,
) -> raqote::Path {
    let mut pb = PathBuilder::new();
    add_simple_glyph_to_path(&mut pb, simple, scale, x_min, y_max, 0.0, 0.0, embolden);
    pb.finish()
}

/// Stem darkening amount in supersample pixels. Each contour point is
/// moved outward along its vertex normal by this amount, uniformly
/// thickening stems. 0.4 SS-px = 0.2 display pixels at SUPERSAMPLE=2.
const STEM_DARKEN_SS_PX: f32 = 0.4;

/// Move each point in a closed contour outward along its vertex normal by
/// `amount` pixels. Outer contours expand; inner contours (holes) shrink —
/// both actions increase the filled area, thickening stems uniformly.
///
/// The vertex normal at each point is the average of the two adjacent edge
/// normals, normalised. At sharp corners (where the averaged normal is very
/// short), the offset is capped at `2 * amount` to prevent miter spikes.
fn embolden_contour(
    points: &mut [(f32, f32, bool)],
    amount: f32,
) {
    let n = points.len();
    if n < 3 || amount <= 0.0 {
        return;
    }

    // Signed area via the shoelace formula. In screen-space (Y-down) a
    // positive result means CW winding (inner/hole contour), negative
    // means CCW (outer contour).
    let mut area2 = 0.0f32;
    for i in 0..n {
        let j = (i + 1) % n;
        area2 += points[i].0 * points[j].1 - points[j].0 * points[i].1;
    }
    // Outer (CCW in screen, negative area): expand outward.
    // Inner (CW in screen, positive area): shrink the hole = expand fill.
    // Both use the same "outward" normal relative to the contour's own
    // winding; we just need to pick a consistent perpendicular direction
    // and flip the sign for inner contours.
    let sign = if area2 >= 0.0 { -amount } else { amount };
    let max_offset = 2.0 * amount;

    let offsets: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            let prev = if i == 0 { n - 1 } else { i - 1 };
            let next = (i + 1) % n;

            let (px, py, _) = points[prev];
            let (cx, cy, _) = points[i];
            let (nx, ny, _) = points[next];

            // Edge vectors.
            let (e1x, e1y) = (cx - px, cy - py);
            let (e2x, e2y) = (nx - cx, ny - cy);

            // Per-edge normals (rotate 90° CCW: (-y, x)).
            let len1 = (e1x * e1x + e1y * e1y).sqrt().max(1e-6);
            let len2 = (e2x * e2x + e2y * e2y).sqrt().max(1e-6);
            let (n1x, n1y) = (-e1y / len1, e1x / len1);
            let (n2x, n2y) = (-e2y / len2, e2x / len2);

            // Average normal. The length naturally shrinks at sharp corners
            // (the two normals point in different directions); we cap the
            // reciprocal to avoid miter spikes.
            let (ax, ay) = (n1x + n2x, n1y + n2y);
            let alen = (ax * ax + ay * ay).sqrt().max(1e-6);
            let offset = (sign / alen).clamp(-max_offset, max_offset);
            (ax * offset, ay * offset)
        })
        .collect();

    for (i, &(dx, dy)) in offsets.iter().enumerate() {
        points[i].0 += dx;
        points[i].1 += dy;
    }
}

fn add_contour_to_path_with_offset(
    pb: &mut PathBuilder,
    contour: &[CurvePoint],
    scale: f32,
    x_min: f32,
    y_max: f32,
    dx: f32,
    dy: f32,
    embolden: f32,
) {
    if contour.is_empty() {
        return;
    }

    let tx = |p: &CurvePoint| -> (f32, f32) {
        let x = p.x as f32 * scale + dx - x_min + 1.0;
        let y = y_max - (p.y as f32 * scale + dy) + 1.0;
        (x, y)
    };

    let mut expanded: Vec<(f32, f32, bool)> = Vec::new();
    for p in contour {
        let (px, py) = tx(p);
        if !expanded.is_empty() && !p.on_curve {
            let (_, _, prev_on) = *expanded.last().unwrap();
            if !prev_on {
                let (lx, ly, _) = *expanded.last().unwrap();
                expanded.push(((lx + px) / 2.0, (ly + py) / 2.0, true));
            }
        }
        expanded.push((px, py, p.on_curve));
    }

    if expanded.is_empty() {
        return;
    }

    let (fx, fy, first_on) = expanded[0];
    let (lx, ly, last_on) = *expanded.last().unwrap();
    if !last_on && !first_on {
        expanded.push(((lx + fx) / 2.0, (ly + fy) / 2.0, true));
    }

    // Stem darkening: dilate the outline along vertex normals.
    embolden_contour(&mut expanded, embolden);

    let start_idx = expanded.iter().position(|p| p.2).unwrap_or(0);
    let n = expanded.len();
    let (sx, sy, _) = expanded[start_idx];
    pb.move_to(sx, sy);

    let mut i = 1;
    while i < n {
        let idx = (start_idx + i) % n;
        let (px, py, on_curve) = expanded[idx];
        if on_curve {
            pb.line_to(px, py);
            i += 1;
        } else {
            let next_idx = (start_idx + i + 1) % n;
            let (nx, ny, _) = expanded[next_idx];
            pb.quad_to(px, py, nx, ny);
            i += 2;
        }
    }
    pb.close();
}

#[cfg(test)]
mod tests {
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
        let unrelated = face_info(
            &["JetBrainsMono Nerd Font"],
            fontdb::Weight::NORMAL,
            fontdb::Style::Normal,
        );

        assert_eq!(nerd_symbol_family_priority(&plain), Some(1));
        assert_eq!(nerd_symbol_family_priority(&mono), Some(2));
        assert_eq!(nerd_symbol_family_priority(&unrelated), None);
    }
}

//! Font discovery, shaping, fallback, and glyph rasterization for `term41`.
//!
//! The renderer asks this crate to shape terminal rows into positioned glyphs
//! and to rasterize individual glyph ids into RGBA bitmaps suitable for the
//! GPU atlas. Font fallback is ordered by user configuration, with embedded
//! Fairfax HD as the final fallback.

use std::collections::HashMap;
use std::sync::Arc;

use harfrust::ShapePlan;
use smol_str::SmolStr;

use crate::attrs::CellAttrs;

/// Per-cell text attribute flags.
pub mod attrs;
mod bitmap;
mod charmap;
mod color_tables;
mod colr;
mod drcs;
mod families;
mod legacy;
mod loader;
mod metrics;
mod rasterize;
mod shape;
mod svg;

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

pub use self::drcs::Context as DrcsContext;
pub use self::drcs::GLYPHS_PER_SET as DRCS_GLYPHS_PER_SET;
pub use self::drcs::GeometryClass as DrcsGeometryClass;
pub use self::drcs::GlyphDef as DrcsGlyphDef;
pub use self::drcs::GlyphMap as DrcsGlyphMap;
pub use self::drcs::encode_char as encode_drcs_char;

/// A shaped glyph with its position info, ready for rendering.
pub struct ShapedGlyph {
    /// Font-specific glyph id.
    pub glyph_id: u16,
    /// Index into the loaded font list, or a sentinel for synthetic fonts.
    pub font_index: usize,
    /// Terminal column where this glyph's cluster starts.
    pub col: u16,
    /// Number of terminal cells this glyph occupies horizontally. For a ZWJ
    /// ligature the font collapses into a single glyph, this counts all the
    /// cells the ligated cluster claims so colour rasterisers can scale the
    /// emoji to fit its visual footprint instead of squashing it into one cell.
    pub cells_wide: u8,
    /// HarfBuzz horizontal positioning adjustment, in pixels.
    pub x_offset: f32,
    /// HarfBuzz vertical positioning adjustment, in pixels.
    pub y_offset: f32,
}

/// Font system: manages an ordered list of fonts with fallback and plan
/// caching. Families are kept in user-declared order; the first entry is the
/// primary text face, and later entries provide fallback glyphs for cells the
/// primary cannot cover. Each family carries up to four weight/style variants;
/// missing variants degrade to `regular` with synthesis at render time.
pub struct FontSystem {
    final_font: Arc<loader::LoadedFont>,
    fonts: Arc<loader::FontSet>,
    pending_fonts: Option<loader::PendingFonts>,

    plan_cache: HashMap<shape::PlanKey, ShapePlan>,
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
    /// Until loading completes, shaping falls through to the embedded Fairfax
    /// HD fallback.
    pub fn new(
        fonts_config: Option<String>,
        font_size: f32,
        supersample: u32,
    ) -> Self {
        let pending_fonts = Some(loader::start_background_font_load(fonts_config));

        let final_font = loader::load_font(Arc::new(FAIRFAX_HD), 0, false, false)
            .expect("Failed to load embedded font");

        let mut font_system = Self {
            final_font: Arc::new(final_font),
            fonts: Arc::new(loader::FontSet::default()),
            pending_fonts,
            plan_cache: HashMap::new(),
            font_generation: 0,
            cell_width: 0,
            cell_height: 0,
            font_size,
            ascent: 0.0,
            base_font_size: font_size,
            supersample,
        };
        font_system.recompute_metrics();
        font_system
    }

    /// Create an independent rendering context that shares the current fonts
    /// and any pending discovery work. Each context installs pending fonts at
    /// its own frame boundary; later reloads affect only the reloaded context.
    /// Shaping plans and display metrics remain local to each context.
    pub fn fork(&self) -> Self {
        Self {
            final_font: self.final_font.clone(),
            fonts: self.fonts.clone(),
            pending_fonts: self.pending_fonts.clone(),
            plan_cache: HashMap::new(),
            font_generation: self.font_generation,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
            font_size: self.font_size,
            ascent: self.ascent,
            base_font_size: self.base_font_size,
            supersample: self.supersample,
        }
    }

    /// Reload fonts, font size, and supersampling factor. Scans system fonts
    /// synchronously (appropriate for config-file hot-reload), then recomputes
    /// cell metrics. The caller must clear the glyph atlas and recalculate the
    /// grid size after calling this.
    pub fn reload(
        &mut self,
        fonts_config: Option<String>,
        font_size: f32,
        supersample: u32,
    ) {
        self.fonts = loader::load_fonts(fonts_config);
        self.pending_fonts = None;
        let scale = self.font_size / self.base_font_size;
        self.base_font_size = font_size;
        self.font_size = font_size * scale;
        self.supersample = supersample;
        self.plan_cache.clear();
        self.font_generation = self.font_generation.wrapping_add(1);
        self.recompute_metrics();
    }

    /// Identity of this instance's installed font snapshot. Stable until reload
    /// or `sync_loaded_fonts`; shaping and rasterization never replace it.
    /// Compare generations only within one instance, including after a fork.
    pub fn font_generation(&self) -> u64 {
        self.font_generation
    }

    /// Pull in newly installed background fonts and recompute the aggregate
    /// cell metrics at a frame boundary. Returns true whenever fonts changed,
    /// including replacements with identical metrics. Callers must invalidate
    /// raster caches on true before starting the next frame.
    pub fn sync_loaded_fonts(&mut self) -> bool {
        let Some(fonts) = self
            .pending_fonts
            .as_ref()
            .and_then(|pending| pending.get())
            .cloned()
        else {
            return false;
        };
        self.fonts = fonts;
        self.pending_fonts = None;
        self.plan_cache.clear();
        self.font_generation = self.font_generation.wrapping_add(1);
        self.recompute_metrics();
        true
    }

    /// Recompute cell_width, cell_height, and ascent from the largest loaded
    /// font metrics and the current effective font_size. Shared by `new()`,
    /// `reload()`, `sync_loaded_fonts()`, and `set_scale_factor()`.
    fn recompute_metrics(&mut self) {
        let metrics =
            metrics::font_system_metrics(&self.fonts.faces, &self.final_font, self.font_size);
        self.cell_width = metrics.cell_width;
        self.cell_height = metrics.cell_height;
        self.ascent = metrics.ascent;
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
        self.fonts
            .faces
            .get(font_index)
            .map(|font| font.is_bold)
            .unwrap_or(self.final_font.is_bold)
    }

    /// Whether `font_index`'s face was loaded as an italic/oblique variant.
    /// When false and the cell wants italic, the renderer synthesizes italic
    /// by shearing the glyph quad.
    pub fn font_is_italic(
        &self,
        font_index: usize,
    ) -> bool {
        if font_index == legacy::FONT_INDEX {
            return false;
        }
        self.fonts
            .faces
            .get(font_index)
            .map(|font| font.is_italic)
            .unwrap_or(self.final_font.is_italic)
    }

    /// Whether `font_index` is a colour-glyph font (COLR/CBDT/sbix/SVG).
    /// Synthetic bold is only safe to apply here; outline fonts would smear
    /// stems unpleasantly under a blind bitmap dilation.
    pub fn font_is_color(
        &self,
        font_index: usize,
    ) -> bool {
        if font_index == legacy::FONT_INDEX {
            return false;
        }
        self.fonts
            .faces
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
    /// this because cached rasters are at the old resolution.
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
    /// Takes `&[SmolStr]` directly from the terminal's SoA storage, plus
    /// parallel `&[CellAttrs]` so each cell can pick the bold/italic variant
    /// of the primary family.
    pub fn shape_row(
        &mut self,
        cells: &[SmolStr],
        attrs: &[CellAttrs],
    ) -> Vec<ShapedGlyph> {
        self.shape_row_with_drcs(cells, attrs, None)
    }

    /// Shape with the session's explicit downloadable-font context.
    pub fn shape_row_with_drcs(
        &mut self,
        cells: &[SmolStr],
        attrs: &[CellAttrs],
        drcs: Option<DrcsContext<'_>>,
    ) -> Vec<ShapedGlyph> {
        shape::shape_row(self, cells, attrs, drcs)
    }

    /// Rasterize a glyph from a specific font in the chain.
    ///
    /// Probes color-glyph tables in scalable-first order (COLR, SVG, sbix,
    /// CBDT), then falls back to `glyf` outlines.
    pub fn rasterize_glyph(
        &self,
        font_index: usize,
        glyph_index: u16,
        cells_wide: u32,
    ) -> RasterizedGlyph {
        self.rasterize_glyph_with_drcs(font_index, glyph_index, cells_wide, None)
    }

    /// Rasterize against the same font snapshot and DRCS context used to shape.
    pub fn rasterize_glyph_with_drcs(
        &self,
        font_index: usize,
        glyph_index: u16,
        cells_wide: u32,
        drcs: Option<DrcsContext<'_>>,
    ) -> RasterizedGlyph {
        rasterize::rasterize_glyph(self, font_index, glyph_index, cells_wide, drcs)
    }
}

#[cfg(test)]
mod tests;

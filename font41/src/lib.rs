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
    final_font: loader::LoadedFont,

    plan_cache: HashMap<shape::PlanKey, ShapePlan>,
    font_generation: u64,
    metrics_generation: u64,
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
        loader::start_background_font_load(fonts_config);

        let final_font = loader::load_font(Arc::new(FAIRFAX_HD), 0, false, false)
            .expect("Failed to load embedded font");
        let generation = loader::installed_font_generation();

        let mut font_system = Self {
            final_font,
            plan_cache: HashMap::new(),
            font_generation: generation,
            metrics_generation: generation,
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
        loader::reload_fonts(fonts_config);
        let scale = self.font_size / self.base_font_size;
        self.base_font_size = font_size;
        self.font_size = font_size * scale;
        self.supersample = supersample;
        self.plan_cache.clear();
        self.font_generation = loader::installed_font_generation();
        self.metrics_generation = self.font_generation;
        self.recompute_metrics();
    }

    fn sync_font_generation(&mut self) {
        let generation = loader::installed_font_generation();
        if self.font_generation != generation {
            self.plan_cache.clear();
            self.font_generation = generation;
        }
    }

    pub fn font_generation(&self) -> u64 {
        loader::installed_font_generation()
    }

    /// Pull in newly installed background fonts and recompute the aggregate
    /// cell metrics. Returns true when the live grid cell size changed.
    pub fn sync_loaded_fonts(&mut self) -> bool {
        let generation = loader::installed_font_generation();
        if self.metrics_generation == generation {
            return false;
        }

        let old_metrics = metrics::ScaledCellMetrics {
            cell_width: self.cell_width,
            cell_height: self.cell_height,
            ascent: self.ascent,
        };
        self.plan_cache.clear();
        self.font_generation = generation;
        self.metrics_generation = generation;
        self.recompute_metrics();

        old_metrics
            != (metrics::ScaledCellMetrics {
                cell_width: self.cell_width,
                cell_height: self.cell_height,
                ascent: self.ascent,
            })
    }

    /// Recompute cell_width, cell_height, and ascent from the largest loaded
    /// font metrics and the current effective font_size. Shared by `new()`,
    /// `reload()`, `sync_loaded_fonts()`, and `set_scale_factor()`.
    fn recompute_metrics(&mut self) {
        let metrics = metrics::font_system_metrics(&self.final_font, self.font_size);
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
        loader::font_faces()
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
        loader::font_faces()
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
        loader::font_faces()
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
        shape::shape_row(self, cells, attrs)
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
        rasterize::rasterize_glyph(self, font_index, glyph_index, cells_wide)
    }
}

#[cfg(test)]
mod tests;

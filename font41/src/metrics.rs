use read_fonts::TableProvider;
use read_fonts::tables::head::Head;
use read_fonts::types::GlyphId;

use crate::charmap::charmap_lookup;
use crate::loader;
use crate::loader::LoadedFont;

#[derive(Debug, Clone, Copy)]
pub(crate) struct FontEmMetrics {
    pub(crate) ascender: f32,
    pub(crate) descender: f32,
    pub(crate) line_gap: f32,
    pub(crate) cell_width: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct ScaledCellMetrics {
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
    pub(crate) ascent: f32,
}

pub(crate) fn font_em_metrics(
    rf: &read_fonts::FontRef<'_>,
    head: Head,
) -> FontEmMetrics {
    let Ok(hhea) = rf.hhea() else {
        return FontEmMetrics {
            ascender: head.x_max() as f32,
            descender: head.y_min() as f32,
            line_gap: 0.0,
            cell_width: head.units_per_em() as f32,
        };
    };

    let cell_width = rf
        .hmtx()
        .ok()
        .and_then(|hmtx| hmtx.advance(GlyphId::new(charmap_lookup(rf, 'M'))))
        .filter(|advance| *advance > 0)
        .unwrap_or_else(|| hhea.advance_width_max().to_u16()) as f32;

    FontEmMetrics {
        ascender: hhea.ascender().to_i16() as f32,
        descender: hhea.descender().to_i16() as f32,
        line_gap: hhea.line_gap().to_i16() as f32,
        cell_width,
    }
}

pub(crate) fn scaled_font_metrics(
    font: &LoadedFont,
    font_size: f32,
) -> ScaledCellMetrics {
    let scale = font_size / font.units_per_em;
    let ascent = (font.metrics.ascender * scale).max(0.0);
    let descent = (-font.metrics.descender * scale).max(0.0);
    let line_gap = (font.metrics.line_gap * scale).max(0.0);
    let cell_width = (font.metrics.cell_width * scale).ceil().max(1.0) as u32;
    let cell_height = (ascent + descent + line_gap).ceil().max(1.0) as u32;

    ScaledCellMetrics {
        cell_width,
        cell_height,
        ascent,
    }
}

pub(crate) fn aggregate_cell_metrics(
    metrics: impl IntoIterator<Item = ScaledCellMetrics>
) -> ScaledCellMetrics {
    let mut cell_width = 1;
    let mut ascent = 1.0_f32;
    let mut descent_and_gap = 0.0_f32;

    for metric in metrics {
        cell_width = cell_width.max(metric.cell_width);
        ascent = ascent.max(metric.ascent);
        descent_and_gap = descent_and_gap.max(metric.cell_height as f32 - metric.ascent);
    }

    ScaledCellMetrics {
        cell_width,
        cell_height: (ascent + descent_and_gap).ceil().max(1.0) as u32,
        ascent,
    }
}

pub(crate) fn font_system_metrics(
    final_font: &LoadedFont,
    font_size: f32,
) -> ScaledCellMetrics {
    let font_faces = loader::font_faces();
    let metrics = font_faces
        .iter()
        .filter_map(|font| font.load())
        .filter(|font| !font.is_color)
        .map(|font| scaled_font_metrics(&font, font_size))
        .chain(std::iter::once(scaled_font_metrics(final_font, font_size)));

    aggregate_cell_metrics(metrics)
}

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;

use font41::FontSystem;
use font41::RasterizedGlyph;
use font41::attrs::CellAttrs;
use font41::attrs::UnderlineStyle;
use image41::decode_image;
use palette::Srgb;
use smol_str::SmolStrBuilder;
use softbuffer::Context;
use softbuffer::Surface;
use terminal41::CursorShape;
use terminal41::LineAttr;
use unicode_segmentation::UnicodeSegmentation;
use winit::window::Window;

use crate::APP_START_TIME;
use crate::InputEndpoint;
use crate::renderer::compute_gutter_width;
use crate::renderer::r#impl::FAILURE;
use crate::renderer::r#impl::RUNNING;
use crate::renderer::r#impl::RowSnapshot;
use crate::renderer::r#impl::SUCCESS;
use crate::renderer::r#impl::TermSnapshot;
use crate::renderer::r#impl::collect_row_glyphs;
use crate::renderer::r#impl::snapshot_terminal;
use crate::renderer::paint::build_tab_bar_plan;
use crate::renderer::paint::resolve_painted_cell;
use crate::renderer::paint::status_line_label_row;

type StartupGlyphKey = (usize, u16, u8, bool, Option<font41::DrcsGeometryClass>);

struct CachedBackground {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

pub(crate) struct StartupPresenter {
    _context: Context<Arc<Window>>,
    surface: Surface<Arc<Window>, Arc<Window>>,
    font_system: FontSystem,
    glyph_cache: HashMap<StartupGlyphKey, RasterizedGlyph>,
    gutter_enabled: bool,
    background: Option<CachedBackground>,
    first_frame: bool,
}

impl StartupPresenter {
    pub(crate) fn new(
        window: Arc<Window>,
        fonts: Option<String>,
        font_size: f32,
        supersampling: i32,
        scale_factor: f64,
        gutter_enabled: bool,
        background_path: Option<PathBuf>,
    ) -> Option<Self> {
        let context = match Context::new(window.clone()) {
            Ok(context) => context,
            Err(err) => {
                warn!("startup presenter: context init failed: {err}");
                return None;
            }
        };
        let surface = match Surface::new(&context, window) {
            Ok(surface) => surface,
            Err(err) => {
                warn!("startup presenter: surface init failed: {err}");
                return None;
            }
        };

        let mut font_system = FontSystem::new(fonts, font_size, supersampling);
        if scale_factor != 1.0 {
            font_system.set_scale_factor(scale_factor as f32);
        }

        Some(Self {
            _context: context,
            surface,
            font_system,
            glyph_cache: HashMap::new(),
            gutter_enabled,
            background: background_path.and_then(|path| load_cached_background(&path)),
            first_frame: true,
        })
    }

    pub(crate) fn present(
        &mut self,
        window: &Arc<Window>,
        target: &InputEndpoint,
        hovered_button: Option<crate::renderer::TabBarHover>,
        maximized: bool,
    ) {
        let size = window.inner_size();
        let Some(width) = NonZeroU32::new(size.width.max(1)) else {
            return;
        };
        let Some(height) = NonZeroU32::new(size.height.max(1)) else {
            return;
        };

        if let Err(err) = self.surface.resize(width, height) {
            warn!("startup presenter: resize failed: {err}");
            return;
        }

        let (title, snap) = {
            let terminal = target.terminal.lock();
            let title = terminal
                .metadata
                .current_title
                .clone()
                .unwrap_or_else(|| "Shell".to_string());
            let snap = snapshot_terminal(&terminal);
            (title, snap)
        };

        let mut buffer = match self.surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(err) => {
                warn!("startup presenter: buffer acquisition failed: {err}");
                return;
            }
        };

        let width = width.get() as usize;
        let height = height.get() as usize;
        let cell_w = self.font_system.cell_width as i32;
        let cell_h = self.font_system.cell_height as i32;
        let baseline = self.font_system.baseline_offset();
        let tab_bar_h = cell_h;
        let gutter_w = if self.gutter_enabled {
            compute_gutter_width(self.font_system.cell_width) as i32
        } else {
            0
        };

        clear(buffer.as_mut(), pack_rgb(snap.palette.bg));
        if let Some(background) = self.background.as_ref() {
            paint_cached_background(buffer.as_mut(), width, height, background);
        }
        paint_tab_bar(
            &mut self.font_system,
            &snap,
            buffer.as_mut(),
            &title,
            cell_w,
            width,
            height,
            tab_bar_h,
            snap.palette.bg,
            snap.palette.fg,
            hovered_button,
            maximized,
        );
        if gutter_w > 0 {
            paint_gutter_markers(
                buffer.as_mut(),
                width,
                height,
                &snap,
                gutter_w,
                cell_h,
                tab_bar_h,
            );
        }
        paint_status_line_chrome(
            &mut self.font_system,
            &snap,
            buffer.as_mut(),
            width,
            height,
            gutter_w,
            cell_h,
            tab_bar_h,
        );

        let block_cursor = match snap.cursor_style.shape {
            CursorShape::Block => snap.cursor,
            _ => None,
        };

        for (row_idx, row) in snap.rows.iter().enumerate() {
            let y = tab_bar_h + row_idx as i32 * cell_h;
            if y >= height as i32 {
                break;
            }

            let is_double_wide = !matches!(row.line_attr, LineAttr::Normal);
            let effective_cell_w = if is_double_wide { cell_w * 2 } else { cell_w };
            let visible_cols = if is_double_wide {
                snap.viewport_cols / 2
            } else {
                snap.viewport_cols
            };

            paint_row_backgrounds(
                buffer.as_mut(),
                width,
                height,
                &snap,
                row,
                row_idx as u32,
                y,
                effective_cell_w,
                cell_h,
                gutter_w,
                block_cursor,
                self.background.is_some(),
            );

            let glyphs = collect_row_glyphs(
                &mut self.font_system,
                &snap,
                row,
                row_idx as u32,
                visible_cols,
                block_cursor,
                false,
                false,
            );

            for glyph in glyphs {
                let raster = cached_glyph(
                    &mut self.glyph_cache,
                    &self.font_system,
                    glyph.font_index,
                    glyph.glyph_id,
                    glyph.cells_wide,
                    glyph.synth_bold,
                    super::r#impl::drcs_geometry_class(&snap)
                        .map(|geometry| (geometry, snap.drcs_glyphs.clone())),
                );
                if raster.width == 0 || raster.height == 0 {
                    continue;
                }

                let scale_x = if is_double_wide { 2.0 } else { 1.0 };
                let gx = glyph.col as f32 * effective_cell_w as f32
                    + raster.bearing_x as f32 * scale_x
                    + glyph.x_offset * scale_x;
                let gx = gx + gutter_w as f32;
                let gy = y as f32 + baseline - raster.bearing_y as f32 - glyph.y_offset;
                blit_glyph(
                    buffer.as_mut(),
                    width,
                    height,
                    gx.round() as i32,
                    gy.round() as i32,
                    &raster,
                    glyph.fg,
                );
            }
        }

        paint_cursor_overlay(
            buffer.as_mut(),
            width,
            height,
            &snap,
            cell_w,
            cell_h,
            tab_bar_h,
            gutter_w,
        );

        if let Err(err) = buffer.present() {
            warn!("startup presenter: present failed: {err}");
        }

        if self.first_frame {
            self.first_frame = false;
            info!(
                "TTFP: {} ms",
                APP_START_TIME.get().unwrap().elapsed().as_millis()
            );
        }
    }
}

fn paint_tab_bar(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    title: &str,
    cell_w: i32,
    width: usize,
    height: usize,
    tab_bar_h: i32,
    bg: Srgb<u8>,
    fg: Srgb<u8>,
    hovered_button: Option<crate::renderer::TabBarHover>,
    maximized: bool,
) {
    let plan = build_tab_bar_plan(
        &[crate::renderer::r#impl::TabInfo {
            label: title,
            active: true,
        }],
        &snap.palette,
        hovered_button,
        maximized,
        width as f32,
        cell_w as f32,
    );

    fill_rect(
        buffer,
        width,
        height,
        0,
        0,
        width as i32,
        tab_bar_h,
        pack_rgb(plan.base_bg),
    );

    for tab in &plan.tabs {
        if let Some(tab_bg) = tab.bg {
            fill_rect(
                buffer,
                width,
                height,
                tab.x.round() as i32,
                0,
                tab.width.round() as i32,
                tab_bar_h,
                pack_rgb(tab_bg),
            );
        }
        let row = label_row(&tab.label, fg, bg, true);
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            tab.label_x,
            0.0,
        );
    }

    for tab in &plan.tabs {
        if let Some(separator) = tab.separator {
            fill_rect(
                buffer,
                width,
                height,
                tab.x.round() as i32 + tab.width.round() as i32,
                0,
                3,
                tab_bar_h,
                pack_rgb(separator),
            );
        }
    }

    if let Some(button_bg) = plan.new_tab_button.bg {
        fill_rect(
            buffer,
            width,
            height,
            plan.new_tab_button.x.round() as i32,
            0,
            plan.new_tab_button.width.round() as i32,
            tab_bar_h,
            pack_rgb(button_bg),
        );
    }
    let row = label_row(plan.new_tab_button.label, fg, plan.base_bg, false);
    let x = plan.new_tab_button.x + (plan.new_tab_button.width - cell_w as f32) * 0.5;
    paint_shaped_label(font_system, snap, buffer, width, height, &row, x, 0.0);

    for button in &plan.buttons {
        if let Some(button_bg) = button.bg {
            fill_rect(
                buffer,
                width,
                height,
                button.x.round() as i32,
                0,
                button.width.round() as i32,
                tab_bar_h,
                pack_rgb(button_bg),
            );
        }
        let row = label_row(button.label, fg, plan.base_bg, false);
        let x = button.x + (button.width - cell_w as f32) * 0.5;
        paint_shaped_label(font_system, snap, buffer, width, height, &row, x, 0.0);
    }
}

fn label_row(
    text: &str,
    fg: Srgb<u8>,
    bg: Srgb<u8>,
    prompt_start: bool,
) -> RowSnapshot {
    let len = text.graphemes(true).count();
    RowSnapshot {
        line_attr: LineAttr::Normal,
        fg: vec![fg; len],
        bg: vec![bg; len],
        attrs: vec![CellAttrs::default(); len],
        selected: vec![false; len],
        matched: vec![false; len],
        active_match: vec![false; len],
        cells: text
            .graphemes(true)
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect(),
        exit_status: None,
        has_link: vec![false; len],
        underline: vec![UnderlineStyle::None; len],
        underline_color: vec![None; len],
        prompt_start,
    }
}

fn paint_shaped_label(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    width: usize,
    height: usize,
    row: &RowSnapshot,
    x: f32,
    y: f32,
) {
    let glyphs = collect_row_glyphs(
        font_system,
        snap,
        row,
        u32::MAX,
        row.cells.len() as u32,
        None,
        false,
        false,
    );

    let baseline = font_system.baseline_offset();
    for glyph in glyphs {
        let raster = cached_glyph(
            &mut HashMap::new(),
            font_system,
            glyph.font_index,
            glyph.glyph_id,
            glyph.cells_wide,
            false,
            super::r#impl::drcs_geometry_class(snap)
                .map(|geometry| (geometry, snap.drcs_glyphs.clone())),
        );
        if raster.width == 0 || raster.height == 0 {
            continue;
        }

        let gx = x
            + glyph.col as f32 * font_system.cell_width as f32
            + raster.bearing_x as f32
            + glyph.x_offset;
        let gy = y + baseline - raster.bearing_y as f32 - glyph.y_offset;
        blit_glyph(
            buffer,
            width,
            height,
            gx.round() as i32,
            gy.round() as i32,
            &raster,
            glyph.fg,
        );
    }
}

fn paint_row_backgrounds(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    snap: &crate::renderer::r#impl::TermSnapshot,
    row: &crate::renderer::r#impl::RowSnapshot,
    row_idx: u32,
    y: i32,
    cell_w: i32,
    cell_h: i32,
    gutter_w: i32,
    block_cursor: Option<(u32, u32)>,
    has_background_image: bool,
) {
    for col in 0..row.attrs.len() {
        let x = gutter_w + col as i32 * cell_w;
        let painted = resolve_painted_cell(
            snap,
            row,
            row_idx,
            col as u32,
            block_cursor,
            has_background_image,
        );
        if let Some(bg) = painted.fill_bg {
            fill_rect(buffer, width, height, x, y, cell_w, cell_h, pack_rgb(bg));
        }
    }
}

fn paint_status_line_chrome(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    width: usize,
    height: usize,
    gutter_w: i32,
    cell_h: i32,
    tab_bar_h: i32,
) {
    let Some(row) = snap.status_line_row else {
        return;
    };
    let y = tab_bar_h + row as i32 * cell_h;
    let color = pack_rgb(snap.palette.status_line_fg);
    let total_w = gutter_w + snap.viewport_cols as i32 * font_system.cell_width as i32;
    fill_rect(buffer, width, height, 0, y, total_w, 1, color);
    fill_rect(buffer, width, height, 0, y + cell_h - 1, total_w, 1, color);
    fill_rect(buffer, width, height, 0, y, 1, cell_h, color);
    fill_rect(buffer, width, height, total_w - 1, y, 1, cell_h, color);

    if gutter_w <= 0 {
        return;
    }
    let row = status_line_label_row("⟫", &snap.palette);
    let x = ((gutter_w - font_system.cell_width as i32) / 2).max(0) as f32;
    paint_shaped_label(font_system, snap, buffer, width, height, &row, x, y as f32);
}

fn load_cached_background(path: &PathBuf) -> Option<CachedBackground> {
    let bytes = std::fs::read(path).ok()?;
    let decoded = decode_image(&bytes)?;
    let frame = decoded.frames.first()?.clone();
    Some(CachedBackground {
        width: decoded.width,
        height: decoded.height,
        pixels: frame.pixels,
    })
}

fn paint_cached_background(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    background: &CachedBackground,
) {
    if background.width as usize != width || background.height as usize != height {
        return;
    }
    for (dst, src) in buffer.iter_mut().zip(background.pixels.chunks_exact(4)) {
        *dst = blend_rgba_over(*dst, src[0], src[1], src[2], src[3]);
    }
}

fn blend_rgba_over(
    dst: u32,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
) -> u32 {
    if a == 255 {
        return ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
    }
    if a == 0 {
        return dst;
    }
    let alpha = a as u32;
    let inv = 255 - alpha;
    let dr = (dst >> 16) & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = dst & 0xff;
    let out_r = (r as u32 * alpha + dr * inv + 127) / 255;
    let out_g = (g as u32 * alpha + dg * inv + 127) / 255;
    let out_b = (b as u32 * alpha + db * inv + 127) / 255;
    (out_r << 16) | (out_g << 8) | out_b
}

fn paint_cursor_overlay(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    snap: &crate::renderer::r#impl::TermSnapshot,
    cell_w: i32,
    cell_h: i32,
    tab_bar_h: i32,
    gutter_w: i32,
) {
    let Some((row, col)) = snap.cursor else {
        return;
    };
    let color = pack_rgb(snap.palette.cursor.unwrap_or(snap.palette.fg));
    let x = gutter_w + col as i32 * cell_w;
    let y = tab_bar_h + row as i32 * cell_h;

    match snap.cursor_style.shape {
        CursorShape::Block => {}
        CursorShape::Underline => {
            let h = ((cell_h as f32 * 0.12).max(2.0)).round() as i32;
            fill_rect(buffer, width, height, x, y + cell_h - h, cell_w, h, color);
        }
        CursorShape::Beam => {
            let w = ((cell_w as f32 * 0.12).max(2.0)).round() as i32;
            fill_rect(buffer, width, height, x, y, w, cell_h, color);
        }
    }
}

fn paint_gutter_markers(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    snap: &TermSnapshot,
    gutter_w: i32,
    cell_h: i32,
    tab_bar_h: i32,
) {
    let bar_w = ((gutter_w as f32) * 0.6).max(3.0).round() as i32;
    let bar_x = (gutter_w - bar_w) / 2;
    let bar_h = ((cell_h as f32) * 0.9).round() as i32;
    let bar_y = (cell_h - bar_h) / 2;

    for (row_idx, row) in snap.rows.iter().enumerate() {
        if !row.prompt_start {
            continue;
        }
        let color = match row.exit_status {
            Some(0) => SUCCESS,
            Some(_) => FAILURE,
            None => RUNNING,
        };
        fill_rect(
            buffer,
            width,
            height,
            bar_x,
            tab_bar_h + row_idx as i32 * cell_h + bar_y,
            bar_w,
            bar_h,
            pack_rgb(Srgb::new(color[0], color[1], color[2])),
        );
    }
}

fn cached_glyph(
    cache: &mut HashMap<StartupGlyphKey, RasterizedGlyph>,
    font_system: &FontSystem,
    font_index: usize,
    glyph_id: u16,
    cells_wide: u8,
    synthetic_bold: bool,
    drcs: Option<(font41::DrcsGeometryClass, font41::DrcsGlyphMap)>,
) -> RasterizedGlyph {
    let synthetic_bold = synthetic_bold && font_system.font_is_color(font_index);
    let key = (
        font_index,
        glyph_id,
        cells_wide,
        synthetic_bold,
        drcs.as_ref().map(|(geometry, _)| *geometry),
    );
    if let Some(glyph) = cache.get(&key) {
        return glyph.clone();
    }

    let _drcs =
        drcs.map(|(geometry, glyphs)| font41::set_drcs_context(Some(geometry), Some(glyphs)));
    let mut glyph = font_system.rasterize_glyph(font_index, glyph_id, cells_wide as u32);
    if synthetic_bold {
        dilate_alpha(&mut glyph);
    }
    cache.insert(key, glyph.clone());
    glyph
}

fn dilate_alpha(glyph: &mut RasterizedGlyph) {
    let w = glyph.width as usize;
    let h = glyph.height as usize;
    if w == 0 || h == 0 {
        return;
    }
    let src = glyph.bitmap.clone();
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            for c in 0..4 {
                let here = src[i + c];
                let left = if x > 0 { src[i - 4 + c] } else { 0 };
                let right = if x + 1 < w { src[i + 4 + c] } else { 0 };
                glyph.bitmap[i + c] = here.max(left).max(right);
            }
        }
    }
}

fn blit_glyph(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    dst_x: i32,
    dst_y: i32,
    glyph: &RasterizedGlyph,
    fg: Srgb<u8>,
) {
    let glyph_w = glyph.width as usize;
    let glyph_h = glyph.height as usize;
    for gy in 0..glyph_h {
        let y = dst_y + gy as i32;
        if y < 0 || y >= height as i32 {
            continue;
        }
        for gx in 0..glyph_w {
            let x = dst_x + gx as i32;
            if x < 0 || x >= width as i32 {
                continue;
            }

            let src = (gy * glyph_w + gx) * 4;
            let alpha = glyph.bitmap[src + 3] as u32;
            if alpha == 0 {
                continue;
            }

            let (src_r, src_g, src_b) = if glyph.is_color {
                (
                    glyph.bitmap[src] as u32,
                    glyph.bitmap[src + 1] as u32,
                    glyph.bitmap[src + 2] as u32,
                )
            } else {
                (
                    fg.red as u32 * alpha / 255,
                    fg.green as u32 * alpha / 255,
                    fg.blue as u32 * alpha / 255,
                )
            };

            let idx = y as usize * width + x as usize;
            let dst = buffer[idx];
            let dst_r = (dst >> 16) & 0xFF;
            let dst_g = (dst >> 8) & 0xFF;
            let dst_b = dst & 0xFF;
            let inv = 255 - alpha;
            let out_r = src_r + dst_r * inv / 255;
            let out_g = src_g + dst_g * inv / 255;
            let out_b = src_b + dst_b * inv / 255;
            buffer[idx] = (out_r << 16) | (out_g << 8) | out_b;
        }
    }
}

fn clear(
    buffer: &mut [u32],
    color: u32,
) {
    buffer.fill(color);
}

fn fill_rect(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    color: u32,
) {
    let left = x.clamp(0, width as i32);
    let top = y.clamp(0, height as i32);
    let right = (x + w).clamp(0, width as i32);
    let bottom = (y + h).clamp(0, height as i32);
    if left >= right || top >= bottom {
        return;
    }
    for row in top as usize..bottom as usize {
        let start = row * width + left as usize;
        let end = row * width + right as usize;
        buffer[start..end].fill(color);
    }
}

fn pack_rgb(color: Srgb<u8>) -> u32 {
    ((color.red as u32) << 16) | ((color.green as u32) << 8) | color.blue as u32
}

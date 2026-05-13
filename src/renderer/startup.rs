use std::collections::HashMap;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use config41::ColorPalette;
use config41::CursorShape;
use font41::FontSystem;
use font41::RasterizedGlyph;
use font41::attrs::CellAttrs;
use image41::decode_image;
use palette::Srgb;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use smol_str::ToSmolStr;
use softbuffer::Context;
use softbuffer::Surface;
use terminal41::LineAttr;
use terminal41::RowSnapshot;
use terminal41::TermSnapshot;
use terminal41::VisibleImage;
use unicode_segmentation::UnicodeSegmentation;
use utils41::lerp_u8;
use winit::window::Window;

use crate::APP_START_TIME;
use crate::renderer::GUTTER_MENU_ITEMS;
use crate::renderer::GutterPopup;
use crate::renderer::POPUP_WIDTH_CELLS;
use crate::renderer::TAB_MENU_ITEMS;
use crate::renderer::TAB_MENU_WIDTH_CELLS;
use crate::renderer::TabContextMenu;
use crate::renderer::compute_gutter_width;
use crate::renderer::gutter_popup_origin;
use crate::renderer::r#impl::CURSOR_BLINK_HALF_PERIOD;
use crate::renderer::r#impl::CommandEditorBoxLayout;
use crate::renderer::r#impl::FAILURE;
use crate::renderer::r#impl::FrameLayout;
use crate::renderer::r#impl::RUNNING;
use crate::renderer::r#impl::SUCCESS;
use crate::renderer::r#impl::TabInfo;
use crate::renderer::r#impl::apply_terminal_layout_offsets;
use crate::renderer::r#impl::collect_row_glyphs;
use crate::renderer::r#impl::command_editor_box_layout;
use crate::renderer::r#impl::gutter_fill_bg_for_col0;
use crate::renderer::r#impl::row_hidden_by_sticky_prompt;
use crate::renderer::r#impl::snapshot_row_y;
use crate::renderer::r#impl::visible_command_editor;
use crate::renderer::paint::blink_animation_enabled;
use crate::renderer::paint::build_tab_bar_plan;
use crate::renderer::paint::centered_ink_origin_x;
use crate::renderer::paint::command_highlight_rgb;
use crate::renderer::paint::resolve_painted_cell;
use crate::renderer::paint::status_line_label_row;
use crate::renderer::paint::visible_row_cols;
use crate::window_host::CommandEditorPopupSide;
use crate::window_host::command_editor_popup_side_for_row;

type StartupGlyphKey = (usize, u16, u8, bool, Option<font41::DrcsGeometryClass>);

struct CachedBackground {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

struct StartupFrame {
    snap: TermSnapshot,
    visible_images: Vec<VisibleImage>,
    cursor_visible: bool,
    blink_off: bool,
    rapid_blink_off: bool,
}

pub(crate) struct StartupPresenter {
    _context: Context<Arc<Window>>,
    surface: Surface<Arc<Window>, Arc<Window>>,
    font_system: FontSystem,
    glyph_cache: HashMap<StartupGlyphKey, RasterizedGlyph>,
    gutter_enabled: bool,
    background: Option<CachedBackground>,
    first_frame: bool,
    terminal_rows: Vec<RowSnapshot>,
    terminal_row_generations: Vec<u64>,
}

impl StartupPresenter {
    pub(crate) fn new(
        window: Arc<Window>,
        fonts: Option<String>,
        font_size: f32,
        supersampling: u32,
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
            terminal_rows: Vec::new(),
            terminal_row_generations: Vec::new(),
        })
    }

    pub(crate) fn present(
        &mut self,
        window: &Arc<Window>,
        mut snap: TermSnapshot,
        visible_images: Vec<VisibleImage>,
        tabs: &[TabInfo<'_>],
        new_tab_text: SmolStr,
        hovered_button: Option<crate::renderer::TabBarHover>,
        tab_context_menu: Option<&TabContextMenu>,
        gutter_popup: Option<&GutterPopup>,
        command_editor: Option<&commands41::CommandLineView>,
        maximized: bool,
    ) -> Option<Duration> {
        let size = window.inner_size();
        let width = NonZeroU32::new(size.width.max(1))?;
        let height = NonZeroU32::new(size.height.max(1))?;

        if let Err(err) = self.surface.resize(width, height) {
            warn!("startup presenter: resize failed: {err}");
            return None;
        }

        let cell_w = self.font_system.cell_width as i32;
        let cell_h = self.font_system.cell_height as i32;
        let gutter_w = if self.gutter_enabled {
            compute_gutter_width(self.font_system.cell_width) as i32
        } else {
            0
        };
        let command_editor = visible_command_editor(command_editor, &snap);
        let mut layout = startup_frame_layout(&self.font_system, gutter_w, !tabs.is_empty());
        apply_terminal_layout_offsets(&mut layout, &snap, command_editor);

        self.apply_terminal_snapshot_rows(&snap);
        snap.rows = self.terminal_rows.clone();
        let frame = build_startup_frame(snap, visible_images, *APP_START_TIME.get().unwrap());

        let mut buffer = match self.surface.buffer_mut() {
            Ok(buffer) => buffer,
            Err(err) => {
                warn!("startup presenter: buffer acquisition failed: {err}");
                return None;
            }
        };

        let width = width.get() as usize;
        let height = height.get() as usize;
        let baseline = self.font_system.baseline_offset();
        let tab_bar_h = layout.tab_bar_h.round() as i32;

        clear(buffer.as_mut(), pack_rgb(frame.snap.palette.bg));
        if let Some(background) = self.background.as_ref() {
            paint_cached_background(buffer.as_mut(), width, height, background);
        }
        paint_tab_bar(
            &mut self.font_system,
            &frame.snap,
            buffer.as_mut(),
            tabs,
            new_tab_text,
            cell_w,
            width,
            height,
            tab_bar_h,
            frame.snap.palette.bg,
            frame.snap.palette.fg,
            hovered_button,
            maximized,
        );
        let terminal_cursor_visible = command_editor.is_none() && frame.cursor_visible;
        let block_cursor = match frame.snap.cursor_style.shape {
            CursorShape::Block if terminal_cursor_visible => frame.snap.cursor,
            _ => None,
        };

        for row in &frame.snap.rows {
            let row_idx = row.screen_row;
            if row_hidden_by_sticky_prompt(row, &frame.snap, &layout) {
                continue;
            }
            let y = snapshot_row_y(row_idx, &frame.snap, &layout).round() as i32;
            if y >= height as i32 {
                break;
            }

            let is_double_wide = !matches!(row.line_attr, LineAttr::Normal);
            let effective_cell_w = if is_double_wide { cell_w * 2 } else { cell_w };
            let visible_cols = visible_row_cols(&frame.snap, row);

            paint_row_backgrounds(
                buffer.as_mut(),
                width,
                height,
                &frame.snap,
                row,
                row_idx,
                y,
                effective_cell_w,
                cell_h,
                gutter_w,
                block_cursor,
                self.background.is_some(),
            );

            let glyphs = collect_row_glyphs(
                &mut self.font_system,
                &frame.snap,
                row,
                row_idx,
                visible_cols,
                block_cursor,
                frame.blink_off,
                frame.rapid_blink_off,
            );

            for glyph in glyphs {
                let raster = cached_glyph(
                    &mut self.glyph_cache,
                    &self.font_system,
                    glyph.font_index,
                    glyph.glyph_id,
                    glyph.cells_wide,
                    glyph.synth_bold,
                    super::r#impl::drcs_geometry_class(&frame.snap)
                        .map(|geometry| (geometry, frame.snap.drcs_glyphs.clone())),
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

        if gutter_w > 0 {
            paint_gutter_markers(buffer.as_mut(), width, height, &frame.snap, &layout);
        }
        paint_status_line_chrome(
            &mut self.font_system,
            &frame.snap,
            buffer.as_mut(),
            width,
            height,
            &layout,
        );

        paint_visible_images(
            buffer.as_mut(),
            width,
            height,
            &frame.visible_images,
            &layout,
            gutter_w,
        );
        paint_cursor_overlay(
            buffer.as_mut(),
            width,
            height,
            &frame.snap,
            terminal_cursor_visible,
            &layout,
            gutter_w,
        );
        if let Some(command_editor) = command_editor {
            paint_command_editor(
                &mut self.font_system,
                &frame.snap,
                command_editor,
                &layout,
                buffer.as_mut(),
                width,
                height,
            );
        }
        if gutter_w > 0
            && let Some(popup) = gutter_popup
        {
            paint_gutter_popup(
                &mut self.font_system,
                &frame.snap,
                buffer.as_mut(),
                width,
                height,
                popup,
                gutter_w,
                cell_w,
                cell_h,
                tab_bar_h,
            );
        }
        if let Some(menu) = tab_context_menu {
            paint_tab_context_menu(
                &mut self.font_system,
                &frame.snap,
                buffer.as_mut(),
                width,
                height,
                menu,
                cell_w,
                cell_h,
            );
        }

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

        next_startup_redraw_delay(&frame, *APP_START_TIME.get().unwrap())
    }

    fn apply_terminal_snapshot_rows(
        &mut self,
        snap: &TermSnapshot,
    ) {
        let total_rows = startup_cached_row_count(snap);
        if snap.reset_cached_rows || self.terminal_rows.len() != total_rows {
            self.terminal_rows = (0..total_rows)
                .map(|row| blank_startup_row(row as u32, snap.viewport_cols, &snap.palette))
                .collect();
            self.terminal_row_generations = vec![u64::MAX; total_rows];
        }

        for row in &snap.rows {
            let idx = row.screen_row as usize;
            if idx >= self.terminal_rows.len() {
                self.terminal_rows.resize_with(idx + 1, || {
                    blank_startup_row(0, snap.viewport_cols, &snap.palette)
                });
                self.terminal_rows[idx].screen_row = idx as u32;
                self.terminal_row_generations.resize(idx + 1, u64::MAX);
            }
            if self
                .terminal_row_generations
                .get(idx)
                .is_some_and(|generation| *generation == row.generation)
            {
                continue;
            }
            self.terminal_rows[idx] = row.clone();
            self.terminal_row_generations[idx] = row.generation;
        }
    }
}

fn startup_cached_row_count(snap: &TermSnapshot) -> usize {
    snap.rows
        .iter()
        .map(|row| row.screen_row as usize + 1)
        .max()
        .unwrap_or(0)
}

fn startup_frame_layout(
    font_system: &FontSystem,
    gutter_w: i32,
    tab_bar_visible: bool,
) -> FrameLayout {
    let cell_w = font_system.cell_width as f32;
    let cell_h = font_system.cell_height as f32;
    FrameLayout {
        cell_w,
        cell_h,
        baseline: font_system.baseline_offset(),
        gutter_px: gutter_w as f32,
        tab_bar_h: if tab_bar_visible { cell_h } else { 0.0 },
        terminal_y_offset: 0.0,
        block_y_offset: 0.0,
    }
}

fn blank_startup_row(
    screen_row: u32,
    cols: u32,
    palette: &ColorPalette,
) -> RowSnapshot {
    let cols = cols as usize;
    RowSnapshot {
        screen_row,
        generation: 0,
        cells: vec![smol_str::SmolStr::new_inline(" "); cols],
        attrs: vec![CellAttrs::default(); cols],
        fg: vec![palette.fg; cols],
        bg: vec![palette.bg; cols],
        underline_color: vec![None; cols],
        has_link: vec![false; cols],
        line_attr: LineAttr::Normal,
        selected: vec![false; cols],
        matched: vec![false; cols],
        active_match: vec![false; cols],
        prompt_start: false,
        exit_status: None,
        block_separator: false,
        sticky_prompt: false,
    }
}

fn build_startup_frame(
    snap: TermSnapshot,
    visible_images: Vec<VisibleImage>,
    started: Instant,
) -> StartupFrame {
    let elapsed = started.elapsed();
    let blink_off = blink_phase_off(elapsed, CURSOR_BLINK_HALF_PERIOD);
    let rapid_blink_off = blink_phase_off(elapsed, CURSOR_BLINK_HALF_PERIOD / 2);
    let cursor_visible = cursor_visible_for_frame(&snap, elapsed);
    StartupFrame {
        snap,
        visible_images,
        cursor_visible,
        blink_off,
        rapid_blink_off,
    }
}

fn blink_phase_off(
    elapsed: Duration,
    half_period: Duration,
) -> bool {
    let half_nanos = half_period.as_nanos().max(1);
    ((elapsed.as_nanos() / half_nanos) & 1) == 1
}

fn cursor_visible_for_frame(
    snap: &TermSnapshot,
    elapsed: Duration,
) -> bool {
    if snap.cursor.is_none() {
        return false;
    }
    if !snap.cursor_style.blink {
        return true;
    }
    let half = CURSOR_BLINK_HALF_PERIOD.as_secs_f32();
    let phase = (elapsed.as_secs_f32() / half) as u64;
    phase & 1 == 0
}

fn next_startup_redraw_delay(
    frame: &StartupFrame,
    started: Instant,
) -> Option<Duration> {
    let mut delay = None;
    if startup_frame_has_cursor_blink(frame)
        || startup_frame_has_blinking_text(frame, CellAttrs::BLINK)
    {
        delay = Some(duration_until_next_phase(started, CURSOR_BLINK_HALF_PERIOD));
    }
    if startup_frame_has_blinking_text(frame, CellAttrs::RAPID_BLINK) {
        let rapid_delay = duration_until_next_phase(started, CURSOR_BLINK_HALF_PERIOD / 2);
        delay = Some(delay.map_or(rapid_delay, |d| d.min(rapid_delay)));
    }
    if startup_frame_has_animated_images(frame) {
        delay = Some(delay.map_or(super::FRAME_DURATION, |d| d.min(super::FRAME_DURATION)));
    }
    delay
}

fn startup_frame_has_cursor_blink(frame: &StartupFrame) -> bool {
    frame.snap.cursor.is_some() && frame.snap.cursor_style.blink
}

fn startup_frame_has_blinking_text(
    frame: &StartupFrame,
    blink_attr: CellAttrs,
) -> bool {
    frame.snap.rows.iter().any(|row| {
        row.attrs
            .iter()
            .copied()
            .any(|attrs| attrs.contains(blink_attr) && blink_animation_enabled(&frame.snap, attrs))
    })
}

fn startup_frame_has_animated_images(frame: &StartupFrame) -> bool {
    frame
        .visible_images
        .iter()
        .any(|image| image.image.is_animated())
}

fn duration_until_next_phase(
    started: Instant,
    half_period: Duration,
) -> Duration {
    duration_until_next_phase_from_elapsed(started.elapsed(), half_period)
}

fn duration_until_next_phase_from_elapsed(
    elapsed: Duration,
    half_period: Duration,
) -> Duration {
    let elapsed_nanos = elapsed.as_nanos();
    let period_nanos = half_period.as_nanos();
    if period_nanos == 0 {
        return Duration::ZERO;
    }
    let remainder = elapsed_nanos % period_nanos;
    if remainder == 0 {
        half_period
    } else {
        Duration::from_nanos((period_nanos - remainder) as u64)
    }
}

fn paint_tab_bar(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    tabs: &[TabInfo<'_>],
    new_tab_text: SmolStr,
    cell_w: i32,
    width: usize,
    height: usize,
    tab_bar_h: i32,
    bg: Srgb<u8>,
    fg: Srgb<u8>,
    hovered_button: Option<crate::renderer::TabBarHover>,
    maximized: bool,
) {
    let tab_infos: Vec<TabInfo<'_>> = tabs
        .iter()
        .map(|tab| TabInfo {
            label: tab.label,
            active: tab.active,
        })
        .collect();
    let plan = build_tab_bar_plan(
        &tab_infos,
        &snap.palette,
        new_tab_text,
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
    let row = label_row(
        &plan.new_tab_button.label.to_smolstr(),
        fg,
        plan.base_bg,
        false,
    );
    let x = centered_label_x(
        font_system,
        snap,
        &row,
        plan.new_tab_button.x,
        plan.new_tab_button.width,
    );
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
        let x = centered_label_x(font_system, snap, &row, button.x, button.width);
        paint_shaped_label(font_system, snap, buffer, width, height, &row, x, 0.0);
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
}

fn paint_gutter_popup(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    width: usize,
    height: usize,
    popup: &GutterPopup,
    gutter_w: i32,
    cell_w: i32,
    cell_h: i32,
    tab_bar_h: i32,
) {
    let header_rows = usize::from(popup.duration_text.is_some());
    let total_rows = header_rows + GUTTER_MENU_ITEMS.len();
    let popup_w = (cell_w as f32 * POPUP_WIDTH_CELLS).round() as i32;
    let popup_h = total_rows as i32 * cell_h;
    let (popup_x, popup_y) = gutter_popup_origin(
        popup,
        popup_w as f32,
        popup_h as f32,
        cell_w as f32,
        cell_h as f32,
        gutter_w as f32,
        width as f32,
        height as f32,
    );
    let popup_x = popup_x.round() as i32;
    let popup_y = popup_y.round().max(tab_bar_h as f32) as i32;

    let panel_bg = Srgb::new(30, 30, 38);
    let border = Srgb::new(80, 80, 100);
    let dim_fg = Srgb::new(140, 140, 160);
    let normal_fg = Srgb::new(220, 220, 220);
    let hover_bg = Srgb::new(55, 55, 70);

    fill_rect(
        buffer,
        width,
        height,
        popup_x,
        popup_y,
        popup_w,
        popup_h,
        pack_rgb(panel_bg),
    );
    fill_rect(
        buffer,
        width,
        height,
        popup_x,
        popup_y,
        popup_w,
        1,
        pack_rgb(border),
    );
    fill_rect(
        buffer,
        width,
        height,
        popup_x,
        popup_y + popup_h - 1,
        popup_w,
        1,
        pack_rgb(border),
    );

    let margin = cell_w as f32 * 0.5;
    let max_chars = ((popup_w as f32 - margin * 2.0) / cell_w as f32).max(1.0) as usize;
    if let Some(duration) = popup.duration_text.as_ref() {
        let label: String = duration.chars().take(max_chars).collect();
        let row = label_row(&label, dim_fg, panel_bg, false);
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            popup_x as f32 + margin,
            popup_y as f32,
        );
    }

    for (idx, item) in GUTTER_MENU_ITEMS.iter().enumerate() {
        let row_y = popup_y + (header_rows + idx) as i32 * cell_h;
        if popup.hovered_item == Some(idx) {
            fill_rect(
                buffer,
                width,
                height,
                popup_x,
                row_y,
                popup_w,
                cell_h,
                pack_rgb(hover_bg),
            );
        }

        let label: String = item.label.chars().take(max_chars).collect();
        let row = label_row(&label, normal_fg, panel_bg, false);
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            popup_x as f32 + margin,
            row_y as f32,
        );
    }
}

fn paint_tab_context_menu(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    width: usize,
    height: usize,
    menu: &TabContextMenu,
    cell_w: i32,
    cell_h: i32,
) {
    let menu_w = (cell_w as f32 * TAB_MENU_WIDTH_CELLS).round() as i32;
    let menu_h = TAB_MENU_ITEMS.len() as i32 * cell_h;
    let menu_x = (menu.x.round() as i32).min(width as i32 - menu_w).max(0);
    let menu_y = cell_h;

    let panel_bg = Srgb::new(30, 30, 38);
    let border = Srgb::new(80, 80, 100);
    let normal_fg = Srgb::new(220, 220, 220);
    let hover_bg = Srgb::new(55, 55, 70);

    fill_rect(
        buffer,
        width,
        height,
        menu_x,
        menu_y,
        menu_w,
        menu_h,
        pack_rgb(panel_bg),
    );
    fill_rect(
        buffer,
        width,
        height,
        menu_x,
        menu_y,
        menu_w,
        1,
        pack_rgb(border),
    );
    fill_rect(
        buffer,
        width,
        height,
        menu_x,
        menu_y + menu_h - 1,
        menu_w,
        1,
        pack_rgb(border),
    );

    let margin = cell_w as f32 * 0.5;
    let max_chars = ((menu_w as f32 - margin * 2.0) / cell_w as f32).max(1.0) as usize;
    for (idx, item) in TAB_MENU_ITEMS.iter().enumerate() {
        let item_y = menu_y + idx as i32 * cell_h;
        if menu.hovered_item == Some(idx) {
            fill_rect(
                buffer,
                width,
                height,
                menu_x,
                item_y,
                menu_w,
                cell_h,
                pack_rgb(hover_bg),
            );
        }

        let label: String = item.label.chars().take(max_chars).collect();
        let row = label_row(&label, normal_fg, panel_bg, false);
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            menu_x as f32 + margin,
            item_y as f32,
        );
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
        screen_row: 0,
        generation: 0,
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
        block_separator: false,
        sticky_prompt: false,
        has_link: vec![false; len],
        underline_color: vec![None; len],
        prompt_start,
    }
}

fn centered_label_x(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    row: &RowSnapshot,
    region_x: f32,
    region_w: f32,
) -> f32 {
    let Some((left, right)) = software_label_ink_bounds(font_system, snap, row) else {
        return region_x;
    };

    centered_ink_origin_x(region_x, region_w, left, right)
}

fn software_label_ink_bounds(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    row: &RowSnapshot,
) -> Option<(f32, f32)> {
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
    let mut cache = HashMap::new();
    let mut left = f32::INFINITY;
    let mut right = f32::NEG_INFINITY;

    for glyph in glyphs {
        let raster = cached_glyph(
            &mut cache,
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

        let glyph_left = glyph.col as f32 * font_system.cell_width as f32
            + raster.bearing_x as f32
            + glyph.x_offset;
        let glyph_right = glyph_left + raster.width as f32;
        left = left.min(glyph_left);
        right = right.max(glyph_right);
    }

    left.is_finite().then_some((left, right))
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
    snap: &TermSnapshot,
    row: &RowSnapshot,
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
            if col == 0
                && gutter_w > 0
                && let Some(gutter_bg) =
                    gutter_fill_bg_for_col0(snap, row, row_idx, block_cursor, has_background_image)
            {
                fill_rect(
                    buffer,
                    width,
                    height,
                    0,
                    y,
                    gutter_w,
                    cell_h,
                    pack_rgb(gutter_bg),
                );
            }
            fill_rect(buffer, width, height, x, y, cell_w, cell_h, pack_rgb(bg));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_command_editor(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    editor: &commands41::CommandLineView,
    layout: &FrameLayout,
    buffer: &mut [u32],
    width: usize,
    height: usize,
) {
    let Some(box_layout) = command_editor_box_layout(snap, layout) else {
        return;
    };
    let border = 2.0;
    let lines = command_editor_line_ranges(&editor.text);
    let cursor = editor.cursor.min(editor.text.len());
    if !editor.text.is_char_boundary(cursor) {
        return;
    }
    let (cursor_line, cursor_line_start) = command_editor_cursor_line(&lines, cursor);
    let visible_start =
        command_editor_visible_line_start(lines.len(), cursor_line, box_layout.editor_rows);
    let visible_end = (visible_start + box_layout.editor_rows).min(lines.len());
    let has_overflow = lines.len() > box_layout.editor_rows;
    let scrollbar_cols = u32::from(has_overflow);
    let content_cols = snap.viewport_cols.saturating_sub(1 + scrollbar_cols).max(1) as usize;
    let cell_w = layout.cell_w;
    let cell_h = layout.cell_h;

    fill_rect_rgba(
        buffer,
        width,
        height,
        rect_i32(
            box_layout.editor_x,
            box_layout.box_y,
            box_layout.editor_w,
            box_layout.box_h,
        ),
        Srgb::new(18, 21, 29),
        248,
    );
    fill_rect_rgba(
        buffer,
        width,
        height,
        rect_i32(
            box_layout.editor_x,
            box_layout.box_y,
            box_layout.editor_w,
            border,
        ),
        Srgb::new(88, 150, 255),
        255,
    );

    if has_overflow {
        paint_command_editor_scrollbar(
            buffer,
            width,
            height,
            &box_layout,
            border,
            layout,
            visible_start,
            lines.len(),
        );
    }

    if let Some(selection) = editor.selection {
        let (selection_start, selection_end) = selection.ordered();
        for (visible_idx, &(line_start, line_end)) in
            lines[visible_start..visible_end].iter().enumerate()
        {
            let start = selection_start.max(line_start);
            let end = selection_end.min(line_end);
            if start >= end {
                continue;
            }
            let start_col = editor.text[line_start..start].graphemes(true).count();
            let end_col = editor.text[line_start..end]
                .graphemes(true)
                .count()
                .min(content_cols);
            if start_col >= end_col || start_col >= content_cols {
                continue;
            }
            fill_rect_rgba(
                buffer,
                width,
                height,
                rect_i32(
                    box_layout.content_x + start_col as f32 * cell_w,
                    box_layout.box_y + visible_idx as f32 * cell_h,
                    (end_col - start_col) as f32 * cell_w,
                    cell_h,
                ),
                Srgb::new(55, 84, 132),
                210,
            );
        }
    }

    for (visible_idx, &(line_start, line_end)) in
        lines[visible_start..visible_end].iter().enumerate()
    {
        let line_y = box_layout.box_y + visible_idx as f32 * cell_h;
        for span in &editor.spans {
            if span.start >= span.end || span.end > editor.text.len() {
                continue;
            }
            let start = span.start.max(line_start);
            let end = span.end.min(line_end);
            if start >= end {
                continue;
            }
            let segment = &editor.text[start..end];
            if segment.trim().is_empty() {
                continue;
            }
            let col = editor.text[line_start..start].graphemes(true).count();
            if col >= content_cols {
                continue;
            }
            let label = truncate_graphemes(segment, content_cols - col);
            let row = label_row(
                &label,
                command_highlight_rgb(span.kind),
                Srgb::new(18, 21, 29),
                false,
            );
            paint_shaped_label(
                font_system,
                snap,
                buffer,
                width,
                height,
                &row,
                box_layout.content_x + col as f32 * cell_w,
                line_y,
            );
        }
    }

    let cursor_line_visible = cursor_line >= visible_start && cursor_line < visible_end;
    let visible_cursor_line = cursor_line.saturating_sub(visible_start);
    let cursor_cell = editor.text[cursor_line_start..cursor]
        .graphemes(true)
        .count()
        .min(content_cols - 1);

    if let Some(completion) = editor.completion.as_deref()
        && cursor_line_visible
        && cursor_cell < content_cols
    {
        let label = truncate_graphemes(completion, content_cols - cursor_cell);
        let row = label_row(
            &label,
            Srgb::new(125, 136, 155),
            Srgb::new(18, 21, 29),
            false,
        );
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            box_layout.content_x + cursor_cell as f32 * cell_w,
            box_layout.box_y + visible_cursor_line as f32 * cell_h,
        );
    }

    if cursor_line_visible {
        match editor.cursor_style {
            commands41::CommandEditorCursorStyle::Beam => fill_rect_rgba(
                buffer,
                width,
                height,
                rect_i32(
                    box_layout.content_x + cursor_cell as f32 * cell_w,
                    box_layout.box_y + visible_cursor_line as f32 * cell_h + 2.0,
                    2.0,
                    cell_h - 4.0,
                ),
                Srgb::new(230, 235, 255),
                255,
            ),
            commands41::CommandEditorCursorStyle::Block => fill_rect_rgba(
                buffer,
                width,
                height,
                rect_i32(
                    box_layout.content_x + cursor_cell as f32 * cell_w,
                    box_layout.box_y + visible_cursor_line as f32 * cell_h + 1.0,
                    cell_w,
                    cell_h - 2.0,
                ),
                Srgb::new(230, 235, 255),
                175,
            ),
        }
    }

    if editor.candidates.is_empty() {
        return;
    }

    let list_cells = editor
        .candidates
        .iter()
        .map(|candidate| candidate.graphemes(true).count() + 2)
        .max()
        .unwrap_or(1)
        .min(content_cols)
        .max(1);
    let list_w = list_cells as f32 * cell_w;
    let list_h = editor.candidates.len() as f32 * cell_h;
    let cursor_y = box_layout.box_y + visible_cursor_line as f32 * cell_h;
    let editor_cursor_screen_row = box_layout.placement.top_row + visible_cursor_line as u32;
    let list_y =
        match command_editor_popup_side_for_row(editor_cursor_screen_row, snap.viewport_rows) {
            CommandEditorPopupSide::Below => {
                let preferred = cursor_y + cell_h;
                preferred
                    .min(layout.tab_bar_h + snap.viewport_rows as f32 * cell_h - list_h)
                    .max(layout.tab_bar_h)
            }
            CommandEditorPopupSide::Above => (cursor_y - list_h).max(layout.tab_bar_h),
        };

    fill_rect_rgba(
        buffer,
        width,
        height,
        rect_i32(box_layout.content_x, list_y, list_w, list_h),
        Srgb::new(22, 25, 34),
        245,
    );
    for (idx, candidate) in editor.candidates.iter().enumerate() {
        let row_y = list_y + idx as f32 * cell_h;
        let active = idx == editor.candidate_index;
        if active {
            fill_rect_rgba(
                buffer,
                width,
                height,
                rect_i32(box_layout.content_x, row_y, list_w, cell_h),
                Srgb::new(42, 55, 78),
                245,
            );
        }
        let label = truncate_graphemes(candidate, list_cells.saturating_sub(1));
        let fg = if active {
            Srgb::new(225, 232, 255)
        } else {
            Srgb::new(170, 180, 200)
        };
        let row = label_row(&label, fg, Srgb::new(22, 25, 34), false);
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            box_layout.content_x + cell_w,
            row_y,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_command_editor_scrollbar(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    box_layout: &CommandEditorBoxLayout,
    border: f32,
    layout: &FrameLayout,
    visible_start: usize,
    total_lines: usize,
) {
    let visible = box_layout.editor_rows.max(1);
    if total_lines <= visible {
        return;
    }
    let track_h = (box_layout.box_h - border).max(1.0);
    let track_w = (layout.cell_w * 0.18).max(2.0);
    let track_x = box_layout.box_x + box_layout.box_w - layout.cell_w * 0.5 - track_w * 0.5;
    let track_y = box_layout.box_y + border;
    fill_rect_rgba(
        buffer,
        width,
        height,
        rect_i32(track_x, track_y, track_w, track_h),
        Srgb::new(54, 62, 78),
        220,
    );

    let thumb_h = (track_h * visible as f32 / total_lines as f32).max(layout.cell_h * 0.45);
    let max_start = total_lines.saturating_sub(visible).max(1);
    let scroll_ratio = visible_start as f32 / max_start as f32;
    let thumb_y = track_y + (track_h - thumb_h).max(0.0) * scroll_ratio;
    fill_rect_rgba(
        buffer,
        width,
        height,
        rect_i32(track_x, thumb_y, track_w, thumb_h),
        Srgb::new(145, 160, 190),
        255,
    );
}

fn command_editor_line_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            ranges.push((start, idx));
            start = idx + ch.len_utf8();
        }
    }
    ranges.push((start, text.len()));
    ranges
}

fn command_editor_cursor_line(
    lines: &[(usize, usize)],
    cursor: usize,
) -> (usize, usize) {
    for (idx, &(start, end)) in lines.iter().enumerate() {
        if cursor <= end {
            return (idx, start);
        }
    }
    lines
        .last()
        .map(|&(start, _)| (lines.len().saturating_sub(1), start))
        .unwrap_or((0, 0))
}

fn command_editor_visible_line_start(
    line_count: usize,
    cursor_line: usize,
    visible_rows: usize,
) -> usize {
    let visible = visible_rows.max(1);
    if line_count <= visible {
        return 0;
    }
    cursor_line.saturating_add(1).saturating_sub(visible)
}

fn truncate_graphemes(
    text: &str,
    max_cells: usize,
) -> String {
    let mut graphemes = text.graphemes(true);
    let mut out = String::new();
    for _ in 0..max_cells {
        let Some(grapheme) = graphemes.next() else {
            return out;
        };
        out.push_str(grapheme);
    }
    if graphemes.next().is_some() && max_cells >= 3 {
        out.truncate(
            out.grapheme_indices(true)
                .nth(max_cells - 3)
                .map_or(0, |(idx, _)| idx),
        );
        out.push_str("...");
    }
    out
}

fn paint_status_line_chrome(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    width: usize,
    height: usize,
    layout: &FrameLayout,
) {
    let Some(row) = snap.status_line_row else {
        return;
    };
    let y = snapshot_row_y(row, snap, layout).round() as i32;
    let color = pack_rgb(snap.palette.status_line_fg);
    let gutter_w = layout.gutter_px.round() as i32;
    let cell_h = layout.cell_h.round() as i32;
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
    let aspect_buffer = background.width as f32 / background.height as f32;
    let aspect_window = width as f32 / height as f32;
    let (dst_x, dst_y, dst_w, dst_h) = if aspect_buffer > aspect_window {
        let w = width as i32;
        let h = (w as f32 / aspect_buffer).round() as i32;
        let x = 0;
        let y = ((height as i32 - h) / 2).max(0);
        (x, y, w, h)
    } else {
        let h = height as i32;
        let w = (h as f32 * aspect_buffer).round() as i32;
        let x = ((width as i32 - w) / 2).max(0);
        let y = 0;
        (x, y, w, h)
    };

    blit_scaled_rgba(
        buffer,
        width,
        height,
        0,
        0,
        dst_x,
        dst_y,
        dst_w,
        dst_h,
        background.width as usize,
        background.height as usize,
        &background.pixels,
    );
}

fn paint_visible_images(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    visible_images: &[VisibleImage],
    layout: &FrameLayout,
    gutter_w: i32,
) {
    for image in visible_images {
        let Some(frame) = image.image.frames.get(image.frame_index) else {
            continue;
        };
        if image.image.width == 0
            || image.image.height == 0
            || image.display_width == 0
            || image.display_height == 0
        {
            continue;
        }

        let dst_x = gutter_w
            + (image.screen_col as f32 * layout.cell_w + image.cell_x_offset as f32).round() as i32;
        let dst_y = (image.screen_row as f32 * layout.cell_h
            + layout.tab_bar_h
            + layout.terminal_y_offset
            + layout.block_y_offset
            + image.cell_y_offset as f32)
            .round() as i32;
        blit_scaled_rgba(
            buffer,
            width,
            height,
            gutter_w,
            layout.tab_bar_h.round() as i32,
            dst_x,
            dst_y,
            image.display_width as i32,
            image.display_height as i32,
            image.image.width as usize,
            image.image.height as usize,
            &frame.pixels,
        );
    }
}

fn blit_scaled_rgba(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    clip_left: i32,
    clip_top: i32,
    dst_x: i32,
    dst_y: i32,
    dst_w: i32,
    dst_h: i32,
    src_w: usize,
    src_h: usize,
    pixels: &[u8],
) {
    if dst_w <= 0 || dst_h <= 0 {
        return;
    }
    let expected = src_w.saturating_mul(src_h).saturating_mul(4);
    if src_w == 0 || src_h == 0 || pixels.len() < expected {
        return;
    }

    let min_x = dst_x.max(clip_left).max(0) as usize;
    let min_y = dst_y.max(clip_top).max(0) as usize;
    let max_x = (dst_x + dst_w).min(width as i32).max(0) as usize;
    let max_y = (dst_y + dst_h).min(height as i32).max(0) as usize;
    if min_x >= max_x || min_y >= max_y {
        return;
    }

    let scale_x = src_w as f32 / dst_w as f32;
    let scale_y = src_h as f32 / dst_h as f32;
    for y in min_y..max_y {
        let src_y = ((y as i32 - dst_y) as f32 * scale_y).clamp(0.0, (src_h - 1) as f32);
        for x in min_x..max_x {
            let src_x = ((x as i32 - dst_x) as f32 * scale_x).clamp(0.0, (src_w - 1) as f32);
            let rgba = sample_bilinear_rgba(pixels, src_w, src_h, src_x, src_y);
            let idx = y * width + x;
            buffer[idx] = blend_rgba_over(buffer[idx], rgba[0], rgba[1], rgba[2], rgba[3]);
        }
    }
}

fn sample_bilinear_rgba(
    pixels: &[u8],
    width: usize,
    height: usize,
    x: f32,
    y: f32,
) -> [u8; 4] {
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let c00 = rgba_at(pixels, width, x0, y0);
    let c10 = rgba_at(pixels, width, x1, y0);
    let c01 = rgba_at(pixels, width, x0, y1);
    let c11 = rgba_at(pixels, width, x1, y1);
    let mut out = [0u8; 4];
    for channel in 0..4 {
        let top = lerp_u8(c00[channel], c10[channel], tx);
        let bottom = lerp_u8(c01[channel], c11[channel], tx);
        out[channel] = lerp_u8(top, bottom, ty);
    }
    out
}

fn rgba_at(
    pixels: &[u8],
    width: usize,
    x: usize,
    y: usize,
) -> [u8; 4] {
    let idx = (y * width + x) * 4;
    [
        pixels[idx],
        pixels[idx + 1],
        pixels[idx + 2],
        pixels[idx + 3],
    ]
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
    snap: &TermSnapshot,
    cursor_visible: bool,
    layout: &FrameLayout,
    gutter_w: i32,
) {
    if !cursor_visible {
        return;
    }
    let Some((row, col)) = snap.cursor else {
        return;
    };
    let color = pack_rgb(snap.palette.cursor.unwrap_or(snap.palette.fg));
    let cell_w = layout.cell_w.round() as i32;
    let cell_h = layout.cell_h.round() as i32;
    let x = gutter_w + col as i32 * cell_w;
    let y = (row as f32 * layout.cell_h
        + layout.tab_bar_h
        + layout.terminal_y_offset
        + layout.block_y_offset)
        .round() as i32;

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
    layout: &FrameLayout,
) {
    let gutter_w = layout.gutter_px.round() as i32;
    let cell_h = layout.cell_h.round() as i32;
    let bar_w = ((gutter_w as f32) * 0.6).max(3.0).round() as i32;
    let bar_x = (gutter_w - bar_w) / 2;
    let bar_h = ((cell_h as f32) * 0.9).round() as i32;
    let bar_y = (cell_h - bar_h) / 2;

    for row in &snap.rows {
        if !row.prompt_start {
            continue;
        }
        if row_hidden_by_sticky_prompt(row, snap, layout) {
            continue;
        }
        let color = match row.exit_status {
            Some(0) => SUCCESS,
            Some(_) => FAILURE,
            None => RUNNING,
        };
        let y = snapshot_row_y(row.screen_row, snap, layout).round() as i32;
        fill_rect(
            buffer,
            width,
            height,
            bar_x,
            y + bar_y,
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

#[derive(Clone, Copy)]
struct PixelRect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

fn rect_i32(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
) -> PixelRect {
    PixelRect {
        x: x.round() as i32,
        y: y.round() as i32,
        w: w.round() as i32,
        h: h.round() as i32,
    }
}

fn fill_rect_rgba(
    buffer: &mut [u32],
    width: usize,
    height: usize,
    rect: PixelRect,
    color: Srgb<u8>,
    alpha: u8,
) {
    if alpha == 255 {
        fill_rect(
            buffer,
            width,
            height,
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            pack_rgb(color),
        );
        return;
    }

    let left = rect.x.clamp(0, width as i32);
    let top = rect.y.clamp(0, height as i32);
    let right = (rect.x + rect.w).clamp(0, width as i32);
    let bottom = (rect.y + rect.h).clamp(0, height as i32);
    if left >= right || top >= bottom {
        return;
    }
    for row in top as usize..bottom as usize {
        let start = row * width + left as usize;
        let end = row * width + right as usize;
        for pixel in &mut buffer[start..end] {
            *pixel = blend_rgba_over(*pixel, color.red, color.green, color.blue, alpha);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blink_phase_toggles_on_half_period_boundaries() {
        let half = Duration::from_millis(500);
        assert!(!blink_phase_off(Duration::from_millis(499), half));
        assert!(blink_phase_off(Duration::from_millis(500), half));
        assert!(blink_phase_off(Duration::from_millis(999), half));
        assert!(!blink_phase_off(Duration::from_millis(1000), half));
    }

    #[test]
    fn duration_until_next_phase_returns_remaining_half_period() {
        let remaining = duration_until_next_phase_from_elapsed(
            Duration::from_millis(125),
            Duration::from_millis(500),
        );
        assert_eq!(remaining, Duration::from_millis(375));
    }

    #[test]
    fn scaled_rgba_blit_clips_negative_destination() {
        let mut buffer = vec![0x000000; 4];
        let pixels = vec![255, 0, 0, 255];
        blit_scaled_rgba(&mut buffer, 2, 2, 0, 0, -1, -1, 2, 2, 1, 1, &pixels);

        assert_eq!(buffer[0], 0xff0000);
        assert_eq!(buffer[1], 0x000000);
        assert_eq!(buffer[2], 0x000000);
        assert_eq!(buffer[3], 0x000000);
    }

    #[test]
    fn scaled_rgba_blit_respects_content_clip() {
        let mut buffer = vec![0x000000; 4];
        let pixels = vec![255, 0, 0, 255];
        blit_scaled_rgba(&mut buffer, 2, 2, 0, 1, 0, 0, 2, 2, 1, 1, &pixels);

        assert_eq!(buffer[0], 0x000000);
        assert_eq!(buffer[1], 0x000000);
        assert_eq!(buffer[2], 0xff0000);
        assert_eq!(buffer[3], 0xff0000);
    }

    #[test]
    fn scaled_rgba_blit_alpha_blends_over_existing_pixel() {
        let mut buffer = vec![0x0000ff];
        let pixels = vec![255, 0, 0, 128];
        blit_scaled_rgba(&mut buffer, 1, 1, 0, 0, 0, 0, 1, 1, 1, 1, &pixels);

        assert_eq!(buffer[0], 0x80007f);
    }

    #[test]
    fn startup_cached_rows_do_not_expand_short_primary_screen_to_viewport() {
        let mut snap = test_snapshot(80, 24, &[0, 1]);

        assert_eq!(snap.total_rows, 24);
        assert_eq!(startup_cached_row_count(&snap), 2);

        snap.status_line_row = Some(24);
        snap.total_rows = 25;
        snap.rows.push(test_row(24, 80, &snap.palette));

        assert_eq!(startup_cached_row_count(&snap), 25);
    }

    fn test_snapshot(
        cols: u32,
        rows: u32,
        row_indices: &[u32],
    ) -> TermSnapshot {
        let palette = ColorPalette::default();
        TermSnapshot {
            generation: 0,
            rows: row_indices
                .iter()
                .copied()
                .map(|row| test_row(row, cols, &palette))
                .collect(),
            total_rows: rows,
            viewport_rows: rows,
            viewport_cols: cols,
            viewport_offset: 0,
            status_line_row: None,
            drcs_glyphs: std::sync::Arc::new(std::collections::HashMap::new()),
            dec_color: terminal41::dec_color_state_from_palette(&palette),
            palette,
            search_active: false,
            search: None,
            cursor: None,
            cursor_style: config41::CursorStyle::default(),
            screen_reverse: false,
            on_alt_screen: false,
            command_editor_hidden: false,
            synchronized_update_active: false,
            current_title: None,
            reset_cached_rows: true,
        }
    }

    fn test_row(
        screen_row: u32,
        cols: u32,
        palette: &ColorPalette,
    ) -> RowSnapshot {
        let mut row = blank_startup_row(screen_row, cols, palette);
        row.generation = screen_row as u64 + 1;
        row
    }
}

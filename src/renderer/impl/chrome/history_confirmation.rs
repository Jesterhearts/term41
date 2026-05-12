use super::super::*;
use super::popup::push_bordered_panel;

pub(in crate::renderer::r#impl) fn render_history_confirmation_modal(
    renderer: &mut Renderer,
    font_system: &mut FontSystem,
    modal: &crate::renderer::HistoryConfirmationModal,
    layout: &FrameLayout,
    bg_vertices: &mut Vec<BgVertex>,
    bg_indices: &mut Vec<u32>,
    fg: &mut FgGeometry,
) {
    let surface_w = renderer.surface_config.width as f32;
    let surface_h = renderer.surface_config.height as f32;
    let surface_cols = (surface_w / layout.cell_w).floor().max(1.0) as usize;
    let title_cells = modal.title.graphemes(true).count();
    let message_cells = modal.message.graphemes(true).count();
    let width_cells = title_cells
        .max(message_cells)
        .saturating_add(4)
        .clamp(36, 88)
        .min(surface_cols)
        .max(1);
    let panel_w = width_cells as f32 * layout.cell_w;
    let panel_h = 6.0 * layout.cell_h;
    let panel_x = ((surface_w - panel_w) * 0.5).max(0.0);
    let panel_y = ((surface_h - panel_h + layout.tab_bar_h) * 0.5).max(layout.tab_bar_h);
    let panel = (panel_x, panel_y, panel_w, panel_h);

    let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 255);
    let border = pack_color(&palette::Srgb::new(132, 132, 164), 255);
    let text_fg = pack_color(&palette::Srgb::new(238, 238, 244), 255);
    let hint_fg = pack_color(&palette::Srgb::new(202, 202, 214), 255);

    push_bordered_panel(panel, panel_bg, border, bg_vertices, bg_indices);

    let text_cells = width_cells.saturating_sub(4);
    let title: String = modal.title.graphemes(true).take(text_cells).collect();
    let message: String = modal.message.graphemes(true).take(text_cells).collect();
    super::shape_centered_popup_line(
        renderer,
        font_system,
        &title,
        panel,
        panel_y + layout.cell_h,
        layout.baseline,
        layout.cell_w,
        text_fg,
        fg,
    );
    super::shape_centered_popup_line(
        renderer,
        font_system,
        &message,
        panel,
        panel_y + 2.0 * layout.cell_h,
        layout.baseline,
        layout.cell_w,
        hint_fg,
        fg,
    );
    super::shape_centered_popup_line(
        renderer,
        font_system,
        "[enter] confirm   [esc] cancel",
        panel,
        panel_y + 4.0 * layout.cell_h,
        layout.baseline,
        layout.cell_w,
        hint_fg,
        fg,
    );
}

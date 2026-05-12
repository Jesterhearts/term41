use super::super::*;

impl Renderer {
    pub(in crate::renderer::r#impl) fn render_permission_modal(
        &mut self,
        font_system: &mut FontSystem,
        modal: &crate::renderer::PermissionModal,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let panel = crate::renderer::permission_panel_rect(
            &modal.feature,
            layout.cell_w,
            layout.cell_h,
            surface_w,
            surface_h,
            layout.tab_bar_h,
        );
        let buttons = crate::renderer::permission_button_layout(
            &modal.feature,
            layout.cell_w,
            layout.cell_h,
            surface_w,
            surface_h,
            layout.tab_bar_h,
        );

        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 255);
        let border = pack_color(&palette::Srgb::new(132, 132, 164), 255);
        let button_bg = pack_color(&palette::Srgb::new(46, 46, 58), 255);
        let button_hover = pack_color(&palette::Srgb::new(74, 74, 94), 255);
        let button_no_bg = pack_color(&palette::Srgb::new(52, 42, 46), 255);
        let button_no_hover = pack_color(&palette::Srgb::new(88, 58, 64), 255);
        let text_fg = pack_color(&palette::Srgb::new(238, 238, 244), 255);
        let hint_fg = pack_color(&palette::Srgb::new(202, 202, 214), 255);

        push_rect(
            panel.0,
            panel.1,
            panel.2,
            panel.3,
            panel_bg,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0,
            panel.1,
            panel.2,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0,
            panel.1 + panel.3 - 1.0,
            panel.2,
            1.0,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0,
            panel.1,
            1.0,
            panel.3,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            panel.0 + panel.2 - 1.0,
            panel.1,
            1.0,
            panel.3,
            border,
            bg_vertices,
            bg_indices,
        );

        let yes_bg = if modal.hovered == Some(crate::renderer::PermissionChoice::Allow) {
            button_hover
        } else {
            button_bg
        };
        let no_bg = if modal.hovered == Some(crate::renderer::PermissionChoice::Deny) {
            button_no_hover
        } else {
            button_no_bg
        };
        push_rect(
            buttons.yes.0,
            buttons.yes.1,
            buttons.yes.2,
            buttons.yes.3,
            yes_bg,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            buttons.no.0,
            buttons.no.1,
            buttons.no.2,
            buttons.no.3,
            no_bg,
            bg_vertices,
            bg_indices,
        );

        let baseline = font_system.baseline_offset();
        let feature_line = crate::renderer::permission_feature_line(&modal.feature);
        self.shape_centered_popup_line(
            font_system,
            &feature_line,
            panel,
            panel.1 + layout.cell_h,
            baseline,
            layout.cell_w,
            text_fg,
            fg,
        );
        self.shape_centered_popup_line(
            font_system,
            "Would you like to allow this?",
            panel,
            panel.1 + 2.0 * layout.cell_h,
            baseline,
            layout.cell_w,
            text_fg,
            fg,
        );
        self.shape_popup_line(
            font_system,
            "[y]es",
            buttons.yes.0 + layout.cell_w,
            buttons.yes.1,
            baseline,
            layout.cell_w,
            layout.cell_h,
            hint_fg,
            fg,
        );
        self.shape_popup_line(
            font_system,
            "[n]o",
            buttons.no.0 + layout.cell_w,
            buttons.no.1,
            baseline,
            layout.cell_w,
            layout.cell_h,
            hint_fg,
            fg,
        );
    }
}

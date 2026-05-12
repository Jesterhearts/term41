use super::super::*;

impl Renderer {
    /// close) are rendered at the right edge.
    pub(in crate::renderer::r#impl) fn render_tab_bar(
        &mut self,
        font_system: &mut FontSystem,
        tabs: &[TabInfo],
        palette: &ColorPalette,
        new_tab_text: SmolStr,
        controls: &WindowControls,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
        overlay_bg_vertices: &mut Vec<BgVertex>,
        overlay_bg_indices: &mut Vec<u32>,
        overlay_fg: &mut FgGeometry,
    ) {
        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let surface_w = self.surface_config.width as f32;
        let plan = build_tab_bar_plan(
            tabs,
            palette,
            new_tab_text,
            controls.hovered,
            controls.maximized,
            surface_w,
            cell_w,
        );

        // Full-width bar background (inactive colour as the base).
        let bar_bg = pack_color(&plan.base_bg, 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [0.0, 0.0],
                color: bar_bg,
            },
            BgVertex {
                pos: [surface_w, 0.0],
                color: bar_bg,
            },
            BgVertex {
                pos: [0.0, cell_h],
                color: bar_bg,
            },
            BgVertex {
                pos: [surface_w, cell_h],
                color: bar_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        let label_fg = pack_color(&palette.fg, 255);

        for tab in &plan.tabs {
            if let Some(bg) = tab.bg {
                let color = pack_color(&bg, 255);
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [tab.x, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width, 0.0],
                        color,
                    },
                    BgVertex {
                        pos: [tab.x, cell_h],
                        color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width, cell_h],
                        color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }

            self.shape_and_render_label(
                font_system,
                &tab.label,
                tab.label_x,
                0.0,
                baseline,
                cell_w,
                None,
                Some(cell_h),
                label_fg,
                fg,
            );
        }

        if let Some(bg) = plan.new_tab_button.bg {
            push_rect(
                plan.new_tab_button.x,
                0.0,
                plan.new_tab_button.width,
                cell_h,
                pack_color(&bg, 255),
                bg_vertices,
                bg_indices,
            );
        }
        self.shape_and_render_label(
            font_system,
            &plan.new_tab_button.label.to_smolstr(),
            plan.new_tab_button.x,
            0.0,
            baseline,
            cell_w,
            Some(plan.new_tab_button.width),
            Some(cell_h),
            label_fg,
            fg,
        );

        for button in &plan.buttons {
            if let Some(bg) = button.bg {
                push_rect(
                    button.x,
                    0.0,
                    button.width,
                    cell_h,
                    pack_color(&bg, 255),
                    bg_vertices,
                    bg_indices,
                );
            }
            self.shape_and_render_label(
                font_system,
                button.label,
                button.x,
                0.0,
                baseline,
                cell_w,
                Some(button.width),
                Some(cell_h),
                label_fg,
                fg,
            );
        }

        // ---- Tab context menu ----
        if let Some((menu_x, hovered_idx)) = controls.tab_menu {
            let menu_items = &crate::renderer::TAB_MENU_ITEMS;
            let menu_w = cell_w * crate::renderer::TAB_MENU_WIDTH_CELLS;
            let menu_h = menu_items.len() as f32 * cell_h;
            let mx = menu_x.min(surface_w - menu_w).max(0.0);
            let my = cell_h; // directly below the tab bar

            // Panel background.
            let panel_bg = pack_color(&Srgb::new(30, 30, 38), 255);
            push_rect(
                mx,
                my,
                menu_w,
                menu_h,
                panel_bg,
                overlay_bg_vertices,
                overlay_bg_indices,
            );

            // Border lines (top and bottom).
            let border_color = pack_color(&Srgb::new(80, 80, 100), 255);
            push_rect(
                mx,
                my,
                menu_w,
                1.0,
                border_color,
                overlay_bg_vertices,
                overlay_bg_indices,
            );
            push_rect(
                mx,
                my + menu_h - 1.0,
                menu_w,
                1.0,
                border_color,
                overlay_bg_vertices,
                overlay_bg_indices,
            );

            let normal_fg = pack_color(&Srgb::new(220, 220, 220), 255);
            let hover_bg = pack_color(&Srgb::new(55, 55, 70), 255);
            let margin = cell_w * 0.5;

            for (i, item) in menu_items.iter().enumerate() {
                let iy = my + i as f32 * cell_h;

                if hovered_idx == Some(i) {
                    push_rect(
                        mx,
                        iy,
                        menu_w,
                        cell_h,
                        hover_bg,
                        overlay_bg_vertices,
                        overlay_bg_indices,
                    );
                }

                self.shape_and_render_label(
                    font_system,
                    item.label,
                    mx + margin,
                    iy,
                    baseline,
                    cell_w,
                    None,
                    Some(cell_h),
                    normal_fg,
                    overlay_fg,
                );
            }
        }

        for tab in &plan.tabs {
            if let Some(separator) = tab.separator {
                let sep_w = 3.0_f32;
                let sep_color = pack_color(&separator, self.bg_alpha);
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [tab.x + tab.width, 0.0],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width + sep_w, 0.0],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width, cell_h],
                        color: sep_color,
                    },
                    BgVertex {
                        pos: [tab.x + tab.width + sep_w, cell_h],
                        color: sep_color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }
        }
    }
}

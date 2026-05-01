use super::*;

impl Renderer {
    /// close) are rendered at the right edge.
    pub(super) fn render_tab_bar(
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

    /// Shape a short text string and emit foreground glyph quads at the
    /// given position. Used by the tab bar for both tab labels and window
    /// control button glyphs.
    pub(super) fn shape_and_render_label(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        x: f32,
        y: f32,
        baseline: f32,
        cell_w: f32,
        centered_width: Option<f32>,
        fitted_height: Option<f32>,
        color: u32,
        fg: &mut FgGeometry,
    ) {
        let cells: Vec<smol_str::SmolStr> = text
            .graphemes(true)
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);
        let mut glyphs = Vec::with_capacity(shaped.len());

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            glyphs.push(LabelGlyph {
                slot,
                col: sg.col,
                x_offset: sg.x_offset,
                y_offset: sg.y_offset,
            });
        }

        let x = match (centered_width, label_ink_bounds(&glyphs, cell_w)) {
            (Some(width), Some((left, right))) => centered_ink_origin_x(x, width, left, right),
            _ => x,
        };
        let y = match (fitted_height, label_ink_y_bounds(&glyphs, baseline)) {
            (Some(height), Some((top, bottom))) => fitted_ink_origin_y(y, height, top, bottom),
            _ => y,
        };

        for glyph in glyphs {
            let sx = glyph.slot.x();
            let sy = glyph.slot.y();
            let sw = glyph.slot.width();
            let sh = glyph.slot.height();

            let gx = x + glyph.col as f32 * cell_w + glyph.slot.bearing_x as f32 + glyph.x_offset;
            let gx = gx.floor();

            let gy = y + baseline - glyph.slot.bearing_y as f32 - glyph.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;

            let flags: u32 = if glyph.slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                glyph.slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                ],
            );
        }
    }

    /// Paint the bottom-of-viewport search bar. The bar is a dark quad
    /// stretching across the viewport's last row, with a prompt + typed
    /// query + match counter shaped through the normal glyph atlas. A
    /// small caret marks the query's end so the user can see where their
    /// next keystroke will land.
    pub(super) fn render_search_bar(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        y_offset: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some(search) = &snap.search else {
            return;
        };

        let cell_w = font_system.cell_width as f32;
        let cell_h = font_system.cell_height as f32;
        let baseline = font_system.baseline_offset();
        let cols = snap.viewport_cols;
        let rows = snap.viewport_rows;
        if rows == 0 || cols == 0 {
            return;
        }

        // Build the visible label. The counter only appears once there are
        // matches to count — an empty query draws just the prompt so the
        // user sees something immediately on `Ctrl+Shift+F`.
        let counter = if search.match_count == 0 {
            if search.query.is_empty() {
                String::new()
            } else {
                "  (no match)".to_string()
            }
        } else {
            format!("  ({}/{})", search.active_idx + 1, search.match_count)
        };
        let label = format!("Find: {}{}", search.query, counter);

        // Truncate to fit the viewport width. We measure by char count —
        // one cell per char is the same approximation we use throughout
        // the ASCII-dominant pieces of this code.
        let max_chars = cols as usize;
        let label_graphemes: Vec<&str> = label.graphemes(true).take(max_chars).collect();

        // Caret sits at the end of the typed query, in column terms. The
        // prompt is exactly "Find: " (6 chars); the caret lives right
        // after the query text, clamped to the truncated label width.
        let prompt_len = "Find: ".chars().count() as u32;
        let caret_col = (prompt_len + search.query.chars().count() as u32).min(cols - 1);

        // Bar background: a dark opaque strip across the last row.
        let bar_y = (rows - 1) as f32 * cell_h + y_offset;
        let bar_w = cols as f32 * cell_w;
        let bar_bg = pack_color(&palette::Srgb::new(24, 24, 32), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [0.0, bar_y],
                color: bar_bg,
            },
            BgVertex {
                pos: [bar_w, bar_y],
                color: bar_bg,
            },
            BgVertex {
                pos: [0.0, bar_y + cell_h],
                color: bar_bg,
            },
            BgVertex {
                pos: [bar_w, bar_y + cell_h],
                color: bar_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Caret: a thin bright bar at the query insertion point so the
        // user can see where their next keystroke will go.
        let caret_x = caret_col as f32 * cell_w;
        let caret_w = (cell_w * 0.1).max(1.0);
        let caret_color = pack_color(&palette::Srgb::new(220, 220, 220), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [caret_x, bar_y + cell_h * 0.1],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x + caret_w, bar_y + cell_h * 0.1],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x, bar_y + cell_h * 0.9],
                color: caret_color,
            },
            BgVertex {
                pos: [caret_x + caret_w, bar_y + cell_h * 0.9],
                color: caret_color,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Label glyphs. Shape through the normal text pipeline so the bar
        // respects whatever font variants are loaded and goes through the
        // atlas LRU like any other glyph.
        let cells: Vec<smol_str::SmolStr> = label_graphemes
            .iter()
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        let label_fg = pack_color(&palette::Srgb::new(220, 220, 220), 255);
        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();

            let gx = sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gx = gx.floor();

            let gy = bar_y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;

            let flags: u32 = if slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: label_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: label_fg,
                        flags,
                    },
                ],
            );
        }
    }

    pub(super) fn render_status_line_chrome(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some(row) = snap.status_line_row else {
            return;
        };
        let y = layout.tab_bar_h + row as f32 * layout.cell_h;
        let border = pack_color(&snap.palette.status_line_fg, 255);
        let left = 0.0;
        let width = layout.gutter_px + snap.viewport_cols as f32 * layout.cell_w;
        let thickness = 1.0_f32.max((layout.cell_h * 0.04).round());
        push_rect(left, y, width, thickness, border, bg_vertices, bg_indices);
        push_rect(
            left,
            y + layout.cell_h - thickness,
            width,
            thickness,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            left,
            y,
            thickness,
            layout.cell_h,
            border,
            bg_vertices,
            bg_indices,
        );
        push_rect(
            left + width - thickness,
            y,
            thickness,
            layout.cell_h,
            border,
            bg_vertices,
            bg_indices,
        );

        if layout.gutter_px <= 0.0 {
            return;
        }

        let row = status_line_label_row("⟫", &snap.palette);
        let shaped = font_system.shape_row(&row.cells, &row.attrs);
        let baseline = font_system.baseline_offset();
        let cell_w = font_system.cell_width as f32;
        let marker_x = ((layout.gutter_px - cell_w) * 0.5).max(0.0);

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }
            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();
            let gx = marker_x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let flags: u32 = if slot.is_color { 1 } else { 0 };
            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx.floor(), gy.floor()],
                        uv: [sx as f32, sy as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor() + sw as f32, gy.floor()],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor(), gy.floor() + sh as f32],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: border,
                        flags,
                    },
                    FgVertex {
                        pos: [gx.floor() + sw as f32, gy.floor() + sh as f32],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: border,
                        flags,
                    },
                ],
            );
        }
    }

    /// Paint the gutter popup: a dark panel with an optional duration header
    /// and four action items, one per row. The hovered item gets a brighter
    /// background so the user sees where their click will land.
    pub(super) fn render_gutter_popup(
        &mut self,
        font_system: &mut FontSystem,
        popup: &GutterPopup,
        gutter_px: f32,
        cell_w: f32,
        cell_h: f32,
        tab_bar_h: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let baseline = font_system.baseline_offset();
        let surface_h = self.surface_config.height as f32;

        let header_rows = if popup.duration_text.is_some() { 1 } else { 0 };
        let total_rows = header_rows + GUTTER_MENU_ITEMS.len();
        let popup_w = cell_w * POPUP_WIDTH_CELLS;
        let popup_h = total_rows as f32 * cell_h;
        let popup_x = gutter_px;
        let popup_y = (popup.screen_row as f32 * cell_h + tab_bar_h)
            .min(surface_h - popup_h)
            .max(tab_bar_h);

        // Panel background.
        let panel_bg = pack_color(&palette::Srgb::new(30, 30, 38), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x, popup_y + popup_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y + popup_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Thin border at top and bottom.
        let border_color = pack_color(&palette::Srgb::new(80, 80, 100), 255);
        let border_h = 1.0_f32;
        for by in [popup_y, popup_y + popup_h - border_h] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, by],
                    color: border_color,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by],
                    color: border_color,
                },
                BgVertex {
                    pos: [popup_x, by + border_h],
                    color: border_color,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by + border_h],
                    color: border_color,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let margin = cell_w * 0.5;
        let max_chars = ((popup_w - margin * 2.0) / cell_w).max(1.0) as usize;

        // Duration header.
        if let Some(ref dur) = popup.duration_text {
            let label: String = dur.chars().take(max_chars).collect();
            let dim_fg = pack_color(&palette::Srgb::new(140, 140, 160), 255);
            self.shape_popup_line(
                font_system,
                &label,
                popup_x + margin,
                popup_y,
                baseline,
                cell_w,
                cell_h,
                dim_fg,
                fg,
            );
        }

        // Menu items.
        let normal_fg = pack_color(&palette::Srgb::new(220, 220, 220), 255);
        let hover_bg = pack_color(&palette::Srgb::new(55, 55, 70), 255);

        for (i, item) in GUTTER_MENU_ITEMS.iter().enumerate() {
            let row_y = popup_y + (header_rows + i) as f32 * cell_h;

            // Hover highlight.
            if popup.hovered_item == Some(i) {
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [popup_x, row_y],
                        color: hover_bg,
                    },
                    BgVertex {
                        pos: [popup_x + popup_w, row_y],
                        color: hover_bg,
                    },
                    BgVertex {
                        pos: [popup_x, row_y + cell_h],
                        color: hover_bg,
                    },
                    BgVertex {
                        pos: [popup_x + popup_w, row_y + cell_h],
                        color: hover_bg,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }

            let label: String = item.label.chars().take(max_chars).collect();
            self.shape_popup_line(
                font_system,
                &label,
                popup_x + margin,
                row_y,
                baseline,
                cell_w,
                cell_h,
                normal_fg,
                fg,
            );
        }
    }

    pub(super) fn render_recording_popup(
        &mut self,
        font_system: &mut FontSystem,
        popup: &crate::renderer::RecordingPopup,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        if popup.lines.is_empty() {
            return;
        }

        let baseline = font_system.baseline_offset();
        let margin_x = layout.cell_w;
        let margin_y = layout.cell_h * 0.5;
        let max_chars = popup
            .lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(1);
        let popup_w = (max_chars as f32 + 2.0) * layout.cell_w;
        let popup_h = popup.lines.len() as f32 * layout.cell_h + margin_y * 2.0;
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let popup_x = ((surface_w - popup_w) * 0.5).max(layout.gutter_px);
        let popup_y = ((surface_h - popup_h + layout.tab_bar_h) * 0.5).max(layout.tab_bar_h);

        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 244);
        let border = pack_color(&palette::Srgb::new(92, 92, 118), 255);
        let text_fg = pack_color(&palette::Srgb::new(232, 232, 236), 255);
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x, popup_y + popup_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y + popup_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        let border_h = 1.0_f32;
        for by in [popup_y, popup_y + popup_h - border_h] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x, by + border_h],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by + border_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let border_w = 1.0_f32;
        for bx in [popup_x, popup_x + popup_w - border_w] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [bx, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx, popup_y + popup_h],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y + popup_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        for (i, line) in popup.lines.iter().enumerate() {
            self.shape_popup_line(
                font_system,
                line,
                popup_x + margin_x,
                popup_y + margin_y + i as f32 * layout.cell_h,
                baseline,
                layout.cell_w,
                layout.cell_h,
                text_fg,
                fg,
            );
        }
    }

    pub(super) fn render_permission_modal(
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

        let dim = pack_color(&palette::Srgb::new(0, 0, 0), 120);
        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 248);
        let border = pack_color(&palette::Srgb::new(132, 132, 164), 255);
        let button_bg = pack_color(&palette::Srgb::new(46, 46, 58), 255);
        let button_hover = pack_color(&palette::Srgb::new(74, 74, 94), 255);
        let button_no_bg = pack_color(&palette::Srgb::new(52, 42, 46), 255);
        let button_no_hover = pack_color(&palette::Srgb::new(88, 58, 64), 255);
        let text_fg = pack_color(&palette::Srgb::new(238, 238, 244), 255);
        let hint_fg = pack_color(&palette::Srgb::new(202, 202, 214), 255);

        push_rect(0.0, 0.0, surface_w, surface_h, dim, bg_vertices, bg_indices);
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

    pub(super) fn shape_centered_popup_line(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        panel: (f32, f32, f32, f32),
        y: f32,
        baseline: f32,
        cell_w: f32,
        color: u32,
        fg: &mut FgGeometry,
    ) {
        let width = text.chars().count() as f32 * cell_w;
        let x = panel.0 + (panel.2 - width) * 0.5;
        self.shape_popup_line(font_system, text, x, y, baseline, cell_w, 0.0, color, fg);
    }

    pub(super) fn render_toast(
        &mut self,
        font_system: &mut FontSystem,
        toast: &crate::renderer::Toast,
        layout: &FrameLayout,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let text_chars = toast.text.chars().count();
        if text_chars == 0 {
            return;
        }

        let width_cells = (text_chars + 2).clamp(3, 100);
        let text_capacity = width_cells.saturating_sub(2);
        let text: String = toast.text.chars().take(text_capacity).collect();
        let popup_w = width_cells as f32 * layout.cell_w;
        let popup_h = 3.0 * layout.cell_h;
        let surface_w = self.surface_config.width as f32;
        let surface_h = self.surface_config.height as f32;
        let popup_x = (surface_w - popup_w).max(layout.gutter_px);
        let popup_y = (surface_h - popup_h).max(layout.tab_bar_h);

        let panel_bg = pack_color(&palette::Srgb::new(24, 24, 32), 244);
        let border = pack_color(&palette::Srgb::new(92, 92, 118), 255);
        let text_fg = pack_color(&palette::Srgb::new(232, 232, 236), 255);

        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [popup_x, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x, popup_y + popup_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [popup_x + popup_w, popup_y + popup_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        let border_h = 1.0_f32;
        for by in [popup_y, popup_y + popup_h - border_h] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [popup_x, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x, by + border_h],
                    color: border,
                },
                BgVertex {
                    pos: [popup_x + popup_w, by + border_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        let border_w = 1.0_f32;
        for bx in [popup_x, popup_x + popup_w - border_w] {
            let bi = bg_vertices.len() as u32;
            bg_vertices.extend_from_slice(&[
                BgVertex {
                    pos: [bx, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y],
                    color: border,
                },
                BgVertex {
                    pos: [bx, popup_y + popup_h],
                    color: border,
                },
                BgVertex {
                    pos: [bx + border_w, popup_y + popup_h],
                    color: border,
                },
            ]);
            bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
        }

        self.shape_popup_line(
            font_system,
            &text,
            popup_x + layout.cell_w,
            popup_y + layout.cell_h,
            font_system.baseline_offset(),
            layout.cell_w,
            layout.cell_h,
            text_fg,
            fg,
        );
    }

    /// Shape a single line of popup text and push its glyph quads.
    pub(super) fn shape_popup_line(
        &mut self,
        font_system: &mut FontSystem,
        text: &str,
        x: f32,
        y: f32,
        baseline: f32,
        cell_w: f32,
        _cell_h: f32,
        color: u32,
        fg: &mut FgGeometry,
    ) {
        let cells: Vec<smol_str::SmolStr> = text
            .chars()
            .map(|c| {
                let mut buf = [0u8; 4];
                smol_str::SmolStr::new_inline(c.encode_utf8(&mut buf))
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();

            let gx = x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gx = gx.floor();

            let gy = y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let gy = gy.floor();

            let gw = sw as f32;
            let gh = sh as f32;
            let flags: u32 = if slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color,
                        flags,
                    },
                ],
            );
        }
    }

    /// Paint the IME preedit: a darkened strip at the cursor cell, an
    /// underline that marks the whole composition as uncommitted, a
    /// thicker underline over the caret/highlighted segment the IME
    /// reported (when it did), and the composed glyphs themselves.
    ///
    /// Composition length is clipped to the remaining columns on the
    /// cursor's row; long compositions trail off past the right edge
    /// rather than wrapping. The IME's candidate popup sits below this
    /// overlay via `set_ime_cursor_area`, so the user can still see their
    /// options.
    pub(super) fn render_preedit(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        preedit: &crate::renderer::PreeditState,
        gutter_px: f32,
        cell_w: f32,
        cell_h: f32,
        baseline: f32,
        tab_bar_h: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some((cursor_row, cursor_col)) = snap.cursor else {
            return;
        };
        if preedit.text.is_empty() {
            return;
        }

        let origin_x = cursor_col as f32 * cell_w + gutter_px;
        let origin_y = cursor_row as f32 * cell_h + tab_bar_h;

        let max_chars = snap.viewport_cols.saturating_sub(cursor_col) as usize;
        if max_chars == 0 {
            return;
        }

        // Per-char iteration keeps the math simple — the overlay treats
        // every codepoint as one cell wide. That's wrong for CJK
        // full-width chars in general, but the preedit is a transient
        // overlay on top of the grid, and the candidate popup does the
        // real work of showing full-width layout options.
        let visible_graphemes: Vec<&str> = preedit.text.graphemes(true).take(max_chars).collect();
        let visible_len = visible_graphemes.len();
        if visible_len == 0 {
            return;
        }

        // Solid dark panel so the glyph being composed doesn't bleed
        // through the cells it's sitting on. Alpha is 255 because we
        // want full occlusion; the whole surface is already composited
        // with the window opacity.
        let panel_bg = pack_color(&palette::Srgb::new(40, 40, 55), 255);
        let panel_w = visible_len as f32 * cell_w;
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [origin_x, origin_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [origin_x + panel_w, origin_y],
                color: panel_bg,
            },
            BgVertex {
                pos: [origin_x, origin_y + cell_h],
                color: panel_bg,
            },
            BgVertex {
                pos: [origin_x + panel_w, origin_y + cell_h],
                color: panel_bg,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // Thin underline across the whole composition — the universal
        // "this text isn't committed yet" hint.
        let underline_color = pack_color(&palette::Srgb::new(180, 180, 220), 255);
        let underline_h = (cell_h * 0.08).max(1.5);
        let underline_y = origin_y + cell_h - underline_h;
        let bi = bg_vertices.len() as u32;
        bg_vertices.extend_from_slice(&[
            BgVertex {
                pos: [origin_x, underline_y],
                color: underline_color,
            },
            BgVertex {
                pos: [origin_x + panel_w, underline_y],
                color: underline_color,
            },
            BgVertex {
                pos: [origin_x, underline_y + underline_h],
                color: underline_color,
            },
            BgVertex {
                pos: [origin_x + panel_w, underline_y + underline_h],
                color: underline_color,
            },
        ]);
        bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);

        // The IME may mark a selected segment (the part the user is
        // currently editing inside a longer composition) via a byte range.
        // Paint a thicker bar over that segment so the user can see where
        // their next keystroke lands. Empty / full-span ranges just mean
        // "caret at position"; we skip them to avoid double-drawing the
        // whole underline.
        if let Some((start_byte, end_byte)) = preedit.cursor
            && start_byte != end_byte
        {
            let (seg_start_char, seg_end_char) =
                byte_range_to_char_range(&preedit.text, start_byte, end_byte, visible_len);
            if seg_end_char > seg_start_char {
                let seg_x = origin_x + seg_start_char as f32 * cell_w;
                let seg_w = (seg_end_char - seg_start_char) as f32 * cell_w;
                let seg_h = (cell_h * 0.14).max(2.5);
                let seg_y = origin_y + cell_h - seg_h;
                let bi = bg_vertices.len() as u32;
                bg_vertices.extend_from_slice(&[
                    BgVertex {
                        pos: [seg_x, seg_y],
                        color: underline_color,
                    },
                    BgVertex {
                        pos: [seg_x + seg_w, seg_y],
                        color: underline_color,
                    },
                    BgVertex {
                        pos: [seg_x, seg_y + seg_h],
                        color: underline_color,
                    },
                    BgVertex {
                        pos: [seg_x + seg_w, seg_y + seg_h],
                        color: underline_color,
                    },
                ]);
                bg_indices.extend_from_slice(&[bi, bi + 1, bi + 2, bi + 2, bi + 1, bi + 3]);
            }
        }

        // Shape the composing text through the same pipeline normal cells
        // use so fonts, ligatures, and fallback chains behave identically.
        let cells: Vec<smol_str::SmolStr> = visible_graphemes
            .iter()
            .map(|g| {
                let mut builder = SmolStrBuilder::new();
                builder.push_str(g);
                builder.finish()
            })
            .collect();
        let attrs = vec![CellAttrs::default(); cells.len()];
        let shaped = font_system.shape_row(&cells, &attrs);

        let glyph_fg = pack_color(&palette::Srgb::new(235, 235, 245), 255);
        for sg in &shaped {
            let slot = match self.glyph_atlas.ensure_cached(
                &self.device,
                &self.queue,
                font_system,
                sg.font_index,
                sg.glyph_id,
                sg.cells_wide,
                false,
                None,
            ) {
                Some(e) => e,
                None => continue,
            };
            if slot.is_empty() {
                continue;
            }

            let sx = slot.x();
            let sy = slot.y();
            let sw = slot.width();
            let sh = slot.height();

            let gx = origin_x + sg.col as f32 * cell_w + slot.bearing_x as f32 + sg.x_offset;
            let gx = gx.floor();

            let gy = origin_y + baseline - slot.bearing_y as f32 - sg.y_offset;
            let gy = gy.ceil();

            let gw = sw as f32;
            let gh = sh as f32;
            let flags: u32 = if slot.is_color { 1 } else { 0 };

            push_fg_quad(
                fg,
                slot.page_index,
                [
                    FgVertex {
                        pos: [gx, gy],
                        uv: [sx as f32, sy as f32],
                        color: glyph_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy],
                        uv: [(sx + sw) as f32, sy as f32],
                        color: glyph_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx, gy + gh],
                        uv: [sx as f32, (sy + sh) as f32],
                        color: glyph_fg,
                        flags,
                    },
                    FgVertex {
                        pos: [gx + gw, gy + gh],
                        uv: [(sx + sw) as f32, (sy + sh) as f32],
                        color: glyph_fg,
                        flags,
                    },
                ],
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn render_command_editor(
        &mut self,
        font_system: &mut FontSystem,
        snap: &TermSnapshot,
        editor: &commands41::CommandLineView,
        gutter_px: f32,
        cell_w: f32,
        cell_h: f32,
        baseline: f32,
        tab_bar_h: f32,
        bg_vertices: &mut Vec<BgVertex>,
        bg_indices: &mut Vec<u32>,
        fg: &mut FgGeometry,
    ) {
        let Some((cursor_row, cursor_col)) = snap.cursor else {
            return;
        };
        if editor.text.is_empty() && editor.completion.is_none() {
            return;
        }

        let origin_x = cursor_col as f32 * cell_w + gutter_px;
        let origin_y = cursor_row as f32 * cell_h + tab_bar_h;
        let text_cells = editor.text.graphemes(true).count();
        let completion_cells = editor
            .completion
            .as_deref()
            .map(|text| text.graphemes(true).count())
            .unwrap_or(0);
        let panel_cells = (text_cells + completion_cells + 1).max(1);
        let remaining_cols = snap.viewport_cols.saturating_sub(cursor_col).max(1) as usize;
        let panel_cells = panel_cells.min(remaining_cols);
        let panel_w = panel_cells as f32 * cell_w;

        push_rect(
            origin_x,
            origin_y,
            panel_w,
            cell_h,
            pack_color(&Srgb::new(28, 32, 42), 245),
            bg_vertices,
            bg_indices,
        );
        push_rect(
            origin_x,
            origin_y + cell_h - 2.0,
            panel_w,
            2.0,
            pack_color(&Srgb::new(88, 150, 255), 255),
            bg_vertices,
            bg_indices,
        );

        for span in &editor.spans {
            if span.start >= span.end || span.end > editor.text.len() {
                continue;
            }
            let segment = &editor.text[span.start..span.end];
            if segment.trim().is_empty() {
                continue;
            }
            let col = editor.text[..span.start].graphemes(true).count();
            if col >= remaining_cols {
                continue;
            }
            self.shape_and_render_label(
                font_system,
                segment,
                origin_x + col as f32 * cell_w,
                origin_y,
                baseline,
                cell_w,
                None,
                Some(cell_h),
                command_highlight_color(span.kind),
                fg,
            );
        }

        if let Some(completion) = editor.completion.as_deref() {
            let col = text_cells;
            if col < remaining_cols {
                self.shape_and_render_label(
                    font_system,
                    completion,
                    origin_x + col as f32 * cell_w,
                    origin_y,
                    baseline,
                    cell_w,
                    None,
                    Some(cell_h),
                    pack_color(&Srgb::new(125, 136, 155), 255),
                    fg,
                );
            }
        }

        let cursor = editor.cursor.min(editor.text.len());
        if !editor.text.is_char_boundary(cursor) {
            return;
        }
        let cursor_cell = editor.text[..cursor]
            .graphemes(true)
            .count()
            .min(remaining_cols - 1);
        push_rect(
            origin_x + cursor_cell as f32 * cell_w,
            origin_y + 2.0,
            2.0,
            cell_h - 4.0,
            pack_color(&Srgb::new(230, 235, 255), 255),
            bg_vertices,
            bg_indices,
        );
    }

    /// Resolve "is the cursor visible right now and what does it look like"
    /// once per frame. Hidden cases — scrolled away from live or in the
    /// blink-off phase — collapse to [`CursorRenderState::Hidden`] so the
    /// per-cell loops don't have to know the rules.
    /// Compute the cursor render state from the snapshot.
    pub(super) fn cursor_state_from_snapshot(
        &self,
        snap: &TermSnapshot,
    ) -> CursorRenderState {
        let Some((row, col)) = snap.cursor else {
            return CursorRenderState::Hidden;
        };
        let style = snap.cursor_style;
        if style.blink {
            let elapsed = APP_START_TIME.get().unwrap().elapsed().as_secs_f32();
            let half = CURSOR_BLINK_HALF_PERIOD.as_secs_f32();
            let phase = (elapsed / half) as u64;
            if phase & 1 == 1 {
                return CursorRenderState::Hidden;
            }
        }
        CursorRenderState::Visible {
            row,
            col,
            shape: style.shape,
        }
    }
}

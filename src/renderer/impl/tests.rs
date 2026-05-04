use super::*;

mod preedit_tests {
    use super::byte_range_to_char_range;

    #[test]
    fn ascii_range_maps_straight_through() {
        assert_eq!(byte_range_to_char_range("hello", 1, 4, 5), (1, 4));
    }

    #[test]
    fn full_range_caps_at_visible_len() {
        assert_eq!(byte_range_to_char_range("hi", 0, 2, 2), (0, 2));
    }

    #[test]
    fn range_past_visible_len_clamps() {
        // visible_len of 3 even though the string has 5 chars simulates
        // clipping at the right edge of the terminal.
        assert_eq!(byte_range_to_char_range("abcde", 0, 5, 3), (0, 3));
    }

    #[test]
    fn multibyte_range_counts_chars_not_bytes() {
        // 啊 and 不 are 3 bytes each in UTF-8. Byte range 3..6 = char 1..2.
        let text = "啊不";
        assert_eq!(byte_range_to_char_range(text, 3, 6, 2), (1, 2));
    }
}

mod geometry_tests {
    use std::sync::Arc;

    use config41::ColorPalette;
    use config41::CursorStyle;
    use font41::DrcsGeometryClass;
    use font41::attrs::CellAttrs;
    use palette::Srgb;
    use terminal41::LineAttr;

    use super::ClipRect;
    use super::FgGeometry;
    use super::FgVertex;
    use super::FrameLayout;
    use super::ImageGeometry;
    use super::ImageQuad;
    use super::PageDrawRange;
    use super::PageGeometryUpload;
    use super::RowGeometry;
    use super::RowSnapshot;
    use super::TermSnapshot;
    use super::append_gutter_marker;
    use super::clip_image_quad;
    use super::drcs_geometry_class;
    use super::fg_batch_for_page;
    use super::fitted_ink_origin_y;
    use super::gutter_marker_color;
    use super::image_batch_for_page;
    use super::image_render_order;
    use super::image_vertex_z;
    use super::snapshot_row_y;
    use super::terminal_block_y_offset_rows;
    use super::terminal_row_y;
    use super::visible_command_editor;

    fn blank_row(cols: usize) -> RowSnapshot {
        RowSnapshot {
            screen_row: 0,
            generation: 0,
            cells: vec![smol_str::SmolStr::new_inline(" "); cols],
            attrs: vec![CellAttrs::default(); cols],
            fg: vec![Srgb::new(255, 255, 255); cols],
            bg: vec![Srgb::new(0, 0, 0); cols],
            underline_color: vec![None; cols],
            has_link: vec![false; cols],
            line_attr: LineAttr::Normal,
            selected: vec![false; cols],
            matched: vec![false; cols],
            active_match: vec![false; cols],
            prompt_start: false,
            exit_status: None,
            block_separator: false,
        }
    }

    fn snapshot(
        cols: u32,
        rows: u32,
    ) -> TermSnapshot {
        let palette = ColorPalette::default();
        TermSnapshot {
            generation: 0,
            rows: (0..rows)
                .map(|row| {
                    let mut snapshot = blank_row(cols as usize);
                    snapshot.screen_row = row;
                    snapshot
                })
                .collect(),
            total_rows: rows,
            viewport_rows: rows,
            viewport_cols: cols,
            viewport_offset: 0,
            status_line_row: None,
            drcs_glyphs: Arc::new(std::collections::HashMap::new()),
            dec_color: terminal41::dec_color_state_from_palette(&palette),
            palette,
            search_active: false,
            search: None,
            cursor: None,
            cursor_style: CursorStyle::default(),
            screen_reverse: false,
            on_alt_screen: false,
            command_editor_hidden: false,
            synchronized_update_active: false,
            current_title: None,
            reset_cached_rows: true,
        }
    }

    #[test]
    fn terminal_blocks_bottom_align_when_shorter_than_viewport() {
        let mut snap = snapshot(4, 5);
        snap.rows.truncate(2);
        snap.rows[0].cells[0] = smol_str::SmolStr::new_inline("$");
        snap.rows[1].cells[0] = smol_str::SmolStr::new_inline("x");

        assert_eq!(terminal_block_y_offset_rows(&snap.rows, &snap), 3);
    }

    #[test]
    fn full_height_terminal_blocks_do_not_bottom_align_to_content() {
        let mut snap = snapshot(4, 5);
        snap.rows[0].cells[0] = smol_str::SmolStr::new_inline("$");
        snap.rows[1].cells[0] = smol_str::SmolStr::new_inline("x");
        snap.rows[2].cells[0] = smol_str::SmolStr::new_inline("y");

        assert_eq!(terminal_block_y_offset_rows(&snap.rows, &snap), 0);
    }

    #[test]
    fn image_batches_coalesce_adjacent_page_runs_only() {
        let mut geometry = ImageGeometry::default();
        image_batch_for_page(&mut geometry, 0);
        image_batch_for_page(&mut geometry, 0);
        image_batch_for_page(&mut geometry, 1);
        image_batch_for_page(&mut geometry, 0);

        let pages: Vec<usize> = geometry
            .batches
            .iter()
            .map(|batch| batch.page_index)
            .collect();
        assert_eq!(pages, vec![0, 1, 0]);
    }

    #[test]
    fn gutter_marker_geometry_is_row_local() {
        let mut row = blank_row(4);
        let mut geometry = RowGeometry::default();
        append_gutter_marker(&row, 12.0, 20.0, 5.0, &mut geometry);
        assert!(geometry.bg.indices.is_empty());

        row.screen_row = 2;
        row.prompt_start = true;
        row.exit_status = Some(0);
        append_gutter_marker(&row, 12.0, 20.0, 5.0, &mut geometry);

        assert_eq!(geometry.bg.vertices.len(), 4);
        assert_eq!(geometry.bg.indices.len(), 6);
        assert!((geometry.bg.vertices[0].pos[0] - 2.4).abs() < 0.0001);
        assert!((geometry.bg.vertices[0].pos[1] - 6.0).abs() < 0.0001);
        assert!((geometry.bg.vertices[3].pos[0] - 9.6).abs() < 0.0001);
        assert!((geometry.bg.vertices[3].pos[1] - 24.0).abs() < 0.0001);
        assert_eq!(geometry.bg.vertices[0].color, gutter_marker_color(Some(0)));
    }

    #[test]
    fn image_quad_clip_trims_position_and_uvs_to_terminal_content() {
        let vertices = clip_image_quad(
            ImageQuad {
                left: 10.0,
                top: -20.0,
                right: 110.0,
                bottom: 80.0,
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
                z: 0.25,
            },
            ClipRect {
                left: 20.0,
                top: 10.0,
                right: 90.0,
                bottom: 70.0,
            },
        )
        .expect("quad overlaps clip rect");

        assert_eq!(vertices[0].pos, [20.0, 10.0]);
        assert_eq!(vertices[1].pos, [90.0, 10.0]);
        assert_eq!(vertices[2].pos, [20.0, 70.0]);
        assert_eq!(vertices[3].pos, [90.0, 70.0]);
        assert_close(vertices[0].uv, [0.1, 0.3]);
        assert_close(vertices[3].uv, [0.8, 0.9]);
        assert_eq!(vertices[0].z, 0.25);
        assert_eq!(vertices[3].z, 0.25);
    }

    #[test]
    fn image_quad_clip_drops_chrome_only_quad() {
        let vertices = clip_image_quad(
            ImageQuad {
                left: 10.0,
                top: 0.0,
                right: 110.0,
                bottom: 9.0,
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
                z: 0.0,
            },
            ClipRect {
                left: 0.0,
                top: 10.0,
                right: 120.0,
                bottom: 100.0,
            },
        );

        assert!(vertices.is_none());
    }

    #[test]
    fn image_vertex_z_is_ordered_and_inside_depth_range() {
        let first = image_vertex_z(0, 3);
        let second = image_vertex_z(1, 3);
        let third = image_vertex_z(2, 3);

        assert!(first > 0.0);
        assert!(first < second);
        assert!(second < third);
        assert!(third < 1.0);
    }

    #[test]
    fn image_render_order_uses_page_position_within_z_index() {
        let layout = FrameLayout {
            cell_w: 10.0,
            cell_h: 20.0,
            baseline: 14.0,
            gutter_px: 0.0,
            tab_bar_h: 0.0,
            terminal_y_offset: 0.0,
            block_y_offset: 0.0,
        };
        let mut images = [
            visible_image(1, 0, 8, 0, 0, 0),
            visible_image(2, 1, 0, 0, 0, 0),
            visible_image(3, 0, 9, 0, 0, 0),
            visible_image(4, 0, 0, 0, 5, 0),
        ];

        images.sort_by(|left, right| image_render_order(left, right, &layout));

        let ids: Vec<u64> = images.iter().map(|image| image.id).collect();
        assert_eq!(ids, vec![1, 3, 4, 2]);
    }

    #[test]
    fn terminal_row_y_includes_editor_offset_for_chrome_alignment() {
        let layout = FrameLayout {
            cell_w: 10.0,
            cell_h: 20.0,
            baseline: 14.0,
            gutter_px: 0.0,
            tab_bar_h: 20.0,
            terminal_y_offset: -60.0,
            block_y_offset: 0.0,
        };

        assert_eq!(terminal_row_y(5, &layout), 60.0);
    }

    #[test]
    fn status_row_y_ignores_editor_offset() {
        let mut snap = snapshot(80, 24);
        snap.status_line_row = Some(24);
        let layout = FrameLayout {
            cell_w: 10.0,
            cell_h: 20.0,
            baseline: 14.0,
            gutter_px: 0.0,
            tab_bar_h: 20.0,
            terminal_y_offset: -60.0,
            block_y_offset: 0.0,
        };

        assert_eq!(snapshot_row_y(23, &snap, &layout), 420.0);
        assert_eq!(snapshot_row_y(24, &snap, &layout), 500.0);
    }

    #[test]
    fn command_editor_is_hidden_while_scrolled_back() {
        let mut snap = snapshot(80, 24);
        let view = commands41::CommandLineView {
            text: String::new(),
            cursor: 0,
            cursor_style: commands41::CommandEditorCursorStyle::Beam,
            spans: Vec::new(),
            selection: None,
            completion: None,
            candidates: Vec::new(),
            candidate_index: 0,
        };

        assert!(visible_command_editor(Some(&view), &snap).is_some());

        snap.viewport_offset = 1;
        assert!(visible_command_editor(Some(&view), &snap).is_none());

        snap.viewport_offset = 0;
        snap.command_editor_hidden = true;
        assert!(visible_command_editor(Some(&view), &snap).is_none());
    }

    fn visible_image(
        id: u64,
        screen_row: i32,
        screen_col: u32,
        cell_x_offset: u32,
        cell_y_offset: u32,
        z_index: i32,
    ) -> terminal41::VisibleImage {
        terminal41::VisibleImage {
            image: image41::DecodedImage::single_frame(1, 1, vec![0, 0, 0, 255]),
            id,
            kitty_image_id: None,
            screen_row,
            screen_col,
            cell_x_offset,
            cell_y_offset,
            display_width: 1,
            display_height: 1,
            frame_index: 0,
            z_index,
        }
    }

    fn assert_close(
        actual: [f32; 2],
        expected: [f32; 2],
    ) {
        assert!((actual[0] - expected[0]).abs() < f32::EPSILON * 8.0);
        assert!((actual[1] - expected[1]).abs() < f32::EPSILON * 8.0);
    }

    #[test]
    fn fg_batches_coalesce_adjacent_page_runs_only() {
        let mut geometry = FgGeometry::default();
        fg_batch_for_page(&mut geometry, 0);
        fg_batch_for_page(&mut geometry, 0);
        fg_batch_for_page(&mut geometry, 1);
        fg_batch_for_page(&mut geometry, 0);

        let pages: Vec<usize> = geometry
            .batches
            .iter()
            .map(|batch| batch.page_index)
            .collect();
        assert_eq!(pages, vec![0, 1, 0]);
    }

    #[test]
    fn label_fit_moves_top_edge_inside_row() {
        assert_eq!(fitted_ink_origin_y(0.0, 28.0, -0.5, 18.0), 1.5);
    }

    #[test]
    fn label_fit_prefers_top_inset_when_ink_is_taller_than_row() {
        assert_eq!(fitted_ink_origin_y(0.0, 10.0, -3.0, 12.0), 4.0);
    }

    #[test]
    fn page_geometry_upload_records_draw_ranges_without_rewriting_indices() {
        let vertex = FgVertex {
            pos: [0.0, 0.0],
            uv: [0.0, 0.0],
            color: 0,
            flags: 0,
        };
        let mut upload = PageGeometryUpload::default();

        upload.push_batch(2, &[vertex; 4], &[0, 1, 2, 2, 1, 3]);
        upload.push_batch(3, &[vertex; 4], &[0, 1, 2, 2, 1, 3]);

        assert_eq!(
            upload.ranges,
            vec![
                PageDrawRange {
                    page_index: 2,
                    index_start: 0,
                    index_count: 6,
                    vertex_base: 0,
                },
                PageDrawRange {
                    page_index: 3,
                    index_start: 6,
                    index_count: 6,
                    vertex_base: 4,
                },
            ]
        );
        assert_eq!(upload.indices, vec![0, 1, 2, 2, 1, 3, 0, 1, 2, 2, 1, 3]);
    }

    #[test]
    fn drcs_geometry_class_buckets_to_nearest_compatible_size() {
        assert_eq!(
            drcs_geometry_class(&snapshot(80, 24)),
            Some(DrcsGeometryClass::Col80Line24)
        );
        assert_eq!(
            drcs_geometry_class(&snapshot(80, 30)),
            Some(DrcsGeometryClass::Col80Line36)
        );
        assert_eq!(
            drcs_geometry_class(&snapshot(80, 60)),
            Some(DrcsGeometryClass::Col80Line48)
        );
        assert_eq!(
            drcs_geometry_class(&snapshot(100, 30)),
            Some(DrcsGeometryClass::Col132Line36)
        );
    }
}

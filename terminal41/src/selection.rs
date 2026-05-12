//! Text-selection model for mouse-driven copy.
//!
//! Positions are stored in **absolute** row coordinates -- `total_popped +
//! index` into the grid -- so selections stay anchored to their content even
//! as scrollback trims the front of the grid or the user scrolls history.

mod active;
mod coords;
mod model;
mod rendered;
pub mod search;
mod text;
mod word;

pub use active::extend_selection;
pub use active::extend_selection_from_start;
pub use active::is_cell_selected;
pub use active::start_selection;
pub(crate) use coords::absolute_row_to_local;
pub use coords::active_screen_row_at_viewport_row;
pub(crate) use coords::active_viewport;
pub use coords::rendered_screen_row_at_viewport_row;
pub use model::Selection;
pub use model::SelectionMode;
pub use model::SelectionPoint;
pub use rendered::extend_rendered_selection;
pub use rendered::is_rendered_cell_selected;
pub use rendered::rendered_document_row_at_viewport_row;
pub use rendered::start_rendered_selection;
pub use search::close_search;
pub use search::is_cell_active_match;
pub use search::is_cell_match;
pub use search::open_search;
pub use search::search_active;
pub use search::search_append;
pub use search::search_backspace;
pub use search::search_state;
pub use search::search_step_next;
pub use search::search_step_prev;
pub use text::copy_selection;
pub use text::selection_text;
pub use word::expand_to_line;
pub use word::expand_to_word;

#[cfg(test)]
mod integration_tests {
    use clip41::Clipboard;
    use clip41::ClipboardKind;

    use super::*;
    use crate::test_support::TestTerm;

    fn write_row(
        term: &mut TestTerm,
        screen_row: u32,
        text: &str,
    ) {
        term.process(format!("\x1b[{};1H", screen_row + 1).as_bytes());
        term.process(text.as_bytes());
    }

    #[test]
    fn start_selection_char_mode_is_empty_initially() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            1,
            SelectionMode::Char,
        );
        assert!(term.selection.is_some());
        assert!(!term.has_selection());
    }

    #[test]
    fn char_selection_extend_produces_text() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn viewport_mouse_row_maps_to_bottom_aligned_active_row() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");

        assert_eq!(
            rendered_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                0,
            ),
            None
        );
        assert_eq!(
            rendered_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                2,
            ),
            Some(0)
        );
        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                2,
            ),
            Some(0)
        );

        let row = active_screen_row_at_viewport_row(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            2,
        )
        .unwrap();
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            row,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            row,
        );

        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn viewport_mouse_row_does_not_select_completed_command_blocks() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);
        term.process(b"old");
        term.process(b"\x1b]133;A\x07new");

        assert_eq!(
            rendered_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                1,
            ),
            Some(0)
        );
        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                1,
            ),
            None
        );
        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                3,
            ),
            Some(0)
        );
    }

    #[test]
    fn viewport_mouse_row_maps_to_active_row_after_multiple_command_blocks() {
        let mut term = TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");

        assert_eq!(
            active_screen_row_at_viewport_row(
                &term.inner.active,
                &term.inner.viewport,
                term.inner.on_alt_screen,
                4,
            ),
            Some(0)
        );
    }

    #[test]
    fn rendered_mouse_selection_can_copy_completed_command_blocks() {
        let mut term = TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");

        term.inner.selection = start_rendered_selection(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_rendered_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            2,
            2,
        );

        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("one\n\ntwo")
        );
    }

    #[test]
    fn bottom_aligned_rendered_mouse_selection_can_copy_visible_blocks() {
        let mut term = TestTerm::new(10, 8, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");

        term.inner.selection = start_rendered_selection(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            0,
            3,
            SelectionMode::Char,
        );
        term.inner.selection = extend_rendered_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            2,
            5,
        );

        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("one\n\ntwo")
        );
    }

    #[test]
    fn rendered_mouse_selection_uses_active_grid_on_alt_screen() {
        let mut term = TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");
        term.process(b"\x1b[?1049h");
        term.process(b"alpha\r\nbeta");

        term.inner.selection = start_rendered_selection(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_rendered_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            3,
            1,
        );

        assert_eq!(
            term.inner
                .selection
                .as_ref()
                .map(|selection| selection.rendered),
            Some(false)
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("alpha\nbeta")
        );
    }

    #[test]
    fn rendered_mouse_selection_uses_retained_active_rows_after_scrollback_recycling() {
        let mut term = TestTerm::new(8, 3, 2, 16, 8);
        for i in 0..8 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.process(b"tail");
        assert!(term.inner.active.grid.total_popped > 0);

        let bottom_row = term.inner.viewport.rows - 1;
        term.inner.selection = start_rendered_selection(
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            0,
            bottom_row,
            SelectionMode::Char,
        );
        term.inner.selection = extend_rendered_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            term.inner.on_alt_screen,
            3,
            bottom_row,
        );

        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("tail")
        );

        let snap = crate::snapshot::snapshot_terminal(&mut term.inner);
        assert_eq!(&snap.rows.last().unwrap().selected[..4], &[true; 4]);
    }

    #[test]
    fn word_selection_snaps_to_boundaries() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
            SelectionMode::Word,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello")
        );
    }

    #[test]
    fn line_selection_covers_full_row_through_test_term() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            5,
            0,
            SelectionMode::Line,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn selection_spans_rows_with_newline_separator() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "abc");
        write_row(&mut term, 1, "def");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            2,
            1,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("abc\ndef")
        );
    }

    #[test]
    fn selection_can_extend_into_scrolled_history() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        for i in 0..6 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.process(b"tail");
        assert!(term.active.grid.scrollback_len(&term.viewport) > 0);

        let live_bottom = term.viewport.rows - 1;
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            3,
            live_bottom,
            SelectionMode::Char,
        );
        let origin = term.inner.selection.as_ref().unwrap().origin;

        let viewport = term.inner.viewport;
        crate::view::scroll_viewport_up(&mut term.inner.active, &viewport, 1);
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
        );

        let selection = term.inner.selection.as_ref().unwrap();
        let (start, end) = selection.ordered();
        assert!(start.row < origin.row);
        assert_eq!(end, origin);
        assert!(
            selection_text(Some(selection), &term.inner.active)
                .unwrap()
                .contains("tail")
        );
    }

    #[test]
    fn shift_extension_uses_selection_start_after_viewport_scroll() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        for i in 0..6 {
            term.process(format!("line{i}\r\n").as_bytes());
        }
        term.process(b"tail");

        let live_bottom = term.viewport.rows - 1;
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            live_bottom,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            3,
            live_bottom,
        );
        let original_start = term.inner.selection.as_ref().unwrap().ordered().0;

        let viewport = term.inner.viewport;
        crate::view::scroll_viewport_up(&mut term.inner.active, &viewport, 1);
        term.inner.selection = extend_selection_from_start(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );

        let selection = term.inner.selection.as_ref().unwrap();
        assert_eq!(selection.anchor, original_start);
        assert_eq!(selection.origin, original_start);
        assert_eq!(
            selection.head,
            SelectionPoint {
                row: coords::screen_row_to_absolute(&term.inner.active, &term.inner.viewport, 0),
                col: 4,
            }
        );
    }

    #[test]
    fn selection_drags_backwards_flips_anchor_head() {
        let mut term = TestTerm::new(20, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            8,
            0,
            SelectionMode::Word,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
        );
        assert_eq!(
            selection_text(term.inner.selection.as_ref(), &term.inner.active).as_deref(),
            Some("hello world")
        );
    }

    #[test]
    fn is_cell_selected_matches_contains() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "abcdefghij");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            2,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            5,
            0,
        );
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            1,
            0
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            2,
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            5,
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            1,
            3,
        ));
    }

    #[test]
    fn search_finds_exact_case_sensitive_matches() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "abc foo xyz FOO bar");
        term.open_search();
        assert!(term.search_active());
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        assert_eq!(term.search.matches.len(), 1);
        let m = term.search.matches[0];
        assert_eq!((m.start_col, m.end_col), (4, 6));
        assert!(is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            4
        ));
        assert!(is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            5
        ));
        assert!(is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            3
        ));
        assert!(!is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            7
        ));
        assert!(!is_cell_match(
            &term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            0,
            12
        ));
    }

    #[test]
    fn search_close_clears_state() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "hello",
        );
        assert_eq!(term.search.matches.len(), 1);
        close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(!term.search_active());
        assert!(term.search.matches.is_empty());
        assert!(term.search.query.is_empty());
    }

    #[test]
    fn search_close_promotes_active_match_to_selection() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "abc foo def");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            4
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            5
        ));
        assert!(is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            6
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            3
        ));
        assert!(!is_cell_selected(
            term.inner.selection.as_ref(),
            &term.inner.active,
            &term.inner.viewport,
            0,
            7
        ));
    }

    #[test]
    fn search_close_without_matches_leaves_prior_selection() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "hello world");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        assert!(term.has_selection());
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "nonexistent",
        );
        close_search(&mut term.inner.search, &mut term.inner.selection);
        assert!(is_cell_selected(
            term.selection.as_ref(),
            &term.active,
            &term.inner.viewport,
            0,
            0
        ));
        assert!(is_cell_selected(
            term.selection.as_ref(),
            &term.active,
            &term.inner.viewport,
            0,
            4
        ));
    }

    #[test]
    fn search_next_wraps_around() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "foo");
        write_row(&mut term, 1, "foo");
        write_row(&mut term, 2, "foo");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foo",
        );
        assert_eq!(term.search.matches.len(), 3);
        let start_idx = term.search.active_idx;
        term.active.offset = search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        term.active.offset = search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        term.active.offset = search_step_next(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        assert_eq!(term.search.active_idx, start_idx);
    }

    #[test]
    fn search_backspace_trims_query_and_rescans() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        write_row(&mut term, 0, "fox foxy fo");
        term.open_search();
        term.active.offset = search_append(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
            "foxy",
        );
        assert_eq!(term.search.matches.len(), 1);
        term.active.offset = search_backspace(
            &mut term.inner.search,
            &term.inner.active,
            &term.inner.viewport,
        );
        assert_eq!(term.search.matches.len(), 2);
    }

    #[test]
    fn copy_selection_writes_to_clipboard() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        term.clipboard = Clipboard::in_memory();
        write_row(&mut term, 0, "copy-me");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            6,
            0,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            6,
            0,
        );
        copy_selection(
            &mut term.inner.clipboard,
            term.inner.selection.as_ref(),
            &term.inner.active,
            ClipboardKind::Clipboard,
        );
        assert_eq!(
            term.clipboard.get(ClipboardKind::Clipboard).as_deref(),
            Some("copy-me")
        );
        assert!(term.has_selection());
    }

    #[test]
    fn clear_selection_drops_state() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);
        write_row(&mut term, 0, "hello");
        term.inner.selection = start_selection(
            &term.inner.active,
            &term.inner.viewport,
            0,
            0,
            SelectionMode::Char,
        );
        term.inner.selection = extend_selection(
            &term.inner.selection.unwrap(),
            &term.inner.active,
            &term.inner.viewport,
            4,
            0,
        );
        term.inner.selection = None;
        assert!(term.inner.selection.is_none());
        assert!(selection_text(term.inner.selection.as_ref(), &term.inner.active).is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Row;

    fn row_from(text: &str) -> Row {
        use crate::ColorPalette;
        let pal = ColorPalette::default();
        let mut r = Row::new(text.chars().count() as u32, pal.fg, pal.bg);
        let mut buf = [0u8; 4];
        for (i, c) in text.chars().enumerate() {
            r.cells[i] = smol_str::SmolStr::new_inline(c.encode_utf8(&mut buf));
        }
        r
    }

    fn pt(
        row: u64,
        col: u32,
    ) -> SelectionPoint {
        SelectionPoint { row, col }
    }

    fn sel(
        anchor: SelectionPoint,
        head: SelectionPoint,
        mode: SelectionMode,
    ) -> Selection {
        Selection {
            anchor,
            head,
            mode,
            rendered: false,
            origin: anchor,
        }
    }

    #[test]
    fn ordered_swaps_when_anchor_after_head() {
        let s = sel(pt(5, 10), pt(2, 3), SelectionMode::Char);
        assert_eq!(s.ordered(), (pt(2, 3), pt(5, 10)));
    }

    #[test]
    fn empty_char_selection_is_empty() {
        let s = sel(pt(3, 4), pt(3, 4), SelectionMode::Char);
        assert!(s.is_empty());
    }

    #[test]
    fn word_selection_is_never_empty() {
        let s = sel(pt(3, 4), pt(3, 4), SelectionMode::Word);
        assert!(!s.is_empty());
    }

    #[test]
    fn contains_inclusive_on_both_ends_single_row() {
        let s = sel(pt(0, 3), pt(0, 7), SelectionMode::Char);
        assert!(!s.contains(pt(0, 2)));
        assert!(s.contains(pt(0, 3)));
        assert!(s.contains(pt(0, 5)));
        assert!(s.contains(pt(0, 7)));
        assert!(!s.contains(pt(0, 8)));
    }

    #[test]
    fn contains_multi_row_excludes_cells_before_start_col() {
        let s = sel(pt(0, 5), pt(2, 3), SelectionMode::Char);
        assert!(!s.contains(pt(0, 4)));
        assert!(s.contains(pt(0, 5)));
        assert!(s.contains(pt(0, 79))); // anywhere in first row past start
        assert!(s.contains(pt(1, 0))); // middle row — everything
        assert!(s.contains(pt(2, 0))); // last row up to end_col
        assert!(s.contains(pt(2, 3)));
        assert!(!s.contains(pt(2, 4)));
    }

    #[test]
    fn line_mode_covers_full_rows() {
        let s = sel(pt(1, 5), pt(3, 2), SelectionMode::Line);
        assert!(!s.contains(pt(0, 100)));
        assert!(s.contains(pt(1, 0)));
        assert!(s.contains(pt(2, 42))); // middle row
        assert!(s.contains(pt(3, 999)));
        assert!(!s.contains(pt(4, 0)));
    }

    #[test]
    fn expand_to_word_picks_word_around_col() {
        let row = row_from("hello world foo");
        // click on `l` in hello
        assert_eq!(expand_to_word(&row, 3), (0, 4));
        // click on space — the whitespace run is the segment
        assert_eq!(expand_to_word(&row, 5), (5, 5));
        // click on `r` in world
        assert_eq!(expand_to_word(&row, 8), (6, 10));
    }

    #[test]
    fn expand_to_word_handles_punctuation_as_own_segment() {
        let row = row_from("foo=bar");
        assert_eq!(expand_to_word(&row, 0), (0, 2)); // foo
        assert_eq!(expand_to_word(&row, 3), (3, 3)); // =
        assert_eq!(expand_to_word(&row, 4), (4, 6)); // bar
    }

    #[test]
    fn expand_to_line_covers_full_row() {
        let row = row_from("hello");
        assert_eq!(expand_to_line(&row), (0, 4));
    }
}

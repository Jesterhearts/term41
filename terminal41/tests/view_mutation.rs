use terminal41::publish_terminal_snapshot;
use terminal41::selection::SelectionMode;
use terminal41::selection::start_rendered_selection;
use terminal41::settings::RuntimeSettings;
use terminal41::terminal_snapshot_buffer;
use terminal41::test_support::TestTerm;
use terminal41::view::SearchAction;

#[test]
fn selection_and_search_changes_publish_highlights_without_manual_invalidation() {
    let mut term = TestTerm::new(10, 3, 100, 16, 8);
    term.process(b"\x1b[?1049hhello");
    let (mut publisher, mut output) = terminal_snapshot_buffer(&mut term);
    output.update();
    let generation = output.read().rows[0].generation;

    let selection = start_rendered_selection(
        term.active(),
        term.viewport(),
        term.on_alt_screen(),
        1,
        0,
        SelectionMode::Word,
    );
    term.set_selection(selection);
    publish_terminal_snapshot(&mut term, &mut publisher);
    output.update();
    let row = &output.read().rows[0];
    assert!(row.generation > generation);
    assert_eq!(&row.selected[..6], &[true, true, true, true, true, false]);

    assert!(term.clear_selection());
    term.update_search(SearchAction::Open);
    term.update_search(SearchAction::Append("hello"));
    publish_terminal_snapshot(&mut term, &mut publisher);
    output.update();
    let row = &output.read().rows[0];
    assert!(row.selected.iter().all(|selected| !selected));
    assert_eq!(&row.matched[..6], &[true, true, true, true, true, false]);
}

#[test]
fn search_navigation_publishes_visible_matches_and_bounded_offsets() {
    let mut term = TestTerm::new(10, 3, 100, 16, 8);
    term.process(b"hit0\r\nblank1\r\nblank2\r\nhit3\r\nblank4\r\nblank5\r\nhit6");
    let (mut publisher, mut output) = terminal_snapshot_buffer(&mut term);
    term.update_search(SearchAction::Open);

    for (action, expected_offset, expected_row) in [
        (SearchAction::Append("hit"), 0, "hit6"),
        (SearchAction::Previous, 2, "hit3"),
        (SearchAction::Previous, 4, "hit0"),
        (SearchAction::Next, 2, "hit3"),
        (SearchAction::Next, 0, "hit6"),
        (SearchAction::Next, 4, "hit0"),
        (SearchAction::Backspace, 4, "hit0"),
    ] {
        term.update_search(action);
        publish_terminal_snapshot(&mut term, &mut publisher);
        output.update();
        let snap = output.read();
        assert_eq!(snap.viewport_offset, expected_offset);
        let active_row = snap
            .rows
            .iter()
            .find(|row| row.active_match.iter().any(|matched| *matched))
            .unwrap_or_else(|| {
                panic!("active match {expected_row} remains visible at offset {expected_offset}")
            });
        assert_eq!(active_row.cells.concat().trim_end(), expected_row);
    }

    term.update_search(SearchAction::Close);
    publish_terminal_snapshot(&mut term, &mut publisher);
    output.update();
    let snap = output.read();
    assert!(!snap.search_active);
    assert_eq!(snap.viewport_offset, 4);
    assert!(
        snap.rows
            .iter()
            .any(|row| row.cells.concat().starts_with("hit0") && row.selected[..2] == [true, true])
    );

    term.set_viewport_offset(u32::MAX);
    assert_eq!(terminal41::view::viewport_offset(term.active()), 4);
    term.set_viewport_offset(0);
    assert_eq!(terminal41::view::viewport_offset(term.active()), 0);
}

#[test]
fn closing_search_selects_the_live_match_with_retained_or_trimmed_history() {
    for scrollback in [100, 2] {
        let mut term = TestTerm::new(10, 3, scrollback, 16, 8);
        term.process(b"hit0\r\nblank1\r\nblank2\r\nhit3\r\nblank4\r\nblank5\r\nhit6");
        let (mut publisher, mut output) = terminal_snapshot_buffer(&mut term);
        term.update_search(SearchAction::Open);
        term.update_search(SearchAction::Append("hit"));
        publish_terminal_snapshot(&mut term, &mut publisher);
        output.update();
        let snap = output.read();
        assert_eq!(snap.viewport_offset, 0);
        let row = snap
            .rows
            .iter()
            .find(|row| row.cells.concat().starts_with("hit6"))
            .unwrap();
        assert_eq!(&row.active_match[..4], &[true, true, true, false]);

        term.update_search(SearchAction::Close);
        publish_terminal_snapshot(&mut term, &mut publisher);
        output.update();
        let snap = output.read();
        assert_eq!(snap.viewport_offset, 0);
        assert!(!snap.search_active);
        let row = snap
            .rows
            .iter()
            .find(|row| row.cells.concat().starts_with("hit6"))
            .unwrap();
        assert_eq!(&row.selected[..4], &[true, true, true, false]);
        assert!(row.active_match.iter().all(|matched| !matched));
    }
}

#[test]
fn settings_recolor_both_screens_and_keep_row_arrays_aligned_after_resize() {
    let mut term = TestTerm::new(10, 3, 100, 16, 8);
    term.process(b"primary\x1b[?1049halternate");
    let (mut publisher, mut output) = terminal_snapshot_buffer(&mut term);
    let mut config = config41::Config::default();
    config.palette.fg = palette::Srgb::new(10, 20, 30);
    config.palette.bg = palette::Srgb::new(40, 50, 60);
    term.apply_settings(RuntimeSettings::from(&config));
    term.set_cell_dimensions(0, 0);
    assert_eq!((term.cell_width(), term.cell_height()), (1, 1));
    term.resize(12, 4);

    for restore_primary in [false, true] {
        if restore_primary {
            term.process(b"\x1b[?1049l");
        }
        publish_terminal_snapshot(&mut term, &mut publisher);
        output.update();
        let snap = output.read();
        assert_eq!(snap.viewport_cols, 12);
        assert_eq!(snap.viewport_rows, 4);
        assert_eq!(snap.palette.fg, config.palette.fg);
        for row in &snap.rows {
            assert_eq!(row.cells.len(), 12);
            assert_eq!(row.attrs.len(), row.cells.len());
            assert_eq!(row.fg.len(), row.cells.len());
            assert_eq!(row.bg.len(), row.cells.len());
            assert!(row.fg.iter().all(|fg| *fg == config.palette.fg));
            assert!(row.bg.iter().all(|bg| *bg == config.palette.bg));
        }
    }
}

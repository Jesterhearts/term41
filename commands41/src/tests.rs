use super::*;

fn settings(words: &[&str]) -> EditorSettings {
    EditorSettings {
        completion_words: words.iter().map(|word| (*word).to_owned()).collect(),
        command_words: Vec::new(),
        history_entries: Vec::new(),
        current_dir: None,
        max_history: 20,
    }
}

fn command_settings(words: &[&str]) -> EditorSettings {
    EditorSettings {
        completion_words: Vec::new(),
        command_words: words.iter().map(|word| (*word).to_owned()).collect(),
        history_entries: Vec::new(),
        current_dir: None,
        max_history: 20,
    }
}

fn path_settings(current_dir: PathBuf) -> EditorSettings {
    EditorSettings {
        completion_words: Vec::new(),
        command_words: Vec::new(),
        history_entries: Vec::new(),
        current_dir: Some(current_dir),
        max_history: 20,
    }
}

#[test]
fn inserts_and_submits_command() {
    let mut editor = CommandEditor::new();
    let outcome = apply_input(
        &mut editor,
        EditorInput::Insert("cargo test".to_owned()),
        &EditorSettings::default(),
    );
    assert_eq!(outcome, EditOutcome::Updated);
    assert_eq!(
        apply_input(&mut editor, EditorInput::Enter, &EditorSettings::default()),
        EditOutcome::Submitted("cargo test".to_owned())
    );
    assert!(editor.is_empty());
}

#[test]
fn submit_replaces_newlines_with_spaces() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo\n test\r\n--workspace".to_owned()),
        &settings,
    );

    assert_eq!(
        apply_input(&mut editor, EditorInput::Enter, &settings),
        EditOutcome::Submitted("cargo test --workspace".to_owned())
    );
}

#[test]
fn selection_replaces_on_insert_and_copies_text() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo test".to_owned()),
        &settings,
    );

    assert_eq!(select_range(&mut editor, 6, 10), EditOutcome::Updated);
    assert_eq!(selected_text(&editor).as_deref(), Some("test"));
    assert_eq!(
        editor.view(&settings).selection,
        Some(EditorSelection {
            anchor: 6,
            head: 10
        })
    );

    assert_eq!(
        apply_input(
            &mut editor,
            EditorInput::Insert("clippy".to_owned()),
            &settings,
        ),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo clippy");
    assert_eq!(editor.view(&settings).selection, None);
}

#[test]
fn selected_text_handles_reverse_selection() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one two three".to_owned()),
        &settings,
    );

    assert_eq!(select_range(&mut editor, 13, 4), EditOutcome::Updated);
    assert_eq!(selected_text(&editor).as_deref(), Some("two three"));
}

#[test]
fn delete_removes_selected_range() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo test".to_owned()),
        &settings,
    );
    select_range(&mut editor, 5, 10);

    assert_eq!(
        apply_input(&mut editor, EditorInput::Backspace, &settings),
        EditOutcome::Updated
    );
    let view = editor.view(&settings);
    assert_eq!(view.text, "cargo");
    assert_eq!(view.cursor, 5);
    assert_eq!(view.selection, None);
}

#[test]
fn undo_and_redo_restore_recent_text_edits() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo".to_owned()),
        &settings,
    );
    apply_input(
        &mut editor,
        EditorInput::Insert(" test".to_owned()),
        &settings,
    );

    assert_eq!(
        apply_input(&mut editor, EditorInput::Undo, &settings),
        EditOutcome::Updated
    );
    let view = editor.view(&settings);
    assert_eq!(view.text, "cargo");
    assert_eq!(view.cursor, "cargo".len());

    assert_eq!(
        apply_input(&mut editor, EditorInput::Redo, &settings),
        EditOutcome::Updated
    );
    let view = editor.view(&settings);
    assert_eq!(view.text, "cargo test");
    assert_eq!(view.cursor, "cargo test".len());
}

#[test]
fn undo_restores_selection_replacement_and_new_edit_clears_redo() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo test".to_owned()),
        &settings,
    );
    select_range(&mut editor, "cargo ".len(), "cargo test".len());
    apply_input(
        &mut editor,
        EditorInput::Insert("clippy".to_owned()),
        &settings,
    );

    assert_eq!(
        apply_input(&mut editor, EditorInput::Undo, &settings),
        EditOutcome::Updated
    );
    let view = editor.view(&settings);
    assert_eq!(view.text, "cargo test");
    assert_eq!(
        view.selection,
        Some(EditorSelection {
            anchor: "cargo ".len(),
            head: "cargo test".len()
        })
    );

    apply_input(
        &mut editor,
        EditorInput::Insert("check".to_owned()),
        &settings,
    );
    assert_eq!(editor.view(&settings).text, "cargo check");
    assert_eq!(
        apply_input(&mut editor, EditorInput::Redo, &settings),
        EditOutcome::Ignored
    );
}

#[test]
fn undo_keeps_a_bounded_number_of_steps() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();

    for _ in 0..35 {
        apply_input(&mut editor, EditorInput::Insert("x".to_owned()), &settings);
    }

    for _ in 0..32 {
        assert_eq!(
            apply_input(&mut editor, EditorInput::Undo, &settings),
            EditOutcome::Updated
        );
    }
    assert_eq!(editor.view(&settings).text, "xxx");
    assert_eq!(
        apply_input(&mut editor, EditorInput::Undo, &settings),
        EditOutcome::Ignored
    );
}

fn vim_text(
    editor: &mut CommandEditor,
    text: &str,
) -> EditOutcome {
    apply_input(
        editor,
        EditorInput::Vim(VimKey::Text(text.to_owned())),
        &EditorSettings::default(),
    )
}

fn vim_escape(editor: &mut CommandEditor) -> EditOutcome {
    apply_input(
        editor,
        EditorInput::Vim(VimKey::Escape),
        &EditorSettings::default(),
    )
}

#[test]
fn vim_i_a_and_escape_switch_between_normal_and_insert() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();

    assert_eq!(
        editor.view(&settings).cursor_style,
        CommandEditorCursorStyle::Block
    );
    assert_eq!(vim_text(&mut editor, "i"), EditOutcome::Updated);
    assert_eq!(
        editor.view(&settings).cursor_style,
        CommandEditorCursorStyle::Beam
    );
    assert_eq!(vim_text(&mut editor, "abc"), EditOutcome::Updated);
    assert_eq!(vim_escape(&mut editor), EditOutcome::Updated);
    assert_eq!(
        editor.view(&settings).cursor_style,
        CommandEditorCursorStyle::Block
    );
    assert_eq!(editor.view(&settings).cursor, "ab".len());

    assert_eq!(vim_text(&mut editor, "a"), EditOutcome::Updated);
    assert_eq!(vim_text(&mut editor, "d"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "abcd");
}

#[test]
fn vim_a_appends_at_line_end_and_enters_insert() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one\ntwo".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, 1);

    assert_eq!(vim_text(&mut editor, "A"), EditOutcome::Updated);
    let view = editor.view(&settings);
    assert_eq!(view.cursor, "one".len());
    assert_eq!(view.cursor_style, CommandEditorCursorStyle::Beam);
    assert_eq!(vim_text(&mut editor, "!"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "one!\ntwo");
}

#[test]
fn vim_o_and_o_open_lines_and_enter_insert() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one\ntwo".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, 1);

    assert_eq!(vim_text(&mut editor, "o"), EditOutcome::Updated);
    let view = editor.view(&settings);
    assert_eq!(view.text, "one\n\ntwo");
    assert_eq!(view.cursor, "one\n".len());
    assert_eq!(view.cursor_style, CommandEditorCursorStyle::Beam);
    assert_eq!(vim_text(&mut editor, "middle"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "one\nmiddle\ntwo");

    assert_eq!(vim_escape(&mut editor), EditOutcome::Updated);
    set_cursor(&mut editor, "one\nmiddle".len());
    assert_eq!(vim_text(&mut editor, "O"), EditOutcome::Updated);
    let view = editor.view(&settings);
    assert_eq!(view.text, "one\n\nmiddle\ntwo");
    assert_eq!(view.cursor, "one\n".len());
    assert_eq!(view.cursor_style, CommandEditorCursorStyle::Beam);
    assert_eq!(vim_text(&mut editor, "above"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "one\nabove\nmiddle\ntwo");
}

#[test]
fn vim_hjkl_move_around_multiline_text() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one\ntwo\nthree".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, "one\ntwo".len());

    assert_eq!(vim_text(&mut editor, "h"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\ntw".len());
    assert_eq!(vim_text(&mut editor, "j"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\ntwo\nth".len());
    assert_eq!(vim_text(&mut editor, "k"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\ntw".len());
    assert_eq!(vim_text(&mut editor, "l"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\ntwo".len());
}

#[test]
fn vim_word_motions_distinguish_punctuation_and_whitespace_words() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("foo.bar baz".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, 0);

    assert_eq!(vim_text(&mut editor, "w"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo".len());
    assert_eq!(vim_text(&mut editor, "e"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo.".len());
    assert_eq!(vim_text(&mut editor, "W"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo.bar ".len());
    set_cursor(&mut editor, 0);
    assert_eq!(vim_text(&mut editor, "E"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo.bar".len());

    set_cursor(&mut editor, "foo.bar baz".len());
    assert_eq!(vim_text(&mut editor, "b"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo.bar ".len());
    assert_eq!(vim_text(&mut editor, "b"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo.".len());
    assert_eq!(vim_text(&mut editor, "b"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo".len());

    set_cursor(&mut editor, "foo.bar baz".len());
    assert_eq!(vim_text(&mut editor, "B"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "foo.bar ".len());
    assert_eq!(vim_text(&mut editor, "B"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, 0);
}

#[test]
fn vim_line_start_and_end_motions_target_current_line_edges() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one\n  two three".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, "one\n  two ".len());

    assert_eq!(vim_text(&mut editor, "0"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\n".len());
    assert_eq!(vim_text(&mut editor, "^"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\n  ".len());
    assert_eq!(vim_text(&mut editor, "$"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\n  two three".len());
}

#[test]
fn vim_paragraph_and_document_motions_move_by_blank_lines_and_edges() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one\n\nthree\nfour".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, 0);

    assert_eq!(vim_text(&mut editor, "}"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\n\n".len());
    assert_eq!(vim_text(&mut editor, "G"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\n\nthree\nfour".len());
    assert_eq!(vim_text(&mut editor, "{"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, "one\n\n".len());
    assert_eq!(vim_text(&mut editor, "g"), EditOutcome::Updated);
    assert_eq!(vim_text(&mut editor, "g"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).cursor, 0);
}

#[test]
fn vim_delete_yank_and_paste_use_editor_clipboard() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("foo bar".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, 0);

    assert_eq!(vim_text(&mut editor, "y"), EditOutcome::Updated);
    assert_eq!(vim_text(&mut editor, "w"), EditOutcome::Updated);
    set_cursor(&mut editor, "foo ".len());
    assert_eq!(vim_text(&mut editor, "P"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "foo foo bar");

    set_cursor(&mut editor, 0);
    assert_eq!(vim_text(&mut editor, "d"), EditOutcome::Updated);
    assert_eq!(vim_text(&mut editor, "w"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "foo bar");
    assert_eq!(vim_text(&mut editor, "p"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "ffoo oo bar");
}

#[test]
fn vim_yy_yanks_current_line() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one\ntwo\nthree".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, "one\nt".len());

    assert_eq!(vim_text(&mut editor, "y"), EditOutcome::Updated);
    assert_eq!(vim_text(&mut editor, "y"), EditOutcome::Updated);
    set_cursor(&mut editor, 0);
    assert_eq!(vim_text(&mut editor, "P"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "twoone\ntwo\nthree");
}

#[test]
fn vim_u_undoes_and_redo_restores_normal_mode_edits() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one two".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, "one ".len());

    assert_eq!(vim_text(&mut editor, "D"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "");
    assert_eq!(vim_text(&mut editor, "u"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "one two");
    assert_eq!(
        apply_input(&mut editor, EditorInput::Redo, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "");
}

#[test]
fn vim_backward_motions_work_with_delete_and_yank() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("foo.bar baz".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, "foo.bar baz".len());

    assert_eq!(vim_text(&mut editor, "y"), EditOutcome::Updated);
    assert_eq!(vim_text(&mut editor, "b"), EditOutcome::Updated);
    set_cursor(&mut editor, 0);
    assert_eq!(vim_text(&mut editor, "P"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "bazfoo.bar baz");

    set_cursor(&mut editor, "bazfoo.bar".len());
    assert_eq!(vim_text(&mut editor, "d"), EditOutcome::Updated);
    assert_eq!(vim_text(&mut editor, "B"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, " baz");
}

#[test]
fn vim_d_deletes_current_line() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one two\nthree".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, "one ".len());

    assert_eq!(vim_text(&mut editor, "D"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "three");
}

#[test]
fn vim_x_deletes_grapheme_under_cursor() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("ab👍cd".to_owned()),
        &settings,
    );
    set_cursor(&mut editor, "ab".len());

    assert_eq!(vim_text(&mut editor, "x"), EditOutcome::Updated);
    let view = editor.view(&settings);
    assert_eq!(view.text, "abcd");
    assert_eq!(view.cursor, "ab".len());
    assert_eq!(vim_text(&mut editor, "P"), EditOutcome::Updated);
    assert_eq!(editor.view(&settings).text, "ab👍cd");
}

#[test]
fn vim_x_ignores_empty_buffer() {
    let mut editor = CommandEditor::new();

    assert_eq!(vim_text(&mut editor, "x"), EditOutcome::Ignored);
}

#[test]
fn up_down_move_between_multiline_editor_rows() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one\ntwo\nthree".to_owned()),
        &settings,
    );

    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).cursor, "one\ntwo".len());
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).cursor, "one".len());
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryNext, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).cursor, "one\ntwo".len());
}

#[test]
fn multiline_vertical_movement_falls_back_to_history_at_edges() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("history".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Enter, &settings);
    apply_input(
        &mut editor,
        EditorInput::Insert("one\ntwo".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::MoveHome, &settings);

    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "history");
}

#[test]
fn history_arrows_restore_draft() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("one".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Enter, &settings);
    apply_input(
        &mut editor,
        EditorInput::Insert("two".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Enter, &settings);
    apply_input(
        &mut editor,
        EditorInput::Insert("draft".to_owned()),
        &settings,
    );

    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "two");
    apply_input(&mut editor, EditorInput::HistoryPrevious, &settings);
    assert_eq!(editor.view(&settings).text, "one");
    apply_input(&mut editor, EditorInput::HistoryNext, &settings);
    apply_input(&mut editor, EditorInput::HistoryNext, &settings);
    assert_eq!(editor.view(&settings).text, "draft");
}

#[test]
fn history_arrows_keep_navigating_when_selected_entry_has_completions() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings {
        history_entries: vec![
            HistoryEntry::external("cargo check"),
            HistoryEntry::external("cargo clippy"),
            HistoryEntry::external("cargo"),
        ],
        ..EditorSettings::default()
    };

    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo");
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo clippy");
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryNext, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo");
}

#[test]
fn tab_completion_accepts_history_navigation_before_editing() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings {
        history_entries: vec![
            HistoryEntry::external("cargo check"),
            HistoryEntry::external("cargo clippy"),
            HistoryEntry::external("cargo"),
        ],
        ..EditorSettings::default()
    };

    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo");
    assert_eq!(
        apply_input(&mut editor, EditorInput::Complete, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo clippy");
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo");
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryNext, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo clippy");
}

#[test]
fn tab_completion_accepts_one_history_path_element() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings {
        history_entries: vec![HistoryEntry::external("cat src/main.rs")],
        ..EditorSettings::default()
    };
    apply_input(
        &mut editor,
        EditorInput::Insert("cat s".to_owned()),
        &settings,
    );

    assert_eq!(
        editor.view(&settings).completion.as_deref(),
        Some("rc/main.rs")
    );
    assert_eq!(
        apply_input(&mut editor, EditorInput::Complete, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cat src/");
    assert_eq!(
        apply_input(&mut editor, EditorInput::Complete, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cat src/main.rs");
}

#[test]
fn right_arrow_accepts_full_visible_history_completion() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings {
        history_entries: vec![HistoryEntry::external("cat src/main.rs")],
        ..EditorSettings::default()
    };
    apply_input(
        &mut editor,
        EditorInput::Insert("cat s".to_owned()),
        &settings,
    );

    assert_eq!(
        apply_input(&mut editor, EditorInput::MoveRight, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cat src/main.rs");
}

#[test]
fn typing_accepts_history_navigation_before_editing() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings {
        history_entries: vec![
            HistoryEntry::external("cargo check"),
            HistoryEntry::external("cargo clippy"),
            HistoryEntry::external("cargo"),
        ],
        ..EditorSettings::default()
    };

    apply_input(&mut editor, EditorInput::HistoryPrevious, &settings);
    assert_eq!(editor.view(&settings).text, "cargo");
    apply_input(
        &mut editor,
        EditorInput::Insert(" build".to_owned()),
        &settings,
    );
    assert_eq!(editor.view(&settings).text, "cargo build");
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo");
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryNext, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo build");
}

#[test]
fn external_history_entries_participate_in_navigation() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings {
        history_entries: vec![
            HistoryEntry::external("cargo check"),
            HistoryEntry::external("cargo test"),
        ],
        ..EditorSettings::default()
    };

    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo test");
    assert_eq!(
        apply_input(&mut editor, EditorInput::HistoryPrevious, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cargo check");
}

#[test]
fn word_motion_skips_shell_words_and_separators() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo test | rg foo".to_owned()),
        &settings,
    );

    apply_input(&mut editor, EditorInput::MoveWordLeft, &settings);
    assert_eq!(editor.view(&settings).cursor, "cargo test | rg ".len());

    apply_input(&mut editor, EditorInput::MoveWordLeft, &settings);
    assert_eq!(editor.view(&settings).cursor, "cargo test | ".len());

    apply_input(&mut editor, EditorInput::MoveWordRight, &settings);
    assert_eq!(editor.view(&settings).cursor, "cargo test | rg".len());
}

#[test]
fn word_delete_updates_kill_buffer_for_yank() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo test".to_owned()),
        &settings,
    );

    apply_input(&mut editor, EditorInput::DeleteWordLeft, &settings);
    assert_eq!(editor.view(&settings).text, "cargo ");
    apply_input(&mut editor, EditorInput::Yank, &settings);
    assert_eq!(editor.view(&settings).text, "cargo test");
}

#[test]
fn delete_removes_grapheme_under_cursor() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("ab👍c".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::MoveLeft, &settings);
    apply_input(&mut editor, EditorInput::MoveLeft, &settings);

    assert_eq!(
        apply_input(&mut editor, EditorInput::Delete, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "abc");
}

#[test]
fn delete_at_end_is_ignored() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("abc".to_owned()),
        &settings,
    );

    assert_eq!(
        apply_input(&mut editor, EditorInput::Delete, &settings),
        EditOutcome::Ignored
    );
    assert_eq!(editor.view(&settings).text, "abc");
}

#[test]
fn line_kill_to_start_and_end_can_yank() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo test --all".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::MoveWordLeft, &settings);

    apply_input(&mut editor, EditorInput::KillToStart, &settings);
    assert_eq!(editor.view(&settings).text, "--all");
    apply_input(&mut editor, EditorInput::Yank, &settings);
    assert_eq!(editor.view(&settings).text, "cargo test --all");

    apply_input(&mut editor, EditorInput::KillToEnd, &settings);
    assert_eq!(editor.view(&settings).text, "cargo test ");
    apply_input(&mut editor, EditorInput::Yank, &settings);
    assert_eq!(editor.view(&settings).text, "cargo test --all");
}

#[test]
fn tab_completion_uses_prefix_match() {
    let mut editor = CommandEditor::new();
    let settings = settings(&["cargo", "cat"]);
    apply_input(
        &mut editor,
        EditorInput::Insert("car".to_owned()),
        &settings,
    );
    assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
    apply_input(&mut editor, EditorInput::Complete, &settings);
    assert_eq!(editor.view(&settings).text, "cargo");
}

#[test]
fn completion_matches_whole_command_prefix_from_history() {
    let mut editor = CommandEditor::new();
    let settings = EditorSettings::default();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo build".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Enter, &settings);
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo b".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("uild"));
}

#[test]
fn command_words_complete_initial_command() {
    let settings = command_settings(&["cargo", "cat"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("car".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
}

#[test]
fn command_words_prefer_shortest_matching_command() {
    let settings = command_settings(&["cargo-audit", "cargo"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("car".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
}

#[test]
fn command_view_shows_top_five_ambiguous_candidates() {
    let settings = command_settings(&[
        "cargo-audit",
        "cargo",
        "cargo-edit",
        "cargo-nextest",
        "cargo-watch",
        "cargo-zigbuild",
    ]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("car".to_owned()),
        &settings,
    );

    assert_eq!(
        editor.view(&settings).candidates,
        [
            "cargo",
            "cargo-edit",
            "cargo-audit",
            "cargo-watch",
            "cargo-nextest"
        ]
    );
    assert_eq!(editor.view(&settings).candidate_index, 0);
}

#[test]
fn command_view_hides_single_candidate_list() {
    let settings = command_settings(&["cargo"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("car".to_owned()),
        &settings,
    );

    assert!(editor.view(&settings).candidates.is_empty());
}

#[test]
fn history_arrows_cycle_ambiguous_completion_selection() {
    let settings = command_settings(&["cargo", "cargo-audit", "cargo-edit"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("car".to_owned()),
        &settings,
    );

    apply_input(&mut editor, EditorInput::HistoryNext, &settings);
    let view = editor.view(&settings);
    assert_eq!(view.candidate_index, 1);
    assert_eq!(view.completion.as_deref(), Some("go-edit"));

    apply_input(&mut editor, EditorInput::HistoryPrevious, &settings);
    let view = editor.view(&settings);
    assert_eq!(view.candidate_index, 0);
    assert_eq!(view.completion.as_deref(), Some("go"));
}

#[test]
fn tab_accepts_selected_completion_candidate() {
    let settings = command_settings(&["cargo", "cargo-audit", "cargo-edit"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("car".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::HistoryNext, &settings);

    apply_input(&mut editor, EditorInput::Complete, &settings);

    assert_eq!(editor.view(&settings).text, "cargo-edit");
    assert!(editor.view(&settings).candidates.is_empty());
}

#[test]
fn tab_accepts_one_word_from_selected_history_completion() {
    let settings = EditorSettings {
        history_entries: vec![
            HistoryEntry::external("cargo clippy --all"),
            HistoryEntry::external("cargo check --workspace"),
        ],
        ..EditorSettings::default()
    };
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::HistoryNext, &settings);

    apply_input(&mut editor, EditorInput::Complete, &settings);

    assert_eq!(editor.view(&settings).text, "cargo clippy");
    assert_eq!(editor.view(&settings).completion.as_deref(), Some(" --all"));
}

#[test]
fn right_arrow_accepts_full_selected_history_completion() {
    let settings = EditorSettings {
        history_entries: vec![
            HistoryEntry::external("cargo clippy --all"),
            HistoryEntry::external("cargo check --workspace"),
        ],
        ..EditorSettings::default()
    };
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cargo".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::HistoryNext, &settings);

    apply_input(&mut editor, EditorInput::MoveRight, &settings);

    assert_eq!(editor.view(&settings).text, "cargo clippy --all");
    assert!(editor.view(&settings).completion.is_none());
}

#[test]
fn history_arrows_fall_back_without_ambiguous_completion() {
    let settings = command_settings(&["cargo"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("git status".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Enter, &settings);

    apply_input(&mut editor, EditorInput::HistoryPrevious, &settings);

    assert_eq!(editor.view(&settings).text, "git status");
}

#[test]
fn history_first_words_win_before_command_words() {
    let settings = command_settings(&["cargo"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat README.md".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Enter, &settings);
    apply_input(&mut editor, EditorInput::Insert("ca".to_owned()), &settings);

    assert_eq!(
        editor.view(&settings).completion.as_deref(),
        Some("t README.md")
    );
}

#[test]
fn command_words_do_not_complete_argument_words() {
    let settings = command_settings(&["cargo"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("echo car".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion, None);
}

#[test]
fn command_words_complete_after_shell_separator() {
    let settings = command_settings(&["cargo"]);
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("echo ok | car".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("go"));
}

#[test]
fn completion_matches_relative_file_path() {
    let root = unique_test_dir("relative-file");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("README.md"), "").expect("write temp file");
    fs::write(root.join("ROADMAP.md"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat REA".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("DME.md"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_marks_directory_with_trailing_slash() {
    let root = unique_test_dir("directory");
    fs::create_dir_all(root.join("src")).expect("create temp dir");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cd sr".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("c/"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_matches_nested_path_prefix() {
    let root = unique_test_dir("nested");
    fs::create_dir_all(root.join("src")).expect("create temp dir");
    fs::write(root.join("src/main.rs"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("vim src/ma".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("in.rs"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_matches_path_inside_double_quotes() {
    let root = unique_test_dir("double-quoted-path");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("foo bar.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat \"foo b".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("ar.txt"));
    apply_input(&mut editor, EditorInput::MoveRight, &settings);
    assert_eq!(editor.view(&settings).text, "cat \"foo bar.txt");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_matches_path_inside_single_quotes() {
    let root = unique_test_dir("single-quoted-path");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("foo bar.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat 'foo b".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("ar.txt"));
    apply_input(&mut editor, EditorInput::MoveRight, &settings);
    assert_eq!(editor.view(&settings).text, "cat 'foo bar.txt");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_escapes_spaces_in_unquoted_paths() {
    let root = unique_test_dir("unquoted-space-path");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("foo bar.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat foo".to_owned()),
        &settings,
    );

    assert_eq!(
        editor.view(&settings).completion.as_deref(),
        Some("\\ bar.txt")
    );
    apply_input(&mut editor, EditorInput::MoveRight, &settings);
    assert_eq!(editor.view(&settings).text, "cat foo\\ bar.txt");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn completion_unescapes_typed_unquoted_path_prefix() {
    let root = unique_test_dir("unescaped-prefix-path");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("foo bar.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat foo\\ b".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("ar.txt"));
    apply_input(&mut editor, EditorInput::MoveRight, &settings);
    assert_eq!(editor.view(&settings).text, "cat foo\\ bar.txt");

    let _ = fs::remove_dir_all(root);
}

#[test]
fn tab_cycles_multiple_path_matches_without_inserting() {
    let root = unique_test_dir("cycle");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("food.txt"), "").expect("write temp file");
    fs::write(root.join("foot.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat foo".to_owned()),
        &settings,
    );
    assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

    assert_eq!(
        apply_input(&mut editor, EditorInput::Complete, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cat foo");
    assert_eq!(editor.view(&settings).completion.as_deref(), Some("t.txt"));

    apply_input(&mut editor, EditorInput::Complete, &settings);
    assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn right_arrow_accepts_active_path_cycle() {
    let root = unique_test_dir("cycle-accept");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("food.txt"), "").expect("write temp file");
    fs::write(root.join("foot.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat foo".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Complete, &settings);

    assert_eq!(
        apply_input(&mut editor, EditorInput::MoveRight, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cat foot.txt");
    assert_eq!(editor.view(&settings).completion, None);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn right_arrow_accepts_visible_path_completion_without_tab() {
    let root = unique_test_dir("visible-accept");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("food.txt"), "").expect("write temp file");
    fs::write(root.join("foot.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat foo".to_owned()),
        &settings,
    );
    assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

    assert_eq!(
        apply_input(&mut editor, EditorInput::MoveRight, &settings),
        EditOutcome::Updated
    );
    assert_eq!(editor.view(&settings).text, "cat food.txt");
    assert_eq!(editor.view(&settings).completion, None);

    let _ = fs::remove_dir_all(root);
}

#[test]
fn typing_resets_active_path_cycle() {
    let root = unique_test_dir("cycle-reset");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("food.txt"), "").expect("write temp file");
    fs::write(root.join("foot.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat foo".to_owned()),
        &settings,
    );
    apply_input(&mut editor, EditorInput::Complete, &settings);
    apply_input(&mut editor, EditorInput::Insert("d".to_owned()), &settings);

    assert_eq!(editor.view(&settings).text, "cat food");
    assert_eq!(editor.view(&settings).completion.as_deref(), Some(".txt"));

    let _ = fs::remove_dir_all(root);
}

#[test]
fn tab_cycle_skips_exact_file_match() {
    let root = unique_test_dir("cycle-exact");
    fs::create_dir_all(&root).expect("create temp dir");
    fs::write(root.join("foo"), "").expect("write temp file");
    fs::write(root.join("food.txt"), "").expect("write temp file");
    fs::write(root.join("foot.txt"), "").expect("write temp file");
    let settings = path_settings(root.clone());
    let mut editor = CommandEditor::new();
    apply_input(
        &mut editor,
        EditorInput::Insert("cat foo".to_owned()),
        &settings,
    );

    assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));
    apply_input(&mut editor, EditorInput::Complete, &settings);
    assert_eq!(editor.view(&settings).completion.as_deref(), Some("t.txt"));
    apply_input(&mut editor, EditorInput::Complete, &settings);
    assert_eq!(editor.view(&settings).completion.as_deref(), Some("d.txt"));

    let _ = fs::remove_dir_all(root);
}

fn unique_test_dir(label: &str) -> PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("commands41-{label}-{nonce}"))
}

#[test]
fn highlights_shell_constructs() {
    let spans = highlight_shell("if echo \"$HOME\" # comment");
    assert!(spans.iter().any(|span| span.kind == HighlightKind::Keyword));
    assert!(spans.iter().any(|span| span.kind == HighlightKind::Builtin));
    assert!(spans.iter().any(|span| span.kind == HighlightKind::String));
    assert!(spans.iter().any(|span| span.kind == HighlightKind::Comment));
}

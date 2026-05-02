use super::*;

#[cfg(test)]
mod selection_autoscroll_tests {
    use super::*;

    #[test]
    fn selection_autoscroll_detects_terminal_edges() {
        assert_eq!(
            selection_autoscroll_direction(15.0, 20, 5),
            Some(SelectionAutoscroll::Up)
        );
        assert_eq!(
            selection_autoscroll_direction(39.0, 20, 5),
            Some(SelectionAutoscroll::Up)
        );
        assert_eq!(selection_autoscroll_direction(60.0, 20, 5), None);
        assert_eq!(
            selection_autoscroll_direction(100.0, 20, 5),
            Some(SelectionAutoscroll::Down)
        );
        assert_eq!(
            selection_autoscroll_direction(125.0, 20, 5),
            Some(SelectionAutoscroll::Down)
        );
    }

    #[test]
    fn selection_autoscroll_ignores_empty_viewports() {
        assert_eq!(selection_autoscroll_direction(0.0, 0, 5), None);
        assert_eq!(selection_autoscroll_direction(0.0, 20, 0), None);
    }
}

#[cfg(test)]
mod viewport_reset_tests {
    use super::*;

    #[test]
    fn reset_viewport_and_invalidate_marks_visible_rows_dirty() {
        let mut terminal = Terminal::new(
            4,
            3,
            10,
            StatusLineMode::Off,
            config41::FeaturePermissions::default(),
            config41::TerminalLimits::default(),
            16,
            8,
            config41::ColorPalette::default(),
        );
        let (mut publisher, mut output) = terminal41::terminal_snapshot_buffer(&mut terminal);
        output.update();
        let first = output.read().clone();
        let first_generations: Vec<u64> = first.rows.iter().map(|row| row.generation).collect();

        terminal.active.offset = 1;
        reset_viewport_and_invalidate(&mut terminal);
        terminal41::publish_terminal_snapshot(&mut terminal, &mut publisher);
        output.update();
        let snap = output.read().clone();

        assert_eq!(terminal.active.offset, 0);
        assert!(!snap.reset_cached_rows);
        assert!(
            snap.rows
                .iter()
                .zip(first_generations)
                .all(|(row, generation)| row.generation > generation)
        );
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;

    #[test]
    fn permission_keys_accept_y_only() {
        assert_eq!(
            permission_key_decision(&Key::Character("y".into())),
            Some(PermissionDecision::Allow)
        );
        assert_eq!(
            permission_key_decision(&Key::Character("Y".into())),
            Some(PermissionDecision::Allow)
        );
    }

    #[test]
    fn permission_keys_default_to_no_for_n_enter_and_escape() {
        assert_eq!(
            permission_key_decision(&Key::Character("n".into())),
            Some(PermissionDecision::Deny)
        );
        assert_eq!(
            permission_key_decision(&Key::Named(NamedKey::Enter)),
            Some(PermissionDecision::Deny)
        );
        assert_eq!(
            permission_key_decision(&Key::Named(NamedKey::Escape)),
            Some(PermissionDecision::Deny)
        );
    }
}

#[cfg(test)]
mod command_editor_context_tests {
    use terminal41::ShellIntegrationPhase;
    use terminal41::test_support::TestTerm;

    use super::*;

    #[test]
    fn command_editor_view_context_requires_primary_screen_only() {
        let mut term = TestTerm::new_80x24();

        assert_eq!(
            command_editor_view_context(&term),
            Some(CommandEditorContext { current_dir: None })
        );
        assert_eq!(command_editor_input_context(&term), None);

        term.process(b"\x1b]133;B\x07");

        assert_eq!(
            command_editor_view_context(&term),
            Some(CommandEditorContext { current_dir: None })
        );
        assert_eq!(
            command_editor_input_context(&term),
            Some(CommandEditorContext { current_dir: None })
        );
    }

    #[test]
    fn command_editor_contexts_are_disabled_on_alt_screen() {
        let mut term = TestTerm::new_80x24();
        term.process(b"\x1b]133;B\x07");
        term.process(b"\x1b[?1049h");

        assert_eq!(
            term.metadata.shell_integration_phase,
            ShellIntegrationPhase::Command
        );
        assert!(term.on_alt_screen);
        assert_eq!(command_editor_view_context(&term), None);
        assert_eq!(command_editor_input_context(&term), None);
    }
}

#[cfg(test)]
mod command_editor_input_tests {
    use winit::keyboard::ModifiersState;

    use super::*;

    #[test]
    fn control_keys_map_to_line_editor_inputs() {
        assert_eq!(
            command_editor_input(&Key::Character("a".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::MoveHome)
        );
        assert_eq!(
            command_editor_input(&Key::Character("d".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::Delete)
        );
        assert_eq!(
            command_editor_input(&Key::Character("e".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::MoveEnd)
        );
        assert_eq!(
            command_editor_input(&Key::Character("k".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::KillToEnd)
        );
        assert_eq!(
            command_editor_input(&Key::Character("u".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::KillToStart)
        );
        assert_eq!(
            command_editor_input(&Key::Character("w".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::DeleteWordLeft)
        );
        assert_eq!(
            command_editor_input(&Key::Character("y".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::Yank)
        );
        assert_eq!(
            command_editor_input(&Key::Character("r".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::Redo)
        );
    }

    #[test]
    fn alt_keys_map_to_word_editor_inputs() {
        assert_eq!(
            command_editor_input(&Key::Character("b".into()), ModifiersState::ALT, false),
            Some(EditorInput::MoveWordLeft)
        );
        assert_eq!(
            command_editor_input(&Key::Character("f".into()), ModifiersState::ALT, false),
            Some(EditorInput::MoveWordRight)
        );
        assert_eq!(
            command_editor_input(&Key::Character("d".into()), ModifiersState::ALT, false),
            Some(EditorInput::DeleteWordRight)
        );
    }

    #[test]
    fn control_shift_keys_still_fall_through_to_keybindings() {
        assert_eq!(
            command_editor_input(
                &Key::Character("D".into()),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                false,
            ),
            None
        );
    }

    #[test]
    fn shift_enter_inserts_newline() {
        assert_eq!(
            command_editor_input(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, false),
            Some(EditorInput::Insert("\n".into()))
        );
    }

    #[test]
    fn vim_mode_maps_plain_keys_to_vim_inputs() {
        assert_eq!(
            command_editor_input(&Key::Character("i".into()), ModifiersState::empty(), true),
            Some(EditorInput::Vim(VimKey::Text("i".into())))
        );
        assert_eq!(
            command_editor_input(&Key::Named(NamedKey::Escape), ModifiersState::empty(), true),
            Some(EditorInput::Vim(VimKey::Escape))
        );
        assert_eq!(
            command_editor_input(&Key::Named(NamedKey::Enter), ModifiersState::SHIFT, true),
            Some(EditorInput::Vim(VimKey::ShiftEnter))
        );
        assert_eq!(
            command_editor_input(&Key::Character("r".into()), ModifiersState::CONTROL, true),
            Some(EditorInput::Redo)
        );
    }

    #[test]
    fn vim_mode_preserves_control_shift_keybindings() {
        assert_eq!(
            command_editor_input(
                &Key::Character("V".into()),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                true,
            ),
            None
        );
    }

    #[test]
    fn non_vim_command_editor_view_forces_beam_cursor() {
        let editor = CommandEditor::new();
        let settings = EditorSettings::default();

        assert_eq!(
            command_editor_view(&editor, &settings, false)
                .expect("view")
                .cursor_style,
            CommandEditorCursorStyle::Beam
        );
        assert_eq!(
            command_editor_view(&editor, &settings, true)
                .expect("view")
                .cursor_style,
            CommandEditorCursorStyle::Block
        );
    }

    #[test]
    fn mouse_cell_maps_to_visible_multiline_editor_text() {
        let view = CommandLineView {
            text: "one\ntwo\nthree\nfour".to_owned(),
            cursor: "one\ntwo\nthree\nfour".len(),
            cursor_style: CommandEditorCursorStyle::Beam,
            spans: Vec::new(),
            selection: None,
            completion: None,
            candidates: Vec::new(),
            candidate_index: 0,
        };

        assert_eq!(
            command_editor_byte_index_at_cell(&view, 80, 0, 1),
            "one\n".len()
        );
        assert_eq!(
            command_editor_byte_index_at_cell(&view, 80, 2, 3),
            "one\ntwo\nthree\nfo".len()
        );
    }
}

#[cfg(test)]
mod popup_command_tests {
    use terminal41::test_support::TestTerm;

    use super::*;

    #[test]
    fn popup_command_text_prefers_screen_observed_command() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        term.process(b"\x1b]633;A\x07");
        term.process(b"$ ");
        term.process(b"\x1b]633;B\x07");
        term.process(b"cargo test");
        term.process(b"\x1b]633;E;cargo\\x20metadata\x07");

        let text = popup_command_text(0, &term.metadata.command_metas, &term.active);
        match text {
            Some(PopupCommandText::Observed(text)) => assert_eq!(text, "cargo test"),
            _ => panic!("expected observed command text"),
        }
    }

    #[test]
    fn popup_command_text_falls_back_to_untrusted_metadata() {
        let mut term = TestTerm::new(20, 4, 100, 16, 8);
        term.process(b"\x1b]633;A\x07");
        term.process(b"\x1b]633;E;cargo\\x20test\x07");

        let text = popup_command_text(0, &term.metadata.command_metas, &term.active);
        match text {
            Some(PopupCommandText::Untrusted(text)) => assert_eq!(text, "cargo test"),
            _ => panic!("expected untrusted command text"),
        }
    }

    #[test]
    fn popup_rerun_text_trims_observed_command_without_enter() {
        let text = popup_rerun_command_text(PopupCommandText::Observed(" cargo test \r".into()));
        assert_eq!(text, "cargo test");
    }

    #[test]
    fn popup_rerun_text_keeps_untrusted_metadata_for_bracketed_paste_review() {
        let text = popup_rerun_command_text(PopupCommandText::Untrusted(
            "cargo test\ncargo publish".into(),
        ));
        assert_eq!(text, "cargo test\ncargo publish");
    }

    #[test]
    fn popup_rerun_pastes_single_line_raw_without_bracketed_mode() {
        let paste = popup_rerun_paste(PopupCommandText::Observed(" cargo test \r".into()), false);
        assert!(matches!(paste, Some((text, PasteMode::Terminal)) if text == "cargo test"));
    }

    #[test]
    fn popup_rerun_pastes_bracketed_when_mode_is_enabled() {
        let paste = popup_rerun_paste(PopupCommandText::Observed("cargo test".into()), true);
        assert!(matches!(paste, Some((text, PasteMode::Bracketed)) if text == "cargo test"));
    }

    #[test]
    fn popup_rerun_rejects_multiline_without_bracketed_mode() {
        let paste = popup_rerun_paste(
            PopupCommandText::Untrusted("cargo test\ncargo publish".into()),
            false,
        );
        assert!(paste.is_none());
    }
}

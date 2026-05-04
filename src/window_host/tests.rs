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

    #[test]
    fn mouse_position_adds_visible_terminal_row_offset() {
        assert_eq!(
            mouse_report_position_from_pixels(28, 20, 10, 20, 8, 80, 24, 0),
            MouseReportPosition {
                col: 2,
                row: 0,
                pixel_x: 20,
                pixel_y: 0,
            }
        );
        assert_eq!(
            mouse_report_position_from_pixels(28, 20, 10, 20, 8, 80, 24, 3),
            MouseReportPosition {
                col: 2,
                row: 3,
                pixel_x: 20,
                pixel_y: 60,
            }
        );
    }

    #[test]
    fn app_mouse_position_maps_rendered_blocks_to_active_rows() {
        let mut term = terminal41::test_support::TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");

        assert_eq!(
            app_mouse_report_position_for_terminal(
                &term.inner,
                MouseReportPosition {
                    col: 3,
                    row: 4,
                    pixel_x: 30,
                    pixel_y: 87,
                },
                20,
            ),
            Some(MouseReportPosition {
                col: 3,
                row: 0,
                pixel_x: 30,
                pixel_y: 7,
            })
        );
        assert_eq!(
            app_mouse_report_position_for_terminal(
                &term.inner,
                MouseReportPosition {
                    col: 3,
                    row: 2,
                    pixel_x: 30,
                    pixel_y: 47,
                },
                20,
            ),
            None
        );
    }

    #[test]
    fn command_editor_mouse_paste_takes_over_when_open() {
        assert_eq!(
            command_editor_mouse_paste_kind(true, true, MouseButton::Right),
            Some(ClipboardKind::Clipboard)
        );
        assert_eq!(
            command_editor_mouse_paste_kind(true, true, MouseButton::Middle),
            Some(ClipboardKind::Primary)
        );
        assert_eq!(
            command_editor_mouse_paste_kind(false, true, MouseButton::Right),
            None
        );
        assert_eq!(
            command_editor_mouse_paste_kind(true, false, MouseButton::Right),
            None
        );
    }

    #[test]
    fn mouse_modifiers_include_keyboard_event_state_when_modifier_event_lags() {
        let mut keyboard = KeyboardRuntime {
            modifiers: ModifiersState::empty(),
            physical_modifiers: PhysicalModifierState::default(),
            ime_preedit_active: false,
        };

        sync_modifier_key_from_keyboard_event(
            &mut keyboard,
            PhysicalKey::Code(KeyCode::ShiftLeft),
            ElementState::Pressed,
        );

        assert!(effective_mouse_modifiers(&keyboard).shift_key());
        assert!(mouse_modifiers(&keyboard).shift);
    }

    #[test]
    fn physical_shift_state_tracks_left_and_right_keys_independently() {
        let mut keyboard = KeyboardRuntime {
            modifiers: ModifiersState::empty(),
            physical_modifiers: PhysicalModifierState::default(),
            ime_preedit_active: false,
        };

        sync_modifier_key_from_keyboard_event(
            &mut keyboard,
            PhysicalKey::Code(KeyCode::ShiftLeft),
            ElementState::Pressed,
        );
        sync_modifier_key_from_keyboard_event(
            &mut keyboard,
            PhysicalKey::Code(KeyCode::ShiftRight),
            ElementState::Pressed,
        );
        sync_modifier_key_from_keyboard_event(
            &mut keyboard,
            PhysicalKey::Code(KeyCode::ShiftLeft),
            ElementState::Released,
        );

        assert!(effective_mouse_modifiers(&keyboard).shift_key());

        sync_modifier_key_from_keyboard_event(
            &mut keyboard,
            PhysicalKey::Code(KeyCode::ShiftRight),
            ElementState::Released,
        );

        assert!(!effective_mouse_modifiers(&keyboard).shift_key());
    }

    #[test]
    fn copy_source_prefers_terminal_selection_over_editor_selection() {
        assert_eq!(
            selection_copy_source(true, true, true),
            Some(SelectionCopySource::Terminal)
        );
        assert_eq!(
            selection_copy_source(false, true, true),
            Some(SelectionCopySource::Editor)
        );
        assert_eq!(selection_copy_source(false, true, false), None);
        assert_eq!(selection_copy_source(false, false, true), None);
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
        assert_eq!(command_editor_input_context(&term, false), None);

        term.process(b"\x1b]133;B\x07");

        assert_eq!(
            command_editor_view_context(&term),
            Some(CommandEditorContext { current_dir: None })
        );
        assert_eq!(
            command_editor_input_context(&term, false),
            Some(CommandEditorContext { current_dir: None })
        );
    }

    #[test]
    fn command_editor_context_hides_while_command_is_running() {
        let mut term = TestTerm::new_80x24();
        term.process(b"\x1b]133;B\x07");
        term.process(b"\x1b]133;C\x07");

        assert_eq!(
            term.metadata.shell_integration_phase,
            ShellIntegrationPhase::Output
        );
        assert_eq!(command_editor_view_context(&term), None);
        assert_eq!(command_editor_input_context(&term, false), None);
        assert_eq!(command_editor_input_context(&term, true), None);
    }

    #[test]
    fn command_editor_view_context_returns_after_command_phase_resumes() {
        let mut term = TestTerm::new_80x24();
        term.process(b"\x1b[?1000h");

        assert!(host::mouse_tracking_enabled(term.modes.mouse_tracking));
        assert_eq!(command_editor_view_context(&term), None);
        assert_eq!(command_editor_input_context(&term, true), None);

        term.process(b"\x1b[?1000l\x1b]133;C\x07");

        assert_eq!(
            term.metadata.shell_integration_phase,
            ShellIntegrationPhase::Output
        );
        assert_eq!(command_editor_view_context(&term), None);

        term.process(b"\x1b]133;B\x07");

        assert_eq!(
            term.metadata.shell_integration_phase,
            ShellIntegrationPhase::Command
        );
        assert_eq!(
            command_editor_view_context(&term),
            Some(CommandEditorContext { current_dir: None })
        );
    }

    #[test]
    fn command_editor_view_context_keeps_prompt_editor_despite_prompt_keypad_modes() {
        let mut term = TestTerm::new_80x24();
        term.process(b"\x1b]133;B\x07");

        assert_eq!(
            term.metadata.shell_integration_phase,
            ShellIntegrationPhase::Command
        );
        assert_eq!(
            command_editor_view_context(&term),
            Some(CommandEditorContext { current_dir: None })
        );

        term.process(b"\x1b[?1h\x1b=");

        assert!(term.active.app_cursor_keys);
        assert!(term.active.app_keypad);
        assert_eq!(
            command_editor_view_context(&term),
            Some(CommandEditorContext { current_dir: None })
        );
        assert_eq!(
            command_editor_input_context(&term, true),
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
        assert_eq!(command_editor_input_context(&term, true), None);
    }

    #[test]
    fn command_editor_terminal_row_offset_requires_visible_editor() {
        let mut term = TestTerm::new_80x24();

        assert_eq!(command_editor_terminal_row_offset(&term, false), 0);
        assert_eq!(
            command_editor_terminal_row_offset(&term, true),
            COMMAND_EDITOR_BOX_ROWS
        );

        term.active.cursor.row = 23;
        assert_eq!(
            command_editor_terminal_row_offset(&term, true),
            COMMAND_EDITOR_BOX_ROWS
        );

        term.active.cursor.row = 21;
        assert_eq!(
            command_editor_terminal_row_offset(&term, true),
            COMMAND_EDITOR_BOX_ROWS
        );

        term.active.cursor.row = 20;
        assert_eq!(
            command_editor_terminal_row_offset(&term, true),
            COMMAND_EDITOR_BOX_ROWS
        );

        let mut term = TestTerm::new(10, 5, 100, 16, 8);
        term.process(b"one");
        term.process(b"\x1b]133;A\x07two");
        term.process(b"\x1b]133;A\x07three");
        assert_eq!(
            command_editor_visual_cursor_row(&term),
            term.viewport.rows - 1
        );
        assert_eq!(
            command_editor_terminal_row_offset(&term, true),
            COMMAND_EDITOR_BOX_ROWS
        );

        open_search(&mut term.search);
        assert_eq!(command_editor_terminal_row_offset(&term, true), 0);

        let mut term = TestTerm::new_80x24();
        term.process(b"\x1b[?1049h");
        assert_eq!(command_editor_terminal_row_offset(&term, true), 0);
    }

    #[test]
    fn command_editor_placement_stays_below_prompt_and_expands_to_available_rows() {
        assert_eq!(
            command_editor_placement_for_cursor(0, 24),
            CommandEditorPlacement {
                top_row: 1,
                rows: 23,
                terminal_row_offset: 0,
            }
        );

        assert_eq!(
            command_editor_placement_for_cursor(20, 24),
            CommandEditorPlacement {
                top_row: 21,
                rows: 3,
                terminal_row_offset: 0,
            }
        );

        assert_eq!(
            command_editor_placement_for_cursor(23, 24),
            CommandEditorPlacement {
                top_row: 21,
                rows: 3,
                terminal_row_offset: 3,
            }
        );
    }

    #[test]
    fn command_editor_popup_side_chooses_from_editor_cursor_position() {
        assert_eq!(
            command_editor_popup_side_for_row(3, 24),
            CommandEditorPopupSide::Below
        );
        assert_eq!(
            command_editor_popup_side_for_row(12, 24),
            CommandEditorPopupSide::Above
        );
    }

    #[test]
    fn command_editor_view_state_is_scoped_to_its_tab() {
        let view = CommandLineView {
            text: "cargo test".to_owned(),
            cursor: "cargo test".len(),
            cursor_style: CommandEditorCursorStyle::Beam,
            spans: Vec::new(),
            selection: None,
            completion: None,
            candidates: Vec::new(),
            candidate_index: 0,
        };
        let mut state = HashMap::new();
        state.insert(TabId(7), view);

        assert!(command_editor_view_for_tab_state(&state, TabId(7)).is_some());
        assert!(command_editor_view_for_tab_state(&state, TabId(8)).is_none());
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
            command_editor_input(&Key::Character("c".into()), ModifiersState::CONTROL, false),
            Some(EditorInput::Cancel)
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
        assert_eq!(
            command_editor_input(&Key::Character("c".into()), ModifiersState::CONTROL, true),
            Some(EditorInput::Cancel)
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
    fn ignored_empty_editor_control_inputs_fall_through_to_pty() {
        assert!(ignored_command_editor_input_falls_through(
            &EditorInput::Cancel,
            &Key::Character("c".into()),
            ModifiersState::CONTROL,
            true,
        ));
        assert!(ignored_command_editor_input_falls_through(
            &EditorInput::Delete,
            &Key::Character("d".into()),
            ModifiersState::CONTROL,
            true,
        ));
        assert!(!ignored_command_editor_input_falls_through(
            &EditorInput::Delete,
            &Key::Character("d".into()),
            ModifiersState::CONTROL,
            false,
        ));
        assert!(!ignored_command_editor_input_falls_through(
            &EditorInput::Delete,
            &Key::Named(NamedKey::Delete),
            ModifiersState::empty(),
            true,
        ));
    }

    #[test]
    fn plain_control_character_excludes_shift_alt_and_super() {
        let key = Key::Character("c".into());
        assert!(plain_control_character_key(
            &key,
            ModifiersState::CONTROL,
            "c"
        ));
        assert!(!plain_control_character_key(
            &key,
            ModifiersState::CONTROL | ModifiersState::SHIFT,
            "c"
        ));
        assert!(!plain_control_character_key(
            &key,
            ModifiersState::CONTROL | ModifiersState::ALT,
            "c"
        ));
        assert!(!plain_control_character_key(
            &key,
            ModifiersState::CONTROL | ModifiersState::SUPER,
            "c"
        ));
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
            command_editor_byte_index_at_cell(&view, 80, 3, 0, 0),
            "one\n".len()
        );
        assert_eq!(
            command_editor_byte_index_at_cell(&view, 80, 3, 2, 2),
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

        let text = popup_command_text(
            PromptRef {
                rendered_row: 0,
                active_abs_row: Some(0),
            },
            &term.metadata.command_metas,
            &term.active,
        );
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

        let text = popup_command_text(
            PromptRef {
                rendered_row: 0,
                active_abs_row: Some(0),
            },
            &term.metadata.command_metas,
            &term.active,
        );
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

use super::*;

impl WindowHost {
    pub(crate) fn handle_recording_popup_key(
        &mut self,
        key: &Key,
    ) -> bool {
        let Some(popup) = self.recording_popup.as_ref() else {
            return false;
        };
        match popup {
            RecordingPopupState::PendingStart { path } => match key {
                Key::Named(NamedKey::Enter) => {
                    let path = path.clone();
                    let Some(target) = self.active_input_target() else {
                        self.dismiss_recording_popup();
                        return true;
                    };
                    match target.recorder.start(path.clone()) {
                        Ok(()) => {
                            self.dismiss_recording_popup();
                        }
                        Err(e) => {
                            self.show_recording_error_popup(e);
                        }
                    }
                    true
                }
                Key::Named(NamedKey::Escape) => {
                    self.dismiss_recording_popup();
                    true
                }
                _ => false,
            },
            RecordingPopupState::Completed { .. } => match key {
                Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Escape) => {
                    self.dismiss_recording_popup();
                    true
                }
                _ => false,
            },
        }
    }

    pub(crate) fn handle_permission_modal_key(
        &mut self,
        key: &Key,
    ) {
        let Some(decision) = permission_key_decision(key) else {
            return;
        };
        self.settle_permission_modal(decision);
    }

    pub(crate) fn handle_command_palette_key(
        &mut self,
        tab_id: TabId,
        key: &Key,
    ) -> bool {
        if !self.command_palette_is_open() {
            return false;
        }

        let action = match key {
            Key::Named(NamedKey::Escape) => {
                self.close_command_palette();
                None
            }
            Key::Named(NamedKey::ArrowUp) => {
                self.move_command_palette_selection(-1);
                None
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.move_command_palette_selection(1);
                None
            }
            Key::Named(NamedKey::Backspace) => {
                self.update_command_palette_query(|query| {
                    query.pop();
                });
                None
            }
            Key::Named(NamedKey::Enter) => self.accept_command_palette_selection(),
            Key::Named(NamedKey::Space)
                if !self.modifiers.control_key()
                    && !self.modifiers.alt_key()
                    && !self.modifiers.super_key() =>
            {
                self.update_command_palette_query(|query| {
                    query.push(' ');
                });
                None
            }
            Key::Character(text)
                if !self.modifiers.control_key()
                    && !self.modifiers.alt_key()
                    && !self.modifiers.super_key() =>
            {
                self.update_command_palette_query(|query| {
                    query.push_str(text);
                });
                None
            }
            _ => None,
        };

        if let Some(action) = action {
            if self.run_local_action(action, tab_id) {
                self.notify_interaction_changed();
            } else {
                self.send(RenderEvent::Action(action));
            }
        }
        true
    }

    pub(crate) fn write_host_bytes(
        target: &mut InputEndpoint,
        host_bytes: Vec<u8>,
        reset_viewport: bool,
    ) {
        if host_bytes.is_empty() {
            return;
        }
        let _ = target.writer.write(&host_bytes);
        if reset_viewport {
            let mut terminal = target.terminal.lock();
            reset_viewport_and_invalidate(&mut terminal);
            unpark_thread_if_started(&target.terminal_thread);
        }
    }

    pub(crate) fn emit_host_input(
        target: &mut InputEndpoint,
        input: HostInput<'_>,
        reset_viewport: bool,
    ) {
        let effects = {
            let mut terminal = target.terminal.lock();
            apply_host_input(&mut terminal, input)
        };
        unpark_thread_if_started(&target.terminal_thread);
        Self::write_host_bytes(target, effects.host_bytes, reset_viewport);
    }

    pub(crate) fn apply_terminal_effects(
        &mut self,
        tab_id: TabId,
        effects: TerminalEffects,
    ) {
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return;
        };
        let TerminalEffects {
            host_bytes,
            resize_request,
            bell,
            clipboard_requests,
            kitty_file_requests,
        } = effects;
        Self::write_host_bytes(target, host_bytes, false);
        if let Some((cols, rows)) = resize_request
            && self.active_input_tab == Some(tab_id)
        {
            self.request_window_grid_size(cols, rows);
        }
        if bell {
            self.send(RenderEvent::Bell(tab_id));
        }
        for request in clipboard_requests {
            self.request_clipboard_permission(tab_id, request);
        }
        for request in kitty_file_requests {
            self.request_kitty_file_permission(tab_id, request);
        }
        if self.active_input_tab == Some(tab_id) {
            self.refresh_command_editor_view();
        }
    }

    pub(crate) fn request_clipboard_permission(
        &mut self,
        tab_id: TabId,
        request: ClipboardRequest,
    ) {
        let (response_tx, response_rx) = mpsc::channel();
        self.request_permission(request.permission_feature().to_string(), response_tx);
        let proxy = self.event_proxy.clone();
        thread::spawn(move || {
            let decision = response_rx.recv().unwrap_or(PermissionDecision::Deny);
            let _ = proxy.send_event(AppEvent::ResolveClipboardRequest {
                tab_id,
                request,
                decision,
            });
        });
    }

    pub(crate) fn resolve_clipboard_request(
        &mut self,
        tab_id: TabId,
        request: ClipboardRequest,
        decision: PermissionDecision,
    ) {
        if decision != PermissionDecision::Allow {
            return;
        }
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return;
        };
        let host_bytes = {
            let mut terminal = target.terminal.lock();
            terminal41::io::clipboard::apply_clipboard_request(&mut terminal.clipboard, request)
        };
        unpark_thread_if_started(&target.terminal_thread);
        Self::write_host_bytes(target, host_bytes, false);
    }

    pub(crate) fn request_kitty_file_permission(
        &mut self,
        tab_id: TabId,
        request: KittyFileRequest,
    ) {
        let (response_tx, response_rx) = mpsc::channel();
        self.request_permission(request.permission_feature(), response_tx);
        let proxy = self.event_proxy.clone();
        thread::spawn(move || {
            let decision = response_rx.recv().unwrap_or(PermissionDecision::Deny);
            let _ = proxy.send_event(AppEvent::ResolveKittyFileRequest {
                tab_id,
                request,
                decision,
            });
        });
    }

    pub(crate) fn resolve_kitty_file_request(
        &mut self,
        tab_id: TabId,
        request: KittyFileRequest,
        decision: PermissionDecision,
    ) {
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return;
        };
        let effects = {
            let mut terminal = target.terminal.lock();
            match decision {
                PermissionDecision::Allow => terminal.apply_kitty_file_request(request),
                PermissionDecision::Deny => terminal.deny_kitty_file_request(request),
            }
        };
        unpark_thread_if_started(&target.terminal_thread);
        Self::write_host_bytes(target, effects.host_bytes, false);
    }

    pub(crate) fn handle_focus_event(
        &mut self,
        focused: bool,
    ) {
        {
            let Some(target) = self.active_input_target() else {
                return;
            };
            Self::emit_host_input(target, HostInput::FocusChanged { focused }, true);
        }
        self.notify_interaction_changed();
    }

    pub(crate) fn handle_search_key(
        &self,
        target: &InputEndpoint,
        key: &Key,
    ) {
        let shift = self.modifiers.shift_key();
        let mut guard = target.terminal.lock();
        let terminal = &mut *guard;
        match key {
            Key::Named(NamedKey::Escape) => {
                close_search(&mut terminal.search, &mut terminal.selection);
            }
            Key::Named(NamedKey::Backspace) => {
                terminal.active.offset =
                    search_backspace(&mut terminal.search, &terminal.active, &terminal.viewport);
            }
            Key::Named(NamedKey::Enter) => {
                if shift {
                    terminal.active.offset = search_step_prev(
                        &mut terminal.search,
                        &terminal.active,
                        &terminal.viewport,
                    );
                } else {
                    terminal.active.offset = search_step_next(
                        &mut terminal.search,
                        &terminal.active,
                        &terminal.viewport,
                    );
                }
            }
            Key::Named(NamedKey::Space) => {
                terminal.active.offset = search_append(
                    &mut terminal.search,
                    &terminal.active,
                    &terminal.viewport,
                    " ",
                );
            }
            Key::Character(s) => {
                terminal.active.offset = search_append(
                    &mut terminal.search,
                    &terminal.active,
                    &terminal.viewport,
                    s,
                );
            }
            _ => {}
        }
        terminal.invalidate_snapshot_rows();
    }

    pub(crate) fn handle_command_editor_key(
        &mut self,
        tab_id: TabId,
        key: &Key,
    ) -> bool {
        let config = self.command_editor_config();
        if !config.enabled {
            return false;
        }
        self.command_catalog.refresh_for_config(&config);
        let command_words = self.command_catalog.names().to_vec();
        let Some(input) = command_editor_input(key, self.modifiers, config.vim_mode) else {
            return false;
        };
        let editor_context = {
            let Some(target) = self.input_endpoints.get(&tab_id) else {
                return false;
            };
            let terminal = target.terminal.lock();
            command_editor_input_context(&terminal)
        };
        let Some(context) = editor_context else {
            return false;
        };
        let history_cwd = context.current_dir.clone();
        let history_entries = self.command_editor_history_entries(&config, history_cwd.as_deref());
        let (handled, view, submitted_command) = {
            let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
                return false;
            };
            let settings = Self::command_editor_settings(
                &config,
                context.current_dir,
                command_words,
                history_entries,
            );
            let outcome = apply_input(&mut target.command_editor, input.clone(), &settings);
            match outcome {
                EditOutcome::Submitted(command) => {
                    let history_command = command.clone();
                    let mut bytes = command.into_bytes();
                    bytes.push(b'\r');
                    Self::write_host_bytes(target, bytes, true);
                    let view =
                        command_editor_view(&target.command_editor, &settings, config.vim_mode);
                    (true, view, Some(history_command))
                }
                EditOutcome::Updated => {
                    let view =
                        command_editor_view(&target.command_editor, &settings, config.vim_mode);
                    (true, view, None)
                }
                EditOutcome::Canceled => {
                    let view =
                        command_editor_view(&target.command_editor, &settings, config.vim_mode);
                    (true, view, None)
                }
                EditOutcome::Ignored => {
                    if input == EditorInput::Cancel {
                        (false, None, None)
                    } else {
                        let view =
                            command_editor_view(&target.command_editor, &settings, config.vim_mode);
                        (true, view, None)
                    }
                }
            }
        };
        if handled {
            if let (Some(command), Some(cwd)) = (submitted_command, history_cwd) {
                self.enqueue_persistent_command_history(command, cwd, &config);
            }
            self.clear_terminal_selection_for_tab(tab_id);
            self.set_command_editor_view(tab_id, view);
        }
        handled
    }

    pub(crate) fn handle_command_editor_clipboard_action(
        &mut self,
        tab_id: TabId,
        action: Action,
    ) -> bool {
        let config = self.command_editor_config();
        if !config.enabled || action != Action::Paste {
            return false;
        }
        self.command_catalog.refresh_for_config(&config);
        let command_words = self.command_catalog.names().to_vec();
        let context = {
            let Some(target) = self.input_endpoints.get(&tab_id) else {
                return false;
            };
            let terminal = target.terminal.lock();
            command_editor_input_context(&terminal)
        };
        let Some(context) = context else {
            return false;
        };
        let history_entries =
            self.command_editor_history_entries(&config, context.current_dir.as_deref());
        let (handled_action, view) = {
            let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
                return false;
            };
            let settings = Self::command_editor_settings(
                &config,
                context.current_dir,
                command_words,
                history_entries,
            );

            match action {
                Action::Paste => {
                    let text = {
                        let mut terminal = target.terminal.lock();
                        terminal.clipboard.get(ClipboardKind::Clipboard)
                    };
                    if let Some(text) = text {
                        apply_input(
                            &mut target.command_editor,
                            EditorInput::Insert(text),
                            &settings,
                        );
                    }
                    (
                        true,
                        command_editor_view(&target.command_editor, &settings, config.vim_mode),
                    )
                }
                _ => (false, None),
            }
        };
        if handled_action {
            self.set_command_editor_view(tab_id, view);
        }
        handled_action
    }

    pub(crate) fn run_local_action(
        &mut self,
        action: Action,
        tab_id: TabId,
    ) -> bool {
        if matches!(action, Action::Copy) {
            self.stop_selection_drag();
        }
        if action == Action::OpenCommandPalette {
            self.open_command_palette();
            return true;
        }
        if action == Action::ToggleCommandEditor {
            self.toggle_command_editor();
            return true;
        }
        if action == Action::Copy {
            let editor_open = self.command_editor_is_open_for_tab(tab_id);
            self.copy_active_selection_to_clipboard(
                tab_id,
                ClipboardKind::Clipboard,
                false,
                editor_open,
            );
            return true;
        }
        if self.handle_command_editor_clipboard_action(tab_id, action) {
            self.clear_terminal_selection_for_tab(tab_id);
            return true;
        }
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return true;
        };
        match action {
            Action::ScrollPageUp => {
                let mut terminal = target.terminal.lock();
                let rows = terminal.viewport.rows;
                let viewport = terminal.viewport;
                view::scroll_viewport_up(&mut terminal.active, &viewport, rows);
                terminal.invalidate_snapshot_rows();
                true
            }
            Action::ScrollPageDown => {
                let mut terminal = target.terminal.lock();
                let rows = terminal.viewport.rows;
                view::scroll_viewport_down(&mut terminal.active, rows);
                terminal.invalidate_snapshot_rows();
                true
            }
            Action::Copy => true,
            Action::Paste => {
                Self::emit_host_input(
                    target,
                    HostInput::PasteFromClipboard {
                        kind: ClipboardKind::Clipboard,
                    },
                    true,
                );
                true
            }
            Action::OpenSearch => {
                let mut terminal = target.terminal.lock();
                open_search(&mut terminal.search);
                terminal.invalidate_snapshot_rows();
                true
            }
            Action::ScrollPrevPrompt => {
                let mut terminal = target.terminal.lock();
                let viewport = terminal.viewport;
                view::scroll_to_prev_prompt(&mut terminal.active, &viewport);
                terminal.invalidate_snapshot_rows();
                true
            }
            Action::ScrollNextPrompt => {
                let mut terminal = target.terminal.lock();
                let viewport = terminal.viewport;
                view::scroll_to_next_prompt(&mut terminal.active, &viewport);
                terminal.invalidate_snapshot_rows();
                true
            }
            Action::OpenNewWindow => {
                let cwd = target.terminal.lock().metadata.current_directory.clone();
                spawn_new_window(cwd);
                true
            }
            Action::CloseWindow => {
                self.send(RenderEvent::Action(Action::CloseWindow));
                true
            }
            Action::ToggleOutputRecording => {
                if target.recorder.is_active() {
                    if let Some(path) = target.recorder.stop() {
                        self.show_recording_completed_popup(path);
                    }
                } else {
                    self.show_recording_start_popup(next_recording_path());
                }
                true
            }
            Action::CycleEmojiCompatibility => {
                let mode = target.terminal.lock().cycle_emoji_compatibility_mode();
                info!("emoji compatibility mode: {}", mode.label());
                self.show_toast(format!("Emoji compatibility: {}", mode.label()));
                true
            }
            Action::NewTab
            | Action::CloseActiveTab
            | Action::NextTab
            | Action::PrevTab
            | Action::PasteAsBackground
            | Action::ClearPastedBackground
            | Action::ToggleCommandEditor
            | Action::OpenCommandPalette => false,
        }
    }

    pub(crate) fn handle_key_event(
        &mut self,
        key: Key,
        location: KeyLocation,
        physical: PhysicalKey,
    ) {
        if self.permission_modal.is_some() {
            self.handle_permission_modal_key(&key);
            return;
        }

        if self.ime_preedit_active && matches!(key, Key::Character(_)) {
            return;
        }

        let Some(active_tab_id) = self.active_input_tab else {
            return;
        };

        if self.handle_command_palette_key(active_tab_id, &key) {
            return;
        }

        if self.recording_popup.is_some() {
            let _ = self.handle_recording_popup_key(&key);
            return;
        }

        let res = {
            let terminal = self.input_endpoints[&active_tab_id].terminal.lock();
            search_active(&terminal.search)
        };
        if res {
            let target = &self.input_endpoints[&active_tab_id];
            self.handle_search_key(target, &key);
            self.notify_interaction_changed();
            return;
        }

        if self.handle_command_editor_key(active_tab_id, &key) {
            return;
        }

        if let Some(action) = self.keybindings().lookup(&key, self.modifiers) {
            if self.run_local_action(action, active_tab_id) {
                self.notify_interaction_changed();
            } else {
                self.send(RenderEvent::Action(action));
            }
            return;
        }

        let Some(target) = self.input_endpoints.get_mut(&active_tab_id) else {
            return;
        };

        if let Some(selector) = dec_udk_selector(&key, self.modifiers) {
            let bytes = { target.terminal.lock().user_defined_key(selector) };
            if let Some(bytes) = bytes {
                reset_viewport_and_invalidate(&mut target.terminal.lock());
                let _ = target.writer.write(&bytes);
                self.notify_interaction_changed();
                return;
            }
        }

        if let Some(selector) = dec_local_function_key_selector(&key, self.modifiers) {
            let control = { target.terminal.lock().local_function_key_control(selector) };
            match control {
                Some(terminal41::LocalFunctionKeyControl::Local)
                | Some(terminal41::LocalFunctionKeyControl::Disabled) => {
                    self.notify_interaction_changed();
                    return;
                }
                Some(terminal41::LocalFunctionKeyControl::SendSequence) | None => {}
            }
        }

        let (kitty_flags, c1_mode) = {
            let terminal = target.terminal.lock();
            (terminal.kitty_keyboard.current(), terminal.modes.c1_mode)
        };
        if let Some(bytes) = kitty_encode_input(&key, self.modifiers, kitty_flags, c1_mode) {
            reset_viewport_and_invalidate(&mut target.terminal.lock());
            let _ = target.writer.write(&bytes);
            self.notify_interaction_changed();
            return;
        }

        if self.modifiers.control_key() {
            let byte = match &key {
                Key::Character(c) => ctrl_byte(c),
                Key::Named(NamedKey::Space) => Some(0x00),
                _ => None,
            };

            if let Some(byte) = byte {
                if byte == 0x03 {
                    crate::perf_ctrl_c::record_ctrl_c_hit(active_tab_id);
                }
                reset_viewport_and_invalidate(&mut target.terminal.lock());
                if self.modifiers.alt_key() {
                    let _ = target.writer.write(&[0x1b, byte]);
                } else {
                    let _ = target.writer.write(&[byte]);
                }
                self.notify_interaction_changed();
                return;
            }
        }

        let (app_cursor_keys, app_keypad, c1_mode) = {
            let terminal = target.terminal.lock();
            (
                terminal.active.app_cursor_keys,
                terminal.active.app_keypad,
                terminal.modes.c1_mode,
            )
        };

        let bytes = match &key {
            Key::Character(c) => {
                if let Some(bytes) = legacy_encode_numpad_character(
                    c,
                    location,
                    physical,
                    self.modifiers,
                    app_keypad,
                    c1_mode,
                ) {
                    Some(bytes)
                } else if self.modifiers.alt_key() {
                    let mut v = vec![0x1b];
                    v.extend_from_slice(c.as_bytes());
                    Some(v)
                } else {
                    Some(c.as_bytes().to_vec())
                }
            }
            Key::Named(named) => legacy_encode_named(
                *named,
                location,
                self.modifiers,
                app_cursor_keys,
                app_keypad,
                c1_mode,
            ),
            _ => None,
        };

        if let Some(bytes) = bytes {
            reset_viewport_and_invalidate(&mut target.terminal.lock());
            let _ = target.writer.write(&bytes);
            self.notify_interaction_changed();
        }
    }

    pub(crate) fn handle_modifiers_changed(
        &mut self,
        mods: ModifiersState,
    ) {
        let old = self.modifiers;
        self.modifiers = mods;

        let Some(target) = self.active_input_target() else {
            return;
        };

        let changes = [
            (
                terminal41::DecModifierKey::LeftShift,
                old.shift_key(),
                mods.shift_key(),
            ),
            (
                terminal41::DecModifierKey::Ctrl,
                old.control_key(),
                mods.control_key(),
            ),
            (
                terminal41::DecModifierKey::LeftAltFunction,
                old.alt_key(),
                mods.alt_key(),
            ),
        ];

        let bytes = {
            let terminal = target.terminal.lock();
            let mut bytes = Vec::new();
            for (key, was_pressed, is_pressed) in changes {
                if was_pressed == is_pressed {
                    continue;
                }
                if let Some(report) = terminal.dec_modifier_key_report(key, is_pressed) {
                    bytes.extend(report);
                }
            }
            bytes
        };
        Self::write_host_bytes(target, bytes, false);
    }

    pub(crate) fn handle_ime_commit(
        &mut self,
        text: &str,
    ) {
        if text.is_empty() {
            return;
        }
        let Some(target) = self.active_input_target() else {
            return;
        };
        let (flags, c1_mode) = {
            let terminal = target.terminal.lock();
            (terminal.kitty_keyboard.current(), terminal.modes.c1_mode)
        };
        let bytes = if flags.contains(terminal41::KittyFlags::REPORT_ASSOCIATED_TEXT) {
            kitty_encode_ime_commit(text, c1_mode)
        } else {
            text.as_bytes().to_vec()
        };
        reset_viewport_and_invalidate(&mut target.terminal.lock());
        let _ = target.writer.write(&bytes);
        self.notify_interaction_changed();
    }
}

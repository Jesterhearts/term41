use super::*;

pub(crate) fn handle_recording_popup_key(
    host: &mut WindowHost,
    key: &Key,
) -> bool {
    let Some(popup) = host.modals.recording_popup.as_ref() else {
        return false;
    };
    match popup {
        RecordingPopupState::PendingStart { path } => match key {
            Key::Named(NamedKey::Enter) => {
                let path = path.clone();
                let Some(target) = active_input_target(&mut host.input) else {
                    dismiss_recording_popup(
                        &mut host.modals,
                        &host.input,
                        &mut host.render,
                        &host.startup,
                        host.window.as_ref(),
                    );
                    return true;
                };
                match target.recorder.start(path.clone()) {
                    Ok(()) => {
                        dismiss_recording_popup(
                            &mut host.modals,
                            &host.input,
                            &mut host.render,
                            &host.startup,
                            host.window.as_ref(),
                        );
                    }
                    Err(e) => {
                        show_recording_error_popup(host, e);
                    }
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                dismiss_recording_popup(
                    &mut host.modals,
                    &host.input,
                    &mut host.render,
                    &host.startup,
                    host.window.as_ref(),
                );
                true
            }
            _ => false,
        },
        RecordingPopupState::Completed { .. } => match key {
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Escape) => {
                dismiss_recording_popup(
                    &mut host.modals,
                    &host.input,
                    &mut host.render,
                    &host.startup,
                    host.window.as_ref(),
                );
                true
            }
            _ => false,
        },
    }
}

pub(crate) fn handle_permission_modal_key(
    host: &mut WindowHost,
    key: &Key,
) {
    let Some(decision) = permission_key_decision(key) else {
        return;
    };
    settle_permission_modal(host, decision);
}

pub(crate) fn handle_command_palette_key(
    host: &mut WindowHost,
    tab_id: TabId,
    key: &Key,
) -> bool {
    if !command_palette_is_open(&host.render) {
        return false;
    }

    let action = match key {
        Key::Named(NamedKey::Escape) => {
            close_command_palette(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
            None
        }
        Key::Named(NamedKey::ArrowUp) => {
            move_host_command_palette_selection(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
                -1,
            );
            None
        }
        Key::Named(NamedKey::ArrowDown) => {
            move_host_command_palette_selection(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
                1,
            );
            None
        }
        Key::Named(NamedKey::Tab) => {
            complete_host_command_palette_selection(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
            None
        }
        Key::Named(NamedKey::Backspace) => {
            update_command_palette_query(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
                |query| {
                    query.pop();
                },
            );
            None
        }
        Key::Named(NamedKey::Enter) => accept_command_palette_selection(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        ),
        Key::Named(NamedKey::Space)
            if !host.keyboard.modifiers.control_key()
                && !host.keyboard.modifiers.alt_key()
                && !host.keyboard.modifiers.super_key() =>
        {
            update_command_palette_query(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
                |query| {
                    query.push(' ');
                },
            );
            None
        }
        Key::Character(text)
            if !host.keyboard.modifiers.control_key()
                && !host.keyboard.modifiers.alt_key()
                && !host.keyboard.modifiers.super_key() =>
        {
            update_command_palette_query(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
                |query| {
                    query.push_str(text);
                },
            );
            None
        }
        _ => None,
    };

    if let Some(invocation) = action {
        let action = invocation.action;
        if run_local_command_palette_invocation(host, invocation, tab_id) {
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
        } else {
            send(&mut host.render, RenderEvent::Action(action));
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

pub(super) fn command_submission_bytes(command: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(command.len() + 1);
    bytes.extend(command.bytes().map(|byte| match byte {
        b'\n' => b'\r',
        byte => byte,
    }));
    bytes.push(b'\r');
    bytes
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
    write_host_bytes(target, effects.host_bytes, reset_viewport);
}

pub(crate) fn apply_terminal_effects(
    host: &mut WindowHost,
    tab_id: TabId,
    effects: TerminalEffects,
) {
    let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
        return;
    };
    let TerminalEffects {
        host_bytes,
        input_context_changed: _,
        resize_request,
        bell,
        clipboard_requests,
        kitty_file_requests,
    } = effects;
    write_host_bytes(target, host_bytes, false);
    if let Some((cols, rows)) = resize_request
        && host.input.active_tab == Some(tab_id)
    {
        request_window_grid_size(host, cols, rows);
    }
    if bell {
        send(&mut host.render, RenderEvent::Bell(tab_id));
    }
    for request in clipboard_requests {
        request_clipboard_permission(host, tab_id, request);
    }
    for request in kitty_file_requests {
        request_kitty_file_permission(host, tab_id, request);
    }
    let has_cached_editor_view = {
        let state = host.render.input_state.lock();
        state.command_editor_views.contains_key(&tab_id)
    };
    if host.input.active_tab == Some(tab_id) || has_cached_editor_view {
        refresh_command_editor_view_for_tab(host, tab_id);
    }
}

pub(crate) fn request_clipboard_permission(
    host: &mut WindowHost,
    tab_id: TabId,
    request: ClipboardRequest,
) {
    let (response_tx, response_rx) = mpsc::channel();
    request_permission(host, request.permission_feature().to_string(), response_tx);
    let proxy = host.render.event_proxy.clone();
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
    input: &mut InputRuntime,
    tab_id: TabId,
    request: ClipboardRequest,
    decision: PermissionDecision,
) {
    if decision != PermissionDecision::Allow {
        return;
    }
    let Some(target) = input.endpoints.get_mut(&tab_id) else {
        return;
    };
    let host_bytes = {
        let mut terminal = target.terminal.lock();
        terminal41::io::clipboard::apply_clipboard_request(&mut terminal.clipboard, request)
    };
    unpark_thread_if_started(&target.terminal_thread);
    write_host_bytes(target, host_bytes, false);
}

pub(crate) fn request_kitty_file_permission(
    host: &mut WindowHost,
    tab_id: TabId,
    request: KittyFileRequest,
) {
    let (response_tx, response_rx) = mpsc::channel();
    request_permission(host, request.permission_feature(), response_tx);
    let proxy = host.render.event_proxy.clone();
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
    input: &mut InputRuntime,
    tab_id: TabId,
    request: KittyFileRequest,
    decision: PermissionDecision,
) {
    let Some(target) = input.endpoints.get_mut(&tab_id) else {
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
    write_host_bytes(target, effects.host_bytes, false);
}

pub(crate) fn handle_focus_event(
    input: &mut InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
    focused: bool,
) {
    {
        let Some(target) = active_input_target(input) else {
            return;
        };
        emit_host_input(target, HostInput::FocusChanged { focused }, true);
    }
    notify_interaction_changed(input, render, startup, window);
}

pub(crate) fn handle_search_key(
    keyboard: &KeyboardRuntime,
    target: &InputEndpoint,
    key: &Key,
) {
    let shift = keyboard.modifiers.shift_key();
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
                terminal.active.offset =
                    search_step_prev(&mut terminal.search, &terminal.active, &terminal.viewport);
            } else {
                terminal.active.offset =
                    search_step_next(&mut terminal.search, &terminal.active, &terminal.viewport);
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
    host: &mut WindowHost,
    tab_id: TabId,
    key: &Key,
) -> bool {
    let config = command_editor_config(&host.render);
    if !config.enabled {
        return false;
    }
    host.command.catalog.refresh_for_config(&config);
    let command_words = host.command.catalog.names().to_vec();
    let Some(input) = command_editor_input(key, host.keyboard.modifiers, config.vim_mode) else {
        return false;
    };
    let command_editor_open = command_editor_is_open_for_tab(&host.render, tab_id);
    let (editor_context, terminal_has_selection) = {
        let Some(target) = host.input.endpoints.get(&tab_id) else {
            return false;
        };
        let terminal = target.terminal.lock();
        (
            command_editor_input_context(&terminal, command_editor_open),
            terminal.has_selection(),
        )
    };
    if terminal_has_selection && plain_control_character_key(key, host.keyboard.modifiers, "c") {
        return false;
    }
    let Some(context) = editor_context else {
        return false;
    };
    let history_cwd = context.current_dir.clone();
    let history_entries = command_editor_history_entries(host, &config, history_cwd.as_deref());
    let (handled, view, submitted_command) = {
        let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
            return false;
        };
        let settings =
            command_editor_settings(&config, context.current_dir, command_words, history_entries);
        let editor_was_empty = target.command_editor.is_empty();
        let outcome = apply_input(&mut target.command_editor, input.clone(), &settings);
        match outcome {
            EditOutcome::Submitted(command) => {
                let history_command = command.clone();
                let bytes = command_submission_bytes(&command);
                write_host_bytes(target, bytes, true);
                let view = command_editor_view(&target.command_editor, &settings, config.vim_mode);
                (true, view, Some(history_command))
            }
            EditOutcome::Updated => {
                let view = command_editor_view(&target.command_editor, &settings, config.vim_mode);
                (true, view, None)
            }
            EditOutcome::Canceled => {
                let view = command_editor_view(&target.command_editor, &settings, config.vim_mode);
                (true, view, None)
            }
            EditOutcome::Ignored => {
                if ignored_command_editor_input_falls_through(
                    &input,
                    key,
                    host.keyboard.modifiers,
                    editor_was_empty,
                ) {
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
            enqueue_persistent_command_history(host, command, cwd, &config);
        }
        reset_tab_viewport_and_invalidate(&host.input.endpoints, tab_id);
        clear_terminal_selection_for_tab(host, tab_id);
        set_command_editor_view(host, tab_id, view);
    }
    handled
}

pub(crate) fn handle_command_editor_clipboard_action(
    host: &mut WindowHost,
    tab_id: TabId,
    action: Action,
) -> bool {
    let config = command_editor_config(&host.render);
    if !config.enabled || action != Action::Paste {
        return false;
    }
    host.command.catalog.refresh_for_config(&config);
    let command_words = host.command.catalog.names().to_vec();
    let command_editor_open = command_editor_is_open_for_tab(&host.render, tab_id);
    let context = {
        let Some(target) = host.input.endpoints.get(&tab_id) else {
            return false;
        };
        let terminal = target.terminal.lock();
        command_editor_input_context(&terminal, command_editor_open)
    };
    let Some(context) = context else {
        return false;
    };
    let history_entries =
        command_editor_history_entries(host, &config, context.current_dir.as_deref());
    let (handled_action, view) = {
        let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
            return false;
        };
        let settings =
            command_editor_settings(&config, context.current_dir, command_words, history_entries);

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
        reset_tab_viewport_and_invalidate(&host.input.endpoints, tab_id);
        set_command_editor_view(host, tab_id, view);
    }
    handled_action
}

pub(crate) fn run_local_action(
    host: &mut WindowHost,
    action: Action,
    tab_id: TabId,
) -> bool {
    if matches!(action, Action::Copy) {
        stop_selection_drag(&mut host.mouse);
    }
    if action == Action::OpenCommandPalette {
        open_command_palette(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return true;
    }
    if action == Action::ToggleCommandEditor {
        toggle_command_editor(host);
        return true;
    }
    if action == Action::Copy {
        let editor_open = command_editor_is_open_for_tab(&host.render, tab_id);
        copy_active_selection_to_clipboard(
            host,
            tab_id,
            ClipboardKind::Clipboard,
            false,
            editor_open,
        );
        return true;
    }
    if handle_command_editor_clipboard_action(host, tab_id, action) {
        clear_terminal_selection_for_tab(host, tab_id);
        return true;
    }
    let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
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
            emit_host_input(
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
            let document =
                command_block_document(&terminal.active, &terminal.metadata.command_metas);
            view::scroll_to_prev_prompt(&mut terminal.active, &viewport, &document);
            terminal.invalidate_snapshot_rows();
            true
        }
        Action::ScrollNextPrompt => {
            let mut terminal = target.terminal.lock();
            let viewport = terminal.viewport;
            let document =
                command_block_document(&terminal.active, &terminal.metadata.command_metas);
            view::scroll_to_next_prompt(&mut terminal.active, &viewport, &document);
            terminal.invalidate_snapshot_rows();
            true
        }
        Action::JumpToPreviousFailed => {
            let mut terminal = target.terminal.lock();
            let viewport = terminal.viewport;
            let document =
                command_block_document(&terminal.active, &terminal.metadata.command_metas);
            view::scroll_to_prev_failed_command(&mut terminal.active, &viewport, &document);
            terminal.invalidate_snapshot_rows();
            true
        }
        Action::JumpToPreviousCommand => {
            let mut terminal = target.terminal.lock();
            let viewport = terminal.viewport;
            let document =
                command_block_document(&terminal.active, &terminal.metadata.command_metas);
            view::scroll_to_prev_command(&mut terminal.active, &viewport, &document);
            terminal.invalidate_snapshot_rows();
            true
        }
        Action::JumpToPreviousSuccessful => {
            let mut terminal = target.terminal.lock();
            let viewport = terminal.viewport;
            let document =
                command_block_document(&terminal.active, &terminal.metadata.command_metas);
            view::scroll_to_prev_successful_command(&mut terminal.active, &viewport, &document);
            terminal.invalidate_snapshot_rows();
            true
        }
        Action::OpenNewWindow => {
            let cwd = target.terminal.lock().metadata.current_directory.clone();
            spawn_new_window(cwd);
            true
        }
        Action::CloseWindow => {
            send(&mut host.render, RenderEvent::Action(Action::CloseWindow));
            true
        }
        Action::ToggleOutputRecording => {
            if target.recorder.is_active() {
                if let Some(path) = target.recorder.stop() {
                    show_recording_completed_popup(host, path);
                }
            } else {
                show_recording_start_popup(host, next_recording_path());
            }
            true
        }
        Action::CycleEmojiCompatibility => {
            let mode = target.terminal.lock().cycle_emoji_compatibility_mode();
            info!("emoji compatibility mode: {}", mode.label());
            show_toast(host, format!("Emoji compatibility: {}", mode.label()));
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

pub(crate) fn run_local_command_palette_invocation(
    host: &mut WindowHost,
    invocation: CommandPaletteInvocation,
    tab_id: TabId,
) -> bool {
    match invocation.argument {
        Some(CommandPaletteArgument::WorkingDirectory(argument))
            if invocation.action == Action::OpenNewWindow =>
        {
            let cwd = host_command_palette_working_directory(host, tab_id, &argument);
            if let Some(dir) = cwd.as_ref()
                && !dir.is_dir()
            {
                show_toast(host, format!("No such directory: {}", dir.display()));
                return true;
            }
            spawn_new_window(cwd);
            true
        }
        Some(CommandPaletteArgument::WorkingDirectory(argument))
            if invocation.action == Action::NewTab =>
        {
            let cwd = host_command_palette_working_directory(host, tab_id, &argument);
            if let Some(dir) = cwd.as_ref()
                && !dir.is_dir()
            {
                show_toast(host, format!("No such directory: {}", dir.display()));
                return true;
            }
            send(&mut host.render, RenderEvent::SpawnNewTab { cwd });
            true
        }
        None => run_local_action(host, invocation.action, tab_id),
        Some(_) => {
            warn!(
                "command-palette: unsupported argument for {:?}",
                invocation.action
            );
            true
        }
    }
}

fn host_command_palette_working_directory(
    host: &WindowHost,
    tab_id: TabId,
    argument: &str,
) -> Option<PathBuf> {
    let current_dir = host
        .input
        .endpoints
        .get(&tab_id)
        .and_then(|target| target.terminal.lock().metadata.current_directory.clone());
    super::resolve_command_palette_working_directory(current_dir, argument)
}

pub(crate) fn handle_key_event(
    host: &mut WindowHost,
    key: Key,
    location: KeyLocation,
    physical: PhysicalKey,
) {
    if host.modals.permission_modal.is_some() {
        handle_permission_modal_key(host, &key);
        return;
    }

    if host.keyboard.ime_preedit_active && matches!(key, Key::Character(_)) {
        return;
    }

    let Some(active_tab_id) = host.input.active_tab else {
        return;
    };

    if handle_command_palette_key(host, active_tab_id, &key) {
        return;
    }

    if host.modals.recording_popup.is_some() {
        let _ = handle_recording_popup_key(host, &key);
        return;
    }

    let res = {
        let terminal = host.input.endpoints[&active_tab_id].terminal.lock();
        search_active(&terminal.search)
    };
    if res {
        let target = &host.input.endpoints[&active_tab_id];
        handle_search_key(&host.keyboard, target, &key);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if handle_command_editor_key(host, active_tab_id, &key) {
        return;
    }

    if let Some(action) = keybindings(&host.render).lookup(&key, host.keyboard.modifiers) {
        if run_local_action(host, action, active_tab_id) {
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
        } else {
            send(&mut host.render, RenderEvent::Action(action));
        }
        return;
    }

    let Some(target) = host.input.endpoints.get_mut(&active_tab_id) else {
        return;
    };

    if let Some(selector) = dec_udk_selector(&key, host.keyboard.modifiers) {
        let bytes = { target.terminal.lock().user_defined_key(selector) };
        if let Some(bytes) = bytes {
            reset_viewport_and_invalidate(&mut target.terminal.lock());
            let _ = target.writer.write(&bytes);
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
            return;
        }
    }

    if let Some(selector) = dec_local_function_key_selector(&key, host.keyboard.modifiers) {
        let control = { target.terminal.lock().local_function_key_control(selector) };
        match control {
            Some(terminal41::LocalFunctionKeyControl::Local)
            | Some(terminal41::LocalFunctionKeyControl::Disabled) => {
                notify_interaction_changed(
                    &host.input,
                    &mut host.render,
                    &host.startup,
                    host.window.as_ref(),
                );
                return;
            }
            Some(terminal41::LocalFunctionKeyControl::SendSequence) | None => {}
        }
    }

    let (kitty_flags, c1_mode) = {
        let terminal = target.terminal.lock();
        (terminal.kitty_keyboard.current(), terminal.modes.c1_mode)
    };
    if let Some(bytes) = kitty_encode_input(&key, host.keyboard.modifiers, kitty_flags, c1_mode) {
        reset_viewport_and_invalidate(&mut target.terminal.lock());
        let _ = target.writer.write(&bytes);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if host.keyboard.modifiers.control_key() {
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
            if host.keyboard.modifiers.alt_key() {
                let _ = target.writer.write(&[0x1b, byte]);
            } else {
                let _ = target.writer.write(&[byte]);
            }
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
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
                host.keyboard.modifiers,
                app_keypad,
                c1_mode,
            ) {
                Some(bytes)
            } else if host.keyboard.modifiers.alt_key() {
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
            host.keyboard.modifiers,
            app_cursor_keys,
            app_keypad,
            c1_mode,
        ),
        _ => None,
    };

    if let Some(bytes) = bytes {
        reset_viewport_and_invalidate(&mut target.terminal.lock());
        let _ = target.writer.write(&bytes);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
    }
}

pub(crate) fn handle_modifiers_changed(
    input: &mut InputRuntime,
    keyboard: &mut KeyboardRuntime,
    mods: ModifiersState,
) {
    let old = keyboard.modifiers;
    keyboard.modifiers = mods;

    let Some(target) = active_input_target(input) else {
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
    write_host_bytes(target, bytes, false);
}

pub(crate) fn sync_modifier_key_from_keyboard_event(
    keyboard: &mut KeyboardRuntime,
    physical: PhysicalKey,
    state: ElementState,
) {
    let pressed = state == ElementState::Pressed;
    match physical {
        PhysicalKey::Code(KeyCode::ShiftLeft) => keyboard.physical_modifiers.shift_left = pressed,
        PhysicalKey::Code(KeyCode::ShiftRight) => keyboard.physical_modifiers.shift_right = pressed,
        PhysicalKey::Code(KeyCode::ControlLeft) => {
            keyboard.physical_modifiers.control_left = pressed;
        }
        PhysicalKey::Code(KeyCode::ControlRight) => {
            keyboard.physical_modifiers.control_right = pressed;
        }
        PhysicalKey::Code(KeyCode::AltLeft) => keyboard.physical_modifiers.alt_left = pressed,
        PhysicalKey::Code(KeyCode::AltRight) => keyboard.physical_modifiers.alt_right = pressed,
        PhysicalKey::Code(KeyCode::SuperLeft) => keyboard.physical_modifiers.super_left = pressed,
        PhysicalKey::Code(KeyCode::SuperRight) => {
            keyboard.physical_modifiers.super_right = pressed;
        }
        _ => {}
    }
}

pub(crate) fn handle_ime_commit(
    input: &mut InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
    text: &str,
) {
    if text.is_empty() {
        return;
    }
    let Some(target) = active_input_target(input) else {
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
    notify_interaction_changed(input, render, startup, window);
}

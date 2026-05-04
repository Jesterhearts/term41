use super::*;

pub(crate) fn handle_cursor_moved(
    host: &mut WindowHost,
    x: f64,
    y: f64,
) {
    host.mouse.pos = (x, y);
    if host.modals.permission_modal.is_some() {
        update_permission_hover(
            host,
            permission_choice_at(&host.render, &host.metrics, x, y),
        );
        return;
    }
    if host.modals.recording_popup.is_some() {
        return;
    }

    let hovered_button = tab_bar_hover_at(&host.mouse, &host.render, &host.metrics);
    update_hovered_tab_bar_button(&host.render, hovered_button);

    let hovered_menu_item =
        tab_menu_item_at(&host.render, &host.metrics, x, y).map(|(_, _, idx)| idx);
    {
        let mut state = host.render.input_state.lock();
        if let Some(menu) = state.tab_context_menu.as_mut() {
            menu.hovered_item = hovered_menu_item;
        }
        let hovered_popup_item = popup_item_at(
            state.gutter_popup.as_ref(),
            x,
            y,
            state.cell_width,
            state.cell_height,
            state.gutter_width,
            host.metrics.window_size.1,
        );
        if let Some(popup) = state.gutter_popup.as_mut() {
            popup.hovered_item = hovered_popup_item;
        }
    }

    if let Some(dir) = resize_direction_at(host.window.as_ref(), &host.mouse, &host.metrics) {
        if let Some(w) = &host.window {
            w.set_cursor(winit::window::CursorIcon::from(dir));
        }
    } else if let Some(w) = &host.window {
        w.set_cursor(winit::window::CursorIcon::Default);
    }

    if host.mouse.command_editor_drag_anchor.is_some() {
        if extend_command_editor_selection_to_mouse(host) {
            host.mouse.selection_drag_moved = true;
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
        }
        return;
    }

    if host.mouse.left_drag_active && extend_selection_to_mouse(host) {
        host.mouse.selection_drag_moved = true;
        refresh_selection_autoscroll_direction(host);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if forward_mouse_to_app(&host.keyboard, &mut host.input)
        && let Some(pos) = app_mouse_report_position_at(host, x, y)
    {
        let motion_position = mouse_motion_position_key(host, pos);
        if host.mouse.last_motion_position == Some(motion_position) {
            return;
        }
        host.mouse.last_motion_position = Some(motion_position);
        let button = host.mouse.mouse_buttons.primary_held();
        let mods = mouse_modifiers(&host.keyboard);
        let Some(target) = active_input_target(&mut host.input) else {
            return;
        };
        emit_host_input(
            target,
            HostInput::Mouse(HostMouse {
                kind: MouseEventKind::Motion,
                button,
                col: pos.col,
                row: pos.row,
                pixel_x: pos.pixel_x,
                pixel_y: pos.pixel_y,
                mods,
            }),
            true,
        );
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn command_editor_offset_at_mouse(
    host: &mut WindowHost,
    x: f64,
    y: f64,
) -> Option<usize> {
    let (cell_w, cell_h, gutter_w, _) = layout_snapshot(&host.render);
    let cell_w = cell_w.max(1);
    let cell_h = cell_h.max(1);
    let raw_x = x.max(0.0) as u32;
    let raw_y = y.max(0.0) as u32;
    if raw_x < gutter_w || raw_y < cell_h {
        return None;
    }

    let tab_id = host.input.active_tab?;
    let target = host.input.endpoints.get(&tab_id)?;
    let command_editor_open = {
        let state = host.render.input_state.lock();
        command_editor_view_open_for_input_tab(&state, Some(tab_id))
    };
    if !command_editor_open {
        return None;
    }
    let (visual_cursor_row, viewport_rows, viewport_cols) = {
        let terminal = target.terminal.lock();
        if !matches!(
            terminal.metadata.shell_integration_phase,
            terminal41::ShellIntegrationPhase::Command | terminal41::ShellIntegrationPhase::None
        ) {
            return None;
        }
        command_editor_input_context(&terminal, command_editor_open)?;
        (
            command_editor_visual_cursor_row(&terminal),
            terminal.viewport.rows.max(1),
            terminal.viewport.cols.max(1),
        )
    };
    let view = {
        let state = host.render.input_state.lock();
        command_editor_view_for_input_tab(&state, tab_id).cloned()
    }?;

    let placement = command_editor_placement_for_cursor(visual_cursor_row, viewport_rows);
    let visible_rows = placement.rows;
    let box_top = placement.top_row as i32;
    let terminal_row = raw_y.saturating_sub(cell_h) / cell_h;
    let visible_row = terminal_row as i32 - box_top;
    if !(0..visible_rows as i32).contains(&visible_row) {
        return None;
    }

    let terminal_x = raw_x.saturating_sub(gutter_w);
    let terminal_width = viewport_cols.saturating_mul(cell_w);
    if terminal_x >= terminal_width {
        return None;
    }
    let col = (terminal_x / cell_w).min(viewport_cols.saturating_sub(1));
    Some(command_editor_byte_index_at_cell(
        &view,
        viewport_cols,
        visible_rows,
        visible_row as u32,
        col,
    ))
}

pub(crate) fn command_editor_settings_for_mouse(
    host: &mut WindowHost,
    tab_id: TabId,
) -> Option<(EditorSettings, bool)> {
    let config = command_editor_config(&host.render);
    if !config.enabled {
        return None;
    }
    let vim_mode = config.vim_mode;
    host.command.catalog.refresh_for_config(&config);
    let command_words = host.command.catalog.names().to_vec();
    let target = host.input.endpoints.get(&tab_id)?;
    let context = {
        let terminal = target.terminal.lock();
        command_editor_view_context(&terminal)
    }?;
    let history_entries =
        command_editor_history_entries(host, &config, context.current_dir.as_deref());
    Some((
        command_editor_settings(&config, context.current_dir, command_words, history_entries),
        vim_mode,
    ))
}

pub(crate) fn start_command_editor_selection(
    host: &mut WindowHost,
    offset: usize,
) -> bool {
    let Some(tab_id) = host.input.active_tab else {
        return false;
    };
    let Some((settings, vim_mode)) = command_editor_settings_for_mouse(host, tab_id) else {
        return false;
    };
    clear_terminal_selection_for_tab(host, tab_id);
    let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
        return false;
    };
    set_cursor(&mut target.command_editor, offset);
    let view = command_editor_view(&target.command_editor, &settings, vim_mode);
    reset_viewport_and_invalidate(&mut target.terminal.lock());
    host.mouse.command_editor_drag_anchor = Some(offset);
    host.mouse.left_drag_active = true;
    host.mouse.selection_drag_moved = false;
    set_command_editor_view(host, tab_id, view);
    true
}

pub(crate) fn extend_command_editor_selection_to_mouse(host: &mut WindowHost) -> bool {
    let Some(anchor) = host.mouse.command_editor_drag_anchor else {
        return false;
    };
    let Some(offset) = command_editor_offset_at_mouse(host, host.mouse.pos.0, host.mouse.pos.1)
    else {
        return false;
    };
    let Some(tab_id) = host.input.active_tab else {
        return false;
    };
    let Some((settings, vim_mode)) = command_editor_settings_for_mouse(host, tab_id) else {
        return false;
    };
    clear_terminal_selection_for_tab(host, tab_id);
    let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
        return false;
    };
    select_range(&mut target.command_editor, anchor, offset);
    let view = command_editor_view(&target.command_editor, &settings, vim_mode);
    reset_viewport_and_invalidate(&mut target.terminal.lock());
    set_command_editor_view(host, tab_id, view);
    true
}

pub(crate) fn finish_command_editor_selection(host: &mut WindowHost) -> bool {
    let Some(tab_id) = host.input.active_tab else {
        return false;
    };
    let Some((settings, vim_mode)) = command_editor_settings_for_mouse(host, tab_id) else {
        return false;
    };
    let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
        return false;
    };
    if let Some(text) = selected_text(&target.command_editor) {
        let mut terminal = target.terminal.lock();
        terminal.clipboard.set(ClipboardKind::Primary, &text);
    }
    let view = command_editor_view(&target.command_editor, &settings, vim_mode);
    reset_viewport_and_invalidate(&mut target.terminal.lock());
    host.mouse.command_editor_drag_anchor = None;
    host.mouse.left_drag_active = false;
    host.mouse.selection_drag_moved = false;
    set_command_editor_view(host, tab_id, view);
    true
}

pub(crate) fn right_click_command_editor(host: &mut WindowHost) -> bool {
    let Some(tab_id) = host.input.active_tab else {
        return false;
    };
    if copy_active_selection_to_clipboard(host, tab_id, ClipboardKind::Clipboard, true, true)
        .is_some()
    {
        return true;
    }
    let Some((settings, vim_mode)) = command_editor_settings_for_mouse(host, tab_id) else {
        return false;
    };
    let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
        return false;
    };
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
    let view = command_editor_view(&target.command_editor, &settings, vim_mode);
    reset_viewport_and_invalidate(&mut target.terminal.lock());
    clear_terminal_selection_for_tab(host, tab_id);
    set_command_editor_view(host, tab_id, view);
    true
}

pub(crate) fn paste_command_editor_selection(
    host: &mut WindowHost,
    kind: ClipboardKind,
) -> bool {
    let Some(tab_id) = host.input.active_tab else {
        return false;
    };
    let Some((settings, vim_mode)) = command_editor_settings_for_mouse(host, tab_id) else {
        return false;
    };
    let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
        return false;
    };
    let text = {
        let mut terminal = target.terminal.lock();
        terminal.clipboard.get(kind)
    };
    if let Some(text) = text {
        apply_input(
            &mut target.command_editor,
            EditorInput::Insert(text),
            &settings,
        );
    }
    let view = command_editor_view(&target.command_editor, &settings, vim_mode);
    reset_viewport_and_invalidate(&mut target.terminal.lock());
    clear_terminal_selection_for_tab(host, tab_id);
    set_command_editor_view(host, tab_id, view);
    true
}

pub(crate) fn permission_choice_at(
    render: &RenderRuntime,
    metrics: &WindowMetrics,
    x: f64,
    y: f64,
) -> Option<PermissionChoice> {
    let state = render.input_state.lock();
    let modal = state.permission_modal.as_ref()?;
    let tab_bar_h = if state.tab_count > 0 {
        state.cell_height as f32
    } else {
        0.0
    };
    renderer::permission_modal_button_at(
        &modal.feature,
        x as f32,
        y as f32,
        state.cell_width as f32,
        state.cell_height as f32,
        metrics.window_size.0 as f32,
        metrics.window_size.1 as f32,
        tab_bar_h,
    )
}

pub(crate) fn handle_mouse_input(
    host: &mut WindowHost,
    pressed: bool,
    button: MouseButton,
) {
    if host.modals.permission_modal.is_some() {
        if pressed
            && button == MouseButton::Left
            && let Some(choice) = permission_choice_at(
                &host.render,
                &host.metrics,
                host.mouse.pos.0,
                host.mouse.pos.1,
            )
        {
            let decision = match choice {
                PermissionChoice::Allow => PermissionDecision::Allow,
                PermissionChoice::Deny => PermissionDecision::Deny,
            };
            settle_permission_modal(host, decision);
        }
        return;
    }
    if host.modals.recording_popup.is_some() {
        return;
    }
    let term_button = match button {
        MouseButton::Left => TermMouseButton::Left,
        MouseButton::Middle => TermMouseButton::Middle,
        MouseButton::Right => TermMouseButton::Right,
        _ => return,
    };
    host.mouse.mouse_buttons.set(button, pressed);

    if pressed
        && button == MouseButton::Left
        && let Some(dir) = resize_direction_at(host.window.as_ref(), &host.mouse, &host.metrics)
    {
        if let Some(w) = &host.window {
            let _ = w.drag_resize_window(dir);
        }
        return;
    }

    if pressed
        && button == MouseButton::Left
        && let Some(btn) = window_button_at(&host.mouse, &host.render, &host.metrics)
    {
        match btn {
            WindowButton::Close => send(&mut host.render, RenderEvent::Action(Action::CloseWindow)),
            WindowButton::Maximize => {
                if let Some(w) = &host.window {
                    w.set_maximized(!w.is_maximized());
                }
            }
            WindowButton::Minimize => {
                if let Some(w) = &host.window {
                    w.set_minimized(true);
                }
            }
        }
        return;
    }

    if pressed
        && button == MouseButton::Left
        && is_on_new_tab_button(&host.mouse, &host.render, &host.metrics)
    {
        close_gutter_popup(&host.render, &mut host.input);
        update_tab_context_menu(&host.render, None);
        send(&mut host.render, RenderEvent::Action(Action::NewTab));
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if pressed
        && button == MouseButton::Left
        && (is_in_titlebar_drag_region(&host.mouse, &host.render, &host.metrics)
            || is_in_tab_bar(&host.mouse, &host.render))
    {
        if is_in_titlebar_drag_region(&host.mouse, &host.render, &host.metrics) {
            let now = Instant::now();
            let double_click = host
                .mouse
                .last_click_time
                .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_WINDOW);
            if double_click {
                if let Some(w) = &host.window {
                    w.set_maximized(!w.is_maximized());
                }
            } else if let Some(w) = &host.window {
                let _ = w.drag_window();
            }
            host.mouse.last_click_time = Some(now);
        }
        if is_in_tab_bar(&host.mouse, &host.render) {
            close_gutter_popup(&host.render, &mut host.input);
            update_tab_context_menu(&host.render, None);
            if let Some(idx) = tab_at_mouse(&host.mouse, &host.render, &host.metrics) {
                send(&mut host.render, RenderEvent::SetActiveTab(idx));
            }
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
        }
        return;
    }

    if pressed && button == MouseButton::Middle && is_in_tab_bar(&host.mouse, &host.render) {
        close_gutter_popup(&host.render, &mut host.input);
        update_tab_context_menu(&host.render, None);
        if let Some(idx) = tab_at_mouse(&host.mouse, &host.render, &host.metrics) {
            send(&mut host.render, RenderEvent::CloseTab(idx));
        }
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if pressed && button == MouseButton::Right && is_in_tab_bar(&host.mouse, &host.render) {
        let has_menu = host.render.input_state.lock().tab_context_menu.is_some();
        if has_menu {
            update_tab_context_menu(&host.render, None);
            if let Some(w) = &host.window {
                let pos = winit::dpi::PhysicalPosition::new(
                    host.mouse.pos.0 as i32,
                    host.mouse.pos.1 as i32,
                );
                w.show_window_menu(pos);
            }
        } else {
            update_tab_context_menu(
                &host.render,
                tab_at_mouse(&host.mouse, &host.render, &host.metrics).map(|idx| TabContextMenu {
                    tab_idx: idx,
                    x: host.mouse.pos.0 as f32,
                    hovered_item: None,
                }),
            );
        }
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if pressed
        && button == MouseButton::Left
        && host.render.input_state.lock().tab_context_menu.is_some()
    {
        if let Some((action, tab_idx, _)) = tab_menu_item_at(
            &host.render,
            &host.metrics,
            host.mouse.pos.0,
            host.mouse.pos.1,
        ) {
            execute_tab_menu_action(host, action, tab_idx);
        }
        update_tab_context_menu(&host.render, None);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if pressed
        && button == MouseButton::Left
        && host.render.input_state.lock().gutter_popup.is_some()
    {
        if let Some(item) = gutter_popup_item_at(
            &host.render,
            &host.metrics,
            host.mouse.pos.0,
            host.mouse.pos.1,
        ) {
            execute_popup_action(host, item);
            return;
        }
        close_gutter_popup(&host.render, &mut host.input);
        if !is_in_gutter(&host.mouse, &host.render) {
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
            return;
        }
    }

    if pressed && button == MouseButton::Left && is_in_gutter(&host.mouse, &host.render) {
        let (_, screen_row) = cell_at(host, host.mouse.pos.0, host.mouse.pos.1);
        open_gutter_popup(host, screen_row);
        return;
    }

    if !pressed && button == MouseButton::Left && host.mouse.command_editor_drag_anchor.is_some() {
        finish_command_editor_selection(host);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if pressed
        && button == MouseButton::Left
        && let Some(offset) =
            command_editor_offset_at_mouse(host, host.mouse.pos.0, host.mouse.pos.1)
    {
        start_command_editor_selection(host, offset);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    let command_editor_open = {
        let state = host.render.input_state.lock();
        command_editor_view_open_for_input_tab(&state, host.input.active_tab)
    };
    if let Some(kind) = command_editor_mouse_paste_kind(command_editor_open, pressed, button) {
        let handled = match kind {
            ClipboardKind::Clipboard => right_click_command_editor(host),
            ClipboardKind::Primary => paste_command_editor_selection(host, ClipboardKind::Primary),
        };
        if handled {
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
            return;
        }
    }

    if pressed {
        host.mouse.last_motion_position = None;
    }

    if !pressed && button == MouseButton::Left && host.mouse.left_drag_active {
        stop_selection_drag(&mut host.mouse);
        if let Some(target) = active_input_target(&mut host.input) {
            let mut guard = target.terminal.lock();
            let terminal = &mut *guard;
            if terminal.has_selection() {
                copy_selection(
                    &mut terminal.clipboard,
                    terminal.selection.as_ref(),
                    &terminal.active,
                    ClipboardKind::Primary,
                );
            } else {
                terminal.selection = None;
                terminal.invalidate_snapshot_rows();
            }
        }
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if forward_mouse_to_app(&host.keyboard, &mut host.input)
        && let Some(pos) = app_mouse_report_position_at(host, host.mouse.pos.0, host.mouse.pos.1)
    {
        let kind = if pressed {
            MouseEventKind::Press
        } else {
            MouseEventKind::Release
        };
        let mods = mouse_modifiers(&host.keyboard);
        let Some(target) = active_input_target(&mut host.input) else {
            return;
        };
        emit_host_input(
            target,
            HostInput::Mouse(HostMouse {
                kind,
                button: term_button,
                col: pos.col,
                row: pos.row,
                pixel_x: pos.pixel_x,
                pixel_y: pos.pixel_y,
                mods,
            }),
            true,
        );
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    let (col, viewport_row) = cell_at(host, host.mouse.pos.0, host.mouse.pos.1);
    match (button, pressed) {
        (MouseButton::Left, true) => {
            if host.keyboard.modifiers.control_key()
                && let Some(target) = active_input_target(&mut host.input)
            {
                let url = target.terminal.lock();
                let row = terminal41::selection::active_screen_row_at_viewport_row(
                    &url.active,
                    &url.viewport,
                    url.on_alt_screen,
                    viewport_row,
                );
                let url = row
                    .and_then(|row| {
                        view::hyperlink_at(&url.active, &url.viewport, &url.hyperlinks, row, col)
                    })
                    .map(str::to_owned);
                if let Some(url) = url {
                    if let Err(e) = open::that_detached(&url) {
                        warn!("failed to open hyperlink {url:?}: {e}");
                    }
                    return;
                }
            }
            if host.keyboard.modifiers.shift_key() {
                let extended = if let Some(target) = active_input_target(&mut host.input) {
                    let mut terminal = target.terminal.lock();
                    if let Some(selection) = terminal.selection.as_ref()
                        && let Some(new_selection) = extend_rendered_selection(
                            selection,
                            &terminal.active,
                            &terminal.viewport,
                            terminal.on_alt_screen,
                            col,
                            viewport_row,
                        )
                    {
                        terminal.selection = Some(new_selection);
                        terminal.invalidate_snapshot_rows();
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if extended {
                    if let Some(tab_id) = host.input.active_tab {
                        clear_command_editor_selection_for_tab(host, tab_id);
                    }
                    host.mouse.left_drag_active = true;
                    host.mouse.selection_drag_moved = true;
                    refresh_selection_autoscroll_direction(host);
                    notify_interaction_changed(
                        &host.input,
                        &mut host.render,
                        &host.startup,
                        host.window.as_ref(),
                    );
                    return;
                }
            }
            host.mouse.click_count = next_click_count(&host.mouse, (col, viewport_row));
            host.mouse.last_click_cell = Some((col, viewport_row));
            host.mouse.last_click_time = Some(Instant::now());
            let mode = match host.mouse.click_count {
                2 => SelectionMode::Word,
                3 => SelectionMode::Line,
                _ => SelectionMode::Char,
            };
            if let Some(target) = active_input_target(&mut host.input) {
                let mut target = target.terminal.lock();
                let target = &mut *target;
                target.selection = start_rendered_selection(
                    &target.active,
                    &target.viewport,
                    target.on_alt_screen,
                    col,
                    viewport_row,
                    mode,
                );
                target.invalidate_snapshot_rows();
            }
            if let Some(tab_id) = host.input.active_tab {
                clear_command_editor_selection_for_tab(host, tab_id);
            }
            host.mouse.left_drag_active = true;
            host.mouse.selection_drag_moved = false;
            refresh_selection_autoscroll_direction(host);
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
        }
        (MouseButton::Left, false) => {}
        (MouseButton::Right, true) => {
            if let Some(target) = active_input_target(&mut host.input) {
                let mut guard = target.terminal.lock();
                let terminal = &mut *guard;
                if terminal.has_selection() {
                    copy_selection(
                        &mut terminal.clipboard,
                        terminal.selection.as_ref(),
                        &terminal.active,
                        ClipboardKind::Clipboard,
                    );
                    terminal.selection = None;
                    terminal.invalidate_snapshot_rows();
                } else {
                    drop(guard);
                    emit_host_input(
                        target,
                        HostInput::PasteFromClipboard {
                            kind: ClipboardKind::Clipboard,
                        },
                        true,
                    );
                    notify_interaction_changed(
                        &host.input,
                        &mut host.render,
                        &host.startup,
                        host.window.as_ref(),
                    );
                    return;
                }
                drop(guard);
            }
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
        }
        _ => {}
    }
}

pub(crate) fn handle_mouse_wheel(
    host: &mut WindowHost,
    raw_x: f64,
    raw_y: f64,
    pixels: bool,
) {
    if host.modals.permission_modal.is_some() {
        return;
    }
    if host.modals.recording_popup.is_some() {
        return;
    }
    close_gutter_popup(&host.render, &mut host.input);
    let (cell_w, cell_h, _, _) = layout_snapshot(&host.render);
    let (x_lines, y_lines) = if pixels {
        let cw = cell_w as i32;
        let ch = cell_h as i32;
        ((raw_x as i32) / cw, -(raw_y as i32) / ch)
    } else {
        (raw_x as i32, -(raw_y as i32))
    };

    if forward_mouse_to_app(&host.keyboard, &mut host.input)
        && let Some(pos) = app_mouse_report_position_at(host, host.mouse.pos.0, host.mouse.pos.1)
    {
        let mods = mouse_modifiers(&host.keyboard);
        let Some(target) = active_input_target(&mut host.input) else {
            return;
        };
        let effects = {
            let mut terminal = target.terminal.lock();
            let mut effects = HostInputEffects::default();
            if y_lines < 0 {
                for _ in 0..y_lines.unsigned_abs() {
                    effects.extend(apply_host_input(
                        &mut terminal,
                        HostInput::Mouse(HostMouse {
                            kind: MouseEventKind::Press,
                            button: TermMouseButton::WheelUp,
                            col: pos.col,
                            row: pos.row,
                            pixel_x: pos.pixel_x,
                            pixel_y: pos.pixel_y,
                            mods,
                        }),
                    ));
                }
            } else if y_lines > 0 {
                for _ in 0..y_lines as u32 {
                    effects.extend(apply_host_input(
                        &mut terminal,
                        HostInput::Mouse(HostMouse {
                            kind: MouseEventKind::Press,
                            button: TermMouseButton::WheelDown,
                            col: pos.col,
                            row: pos.row,
                            pixel_x: pos.pixel_x,
                            pixel_y: pos.pixel_y,
                            mods,
                        }),
                    ));
                }
            }
            if x_lines < 0 {
                for _ in 0..x_lines.unsigned_abs() {
                    effects.extend(apply_host_input(
                        &mut terminal,
                        HostInput::Mouse(HostMouse {
                            kind: MouseEventKind::Press,
                            button: TermMouseButton::WheelLeft,
                            col: pos.col,
                            row: pos.row,
                            pixel_x: pos.pixel_x,
                            pixel_y: pos.pixel_y,
                            mods,
                        }),
                    ));
                }
            } else if x_lines > 0 {
                for _ in 0..x_lines as u32 {
                    effects.extend(apply_host_input(
                        &mut terminal,
                        HostInput::Mouse(HostMouse {
                            kind: MouseEventKind::Press,
                            button: TermMouseButton::WheelRight,
                            col: pos.col,
                            row: pos.row,
                            pixel_x: pos.pixel_x,
                            pixel_y: pos.pixel_y,
                            mods,
                        }),
                    ));
                }
            }
            effects
        };
        write_host_bytes(target, effects.host_bytes, true);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    }

    if let Some(target) = active_input_target(&mut host.input) {
        let mut terminal = target.terminal.lock();
        if y_lines < 0 {
            let viewport = terminal.viewport;
            view::scroll_viewport_up(&mut terminal.active, &viewport, y_lines.unsigned_abs());
        } else if y_lines > 0 {
            view::scroll_viewport_down(&mut terminal.active, y_lines as u32);
        }
        if y_lines != 0 {
            terminal.invalidate_snapshot_rows();
        }
    }
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn execute_tab_menu_action(
    host: &mut WindowHost,
    action: TabMenuActionLocal,
    tab_idx: usize,
) {
    match action {
        TabMenuActionLocal::NewTab => send(&mut host.render, RenderEvent::Action(Action::NewTab)),
        TabMenuActionLocal::CloseTab => send(&mut host.render, RenderEvent::CloseTab(tab_idx)),
        TabMenuActionLocal::CloseOtherTabs => {
            send(&mut host.render, RenderEvent::CloseOtherTabs(tab_idx));
        }
    }
}

pub(crate) fn close_gutter_popup(
    render: &RenderRuntime,
    input: &mut InputRuntime,
) {
    let had_popup = render.input_state.lock().gutter_popup.take().is_some();
    if had_popup && let Some(target) = active_input_target(input) {
        let mut terminal = target.terminal.lock();
        terminal.selection = None;
        terminal.invalidate_snapshot_rows();
    }
}

pub(crate) fn open_gutter_popup(
    host: &mut WindowHost,
    screen_row: u32,
) {
    let Some(target) = active_input_target(&mut host.input) else {
        return;
    };
    let mut guard = target.terminal.lock();
    let terminal = &mut *guard;
    let Some(prompt_abs) =
        find_prompt_for_screen_row(&terminal.active, &terminal.viewport, screen_row)
    else {
        return;
    };
    select_command_at(
        &mut terminal.selection,
        prompt_abs,
        &terminal.metadata.command_metas,
        &terminal.active,
    );
    terminal.invalidate_snapshot_rows();
    let duration_text =
        command_duration_at(prompt_abs, &terminal.metadata.command_metas).map(format_duration);
    drop(guard);
    if let Some(tab_id) = host.input.active_tab {
        clear_command_editor_selection_for_tab(host, tab_id);
    }
    update_gutter_popup(
        &host.render,
        Some(renderer::GutterPopup {
            prompt_abs_row: prompt_abs,
            screen_row,
            duration_text,
            hovered_item: None,
        }),
    );
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn execute_popup_action(
    host: &mut WindowHost,
    item_idx: usize,
) {
    let popup = host.render.input_state.lock().gutter_popup.take();
    let Some(popup) = popup else {
        return;
    };
    let Some(target) = active_input_target(&mut host.input) else {
        return;
    };
    match item_idx {
        0 => {
            let mut guard = target.terminal.lock();
            let terminal = &mut *guard;
            if let Some(cmd) = popup_command_text(
                popup.prompt_abs_row,
                &terminal.metadata.command_metas,
                &terminal.active,
            ) {
                let bracketed_paste_enabled = terminal.modes.bracketed_paste;
                terminal.selection = None;
                terminal.invalidate_snapshot_rows();
                drop(guard);
                if let Some((text, mode)) = popup_rerun_paste(cmd, bracketed_paste_enabled) {
                    emit_host_input(target, HostInput::PasteText { text: &text, mode }, true);
                    show_toast(host, "Pasted command; review before Enter");
                } else {
                    show_toast(
                        host,
                        "Multiline command needs bracketed paste; use Copy Command",
                    );
                }
            }
        }
        1 => {
            let mut guard = target.terminal.lock();
            let terminal = &mut *guard;
            if let Some(text) = popup_command_text(
                popup.prompt_abs_row,
                &terminal.metadata.command_metas,
                &terminal.active,
            ) {
                let text = match text {
                    PopupCommandText::Observed(text) => text.trim().to_owned(),
                    PopupCommandText::Untrusted(text) => text,
                };
                copy_to_clipboard(&mut terminal.clipboard, &text);
            }
            terminal.selection = None;
            terminal.invalidate_snapshot_rows();
        }
        2 => {
            let mut terminal = target.terminal.lock();
            if let Some(text) = command_and_output_text_at(
                popup.prompt_abs_row,
                &terminal.metadata.command_metas,
                &terminal.active,
            ) {
                copy_to_clipboard(&mut terminal.clipboard, text.trim());
            }
            terminal.selection = None;
            terminal.invalidate_snapshot_rows();
        }
        3 => {
            let mut terminal = target.terminal.lock();
            if let Some(text) = output_text_at(
                popup.prompt_abs_row,
                &terminal.metadata.command_metas,
                &terminal.active,
            ) {
                copy_to_clipboard(&mut terminal.clipboard, text.trim());
            }
            terminal.selection = None;
            terminal.invalidate_snapshot_rows();
        }
        _ => return,
    }
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn mouse_modifiers(keyboard: &KeyboardRuntime) -> MouseModifiers {
    MouseModifiers {
        shift: keyboard.modifiers.shift_key(),
        alt: keyboard.modifiers.alt_key(),
        ctrl: keyboard.modifiers.control_key(),
    }
}

pub(crate) fn forward_mouse_to_app(
    keyboard: &KeyboardRuntime,
    input: &mut InputRuntime,
) -> bool {
    let is_shift = keyboard.modifiers.shift_key();
    active_input_target(input).is_some_and(|target| {
        let terminal = target.terminal.lock();
        host::mouse_tracking_enabled(terminal.modes.mouse_tracking)
            && !is_shift
            && terminal.metadata.shell_integration_phase
                == terminal41::ShellIntegrationPhase::Output
    })
}

pub(crate) fn next_click_count(
    mouse: &MouseRuntime,
    cell: (u32, u32),
) -> u32 {
    let within_window = mouse
        .last_click_time
        .is_some_and(|t| t.elapsed() <= MULTI_CLICK_WINDOW);
    let same_cell = mouse.last_click_cell == Some(cell);
    if within_window && same_cell && mouse.click_count < 3 {
        mouse.click_count + 1
    } else {
        1
    }
}

pub(crate) fn cell_at(
    host: &mut WindowHost,
    x: f64,
    y: f64,
) -> (u32, u32) {
    let pos = mouse_report_position_at(host, x, y);
    (pos.col, pos.row)
}

pub(crate) fn mouse_report_position_at(
    host: &mut WindowHost,
    x: f64,
    y: f64,
) -> MouseReportPosition {
    let (cell_w, cell_h, gutter_w, _) = layout_snapshot(&host.render);
    let raw_x = x.max(0.0) as u32;
    let raw_y = y.max(0.0) as u32;
    let command_editor_view_present = {
        let state = host.render.input_state.lock();
        command_editor_view_open_for_input_tab(&state, host.input.active_tab)
    };
    let Some(target) = active_input_target(&mut host.input) else {
        return MouseReportPosition {
            col: 0,
            row: 0,
            pixel_x: 0,
            pixel_y: 0,
        };
    };
    let terminal = target.terminal.lock();
    let cols = terminal.viewport.cols.max(1);
    let rows = terminal.viewport.rows.max(1);
    let row_offset = command_editor_terminal_row_offset(&terminal, command_editor_view_present);
    mouse_report_position_from_pixels(
        raw_x, raw_y, cell_w, cell_h, gutter_w, cols, rows, row_offset,
    )
}

pub(crate) fn app_mouse_report_position_at(
    host: &mut WindowHost,
    x: f64,
    y: f64,
) -> Option<MouseReportPosition> {
    let (_, cell_h, _, _) = layout_snapshot(&host.render);
    let pos = mouse_report_position_at(host, x, y);
    let target = active_input_target(&mut host.input)?;
    let terminal = target.terminal.lock();
    app_mouse_report_position_for_terminal(&terminal, pos, cell_h)
}

pub(crate) fn app_mouse_report_position_for_terminal(
    terminal: &terminal41::Terminal,
    pos: MouseReportPosition,
    cell_h: u32,
) -> Option<MouseReportPosition> {
    let row = terminal41::selection::active_screen_row_at_viewport_row(
        &terminal.active,
        &terminal.viewport,
        terminal.on_alt_screen,
        pos.row,
    )?;
    let cell_h = cell_h.max(1);
    let pixel_y = row
        .saturating_mul(cell_h)
        .saturating_add(pos.pixel_y % cell_h)
        .min(
            terminal
                .viewport
                .rows
                .max(1)
                .saturating_mul(cell_h)
                .saturating_sub(1),
        );
    Some(MouseReportPosition {
        row,
        pixel_y,
        ..pos
    })
}

pub(crate) fn mouse_motion_position_key(
    host: &mut WindowHost,
    pos: MouseReportPosition,
) -> (u32, u32) {
    let pixel_reporting = active_input_target(&mut host.input).is_some_and(|target| {
        target.terminal.lock().modes.mouse_encoding == terminal41::MouseEncoding::SgrPixels
    });
    if pixel_reporting {
        (pos.pixel_x, pos.pixel_y)
    } else {
        (pos.col, pos.row)
    }
}

pub(crate) fn is_in_tab_bar(
    mouse: &MouseRuntime,
    render: &RenderRuntime,
) -> bool {
    let (_, cell_h, _, _) = layout_snapshot(render);
    (mouse.pos.1.max(0.0) as u32) < cell_h
}

pub(crate) fn window_button_at(
    mouse: &MouseRuntime,
    render: &RenderRuntime,
    metrics: &WindowMetrics,
) -> Option<WindowButton> {
    match tab_bar_hover_at(mouse, render, metrics) {
        Some(renderer::TabBarHover::Minimize) => Some(WindowButton::Minimize),
        Some(renderer::TabBarHover::Maximize) => Some(WindowButton::Maximize),
        Some(renderer::TabBarHover::Close) => Some(WindowButton::Close),
        _ => None,
    }
}

pub(crate) fn tab_at_mouse(
    mouse: &MouseRuntime,
    render: &RenderRuntime,
    metrics: &WindowMetrics,
) -> Option<usize> {
    let (cell_w, _, _, tab_count) = layout_snapshot(render);
    if tab_count == 0 {
        return None;
    }
    let mx = mouse.pos.0.max(0.0) as f32;
    let layout = build_tab_bar_layout(tab_count, metrics.window_size.0 as f32, cell_w as f32);
    layout
        .tabs
        .iter()
        .position(|tab| mx >= tab.x && mx < tab.x + tab.width)
}

pub(crate) fn is_on_new_tab_button(
    mouse: &MouseRuntime,
    render: &RenderRuntime,
    metrics: &WindowMetrics,
) -> bool {
    matches!(
        tab_bar_hover_at(mouse, render, metrics),
        Some(renderer::TabBarHover::NewTab)
    )
}

pub(crate) fn is_in_titlebar_drag_region(
    mouse: &MouseRuntime,
    render: &RenderRuntime,
    metrics: &WindowMetrics,
) -> bool {
    is_in_tab_bar(mouse, render) && tab_bar_hover_at(mouse, render, metrics).is_none()
}

pub(crate) fn tab_bar_hover_at(
    mouse: &MouseRuntime,
    render: &RenderRuntime,
    metrics: &WindowMetrics,
) -> Option<renderer::TabBarHover> {
    if !is_in_tab_bar(mouse, render) {
        return None;
    }
    let (cell_w, _, _, tab_count) = layout_snapshot(render);
    let mx = mouse.pos.0.max(0.0) as f32;
    let layout = build_tab_bar_layout(tab_count, metrics.window_size.0 as f32, cell_w as f32);
    if mx >= layout.new_tab_button.x && mx < layout.new_tab_button.x + layout.new_tab_button.width {
        return Some(renderer::TabBarHover::NewTab);
    }
    layout
        .buttons
        .iter()
        .find(|button| mx >= button.x && mx < button.x + button.width)
        .and_then(|button| button.button)
}

pub(crate) fn resize_direction_at(
    window: Option<&Arc<Window>>,
    mouse: &MouseRuntime,
    metrics: &WindowMetrics,
) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection;
    if window.is_some_and(|w| w.is_maximized()) {
        return None;
    }
    let (w, h) = metrics.window_size;
    let (mx, my) = (mouse.pos.0 as f32, mouse.pos.1 as f32);
    let wf = w as f32;
    let hf = h as f32;
    let left = mx < RESIZE_BORDER;
    let right = mx >= wf - RESIZE_BORDER;
    let top = my < RESIZE_BORDER;
    let bottom = my >= hf - RESIZE_BORDER;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

pub(crate) fn tab_menu_item_at(
    render: &RenderRuntime,
    metrics: &WindowMetrics,
    mx: f64,
    my: f64,
) -> Option<(TabMenuActionLocal, usize, usize)> {
    let state = render.input_state.lock();
    let menu = state.tab_context_menu.as_ref()?;
    let pw = state.cell_width as f32 * TAB_MENU_WIDTH_CELLS;
    let ph = 3.0 * state.cell_height as f32;
    let px = menu.x.min(metrics.window_size.0 as f32 - pw);
    let py = state.cell_height as f32;
    let fx = mx as f32;
    let fy = my as f32;
    if fx < px || fx >= px + pw || fy < py || fy >= py + ph {
        return None;
    }
    let idx = ((fy - py) / state.cell_height as f32) as usize;
    let action = match idx {
        0 => TabMenuActionLocal::NewTab,
        1 => TabMenuActionLocal::CloseTab,
        2 => TabMenuActionLocal::CloseOtherTabs,
        _ => return None,
    };
    Some((action, menu.tab_idx, idx))
}

pub(crate) fn is_in_gutter(
    mouse: &MouseRuntime,
    render: &RenderRuntime,
) -> bool {
    let (_, _, gutter_w, _) = layout_snapshot(render);
    gutter_w > 0 && (mouse.pos.0.max(0.0) as u32) < gutter_w
}

pub(crate) fn gutter_popup_item_at(
    render: &RenderRuntime,
    metrics: &WindowMetrics,
    x: f64,
    y: f64,
) -> Option<usize> {
    let state = render.input_state.lock();
    super::popup_item_at(
        state.gutter_popup.as_ref(),
        x,
        y,
        state.cell_width,
        state.cell_height,
        state.gutter_width,
        metrics.window_size.1,
    )
}

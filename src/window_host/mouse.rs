use super::*;

impl WindowHost {
    pub(crate) fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
    ) {
        self.mouse_pos = (x, y);
        if self.permission_modal.is_some() {
            self.update_permission_hover(self.permission_choice_at(x, y));
            return;
        }
        if self.recording_popup.is_some() {
            return;
        }

        let hovered_button = self.tab_bar_hover_at();
        self.update_hovered_tab_bar_button(hovered_button);

        let hovered_menu_item = self.tab_menu_item_at(x, y).map(|(_, _, idx)| idx);
        {
            let mut state = self.input_state.lock();
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
                self.window_size.1,
            );
            if let Some(popup) = state.gutter_popup.as_mut() {
                popup.hovered_item = hovered_popup_item;
            }
        }

        if let Some(dir) = self.resize_direction_at() {
            if let Some(w) = &self.window {
                w.set_cursor(winit::window::CursorIcon::from(dir));
            }
        } else if let Some(w) = &self.window {
            w.set_cursor(winit::window::CursorIcon::Default);
        }

        if self.command_editor_drag_anchor.is_some() {
            if self.extend_command_editor_selection_to_mouse() {
                self.selection_drag_moved = true;
                self.notify_interaction_changed();
            }
            return;
        }

        let pos = self.mouse_report_position_at(x, y);
        if self.forward_mouse_to_app() {
            let motion_position = self.mouse_motion_position_key(pos);
            if self.last_motion_position == Some(motion_position) {
                return;
            }
            self.last_motion_position = Some(motion_position);
            let button = self.mouse_buttons.primary_held();
            let mods = self.mouse_modifiers();
            let Some(target) = self.active_input_target() else {
                return;
            };
            Self::emit_host_input(
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
            self.notify_interaction_changed();
            return;
        }

        if self.left_drag_active && self.extend_selection_to_mouse() {
            self.selection_drag_moved = true;
            self.refresh_selection_autoscroll_direction();
            self.notify_interaction_changed();
            return;
        }

        self.notify_interaction_changed();
    }

    pub(crate) fn command_editor_offset_at_mouse(
        &mut self,
        x: f64,
        y: f64,
    ) -> Option<usize> {
        let (cell_w, cell_h, gutter_w, _) = self.layout_snapshot();
        let cell_w = cell_w.max(1);
        let cell_h = cell_h.max(1);
        let raw_x = x.max(0.0) as u32;
        let raw_y = y.max(0.0) as u32;
        if raw_x < gutter_w || raw_y < cell_h {
            return None;
        }

        let target = self.active_input_target()?;
        let (cursor_row, viewport_cols) = {
            let terminal = target.terminal.lock();
            command_editor_view_context(&terminal)?;
            (terminal.active.cursor.row, terminal.viewport.cols.max(1))
        };
        let view = self.input_state.lock().command_editor_view.clone()?;

        let box_top = cursor_row as i32 + 1 - COMMAND_EDITOR_BOX_ROWS as i32;
        let terminal_row = raw_y.saturating_sub(cell_h) / cell_h;
        let visible_row = terminal_row as i32 - box_top;
        if !(0..COMMAND_EDITOR_BOX_ROWS as i32).contains(&visible_row) {
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
            visible_row as u32,
            col,
        ))
    }

    pub(crate) fn command_editor_settings_for_mouse(
        &mut self,
        tab_id: TabId,
    ) -> Option<(EditorSettings, bool)> {
        let config = self.command_editor_config();
        if !config.enabled {
            return None;
        }
        let vim_mode = config.vim_mode;
        self.command_catalog.refresh_for_config(&config);
        let command_words = self.command_catalog.names().to_vec();
        let target = self.input_endpoints.get(&tab_id)?;
        let context = {
            let terminal = target.terminal.lock();
            command_editor_view_context(&terminal)
        }?;
        let history_entries = self.command_editor_history_entries(&config);
        Some((
            Self::command_editor_settings(
                &config,
                context.current_dir,
                command_words,
                history_entries,
            ),
            vim_mode,
        ))
    }

    pub(crate) fn start_command_editor_selection(
        &mut self,
        offset: usize,
    ) -> bool {
        let Some(tab_id) = self.active_input_tab else {
            return false;
        };
        let Some((settings, vim_mode)) = self.command_editor_settings_for_mouse(tab_id) else {
            return false;
        };
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return false;
        };
        set_cursor(&mut target.command_editor, offset);
        let view = command_editor_view(&target.command_editor, &settings, vim_mode);
        self.command_editor_drag_anchor = Some(offset);
        self.left_drag_active = true;
        self.selection_drag_moved = false;
        self.set_command_editor_view(view);
        true
    }

    pub(crate) fn extend_command_editor_selection_to_mouse(&mut self) -> bool {
        let Some(anchor) = self.command_editor_drag_anchor else {
            return false;
        };
        let Some(offset) = self.command_editor_offset_at_mouse(self.mouse_pos.0, self.mouse_pos.1)
        else {
            return false;
        };
        let Some(tab_id) = self.active_input_tab else {
            return false;
        };
        let Some((settings, vim_mode)) = self.command_editor_settings_for_mouse(tab_id) else {
            return false;
        };
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return false;
        };
        select_range(&mut target.command_editor, anchor, offset);
        let view = command_editor_view(&target.command_editor, &settings, vim_mode);
        self.set_command_editor_view(view);
        true
    }

    pub(crate) fn finish_command_editor_selection(&mut self) -> bool {
        let Some(tab_id) = self.active_input_tab else {
            return false;
        };
        let Some((settings, vim_mode)) = self.command_editor_settings_for_mouse(tab_id) else {
            return false;
        };
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return false;
        };
        if let Some(text) = selected_text(&target.command_editor) {
            let mut terminal = target.terminal.lock();
            terminal.clipboard.set(ClipboardKind::Primary, &text);
        }
        let view = command_editor_view(&target.command_editor, &settings, vim_mode);
        self.command_editor_drag_anchor = None;
        self.left_drag_active = false;
        self.selection_drag_moved = false;
        self.set_command_editor_view(view);
        true
    }

    pub(crate) fn right_click_command_editor(&mut self) -> bool {
        let Some(tab_id) = self.active_input_tab else {
            return false;
        };
        let Some((settings, vim_mode)) = self.command_editor_settings_for_mouse(tab_id) else {
            return false;
        };
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
            return false;
        };
        if let Some(text) = selected_text(&target.command_editor) {
            let mut terminal = target.terminal.lock();
            terminal.clipboard.set(ClipboardKind::Clipboard, &text);
            drop(terminal);
            clear_editor_selection(&mut target.command_editor);
        } else {
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
        }
        let view = command_editor_view(&target.command_editor, &settings, vim_mode);
        self.set_command_editor_view(view);
        true
    }

    pub(crate) fn paste_command_editor_selection(
        &mut self,
        kind: ClipboardKind,
    ) -> bool {
        let Some(tab_id) = self.active_input_tab else {
            return false;
        };
        let Some((settings, vim_mode)) = self.command_editor_settings_for_mouse(tab_id) else {
            return false;
        };
        let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
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
        self.set_command_editor_view(view);
        true
    }

    pub(crate) fn permission_choice_at(
        &self,
        x: f64,
        y: f64,
    ) -> Option<PermissionChoice> {
        let state = self.input_state.lock();
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
            self.window_size.0 as f32,
            self.window_size.1 as f32,
            tab_bar_h,
        )
    }

    pub(crate) fn handle_mouse_input(
        &mut self,
        pressed: bool,
        button: MouseButton,
    ) {
        if self.permission_modal.is_some() {
            if pressed
                && button == MouseButton::Left
                && let Some(choice) = self.permission_choice_at(self.mouse_pos.0, self.mouse_pos.1)
            {
                let decision = match choice {
                    PermissionChoice::Allow => PermissionDecision::Allow,
                    PermissionChoice::Deny => PermissionDecision::Deny,
                };
                self.settle_permission_modal(decision);
            }
            return;
        }
        if self.recording_popup.is_some() {
            return;
        }
        let term_button = match button {
            MouseButton::Left => TermMouseButton::Left,
            MouseButton::Middle => TermMouseButton::Middle,
            MouseButton::Right => TermMouseButton::Right,
            _ => return,
        };
        self.mouse_buttons.set(button, pressed);

        if pressed
            && button == MouseButton::Left
            && let Some(dir) = self.resize_direction_at()
        {
            if let Some(w) = &self.window {
                let _ = w.drag_resize_window(dir);
            }
            return;
        }

        if pressed
            && button == MouseButton::Left
            && let Some(btn) = self.window_button_at()
        {
            match btn {
                WindowButton::Close => self.send(RenderEvent::Action(Action::CloseWindow)),
                WindowButton::Maximize => {
                    if let Some(w) = &self.window {
                        w.set_maximized(!w.is_maximized());
                    }
                }
                WindowButton::Minimize => {
                    if let Some(w) = &self.window {
                        w.set_minimized(true);
                    }
                }
            }
            return;
        }

        if pressed && button == MouseButton::Left && self.is_on_new_tab_button() {
            self.close_gutter_popup();
            self.update_tab_context_menu(None);
            self.send(RenderEvent::Action(Action::NewTab));
            self.notify_interaction_changed();
            return;
        }

        if pressed
            && button == MouseButton::Left
            && (self.is_in_titlebar_drag_region() || self.is_in_tab_bar())
        {
            if self.is_in_titlebar_drag_region() {
                let now = Instant::now();
                let double_click = self
                    .last_click_time
                    .is_some_and(|t| now.duration_since(t) <= MULTI_CLICK_WINDOW);
                if double_click {
                    if let Some(w) = &self.window {
                        w.set_maximized(!w.is_maximized());
                    }
                } else if let Some(w) = &self.window {
                    let _ = w.drag_window();
                }
                self.last_click_time = Some(now);
            }
            if self.is_in_tab_bar() {
                self.close_gutter_popup();
                self.update_tab_context_menu(None);
                if let Some(idx) = self.tab_at_mouse() {
                    self.send(RenderEvent::SetActiveTab(idx));
                }
                self.notify_interaction_changed();
            }
            return;
        }

        if pressed && button == MouseButton::Middle && self.is_in_tab_bar() {
            self.close_gutter_popup();
            self.update_tab_context_menu(None);
            if let Some(idx) = self.tab_at_mouse() {
                self.send(RenderEvent::CloseTab(idx));
            }
            self.notify_interaction_changed();
            return;
        }

        if pressed && button == MouseButton::Right && self.is_in_tab_bar() {
            let has_menu = self.input_state.lock().tab_context_menu.is_some();
            if has_menu {
                self.update_tab_context_menu(None);
                if let Some(w) = &self.window {
                    let pos = winit::dpi::PhysicalPosition::new(
                        self.mouse_pos.0 as i32,
                        self.mouse_pos.1 as i32,
                    );
                    w.show_window_menu(pos);
                }
            } else {
                self.update_tab_context_menu(self.tab_at_mouse().map(|idx| TabContextMenu {
                    tab_idx: idx,
                    x: self.mouse_pos.0 as f32,
                    hovered_item: None,
                }));
            }
            self.notify_interaction_changed();
            return;
        }

        if pressed
            && button == MouseButton::Left
            && self.input_state.lock().tab_context_menu.is_some()
        {
            if let Some((action, tab_idx, _)) =
                self.tab_menu_item_at(self.mouse_pos.0, self.mouse_pos.1)
            {
                self.execute_tab_menu_action(action, tab_idx);
            }
            self.update_tab_context_menu(None);
            self.notify_interaction_changed();
            return;
        }

        if pressed && button == MouseButton::Left && self.input_state.lock().gutter_popup.is_some()
        {
            if let Some(item) = self.popup_item_at(self.mouse_pos.0, self.mouse_pos.1) {
                self.execute_popup_action(item);
                return;
            }
            self.close_gutter_popup();
            if !self.is_in_gutter() {
                self.notify_interaction_changed();
                return;
            }
        }

        if pressed && button == MouseButton::Left && self.is_in_gutter() {
            let (_, screen_row) = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
            self.open_gutter_popup(screen_row);
            return;
        }

        if !pressed && button == MouseButton::Left && self.command_editor_drag_anchor.is_some() {
            self.finish_command_editor_selection();
            self.notify_interaction_changed();
            return;
        }

        if pressed
            && button == MouseButton::Left
            && let Some(offset) =
                self.command_editor_offset_at_mouse(self.mouse_pos.0, self.mouse_pos.1)
        {
            self.start_command_editor_selection(offset);
            self.notify_interaction_changed();
            return;
        }

        let command_editor_open = self.input_state.lock().command_editor_view.is_some();
        if let Some(kind) = command_editor_mouse_paste_kind(command_editor_open, pressed, button) {
            let handled = match kind {
                ClipboardKind::Clipboard => self.right_click_command_editor(),
                ClipboardKind::Primary => {
                    self.paste_command_editor_selection(ClipboardKind::Primary)
                }
            };
            if handled {
                self.notify_interaction_changed();
                return;
            }
        }

        if pressed {
            self.last_motion_position = None;
        }

        if self.forward_mouse_to_app() {
            let pos = self.mouse_report_position_at(self.mouse_pos.0, self.mouse_pos.1);
            let kind = if pressed {
                MouseEventKind::Press
            } else {
                MouseEventKind::Release
            };
            let mods = self.mouse_modifiers();
            let Some(target) = self.active_input_target() else {
                return;
            };
            Self::emit_host_input(
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
            self.notify_interaction_changed();
            return;
        }

        let (col, row) = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
        match (button, pressed) {
            (MouseButton::Left, true) => {
                if self.modifiers.control_key()
                    && let Some(target) = self.active_input_target()
                {
                    let url = target.terminal.lock();
                    let url =
                        view::hyperlink_at(&url.active, &url.viewport, &url.hyperlinks, row, col)
                            .map(str::to_owned);
                    if let Some(url) = url {
                        if let Err(e) = open::that_detached(&url) {
                            warn!("failed to open hyperlink {url:?}: {e}");
                        }
                        return;
                    }
                }
                if self.modifiers.shift_key() {
                    let extended = if let Some(target) = self.active_input_target() {
                        let mut terminal = target.terminal.lock();
                        if let Some(selection) = terminal.selection.as_ref()
                            && let Some(new_selection) = extend_selection_from_start(
                                selection,
                                &terminal.active,
                                &terminal.viewport,
                                col,
                                row,
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
                        self.left_drag_active = true;
                        self.selection_drag_moved = true;
                        self.refresh_selection_autoscroll_direction();
                        self.notify_interaction_changed();
                        return;
                    }
                }
                self.click_count = self.next_click_count((col, row));
                self.last_click_cell = Some((col, row));
                self.last_click_time = Some(Instant::now());
                let mode = match self.click_count {
                    2 => SelectionMode::Word,
                    3 => SelectionMode::Line,
                    _ => SelectionMode::Char,
                };
                if let Some(target) = self.active_input_target() {
                    let mut target = target.terminal.lock();
                    let target = &mut *target;
                    target.selection =
                        start_selection(&target.active, &target.viewport, col, row, mode);
                    target.invalidate_snapshot_rows();
                }
                self.left_drag_active = true;
                self.selection_drag_moved = false;
                self.refresh_selection_autoscroll_direction();
                self.notify_interaction_changed();
            }
            (MouseButton::Left, false) => {
                self.stop_selection_drag();
                if let Some(target) = self.active_input_target() {
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
                self.notify_interaction_changed();
            }
            (MouseButton::Right, true) => {
                if let Some(target) = self.active_input_target() {
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
                        Self::emit_host_input(
                            target,
                            HostInput::PasteFromClipboard {
                                kind: ClipboardKind::Clipboard,
                            },
                            true,
                        );
                        self.notify_interaction_changed();
                        return;
                    }
                    drop(guard);
                }
                self.notify_interaction_changed();
            }
            _ => {}
        }
    }

    pub(crate) fn handle_mouse_wheel(
        &mut self,
        raw_x: f64,
        raw_y: f64,
        pixels: bool,
    ) {
        if self.permission_modal.is_some() {
            return;
        }
        if self.recording_popup.is_some() {
            return;
        }
        self.close_gutter_popup();
        let (cell_w, cell_h, _, _) = self.layout_snapshot();
        let (x_lines, y_lines) = if pixels {
            let cw = cell_w as i32;
            let ch = cell_h as i32;
            ((raw_x as i32) / cw, -(raw_y as i32) / ch)
        } else {
            (raw_x as i32, -(raw_y as i32))
        };

        if self.forward_mouse_to_app() {
            let pos = self.mouse_report_position_at(self.mouse_pos.0, self.mouse_pos.1);
            let mods = self.mouse_modifiers();
            let Some(target) = self.active_input_target() else {
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
            Self::write_host_bytes(target, effects.host_bytes, true);
            self.notify_interaction_changed();
            return;
        }

        if let Some(target) = self.active_input_target() {
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
        self.notify_interaction_changed();
    }

    pub(crate) fn execute_tab_menu_action(
        &mut self,
        action: TabMenuActionLocal,
        tab_idx: usize,
    ) {
        match action {
            TabMenuActionLocal::NewTab => self.send(RenderEvent::Action(Action::NewTab)),
            TabMenuActionLocal::CloseTab => self.send(RenderEvent::CloseTab(tab_idx)),
            TabMenuActionLocal::CloseOtherTabs => {
                self.send(RenderEvent::CloseOtherTabs(tab_idx));
            }
        }
    }

    pub(crate) fn close_gutter_popup(&mut self) {
        let had_popup = self.input_state.lock().gutter_popup.take().is_some();
        if had_popup && let Some(target) = self.active_input_target() {
            let mut terminal = target.terminal.lock();
            terminal.selection = None;
            terminal.invalidate_snapshot_rows();
        }
    }

    pub(crate) fn open_gutter_popup(
        &mut self,
        screen_row: u32,
    ) {
        let Some(target) = self.active_input_target() else {
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
        self.update_gutter_popup(Some(renderer::GutterPopup {
            prompt_abs_row: prompt_abs,
            screen_row,
            duration_text,
            hovered_item: None,
        }));
        self.notify_interaction_changed();
    }

    pub(crate) fn execute_popup_action(
        &mut self,
        item_idx: usize,
    ) {
        let popup = self.input_state.lock().gutter_popup.take();
        let Some(popup) = popup else {
            return;
        };
        let Some(target) = self.active_input_target() else {
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
                        Self::emit_host_input(
                            target,
                            HostInput::PasteText { text: &text, mode },
                            true,
                        );
                        self.show_toast("Pasted command; review before Enter");
                    } else {
                        self.show_toast(
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
        self.notify_interaction_changed();
    }

    pub(crate) fn mouse_modifiers(&self) -> MouseModifiers {
        MouseModifiers {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            ctrl: self.modifiers.control_key(),
        }
    }

    pub(crate) fn forward_mouse_to_app(&mut self) -> bool {
        let is_shift = self.modifiers.shift_key();
        self.active_input_target().is_some_and(|target| {
            host::mouse_tracking_enabled(target.terminal.lock().modes.mouse_tracking) && !is_shift
        })
    }

    pub(crate) fn next_click_count(
        &self,
        cell: (u32, u32),
    ) -> u32 {
        let within_window = self
            .last_click_time
            .is_some_and(|t| t.elapsed() <= MULTI_CLICK_WINDOW);
        let same_cell = self.last_click_cell == Some(cell);
        if within_window && same_cell && self.click_count < 3 {
            self.click_count + 1
        } else {
            1
        }
    }

    pub(crate) fn cell_at(
        &mut self,
        x: f64,
        y: f64,
    ) -> (u32, u32) {
        let pos = self.mouse_report_position_at(x, y);
        (pos.col, pos.row)
    }

    pub(crate) fn mouse_report_position_at(
        &mut self,
        x: f64,
        y: f64,
    ) -> MouseReportPosition {
        let (cell_w, cell_h, gutter_w, _) = self.layout_snapshot();
        let raw_x = x.max(0.0) as u32;
        let raw_y = y.max(0.0) as u32;
        let command_editor_view_present = self.input_state.lock().command_editor_view.is_some();
        let Some(target) = self.active_input_target() else {
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

    pub(crate) fn mouse_motion_position_key(
        &mut self,
        pos: MouseReportPosition,
    ) -> (u32, u32) {
        let pixel_reporting = self.active_input_target().is_some_and(|target| {
            target.terminal.lock().modes.mouse_encoding == terminal41::MouseEncoding::SgrPixels
        });
        if pixel_reporting {
            (pos.pixel_x, pos.pixel_y)
        } else {
            (pos.col, pos.row)
        }
    }

    pub(crate) fn is_in_tab_bar(&self) -> bool {
        let (_, cell_h, _, _) = self.layout_snapshot();
        (self.mouse_pos.1.max(0.0) as u32) < cell_h
    }

    pub(crate) fn window_button_at(&self) -> Option<WindowButton> {
        match self.tab_bar_hover_at() {
            Some(renderer::TabBarHover::Minimize) => Some(WindowButton::Minimize),
            Some(renderer::TabBarHover::Maximize) => Some(WindowButton::Maximize),
            Some(renderer::TabBarHover::Close) => Some(WindowButton::Close),
            _ => None,
        }
    }

    pub(crate) fn tab_at_mouse(&self) -> Option<usize> {
        let (cell_w, _, _, tab_count) = self.layout_snapshot();
        if tab_count == 0 {
            return None;
        }
        let mx = self.mouse_pos.0.max(0.0) as f32;
        let layout = build_tab_bar_layout(tab_count, self.window_size.0 as f32, cell_w as f32);
        layout
            .tabs
            .iter()
            .position(|tab| mx >= tab.x && mx < tab.x + tab.width)
    }

    pub(crate) fn is_on_new_tab_button(&self) -> bool {
        matches!(self.tab_bar_hover_at(), Some(renderer::TabBarHover::NewTab))
    }

    pub(crate) fn is_in_titlebar_drag_region(&self) -> bool {
        self.is_in_tab_bar() && self.tab_bar_hover_at().is_none()
    }

    pub(crate) fn tab_bar_hover_at(&self) -> Option<renderer::TabBarHover> {
        if !self.is_in_tab_bar() {
            return None;
        }
        let (cell_w, _, _, tab_count) = self.layout_snapshot();
        let mx = self.mouse_pos.0.max(0.0) as f32;
        let layout = build_tab_bar_layout(tab_count, self.window_size.0 as f32, cell_w as f32);
        if mx >= layout.new_tab_button.x
            && mx < layout.new_tab_button.x + layout.new_tab_button.width
        {
            return Some(renderer::TabBarHover::NewTab);
        }
        layout
            .buttons
            .iter()
            .find(|button| mx >= button.x && mx < button.x + button.width)
            .and_then(|button| button.button)
    }

    pub(crate) fn resize_direction_at(&self) -> Option<winit::window::ResizeDirection> {
        use winit::window::ResizeDirection;
        if self.window.as_ref().is_some_and(|w| w.is_maximized()) {
            return None;
        }
        let (w, h) = self.window_size;
        let (mx, my) = (self.mouse_pos.0 as f32, self.mouse_pos.1 as f32);
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
        &self,
        mx: f64,
        my: f64,
    ) -> Option<(TabMenuActionLocal, usize, usize)> {
        let state = self.input_state.lock();
        let menu = state.tab_context_menu.as_ref()?;
        let pw = state.cell_width as f32 * TAB_MENU_WIDTH_CELLS;
        let ph = 3.0 * state.cell_height as f32;
        let px = menu.x.min(self.window_size.0 as f32 - pw);
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

    pub(crate) fn is_in_gutter(&self) -> bool {
        let (_, _, gutter_w, _) = self.layout_snapshot();
        gutter_w > 0 && (self.mouse_pos.0.max(0.0) as u32) < gutter_w
    }

    pub(crate) fn popup_item_at(
        &self,
        x: f64,
        y: f64,
    ) -> Option<usize> {
        let state = self.input_state.lock();
        popup_item_at(
            state.gutter_popup.as_ref(),
            x,
            y,
            state.cell_width,
            state.cell_height,
            state.gutter_width,
            self.window_size.1,
        )
    }
}

use super::*;

impl WindowHost {
    pub(crate) fn send(
        &mut self,
        ev: RenderEvent,
    ) {
        let _ = self.event_tx.push(ev);
        if let Some(thread) = self.render_thread_handle.get() {
            thread.unpark();
        }
    }

    pub(crate) fn active_input_target(&mut self) -> Option<&mut InputEndpoint> {
        let tab_id = self.active_input_tab?;
        self.input_endpoints.get_mut(&tab_id)
    }

    pub(crate) fn startup_tab_titles(&mut self) -> Vec<(String, bool)> {
        let tab_order = self.input_state.lock().tab_order.clone();
        let mut titles: Vec<(String, bool)> = tab_order
            .iter()
            .filter_map(|tab_id| {
                let tab = self.startup_tabs.iter_mut().find(|tab| tab.id == *tab_id)?;
                let title = tab
                    .snapshot_output
                    .read()
                    .current_title
                    .clone()
                    .unwrap_or_else(|| "Shell".to_owned());
                Some((title, Some(*tab_id) == self.active_input_tab))
            })
            .collect();

        if titles.is_empty()
            && let Some(tab_id) = self.active_input_tab
            && let Some(tab) = self.startup_tabs.iter_mut().find(|tab| tab.id == tab_id)
        {
            let title = tab
                .snapshot_output
                .read()
                .current_title
                .clone()
                .unwrap_or_else(|| "Shell".to_owned());
            titles.push((title, true));
        }

        titles
    }

    pub(crate) fn startup_interaction_snapshot(
        &self
    ) -> (
        Option<renderer::TabBarHover>,
        Option<TabContextMenu>,
        Option<renderer::GutterPopup>,
    ) {
        let state = self.input_state.lock();
        (
            state.hovered_tab_bar_button,
            state.tab_context_menu.clone(),
            state.gutter_popup.clone(),
        )
    }

    pub(crate) fn present_startup_frame(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
    ) -> bool {
        let Some(tab_id) = self.active_input_tab else {
            return false;
        };
        let Some(active_startup_tab_idx) =
            self.startup_tabs.iter().position(|tab| tab.id == tab_id)
        else {
            return false;
        };

        let tab_titles = self.startup_tab_titles();
        let tabs: Vec<renderer::TabInfo<'_>> = tab_titles
            .iter()
            .map(|(label, active)| renderer::TabInfo {
                label,
                active: *active,
            })
            .collect();
        let active_startup_tab = &mut self.startup_tabs[active_startup_tab_idx];
        let snap = active_startup_tab.snapshot_output.read().clone();
        let visible_images = {
            let terminal = active_startup_tab.terminal.lock();
            terminal41::view::visible_images(
                &terminal.active,
                &terminal.viewport,
                terminal.cell_height(),
                terminal.cell_width(),
                terminal.kitty_images(),
                &terminal.palette,
                Instant::now(),
            )
            .collect::<Vec<_>>()
        };
        let (hovered_button, tab_context_menu, gutter_popup) = self.startup_interaction_snapshot();
        let maximized = window.is_maximized();
        let Some(presenter) = self.startup_presenter.as_mut() else {
            return false;
        };

        let delay = presenter.present(
            window,
            snap,
            visible_images,
            &tabs,
            self.new_tab_text.clone(),
            hovered_button,
            tab_context_menu.as_ref(),
            gutter_popup.as_ref(),
            maximized,
        );
        self.schedule_startup_redraw(event_loop, delay);
        true
    }

    pub(crate) fn layout_snapshot(&self) -> (u32, u32, u32, usize) {
        let state = self.input_state.lock();
        (
            state.cell_width,
            state.cell_height,
            state.gutter_width,
            state.tab_count,
        )
    }

    pub(crate) fn keybindings(&self) -> Keybindings {
        self.input_state.lock().keybindings.clone()
    }

    pub(crate) fn command_editor_config(&self) -> CommandEditorConfig {
        self.input_state.lock().command_editor_config.clone()
    }

    pub(crate) fn command_editor_is_open_for_tab(
        &self,
        tab_id: TabId,
    ) -> bool {
        let state = self.input_state.lock();
        command_editor_view_open_for_input_tab(&state, Some(tab_id))
    }

    pub(crate) fn clear_terminal_selection_for_tab(
        &mut self,
        tab_id: TabId,
    ) -> bool {
        let Some(target) = self.input_endpoints.get(&tab_id) else {
            return false;
        };
        let mut terminal = target.terminal.lock();
        if terminal.selection.take().is_none() {
            return false;
        }
        terminal.invalidate_snapshot_rows();
        true
    }

    pub(crate) fn clear_command_editor_selection_for_tab(
        &mut self,
        tab_id: TabId,
    ) -> bool {
        let changed = {
            let Some(target) = self.input_endpoints.get_mut(&tab_id) else {
                return false;
            };
            clear_editor_selection(&mut target.command_editor) == EditOutcome::Updated
        };
        if changed && self.active_input_tab == Some(tab_id) {
            self.refresh_command_editor_view();
        }
        changed
    }

    pub(crate) fn copy_active_selection_to_clipboard(
        &mut self,
        tab_id: TabId,
        kind: ClipboardKind,
        clear_after_copy: bool,
        editor_open: bool,
    ) -> Option<SelectionCopySource> {
        let target = self.input_endpoints.get_mut(&tab_id)?;
        let terminal_has_selection = target.terminal.lock().has_selection();
        let editor_selection = selected_text(&target.command_editor);
        let source = selection_copy_source(
            terminal_has_selection,
            editor_selection.is_some(),
            editor_open,
        )?;

        match source {
            SelectionCopySource::Terminal => {
                let mut terminal = target.terminal.lock();
                if let Some(text) = selection_text(terminal.selection.as_ref(), &terminal.active) {
                    terminal.clipboard.set(kind, &text);
                }
                if clear_after_copy {
                    terminal.selection = None;
                    terminal.invalidate_snapshot_rows();
                }
            }
            SelectionCopySource::Editor => {
                let text = editor_selection.expect("source requires editor selection");
                {
                    let mut terminal = target.terminal.lock();
                    terminal.clipboard.set(kind, &text);
                }
                if clear_after_copy {
                    clear_editor_selection(&mut target.command_editor);
                }
            }
        }

        if clear_after_copy && source == SelectionCopySource::Editor {
            self.refresh_command_editor_view();
        }
        Some(source)
    }

    pub(crate) fn set_command_editor_view(
        &mut self,
        tab_id: TabId,
        view: Option<CommandLineView>,
    ) {
        self.input_state.lock().command_editor_view =
            view.map(|view| CommandEditorViewState { tab_id, view });
        self.notify_interaction_changed();
    }

    pub(crate) fn refresh_command_editor_view(&mut self) {
        let Some(tab_id) = self.active_input_tab else {
            self.input_state.lock().command_editor_view = None;
            self.notify_interaction_changed();
            return;
        };
        let view = self.command_editor_view_for_tab(tab_id);
        self.set_command_editor_view(tab_id, view);
    }

    pub(crate) fn command_editor_view_for_tab(
        &mut self,
        tab_id: TabId,
    ) -> Option<CommandLineView> {
        let config = self.command_editor_config();
        if !config.enabled {
            return None;
        }
        self.command_catalog.refresh_for_config(&config);
        let context = {
            let target = self.input_endpoints.get(&tab_id)?;
            let terminal = target.terminal.lock();
            command_editor_view_context(&terminal)
        }?;
        let history_entries = self.command_editor_history_entries(&config);
        let target = self.input_endpoints.get(&tab_id)?;
        let settings = Self::command_editor_settings(
            &config,
            context.current_dir,
            self.command_catalog.names().to_vec(),
            history_entries,
        );
        command_editor_view(&target.command_editor, &settings, config.vim_mode)
    }

    pub(crate) fn command_editor_history_entries(
        &mut self,
        config: &CommandEditorConfig,
    ) -> Vec<HistoryEntry> {
        if !config.deep_history_integration {
            self.command_history_entries.clear();
            self.command_history_loaded = false;
            self.command_history_enabled = false;
            return Vec::new();
        }

        if self.command_history_enabled != config.deep_history_integration {
            self.command_history_entries.clear();
            self.command_history_loaded = false;
            self.command_history_enabled = config.deep_history_integration;
        }

        if !self.command_history_loaded {
            self.command_history_entries =
                match shellhist41::load_current_shell_history(&shellhist41::ShellHistoryOptions {
                    max_entries: config.max_history,
                    ..shellhist41::ShellHistoryOptions::default()
                }) {
                    Ok(entries) => entries
                        .into_iter()
                        .map(|entry| HistoryEntry::external(entry.command))
                        .collect(),
                    Err(error) => {
                        log::debug!("command editor deep history unavailable: {error}");
                        Vec::new()
                    }
                };
            self.command_history_loaded = true;
        }

        self.command_history_entries.clone()
    }

    pub(crate) fn command_editor_settings(
        config: &CommandEditorConfig,
        current_dir: Option<PathBuf>,
        command_words: Vec<String>,
        history_entries: Vec<HistoryEntry>,
    ) -> EditorSettings {
        EditorSettings {
            completion_words: config.completions.clone(),
            command_words,
            history_entries,
            current_dir,
            max_history: config.max_history,
        }
    }

    pub(crate) fn toggle_command_editor(&mut self) {
        let enabled = {
            let mut state = self.input_state.lock();
            state.command_editor_config.enabled = !state.command_editor_config.enabled;
            state.command_editor_config.enabled
        };
        self.refresh_command_editor_view();
        self.show_toast(format!(
            "Command editor: {}",
            if enabled { "on" } else { "off" }
        ));
    }

    pub(crate) fn request_window_grid_size(
        &self,
        cols: u32,
        rows: u32,
    ) {
        let Some(window) = &self.window else {
            return;
        };
        let (cell_width, cell_height, gutter_width, _) = self.layout_snapshot();
        let width = cols.saturating_mul(cell_width).saturating_add(gutter_width);
        let height = rows.saturating_mul(cell_height).saturating_add(cell_height);
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(width, height));
    }

    pub(crate) fn request_window_size_for_tab(
        &self,
        tab_id: TabId,
    ) {
        let Some(endpoint) = self.input_endpoints.get(&tab_id) else {
            return;
        };
        let terminal = endpoint.terminal.lock();
        self.request_window_grid_size(
            terminal.viewport.cols,
            view::total_rows(&terminal.active, &terminal.viewport),
        );
    }

    pub(crate) fn update_preedit(
        &mut self,
        preedit: Option<PreeditState>,
    ) {
        self.input_state.lock().preedit = preedit;
        self.notify_interaction_changed();
    }

    pub(crate) fn update_hovered_tab_bar_button(
        &mut self,
        hovered_button: Option<renderer::TabBarHover>,
    ) {
        self.input_state.lock().hovered_tab_bar_button = hovered_button;
    }

    pub(crate) fn update_tab_context_menu(
        &mut self,
        menu: Option<TabContextMenu>,
    ) {
        self.input_state.lock().tab_context_menu = menu;
    }

    pub(crate) fn update_gutter_popup(
        &mut self,
        popup: Option<renderer::GutterPopup>,
    ) {
        self.input_state.lock().gutter_popup = popup;
    }

    pub(crate) fn notify_interaction_changed(&mut self) {
        self.publish_active_input_snapshot();
        let _ = self.event_tx.push(RenderEvent::None);
        if let Some(thread) = self.render_thread_handle.get() {
            thread.unpark();
        }
        if self.startup_presenter.is_some()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    pub(crate) fn extend_selection_to_mouse(&mut self) -> bool {
        let cell = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
        let Some(target) = self.active_input_target() else {
            return false;
        };
        let mut guard = target.terminal.lock();
        let terminal = &mut *guard;
        let Some(selection) = terminal.selection.as_ref() else {
            return false;
        };
        let Some(new_sel) = extend_selection(
            selection,
            &terminal.active,
            &terminal.viewport,
            cell.0,
            cell.1,
        ) else {
            return false;
        };
        terminal.selection = Some(new_sel);
        terminal.invalidate_snapshot_rows();
        drop(guard);
        if let Some(tab_id) = self.active_input_tab {
            self.clear_command_editor_selection_for_tab(tab_id);
        }
        true
    }

    pub(crate) fn current_selection_autoscroll_direction(&mut self) -> Option<SelectionAutoscroll> {
        if !self.left_drag_active
            || !self.selection_drag_moved
            || self.command_editor_drag_anchor.is_some()
            || self.permission_modal.is_some()
            || self.recording_popup.is_some()
        {
            return None;
        }
        let mouse_y = self.mouse_pos.1;
        let (_, cell_height, _, _) = self.layout_snapshot();
        let command_editor_view_present = {
            let state = self.input_state.lock();
            command_editor_view_open_for_input_tab(&state, self.active_input_tab)
        };
        let target = self.active_input_target()?;
        let terminal = target.terminal.lock();
        terminal.selection.as_ref()?;
        let viewport_rows =
            terminal
                .viewport
                .rows
                .saturating_sub(command_editor_terminal_row_offset(
                    &terminal,
                    command_editor_view_present,
                ));
        selection_autoscroll_direction(mouse_y, cell_height, viewport_rows)
    }

    pub(crate) fn refresh_selection_autoscroll_direction(&mut self) -> Option<SelectionAutoscroll> {
        let direction = self.current_selection_autoscroll_direction();
        if self.selection_autoscroll_direction != direction {
            self.selection_autoscroll_next = None;
            self.selection_autoscroll_direction = direction;
        }
        direction
    }

    pub(crate) fn clear_selection_autoscroll(&mut self) {
        self.selection_autoscroll_direction = None;
        self.selection_autoscroll_next = None;
    }

    pub(crate) fn stop_selection_drag(&mut self) {
        self.left_drag_active = false;
        self.selection_drag_moved = false;
        self.command_editor_drag_anchor = None;
        self.clear_selection_autoscroll();
    }

    pub(crate) fn set_idle_control_flow(
        &self,
        event_loop: &ActiveEventLoop,
    ) {
        if let Some(when) = self.startup_next_redraw {
            event_loop.set_control_flow(ControlFlow::WaitUntil(when));
        } else {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    pub(crate) fn apply_selection_autoscroll(
        &mut self,
        direction: SelectionAutoscroll,
    ) -> bool {
        let cell = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
        let Some(target) = self.active_input_target() else {
            return false;
        };
        let mut guard = target.terminal.lock();
        let terminal = &mut *guard;
        if terminal.selection.is_none() {
            return false;
        }
        let scrolled = match direction {
            SelectionAutoscroll::Up => {
                let viewport = terminal.viewport;
                view::scroll_viewport_up(&mut terminal.active, &viewport, 1)
            }
            SelectionAutoscroll::Down => view::scroll_viewport_down(&mut terminal.active, 1),
        };
        if scrolled == 0 {
            return false;
        }
        if let Some(selection) = terminal.selection.as_ref()
            && let Some(new_sel) = extend_selection(
                selection,
                &terminal.active,
                &terminal.viewport,
                cell.0,
                cell.1,
            )
        {
            terminal.selection = Some(new_sel);
        }
        terminal.invalidate_snapshot_rows();
        true
    }

    pub(crate) fn run_selection_autoscroll(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        let Some(direction) = self.refresh_selection_autoscroll_direction() else {
            self.clear_selection_autoscroll();
            self.set_idle_control_flow(event_loop);
            return;
        };

        let now = Instant::now();
        let due = self.selection_autoscroll_next.unwrap_or(now);
        if now < due {
            event_loop.set_control_flow(ControlFlow::WaitUntil(due));
            return;
        }

        if self.apply_selection_autoscroll(direction) {
            self.notify_interaction_changed();
            let next = now + SELECTION_AUTOSCROLL_INTERVAL;
            self.selection_autoscroll_next = Some(next);
            event_loop.set_control_flow(ControlFlow::WaitUntil(next));
        } else {
            self.clear_selection_autoscroll();
            self.set_idle_control_flow(event_loop);
        }
    }

    pub(crate) fn publish_active_input_snapshot(&mut self) {
        let Some(tab_id) = self.active_input_tab else {
            return;
        };
        let Some(target) = self.input_endpoints.get(&tab_id) else {
            return;
        };
        unpark_thread_if_started(&target.terminal_thread);
    }

    pub(crate) fn schedule_startup_redraw(
        &mut self,
        event_loop: &ActiveEventLoop,
        delay: Option<Duration>,
    ) {
        let Some(delay) = delay else {
            self.startup_next_redraw = None;
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        };
        let when = Instant::now() + delay.max(Duration::from_millis(1));
        self.startup_next_redraw = Some(when);
        event_loop.set_control_flow(ControlFlow::WaitUntil(when));
    }

    pub(crate) fn request_due_startup_redraw(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        if self.startup_presenter.is_none() {
            return;
        }
        let Some(when) = self.startup_next_redraw else {
            return;
        };
        if Instant::now() < when {
            event_loop.set_control_flow(ControlFlow::WaitUntil(when));
            return;
        }
        self.startup_next_redraw = None;
        event_loop.set_control_flow(ControlFlow::Wait);
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }

    pub(crate) fn update_recording_popup_view(
        &mut self,
        popup: Option<RecordingPopupView>,
    ) {
        self.input_state.lock().recording_popup = popup;
        self.notify_interaction_changed();
    }

    pub(crate) fn update_permission_modal_view(
        &mut self,
        modal: Option<PermissionModal>,
    ) {
        self.input_state.lock().permission_modal = modal;
        self.notify_interaction_changed();
    }

    pub(crate) fn update_permission_hover(
        &mut self,
        hovered: Option<PermissionChoice>,
    ) {
        let mut state = self.input_state.lock();
        let Some(modal) = state.permission_modal.as_mut() else {
            return;
        };
        if modal.hovered != hovered {
            modal.hovered = hovered;
            drop(state);
            self.notify_interaction_changed();
        }
    }

    pub(crate) fn update_toast_view(
        &mut self,
        toast: Option<ToastView>,
    ) {
        self.input_state.lock().toast = toast;
        self.notify_interaction_changed();
    }

    pub(crate) fn show_toast(
        &mut self,
        text: impl Into<String>,
    ) {
        let token = self.next_toast_token;
        self.next_toast_token += 1;
        self.update_toast_view(Some(ToastView { text: text.into() }));
        let proxy = self.event_proxy.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(2));
            let _ = proxy.send_event(AppEvent::DismissToast(token));
        });
    }

    pub(crate) fn show_recording_start_popup(
        &mut self,
        path: PathBuf,
    ) {
        let lines = vec![
            "Output recording started.".to_string(),
            format!("Recording to {}.", path.display()),
            "Press enter to start recording.".to_string(),
            "Escape to cancel.".to_string(),
        ];
        self.recording_popup = Some(RecordingPopupState::PendingStart { path });
        self.update_recording_popup_view(Some(RecordingPopupView { lines }));
    }

    pub(crate) fn request_permission(
        &mut self,
        feature: String,
        response_tx: mpsc::Sender<PermissionDecision>,
    ) {
        let request = PermissionRequest {
            feature,
            response_tx,
        };
        if self.permission_modal.is_some() {
            self.queued_permission_requests.push_back(request);
            return;
        }
        self.show_permission_modal(request);
    }

    pub(crate) fn show_permission_modal(
        &mut self,
        request: PermissionRequest,
    ) {
        self.close_gutter_popup();
        self.update_tab_context_menu(None);
        self.permission_modal = Some(PermissionModalState {
            response_tx: request.response_tx,
        });
        self.update_permission_modal_view(Some(PermissionModal {
            feature: request.feature,
            hovered: None,
        }));
    }

    pub(crate) fn settle_permission_modal(
        &mut self,
        decision: PermissionDecision,
    ) {
        if let Some(modal) = self.permission_modal.take() {
            let _ = modal.response_tx.send(decision);
        }
        self.update_permission_modal_view(None);
        if let Some(next) = self.queued_permission_requests.pop_front() {
            self.show_permission_modal(next);
        }
    }

    pub(crate) fn show_recording_completed_popup(
        &mut self,
        path: PathBuf,
    ) {
        let token = self.next_recording_popup_token;
        self.next_recording_popup_token += 1;
        self.recording_popup = Some(RecordingPopupState::Completed { token });
        self.update_recording_popup_view(Some(RecordingPopupView {
            lines: vec![format!("Recorded to {}.", path.display())],
        }));
        let proxy = self.event_proxy.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(3));
            let _ = proxy.send_event(AppEvent::DismissRecordingPopup(token));
        });
    }

    pub(crate) fn show_recording_error_popup(
        &mut self,
        error: std::io::Error,
    ) {
        let token = self.next_recording_popup_token;
        self.next_recording_popup_token += 1;
        self.recording_popup = Some(RecordingPopupState::Completed { token });
        self.update_recording_popup_view(Some(RecordingPopupView {
            lines: vec![
                "Failed to start output recording.".to_string(),
                error.to_string(),
            ],
        }));
        let proxy = self.event_proxy.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(3));
            let _ = proxy.send_event(AppEvent::DismissRecordingPopup(token));
        });
    }

    pub(crate) fn dismiss_recording_popup(&mut self) {
        self.recording_popup = None;
        self.update_recording_popup_view(None);
    }
}

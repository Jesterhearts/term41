use super::*;

pub(crate) fn send(
    render: &mut RenderRuntime,
    ev: RenderEvent,
) {
    let _ = render.event_tx.push(ev);
    if let Some(thread) = render.thread_handle.get() {
        thread.unpark();
    }
}

pub(crate) fn active_input_target(input: &mut InputRuntime) -> Option<&mut InputEndpoint> {
    let tab_id = input.active_tab?;
    input.endpoints.get_mut(&tab_id)
}

pub(crate) fn startup_tab_titles(
    startup: &mut StartupState,
    render: &RenderRuntime,
    active_tab: Option<TabId>,
) -> Vec<(String, bool)> {
    let tab_order = render.input_state.lock().tab_order.clone();
    let mut titles: Vec<(String, bool)> = tab_order
        .iter()
        .filter_map(|tab_id| {
            let tab = startup.tabs.iter_mut().find(|tab| tab.id == *tab_id)?;
            let title = tab
                .snapshot_output
                .read()
                .current_title
                .clone()
                .unwrap_or_else(|| "Shell".to_owned());
            Some((title, Some(*tab_id) == active_tab))
        })
        .collect();

    if titles.is_empty()
        && let Some(tab_id) = active_tab
        && let Some(tab) = startup.tabs.iter_mut().find(|tab| tab.id == tab_id)
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
    render: &RenderRuntime
) -> (
    Option<renderer::TabBarHover>,
    Option<TabContextMenu>,
    Option<renderer::GutterPopup>,
) {
    let state = render.input_state.lock();
    (
        state.hovered_tab_bar_button,
        state.tab_context_menu.clone(),
        state.gutter_popup.clone(),
    )
}

pub(crate) fn present_startup_frame(
    host: &mut WindowHost,
    event_loop: &ActiveEventLoop,
    window: &Arc<Window>,
) -> bool {
    let Some(tab_id) = host.input.active_tab else {
        return false;
    };
    let Some(active_startup_tab_idx) = host.startup.tabs.iter().position(|tab| tab.id == tab_id)
    else {
        return false;
    };

    let tab_titles = startup_tab_titles(&mut host.startup, &host.render, host.input.active_tab);
    let tabs: Vec<renderer::TabInfo<'_>> = tab_titles
        .iter()
        .map(|(label, active)| renderer::TabInfo {
            label,
            active: *active,
        })
        .collect();
    let active_startup_tab = &mut host.startup.tabs[active_startup_tab_idx];
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
    let (hovered_button, tab_context_menu, gutter_popup) =
        startup_interaction_snapshot(&host.render);
    let maximized = window.is_maximized();
    let Some(presenter) = host.startup.presenter.as_mut() else {
        return false;
    };

    let delay = presenter.present(
        window,
        snap,
        visible_images,
        &tabs,
        host.metrics.new_tab_text.clone(),
        hovered_button,
        tab_context_menu.as_ref(),
        gutter_popup.as_ref(),
        maximized,
    );
    schedule_startup_redraw(&mut host.startup, event_loop, delay);
    true
}

pub(crate) fn layout_snapshot(render: &RenderRuntime) -> (u32, u32, u32, usize) {
    let state = render.input_state.lock();
    (
        state.cell_width,
        state.cell_height,
        state.gutter_width,
        state.tab_count,
    )
}

pub(crate) fn keybindings(render: &RenderRuntime) -> Keybindings {
    render.input_state.lock().keybindings.clone()
}

pub(crate) fn command_editor_config(render: &RenderRuntime) -> CommandEditorConfig {
    render.input_state.lock().command_editor_config.clone()
}

pub(crate) fn command_palette_is_open(render: &RenderRuntime) -> bool {
    render.input_state.lock().command_palette.is_some()
}

pub(crate) fn history_confirmation_is_open(render: &RenderRuntime) -> bool {
    render.input_state.lock().history_confirmation.is_some()
}

pub(crate) fn history_deletion_is_open(render: &RenderRuntime) -> bool {
    render.input_state.lock().history_deletion.is_some()
}

pub(crate) fn open_command_palette(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
) {
    render.input_state.lock().command_palette = Some(command_palette_view());
    notify_interaction_changed(input, render, startup, window);
}

pub(crate) fn close_command_palette(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
) {
    render.input_state.lock().command_palette = None;
    notify_interaction_changed(input, render, startup, window);
}

pub(crate) fn move_host_command_palette_selection(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
    delta: isize,
) {
    let mut state = render.input_state.lock();
    let Some(view) = state.command_palette.as_mut() else {
        return;
    };
    super::move_command_palette_selection(view, delta);
    drop(state);
    notify_interaction_changed(input, render, startup, window);
}

pub(crate) fn update_command_palette_query(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
    update: impl FnOnce(&mut String),
) {
    let mut state = render.input_state.lock();
    let Some(view) = state.command_palette.as_mut() else {
        return;
    };
    let mut query = view.query.clone();
    update(&mut query);
    set_command_palette_query(view, query);
    drop(state);
    notify_interaction_changed(input, render, startup, window);
}

pub(crate) fn update_history_deletion_query(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
    update: impl FnOnce(&mut String),
) {
    let mut state = render.input_state.lock();
    let Some(view) = state.history_deletion.as_mut() else {
        return;
    };
    let mut query = view.query.clone();
    update(&mut query);
    set_history_deletion_query(view, query);
    drop(state);
    notify_interaction_changed(input, render, startup, window);
}

pub(crate) fn scroll_host_history_deletion(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
    delta: isize,
) {
    let mut state = render.input_state.lock();
    let Some(view) = state.history_deletion.as_mut() else {
        return;
    };
    scroll_history_deletion_view(view, delta, 1);
    drop(state);
    notify_interaction_changed(input, render, startup, window);
}

pub(crate) fn complete_host_command_palette_selection(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
) {
    let mut state = render.input_state.lock();
    let Some(view) = state.command_palette.as_mut() else {
        return;
    };
    if super::complete_command_palette_selection(view) {
        drop(state);
        notify_interaction_changed(input, render, startup, window);
    }
}

pub(crate) fn accept_command_palette_selection(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
) -> Option<CommandPaletteInvocation> {
    let mut state = render.input_state.lock();
    let view = state.command_palette.as_mut()?;
    match command_palette_selected_invocation(view)? {
        CommandPaletteAccept::Ready(invocation) => {
            state.command_palette = None;
            drop(state);
            notify_interaction_changed(input, render, startup, window);
            Some(invocation)
        }
        CommandPaletteAccept::NeedsArgument => {
            let completed = super::complete_command_palette_selection(view);
            drop(state);
            if completed {
                notify_interaction_changed(input, render, startup, window);
            }
            None
        }
    }
}

pub(crate) fn command_editor_is_open_for_tab(
    render: &RenderRuntime,
    tab_id: TabId,
) -> bool {
    let state = render.input_state.lock();
    command_editor_view_open_for_input_tab(&state, Some(tab_id))
}

pub(crate) fn clear_terminal_selection_for_tab(
    host: &mut WindowHost,
    tab_id: TabId,
) -> bool {
    let Some(target) = host.input.endpoints.get(&tab_id) else {
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
    host: &mut WindowHost,
    tab_id: TabId,
) -> bool {
    let changed = {
        let Some(target) = host.input.endpoints.get_mut(&tab_id) else {
            return false;
        };
        clear_editor_selection(&mut target.command_editor) == EditOutcome::Updated
    };
    if changed && host.input.active_tab == Some(tab_id) {
        refresh_command_editor_view(host);
    }
    changed
}

pub(crate) fn copy_active_selection_to_clipboard(
    host: &mut WindowHost,
    tab_id: TabId,
    kind: ClipboardKind,
    clear_after_copy: bool,
    editor_open: bool,
) -> Option<SelectionCopySource> {
    let target = host.input.endpoints.get_mut(&tab_id)?;
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
        refresh_command_editor_view(host);
    }
    Some(source)
}

pub(crate) fn set_command_editor_view(
    host: &mut WindowHost,
    tab_id: TabId,
    view: Option<CommandLineView>,
) {
    {
        let mut state = host.render.input_state.lock();
        if let Some(view) = view {
            state.command_editor_views.insert(tab_id, view);
        } else {
            state.command_editor_views.remove(&tab_id);
        }
    }
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn refresh_command_editor_view(host: &mut WindowHost) {
    let Some(tab_id) = host.input.active_tab else {
        host.render.input_state.lock().command_editor_views.clear();
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        return;
    };
    refresh_command_editor_view_for_tab(host, tab_id);
}

pub(crate) fn refresh_command_editor_view_for_tab(
    host: &mut WindowHost,
    tab_id: TabId,
) {
    let view = command_editor_view_for_tab(host, tab_id);
    set_command_editor_view(host, tab_id, view);
}

pub(crate) fn command_editor_view_for_tab(
    host: &mut WindowHost,
    tab_id: TabId,
) -> Option<CommandLineView> {
    let config = command_editor_config(&host.render);
    if !config.enabled {
        return None;
    }
    host.command.catalog.refresh_for_config(&config);
    let context = {
        let target = host.input.endpoints.get(&tab_id)?;
        let terminal = target.terminal.lock();
        command_editor_view_context(&terminal)
    }?;
    let history_entries =
        command_editor_history_entries(host, &config, context.current_dir.as_deref());
    let target = host.input.endpoints.get(&tab_id)?;
    let settings = command_editor_settings(
        &config,
        context.current_dir,
        host.command.catalog.names().to_vec(),
        history_entries,
    );
    command_editor_view(&target.command_editor, &settings, config.vim_mode)
}

pub(crate) fn command_editor_history_entries(
    host: &mut WindowHost,
    config: &CommandEditorConfig,
    current_dir: Option<&std::path::Path>,
) -> Vec<HistoryEntry> {
    let mut entries = shell_history_entries(host, config);
    entries.extend(persistent_history_entries(host, config, current_dir));
    entries
}

pub(crate) fn shell_history_entries(
    host: &mut WindowHost,
    config: &CommandEditorConfig,
) -> Vec<HistoryEntry> {
    if !config.deep_history_integration {
        host.command.shell_history_entries.clear();
        host.command.shell_history_loaded = false;
        host.command.shell_history_enabled = false;
        return Vec::new();
    }

    if host.command.shell_history_enabled != config.deep_history_integration {
        host.command.shell_history_entries.clear();
        host.command.shell_history_loaded = false;
        host.command.shell_history_enabled = config.deep_history_integration;
    }

    if !host.command.shell_history_loaded {
        host.command.shell_history_entries =
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
        host.command.shell_history_loaded = true;
    }

    host.command.shell_history_entries.clone()
}

pub(crate) fn persistent_history_entries(
    host: &WindowHost,
    config: &CommandEditorConfig,
    current_dir: Option<&std::path::Path>,
) -> Vec<HistoryEntry> {
    let Some(store) = &host.command.history_store else {
        return Vec::new();
    };
    let Some(current_dir) = current_dir else {
        return Vec::new();
    };
    match history41::recent_commands(
        store,
        history41::HistoryQuery {
            cwd: current_dir.to_owned(),
            limit: config.max_history,
            include_global_fallback: true,
        },
    ) {
        Ok(entries) => entries
            .into_iter()
            .rev()
            .map(|entry| HistoryEntry::external(entry.command))
            .collect(),
        Err(error) => {
            debug!("persistent command history unavailable: {error}");
            Vec::new()
        }
    }
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
        command_completions: command_completion_settings(config),
        history_entries,
        current_dir,
        max_history: config.max_history,
        escape_character: hooks41::current_shell_escape_character(),
    }
}

fn command_completion_settings(config: &CommandEditorConfig) -> Vec<commands41::CommandCompletion> {
    config
        .command_completions
        .iter()
        .map(|completion| commands41::CommandCompletion {
            command: completion.command.clone(),
            subcommands: completion
                .subcommands
                .iter()
                .map(|subcommand| commands41::SubcommandCompletion {
                    name: subcommand.name.clone(),
                    arguments: subcommand.arguments.clone(),
                })
                .collect(),
        })
        .collect()
}

pub(crate) fn enqueue_persistent_command_history(
    host: &mut WindowHost,
    command: String,
    cwd: PathBuf,
    config: &CommandEditorConfig,
) {
    let Some(writer) = &mut host.command.history_writer else {
        return;
    };
    writer.enqueue(history_runtime::store_request(command, cwd, config));
}

pub(crate) fn flush_persistent_command_history(host: &mut WindowHost) {
    let store = host.command.history_store.clone();
    if let Some(writer) = host.command.history_writer.take() {
        writer.finish();
    }
    host.command.history_writer = store.and_then(history_runtime::spawn_history_writer);
}

pub(crate) fn open_clear_all_history_confirmation(host: &mut WindowHost) {
    if host.command.history_store.is_none() {
        show_toast(host, "Persistent history is not available");
        return;
    }
    host.modals.history_confirmation = Some(HistoryConfirmation::ClearAll);
    update_history_confirmation_view(
        host,
        Some(HistoryConfirmationView {
            title: "Clear all history?".to_owned(),
            message: "Enter clears all persistent command history. Escape cancels.".to_owned(),
        }),
    );
}

pub(crate) fn open_clear_directory_history_confirmation(
    host: &mut WindowHost,
    tab_id: TabId,
) {
    if host.command.history_store.is_none() {
        show_toast(host, "Persistent history is not available");
        return;
    }
    let Some(target) = host.input.endpoints.get(&tab_id) else {
        return;
    };
    let Some(cwd) = target.terminal.lock().metadata.current_directory.clone() else {
        show_toast(host, "Current directory is not available");
        return;
    };
    host.modals.history_confirmation = Some(HistoryConfirmation::ClearDirectory(cwd.clone()));
    update_history_confirmation_view(
        host,
        Some(HistoryConfirmationView {
            title: "Clear directory history?".to_owned(),
            message: format!(
                "Enter clears persistent history for {}. Escape cancels.",
                cwd.display()
            ),
        }),
    );
}

pub(crate) fn open_history_deletion(host: &mut WindowHost) {
    let Some(store) = host.command.history_store.clone() else {
        show_toast(host, "Persistent history is not available");
        return;
    };
    flush_persistent_command_history(host);
    match history41::all_commands(&store) {
        Ok(entries) if entries.is_empty() => show_toast(host, "Persistent history is empty"),
        Ok(entries) => {
            host.render.input_state.lock().history_deletion = Some(history_deletion_view(entries));
            notify_interaction_changed(
                &host.input,
                &mut host.render,
                &host.startup,
                host.window.as_ref(),
            );
        }
        Err(error) => {
            debug!("persistent command history query failed: {error}");
            show_toast(host, "Could not load persistent history");
        }
    }
}

pub(crate) fn close_history_deletion(host: &mut WindowHost) {
    host.render.input_state.lock().history_deletion = None;
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn confirm_history_clear(host: &mut WindowHost) {
    let Some(confirmation) = host.modals.history_confirmation.take() else {
        return;
    };
    update_history_confirmation_view(host, None);
    let Some(store) = host.command.history_store.clone() else {
        show_toast(host, "Persistent history is not available");
        return;
    };
    flush_persistent_command_history(host);
    let result = match confirmation {
        HistoryConfirmation::ClearAll => history41::clear_all(&store),
        HistoryConfirmation::ClearDirectory(cwd) => history41::clear_cwd(&store, &cwd),
    };
    match result {
        Ok(_) => {
            refresh_command_editor_view(host);
            show_toast(host, "History cleared");
        }
        Err(error) => {
            debug!("persistent command history clear failed: {error}");
            show_toast(host, "Could not clear history");
        }
    }
}

pub(crate) fn cancel_history_confirmation(host: &mut WindowHost) {
    host.modals.history_confirmation = None;
    update_history_confirmation_view(host, None);
}

pub(crate) fn delete_displayed_history_entries(host: &mut WindowHost) {
    let Some(view) = host.render.input_state.lock().history_deletion.clone() else {
        return;
    };
    close_history_deletion(host);
    if view.query.trim().is_empty() {
        show_toast(host, "History deletion canceled");
        return;
    }
    let Some(store) = host.command.history_store.clone() else {
        show_toast(host, "Persistent history is not available");
        return;
    };
    let keys: Vec<_> = view
        .displayed
        .iter()
        .filter_map(|idx| view.entries.get(*idx))
        .map(|entry| entry.key.clone())
        .collect();
    if keys.is_empty() {
        show_toast(host, "No matching history entries");
        return;
    }
    flush_persistent_command_history(host);
    match history41::delete_entries(&store, &keys) {
        Ok(_) => {
            refresh_command_editor_view(host);
            show_toast(host, format!("Deleted {} history entries", keys.len()));
        }
        Err(error) => {
            debug!("persistent command history delete failed: {error}");
            show_toast(host, "Could not delete history entries");
        }
    }
}

pub(crate) fn toggle_command_editor(host: &mut WindowHost) {
    let enabled = {
        let mut state = host.render.input_state.lock();
        state.command_editor_config.enabled = !state.command_editor_config.enabled;
        if !state.command_editor_config.enabled {
            state.command_editor_views.clear();
        }
        state.command_editor_config.enabled
    };
    refresh_command_editor_view(host);
    show_toast(
        host,
        format!("Command editor: {}", if enabled { "on" } else { "off" }),
    );
}

pub(crate) fn request_window_grid_size(
    host: &WindowHost,
    cols: u32,
    rows: u32,
) {
    let Some(window) = &host.window else {
        return;
    };
    let (cell_width, cell_height, gutter_width, _) = layout_snapshot(&host.render);
    let width = cols.saturating_mul(cell_width).saturating_add(gutter_width);
    let height = rows.saturating_mul(cell_height).saturating_add(cell_height);
    let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(width, height));
}

pub(crate) fn request_window_size_for_tab(
    host: &WindowHost,
    tab_id: TabId,
) {
    let Some(endpoint) = host.input.endpoints.get(&tab_id) else {
        return;
    };
    let terminal = endpoint.terminal.lock();
    request_window_grid_size(
        host,
        terminal.viewport.cols,
        view::total_rows(&terminal.active, &terminal.viewport),
    );
}

pub(crate) fn update_preedit(
    host: &mut WindowHost,
    preedit: Option<PreeditState>,
) {
    host.render.input_state.lock().preedit = preedit;
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn update_hovered_tab_bar_button(
    render: &RenderRuntime,
    hovered_button: Option<renderer::TabBarHover>,
) {
    render.input_state.lock().hovered_tab_bar_button = hovered_button;
}

pub(crate) fn update_tab_context_menu(
    render: &RenderRuntime,
    menu: Option<TabContextMenu>,
) {
    render.input_state.lock().tab_context_menu = menu;
}

pub(crate) fn update_gutter_popup(
    render: &RenderRuntime,
    popup: Option<renderer::GutterPopup>,
) {
    render.input_state.lock().gutter_popup = popup;
}

pub(crate) fn notify_interaction_changed(
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
) {
    publish_active_input_snapshot(input);
    let _ = render.event_tx.push(RenderEvent::None);
    if let Some(thread) = render.thread_handle.get() {
        thread.unpark();
    }
    if startup.presenter.is_some()
        && let Some(window) = window
    {
        window.request_redraw();
    }
}

pub(crate) fn extend_selection_to_mouse(host: &mut WindowHost) -> bool {
    let cell = cell_at(host, host.mouse.pos.0, host.mouse.pos.1);
    let Some(target) = active_input_target(&mut host.input) else {
        return false;
    };
    let mut guard = target.terminal.lock();
    let terminal = &mut *guard;
    let Some(selection) = terminal.selection.as_ref() else {
        return false;
    };
    let Some(new_sel) = extend_rendered_selection(
        selection,
        &terminal.active,
        &terminal.viewport,
        terminal.on_alt_screen,
        cell.0,
        cell.1,
    ) else {
        return false;
    };
    terminal.selection = Some(new_sel);
    terminal.invalidate_snapshot_rows();
    drop(guard);
    if let Some(tab_id) = host.input.active_tab {
        clear_command_editor_selection_for_tab(host, tab_id);
    }
    true
}

pub(crate) fn current_selection_autoscroll_direction(
    host: &mut WindowHost
) -> Option<SelectionAutoscroll> {
    if !host.mouse.left_drag_active
        || !host.mouse.selection_drag_moved
        || host.mouse.command_editor_drag_anchor.is_some()
        || host.modals.permission_modal.is_some()
        || host.modals.recording_popup.is_some()
    {
        return None;
    }
    let mouse_y = host.mouse.pos.1;
    let (_, cell_height, _, _) = layout_snapshot(&host.render);
    let command_editor_view_present = {
        let state = host.render.input_state.lock();
        command_editor_view_open_for_input_tab(&state, host.input.active_tab)
    };
    let target = active_input_target(&mut host.input)?;
    let terminal = target.terminal.lock();
    terminal.selection.as_ref()?;
    let viewport_rows = terminal
        .viewport
        .rows
        .saturating_sub(command_editor_terminal_row_offset(
            &terminal,
            command_editor_view_present,
        ));
    selection_autoscroll_direction(mouse_y, cell_height, viewport_rows)
}

pub(crate) fn refresh_selection_autoscroll_direction(
    host: &mut WindowHost
) -> Option<SelectionAutoscroll> {
    let direction = current_selection_autoscroll_direction(host);
    if host.mouse.selection_autoscroll_direction != direction {
        host.mouse.selection_autoscroll_next = None;
        host.mouse.selection_autoscroll_direction = direction;
    }
    direction
}

pub(crate) fn clear_selection_autoscroll(mouse: &mut MouseRuntime) {
    mouse.selection_autoscroll_direction = None;
    mouse.selection_autoscroll_next = None;
}

pub(crate) fn stop_selection_drag(mouse: &mut MouseRuntime) {
    mouse.left_drag_active = false;
    mouse.selection_drag_moved = false;
    mouse.command_editor_drag_anchor = None;
    clear_selection_autoscroll(mouse);
}

pub(crate) fn set_idle_control_flow(
    startup: &StartupState,
    event_loop: &ActiveEventLoop,
) {
    if let Some(when) = startup.next_redraw {
        event_loop.set_control_flow(ControlFlow::WaitUntil(when));
    } else {
        event_loop.set_control_flow(ControlFlow::Wait);
    }
}

pub(crate) fn apply_selection_autoscroll(
    host: &mut WindowHost,
    direction: SelectionAutoscroll,
) -> bool {
    let cell = cell_at(host, host.mouse.pos.0, host.mouse.pos.1);
    let Some(target) = active_input_target(&mut host.input) else {
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
        && let Some(new_sel) = extend_rendered_selection(
            selection,
            &terminal.active,
            &terminal.viewport,
            terminal.on_alt_screen,
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
    host: &mut WindowHost,
    event_loop: &ActiveEventLoop,
) {
    let Some(direction) = refresh_selection_autoscroll_direction(host) else {
        clear_selection_autoscroll(&mut host.mouse);
        set_idle_control_flow(&host.startup, event_loop);
        return;
    };

    let now = Instant::now();
    let due = host.mouse.selection_autoscroll_next.unwrap_or(now);
    if now < due {
        event_loop.set_control_flow(ControlFlow::WaitUntil(due));
        return;
    }

    if apply_selection_autoscroll(host, direction) {
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
        let next = now + SELECTION_AUTOSCROLL_INTERVAL;
        host.mouse.selection_autoscroll_next = Some(next);
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    } else {
        clear_selection_autoscroll(&mut host.mouse);
        set_idle_control_flow(&host.startup, event_loop);
    }
}

pub(crate) fn publish_active_input_snapshot(input: &InputRuntime) {
    let Some(tab_id) = input.active_tab else {
        return;
    };
    let Some(target) = input.endpoints.get(&tab_id) else {
        return;
    };
    unpark_thread_if_started(&target.terminal_thread);
}

pub(crate) fn schedule_startup_redraw(
    startup: &mut StartupState,
    event_loop: &ActiveEventLoop,
    delay: Option<Duration>,
) {
    let Some(delay) = delay else {
        startup.next_redraw = None;
        event_loop.set_control_flow(ControlFlow::Wait);
        return;
    };
    let when = Instant::now() + delay.max(Duration::from_millis(1));
    startup.next_redraw = Some(when);
    event_loop.set_control_flow(ControlFlow::WaitUntil(when));
}

pub(crate) fn request_due_startup_redraw(
    startup: &mut StartupState,
    window: Option<&Arc<Window>>,
    event_loop: &ActiveEventLoop,
) {
    if startup.presenter.is_none() {
        return;
    }
    let Some(when) = startup.next_redraw else {
        return;
    };
    if Instant::now() < when {
        event_loop.set_control_flow(ControlFlow::WaitUntil(when));
        return;
    }
    startup.next_redraw = None;
    event_loop.set_control_flow(ControlFlow::Wait);
    if let Some(window) = window {
        window.request_redraw();
    }
}

pub(crate) fn update_recording_popup_view(
    host: &mut WindowHost,
    popup: Option<RecordingPopupView>,
) {
    host.render.input_state.lock().recording_popup = popup;
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn update_permission_modal_view(
    host: &mut WindowHost,
    modal: Option<PermissionModal>,
) {
    host.render.input_state.lock().permission_modal = modal;
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn update_history_confirmation_view(
    host: &mut WindowHost,
    modal: Option<HistoryConfirmationView>,
) {
    host.render.input_state.lock().history_confirmation = modal;
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn update_permission_hover(
    host: &mut WindowHost,
    hovered: Option<PermissionChoice>,
) {
    let mut state = host.render.input_state.lock();
    let Some(modal) = state.permission_modal.as_mut() else {
        return;
    };
    if modal.hovered != hovered {
        modal.hovered = hovered;
        drop(state);
        notify_interaction_changed(
            &host.input,
            &mut host.render,
            &host.startup,
            host.window.as_ref(),
        );
    }
}

pub(crate) fn update_toast_view(
    host: &mut WindowHost,
    toast: Option<ToastView>,
) {
    host.render.input_state.lock().toast = toast;
    notify_interaction_changed(
        &host.input,
        &mut host.render,
        &host.startup,
        host.window.as_ref(),
    );
}

pub(crate) fn show_toast(
    host: &mut WindowHost,
    text: impl Into<String>,
) {
    let token = host.modals.next_toast_token;
    host.modals.next_toast_token += 1;
    update_toast_view(host, Some(ToastView { text: text.into() }));
    let proxy = host.render.event_proxy.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(2));
        let _ = proxy.send_event(AppEvent::DismissToast(token));
    });
}

pub(crate) fn show_recording_start_popup(
    host: &mut WindowHost,
    path: PathBuf,
) {
    let lines = vec![
        "Output recording started.".to_string(),
        format!("Recording to {}.", path.display()),
        "Press enter to start recording.".to_string(),
        "Escape to cancel.".to_string(),
    ];
    host.modals.recording_popup = Some(RecordingPopupState::PendingStart { path });
    update_recording_popup_view(host, Some(RecordingPopupView { lines }));
}

pub(crate) fn request_permission(
    host: &mut WindowHost,
    feature: String,
    response_tx: mpsc::Sender<PermissionDecision>,
) {
    let request = PermissionRequest {
        feature,
        response_tx,
    };
    if host.modals.permission_modal.is_some() {
        host.modals.queued_permission_requests.push_back(request);
        return;
    }
    show_permission_modal(host, request);
}

pub(crate) fn show_permission_modal(
    host: &mut WindowHost,
    request: PermissionRequest,
) {
    close_gutter_popup(&host.render, &mut host.input);
    update_tab_context_menu(&host.render, None);
    host.modals.permission_modal = Some(PermissionModalState {
        response_tx: request.response_tx,
    });
    update_permission_modal_view(
        host,
        Some(PermissionModal {
            feature: request.feature,
            hovered: None,
        }),
    );
}

pub(crate) fn settle_permission_modal(
    host: &mut WindowHost,
    decision: PermissionDecision,
) {
    if let Some(modal) = host.modals.permission_modal.take() {
        let _ = modal.response_tx.send(decision);
    }
    update_permission_modal_view(host, None);
    if let Some(next) = host.modals.queued_permission_requests.pop_front() {
        show_permission_modal(host, next);
    }
}

pub(crate) fn show_recording_completed_popup(
    host: &mut WindowHost,
    path: PathBuf,
) {
    let token = host.modals.next_recording_popup_token;
    host.modals.next_recording_popup_token += 1;
    host.modals.recording_popup = Some(RecordingPopupState::Completed { token });
    update_recording_popup_view(
        host,
        Some(RecordingPopupView {
            lines: vec![format!("Recorded to {}.", path.display())],
        }),
    );
    let proxy = host.render.event_proxy.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        let _ = proxy.send_event(AppEvent::DismissRecordingPopup(token));
    });
}

pub(crate) fn show_recording_error_popup(
    host: &mut WindowHost,
    error: std::io::Error,
) {
    let token = host.modals.next_recording_popup_token;
    host.modals.next_recording_popup_token += 1;
    host.modals.recording_popup = Some(RecordingPopupState::Completed { token });
    update_recording_popup_view(
        host,
        Some(RecordingPopupView {
            lines: vec![
                "Failed to start output recording.".to_string(),
                error.to_string(),
            ],
        }),
    );
    let proxy = host.render.event_proxy.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(3));
        let _ = proxy.send_event(AppEvent::DismissRecordingPopup(token));
    });
}

pub(crate) fn dismiss_recording_popup(
    modals: &mut ModalRuntime,
    input: &InputRuntime,
    render: &mut RenderRuntime,
    startup: &StartupState,
    window: Option<&Arc<Window>>,
) {
    modals.recording_popup = None;
    render.input_state.lock().recording_popup = None;
    notify_interaction_changed(input, render, startup, window);
}

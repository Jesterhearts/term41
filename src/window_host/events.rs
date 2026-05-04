use super::*;

impl ApplicationHandler<AppEvent> for WindowHost {
    fn about_to_wait(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        request_due_startup_redraw(&mut self.startup, self.window.as_ref(), event_loop);
        run_selection_autoscroll(self, event_loop);
    }

    fn user_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        event: AppEvent,
    ) {
        match event {
            AppEvent::SetTitle(title) => {
                if let Some(w) = &self.window {
                    w.set_title(&title);
                }
            }
            AppEvent::RequestUserAttention => {
                if let Some(w) = &self.window {
                    w.request_user_attention(Some(winit::window::UserAttentionType::Informational));
                }
            }
            AppEvent::RequestStartupRedraw => {
                if self.startup.presenter.is_some()
                    && let Some(window) = &self.window
                {
                    window.request_redraw();
                }
            }
            AppEvent::ReleaseStartupSurface => {
                info!(
                    "Releasing startup surface: {} ms",
                    APP_START_TIME.get().unwrap().elapsed().as_millis()
                );
                self.startup.presenter = None;
                self.startup.next_redraw = None;
                if let Some(tx) = self.startup.release_tx.take() {
                    let _ = tx.send(std::mem::take(&mut self.startup.tabs));
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            AppEvent::ApplyTerminalEffects { tab_id, effects } => {
                apply_terminal_effects(self, tab_id, effects);
            }
            AppEvent::RegisterInputEndpoint {
                tab_id,
                terminal,
                writer,
                recorder,
                terminal_thread,
            } => {
                self.input.endpoints.insert(
                    tab_id,
                    InputEndpoint {
                        terminal,
                        writer,
                        recorder,
                        terminal_thread,
                        command_editor: CommandEditor::new(),
                    },
                );
                if self.input.active_tab == Some(tab_id) {
                    refresh_command_editor_view(self);
                }
            }
            AppEvent::RemoveInputEndpoint(tab_id) => {
                self.input.endpoints.remove(&tab_id);
                self.render
                    .input_state
                    .lock()
                    .command_editor_views
                    .remove(&tab_id);
                self.startup.tabs.retain(|tab| tab.id != tab_id);
            }
            AppEvent::SetActiveInputTab(tab_id) => {
                self.input.active_tab = tab_id;
                if let Some(tab_id) = tab_id {
                    request_window_size_for_tab(self, tab_id);
                }
                refresh_command_editor_view(self);
            }
            AppEvent::ResolveClipboardRequest {
                tab_id,
                request,
                decision,
            } => {
                resolve_clipboard_request(&mut self.input, tab_id, request, decision);
            }
            AppEvent::ResolveKittyFileRequest {
                tab_id,
                request,
                decision,
            } => {
                resolve_kitty_file_request(&mut self.input, tab_id, request, decision);
            }
            AppEvent::DismissRecordingPopup(token) => {
                let dismiss = matches!(
                    self.modals.recording_popup,
                    Some(RecordingPopupState::Completed { token: current }) if current == token
                );
                if dismiss {
                    dismiss_recording_popup(
                        &mut self.modals,
                        &self.input,
                        &mut self.render,
                        &self.startup,
                        self.window.as_ref(),
                    );
                }
            }
            AppEvent::ShowToast(message) => {
                show_toast(self, message);
            }
            AppEvent::DismissToast(token) => {
                if token + 1 == self.modals.next_toast_token {
                    update_toast_view(self, None);
                }
            }
        }
    }

    fn resumed(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        if self.window.is_some() {
            return;
        }

        let pixel_width =
            INITIAL_COLS * self.metrics.cell_width + compute_gutter_width(self.metrics.cell_width);
        // One extra cell_height for the tab bar, which is always visible
        // (it doubles as the titlebar for CSD window management).
        let pixel_height = INITIAL_ROWS * self.metrics.cell_height + self.metrics.cell_height;
        let transparent = self.metrics.opacity < 1.0;
        // LogicalSize so the window occupies the same visual area regardless
        // of the monitor's DPI scale factor. Cell metrics are computed at 1x
        // here; the render thread rescales them once it knows the actual
        // scale factor.
        let attrs = Window::default_attributes()
            .with_decorations(false)
            .with_title("term41")
            .with_transparent(transparent)
            .with_inner_size(winit::dpi::LogicalSize::new(pixel_width, pixel_height));

        // On Wayland, also set the app ID and class to help with
        // compositor-specific configuration. The app ID is supposed to be
        // the executable name, but winit doesn't have a way to get that
        // directly, so we just hardcode it.
        #[cfg(target_os = "linux")]
        let attrs = attrs.with_name("com.jesterhearts.term41", "com.jesterhearts.term41");

        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let startup_window_size = window.inner_size();

        let scale_factor = self
            .metrics
            .startup_dpi_scale
            .map(|s| s as f64)
            .unwrap_or_else(|| window.scale_factor());
        let startup_background = startup_snapshot_path();
        self.startup.presenter = StartupPresenter::new(
            window.clone(),
            self.metrics.startup_fonts.clone(),
            self.metrics.startup_font_size,
            self.metrics.startup_supersampling,
            scale_factor,
            self.metrics.startup_gutter,
            startup_background,
        );
        if present_startup_frame(self, event_loop, &window) {
            window.request_redraw();
        }

        // Opt into IME events. `ImePurpose::Terminal` is a hint some
        // Wayland compositors and Android IMEs use to expose extra keys
        // (arrows, Tab, etc.) that wouldn't normally appear on a text-input
        // OSK; on platforms that don't understand it, it's a no-op.
        window.set_ime_allowed(true);
        window.set_ime_purpose(winit::window::ImePurpose::Terminal);

        if let Some(tx) = self.render.window_tx.take() {
            let _ = tx.send((window.clone(), event_loop.owned_display_handle()));
        }

        self.metrics.window_size = (startup_window_size.width, startup_window_size.height);
        self.window = Some(window);

        if self.startup.next_redraw.is_none() {
            event_loop.set_control_flow(ControlFlow::Wait);
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let ev = match event {
            WindowEvent::CloseRequested => {
                send(&mut self.render, RenderEvent::Action(Action::CloseWindow));
                return;
            }

            WindowEvent::Resized(size) => {
                self.metrics.window_size = (size.width, size.height);
                refresh_command_editor_view(self);
                RenderEvent::Resized {
                    width: size.width,
                    height: size.height,
                }
            }

            WindowEvent::RedrawRequested => {
                if let Some(window) = self.window.as_ref().cloned()
                    && present_startup_frame(self, event_loop, &window)
                {
                    return;
                }
                return;
            }

            WindowEvent::Focused(f) => {
                if !f {
                    self.keyboard.physical_modifiers = PhysicalModifierState::default();
                }
                handle_focus_event(
                    &mut self.input,
                    &mut self.render,
                    &self.startup,
                    self.window.as_ref(),
                    f,
                );
                return;
            }

            WindowEvent::KeyboardInput { event, .. } => {
                sync_modifier_key_from_keyboard_event(
                    &mut self.keyboard,
                    event.physical_key,
                    event.state,
                );
                if event.state != ElementState::Pressed {
                    return;
                }
                match &event.logical_key {
                    Key::Character(_) | Key::Named(_) => handle_key_event(
                        self,
                        event.logical_key,
                        event.location,
                        event.physical_key,
                    ),
                    _ => return,
                }
                return;
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                RenderEvent::ScaleFactorChanged { scale_factor }
            }

            WindowEvent::ModifiersChanged(mods) => {
                handle_modifiers_changed(&mut self.input, &mut self.keyboard, mods.state());
                return;
            }

            WindowEvent::CursorMoved { position, .. } => {
                handle_cursor_moved(self, position.x, position.y);
                return;
            }

            WindowEvent::MouseInput { state, button, .. } => {
                handle_mouse_input(self, state == ElementState::Pressed, button);
                return;
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y, pixels) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64, false),
                    MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y, true),
                };
                handle_mouse_wheel(self, x, y, pixels);
                return;
            }

            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => return,
                Ime::Disabled => {
                    self.keyboard.ime_preedit_active = false;
                    update_preedit(self, None);
                    return;
                }
                Ime::Preedit(text, cursor) => {
                    self.keyboard.ime_preedit_active = !text.is_empty();
                    let preedit = (!text.is_empty()).then_some(PreeditState { text, cursor });
                    update_preedit(self, preedit);
                    return;
                }
                Ime::Commit(text) => {
                    handle_ime_commit(
                        &mut self.input,
                        &mut self.render,
                        &self.startup,
                        self.window.as_ref(),
                        &text,
                    );
                    self.keyboard.ime_preedit_active = false;
                    update_preedit(self, None);
                    return;
                }
            },

            _ => return,
        };
        send(&mut self.render, ev);
    }
}

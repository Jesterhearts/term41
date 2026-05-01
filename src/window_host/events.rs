use super::*;

impl ApplicationHandler<AppEvent> for WindowHost {
    fn about_to_wait(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        self.request_due_startup_redraw(event_loop);
        self.run_selection_autoscroll(event_loop);
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
                if self.startup_presenter.is_some()
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
                self.startup_presenter = None;
                self.startup_next_redraw = None;
                if let Some(tx) = self.startup_release_tx.take() {
                    let _ = tx.send(std::mem::take(&mut self.startup_tabs));
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            AppEvent::ApplyTerminalEffects { tab_id, effects } => {
                self.apply_terminal_effects(tab_id, effects);
            }
            AppEvent::RegisterInputEndpoint {
                tab_id,
                terminal,
                writer,
                recorder,
                terminal_thread,
            } => {
                self.input_endpoints.insert(
                    tab_id,
                    InputEndpoint {
                        terminal,
                        writer,
                        recorder,
                        terminal_thread,
                    },
                );
            }
            AppEvent::RemoveInputEndpoint(tab_id) => {
                self.input_endpoints.remove(&tab_id);
                self.startup_tabs.retain(|tab| tab.id != tab_id);
            }
            AppEvent::SetActiveInputTab(tab_id) => {
                self.active_input_tab = tab_id;
                if let Some(tab_id) = tab_id {
                    self.request_window_size_for_tab(tab_id);
                }
            }
            AppEvent::ResolveClipboardRequest {
                tab_id,
                request,
                decision,
            } => {
                self.resolve_clipboard_request(tab_id, request, decision);
            }
            AppEvent::ResolveKittyFileRequest {
                tab_id,
                request,
                decision,
            } => {
                self.resolve_kitty_file_request(tab_id, request, decision);
            }
            AppEvent::DismissRecordingPopup(token) => {
                let dismiss = matches!(
                    self.recording_popup,
                    Some(RecordingPopupState::Completed { token: current }) if current == token
                );
                if dismiss {
                    self.dismiss_recording_popup();
                }
            }
            AppEvent::ShowToast(message) => {
                self.show_toast(message);
            }
            AppEvent::DismissToast(token) => {
                if token + 1 == self.next_toast_token {
                    self.update_toast_view(None);
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

        let pixel_width = INITIAL_COLS * self.cell_width + compute_gutter_width(self.cell_width);
        // One extra cell_height for the tab bar, which is always visible
        // (it doubles as the titlebar for CSD window management).
        let pixel_height = INITIAL_ROWS * self.cell_height + self.cell_height;
        let transparent = self.opacity < 1.0;
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
            .startup_dpi_scale
            .map(|s| s as f64)
            .unwrap_or_else(|| window.scale_factor());
        let startup_background = startup_snapshot_path();
        self.startup_presenter = StartupPresenter::new(
            window.clone(),
            self.startup_fonts.clone(),
            self.startup_font_size,
            self.startup_supersampling,
            scale_factor,
            self.startup_gutter,
            startup_background,
        );
        if self.present_startup_frame(event_loop, &window) {
            window.request_redraw();
        }

        // Opt into IME events. `ImePurpose::Terminal` is a hint some
        // Wayland compositors and Android IMEs use to expose extra keys
        // (arrows, Tab, etc.) that wouldn't normally appear on a text-input
        // OSK; on platforms that don't understand it, it's a no-op.
        window.set_ime_allowed(true);
        window.set_ime_purpose(winit::window::ImePurpose::Terminal);

        if let Some(tx) = self.window_tx.take() {
            let _ = tx.send((window.clone(), event_loop.owned_display_handle()));
        }

        self.window_size = (startup_window_size.width, startup_window_size.height);
        self.window = Some(window);

        if self.startup_next_redraw.is_none() {
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
                self.send(RenderEvent::Action(Action::CloseWindow));
                return;
            }

            WindowEvent::Resized(size) => {
                self.window_size = (size.width, size.height);
                RenderEvent::Resized {
                    width: size.width,
                    height: size.height,
                }
            }

            WindowEvent::RedrawRequested => {
                if let Some(window) = self.window.as_ref().cloned()
                    && self.present_startup_frame(event_loop, &window)
                {
                    return;
                }
                return;
            }

            WindowEvent::Focused(f) => {
                self.handle_focus_event(f);
                return;
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                match &event.logical_key {
                    Key::Character(_) | Key::Named(_) => {
                        self.handle_key_event(event.logical_key, event.location, event.physical_key)
                    }
                    _ => return,
                }
                return;
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                RenderEvent::ScaleFactorChanged { scale_factor }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.handle_modifiers_changed(mods.state());
                return;
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position.x, position.y);
                return;
            }

            WindowEvent::MouseInput { state, button, .. } => {
                self.handle_mouse_input(state == ElementState::Pressed, button);
                return;
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y, pixels) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64, false),
                    MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y, true),
                };
                self.handle_mouse_wheel(x, y, pixels);
                return;
            }

            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => return,
                Ime::Disabled => {
                    self.ime_preedit_active = false;
                    self.update_preedit(None);
                    return;
                }
                Ime::Preedit(text, cursor) => {
                    self.ime_preedit_active = !text.is_empty();
                    let preedit = (!text.is_empty()).then_some(PreeditState { text, cursor });
                    self.update_preedit(preedit);
                    return;
                }
                Ime::Commit(text) => {
                    self.handle_ime_commit(&text);
                    self.ime_preedit_active = false;
                    self.update_preedit(None);
                    return;
                }
            },

            _ => return,
        };
        self.send(ev);
    }
}

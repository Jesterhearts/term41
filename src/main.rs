#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod config;
mod keybindings;
mod output_recording;
mod perf_ctrl_c;
mod renderer;

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
use std::time::Instant;

use clip41::ClipboardKind;
use config::Config;
use font41::FontSystem;
use parking_lot::Mutex;
use pty_pipe41::Pty;
use pty_pipe41::PtyWriter;
use renderer::RenderHost;
use renderer::startup::StartupPresenter;
use renderer::startup::StartupTab;
use terminal41::HostInput;
use terminal41::HostInputEffects;
use terminal41::HostMouse;
use terminal41::MouseButton as TermMouseButton;
use terminal41::MouseEventKind;
use terminal41::MouseModifiers;
use terminal41::Terminal;
use terminal41::TerminalEffects;
use terminal41::TerminalThread;
use terminal41::apply_host_input;
use terminal41::host;
use terminal41::io::clipboard::copy_to_clipboard;
use terminal41::prompt::command_and_output_text_at;
use terminal41::prompt::command_duration_at;
use terminal41::prompt::command_text_at;
use terminal41::prompt::find_prompt_for_screen_row;
use terminal41::prompt::output_text_at;
use terminal41::prompt::select_command_at;
use terminal41::selection::SelectionMode;
use terminal41::selection::close_search;
use terminal41::selection::copy_selection;
use terminal41::selection::extend_selection;
use terminal41::selection::open_search;
use terminal41::selection::search_active;
use terminal41::selection::search_append;
use terminal41::selection::search_backspace;
use terminal41::selection::search_step_next;
use terminal41::selection::search_step_prev;
use terminal41::selection::start_selection;
use terminal41::settings;
use terminal41::view;
use winit::application::ApplicationHandler;
use winit::event::ElementState;
use winit::event::Ime;
use winit::event::MouseButton;
use winit::event::MouseScrollDelta;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::event_loop::EventLoopProxy;
use winit::event_loop::OwnedDisplayHandle;
use winit::keyboard::Key;
use winit::keyboard::KeyLocation;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::keyboard::PhysicalKey;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::Window;
use winit::window::WindowId;

use crate::keybindings::Action;
use crate::keybindings::Keybindings;
use crate::output_recording::RecorderControl;
use crate::output_recording::next_recording_path;
use crate::renderer::PreeditState;
use crate::renderer::RenderEvent;
use crate::renderer::TabContextMenu;
use crate::renderer::background::startup_snapshot_path;
use crate::renderer::compute_gutter_width;
use crate::renderer::ctrl_byte;
use crate::renderer::kitty_encode_ime_commit;
use crate::renderer::kitty_encode_input;
use crate::renderer::legacy_encode_named;
use crate::renderer::legacy_encode_numpad_character;
use crate::renderer::paint::build_tab_bar_layout;

#[macro_use]
extern crate log;

static APP_START_TIME: OnceLock<Instant> = OnceLock::new();
static LOG_TOAST_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();

const INITIAL_COLS: u32 = 80;
const INITIAL_ROWS: u32 = 24;

/// Size of the cueue ring buffer for window→renderer events (in elements).
const EVENT_QUEUE_SIZE: usize = 4096;

/// Stable identifier for a tab. Monotonically increasing; never reused, so
/// background threads that race with a tab close can't accidentally address
/// the wrong session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

impl From<TabId> for u64 {
    fn from(val: TabId) -> Self {
        val.0
    }
}

/// Commands sent from the render thread back to the window thread.
enum AppEvent {
    SetTitle(String),
    RequestUserAttention,
    RequestStartupRedraw,
    ReleaseStartupSurface,
    ApplyTerminalEffects {
        tab_id: TabId,
        effects: TerminalEffects,
    },
    RegisterInputEndpoint {
        tab_id: TabId,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
        recorder: RecorderControl,
    },
    RemoveInputEndpoint(TabId),
    SetActiveInputTab(Option<TabId>),
    DismissRecordingPopup(u64),
    ShowToast(String),
    DismissToast(u64),
}

#[derive(Default)]
struct LogToastVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

struct LogToastLayer;

impl tracing::field::Visit for LogToastVisitor {
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn fmt::Debug,
    ) {
        let value = clean_log_field_value(format!("{value:?}"));
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for LogToastLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        if !matches!(level, tracing::Level::WARN | tracing::Level::ERROR) {
            return;
        }

        let mut visitor = LogToastVisitor::default();
        event.record(&mut visitor);
        let message = log_toast_message(visitor, event.metadata().target());
        enqueue_log_toast(format!("{level}: {message}"));
    }
}

fn clean_log_field_value(value: String) -> String {
    let Some(trimmed) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value;
    };
    trimmed.to_string()
}

fn log_toast_message(
    visitor: LogToastVisitor,
    target: &str,
) -> String {
    if let Some(message) = visitor.message {
        return message;
    }
    if !visitor.fields.is_empty() {
        return visitor.fields.join(" ");
    }
    target.to_string()
}

fn enqueue_log_toast(message: String) {
    if let Some(tx) = LOG_TOAST_TX.get() {
        let _ = tx.send(message);
    }
}

fn install_log_toast_forwarder(proxy: EventLoopProxy<AppEvent>) {
    let (tx, rx) = mpsc::channel();
    if LOG_TOAST_TX.set(tx).is_err() {
        return;
    }

    thread::Builder::new()
        .name("log-toast-forwarder".into())
        .spawn(move || {
            for message in rx {
                let _ = proxy.send_event(AppEvent::ShowToast(message));
            }
        })
        .expect("spawn log toast forwarder");
}

struct Tab {
    id: TabId,
    terminal: Arc<Mutex<Terminal>>,
    pty: Pty,
    window_sync_epoch: u64,
    /// Kept alive for its Drop impl which signals the thread to stop.
    _terminal_thread: TerminalThread,
}

struct InputEndpoint {
    terminal: Arc<Mutex<Terminal>>,
    writer: RefCell<PtyWriter>,
    recorder: RecorderControl,
}

#[derive(Clone)]
struct RecordingPopupView {
    lines: Vec<String>,
}

#[derive(Clone)]
struct ToastView {
    text: String,
}

enum RecordingPopupState {
    PendingStart { path: PathBuf },
    Completed { token: u64 },
}

pub(crate) struct InputState {
    keybindings: Keybindings,
    tab_count: usize,
    tab_order: Vec<TabId>,
    cell_width: u32,
    cell_height: u32,
    gutter_width: u32,
    hovered_tab_bar_button: Option<renderer::TabBarHover>,
    tab_context_menu: Option<TabContextMenu>,
    gutter_popup: Option<renderer::GutterPopup>,
    recording_popup: Option<RecordingPopupView>,
    toast: Option<ToastView>,
    preedit: Option<PreeditState>,
}

struct WindowHost {
    window: Option<Arc<Window>>,
    startup_presenter: Option<StartupPresenter>,
    startup_next_redraw: Option<Instant>,
    startup_release_tx: Option<mpsc::SyncSender<()>>,
    input_endpoints: HashMap<TabId, InputEndpoint>,
    active_input_tab: Option<TabId>,
    input_state: Arc<Mutex<InputState>>,
    event_tx: cueue::Writer<RenderEvent>,
    /// One-shot channel to deliver the window + display handle to the render
    /// thread after `resumed()` creates the window. Taken (set to `None`)
    /// after the first send.
    window_tx: Option<mpsc::SyncSender<(Arc<Window>, OwnedDisplayHandle)>>,
    modifiers: ModifiersState,
    ime_preedit_active: bool,
    mouse_pos: (f64, f64),
    mouse_buttons: MouseButtonState,
    last_motion_cell: Option<(u32, u32)>,
    last_click_time: Option<Instant>,
    last_click_cell: Option<(u32, u32)>,
    click_count: u32,
    left_drag_active: bool,
    window_size: (u32, u32),
    opacity: f32,
    cell_width: u32,
    cell_height: u32,
    startup_fonts: Option<String>,
    startup_font_size: f32,
    startup_supersampling: u32,
    startup_dpi_scale: Option<f32>,
    startup_gutter: bool,
    render_thread_handle: Arc<OnceLock<std::thread::Thread>>,
    event_proxy: EventLoopProxy<AppEvent>,
    recording_popup: Option<RecordingPopupState>,
    next_recording_popup_token: u64,
    next_toast_token: u64,
}

impl WindowHost {
    fn send(
        &mut self,
        ev: RenderEvent,
    ) {
        let _ = self.event_tx.push(ev);
        if let Some(thread) = self.render_thread_handle.get() {
            thread.unpark();
        }
    }

    fn active_input_target(&self) -> Option<&InputEndpoint> {
        let tab_id = self.active_input_tab?;
        self.input_endpoints.get(&tab_id)
    }

    fn startup_tab_titles(&self) -> Vec<(String, bool)> {
        let tab_order = self.input_state.lock().tab_order.clone();
        let mut titles: Vec<(String, bool)> = tab_order
            .iter()
            .filter_map(|tab_id| {
                let target = self.input_endpoints.get(tab_id)?;
                let title = target
                    .terminal
                    .lock()
                    .metadata
                    .current_title
                    .clone()
                    .unwrap_or_else(|| "Shell".to_owned());
                Some((title, Some(*tab_id) == self.active_input_tab))
            })
            .collect();

        if titles.is_empty()
            && let Some(tab_id) = self.active_input_tab
            && let Some(target) = self.input_endpoints.get(&tab_id)
        {
            let title = target
                .terminal
                .lock()
                .metadata
                .current_title
                .clone()
                .unwrap_or_else(|| "Shell".to_owned());
            titles.push((title, true));
        }

        titles
    }

    fn startup_interaction_snapshot(
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

    fn present_startup_frame(
        &mut self,
        event_loop: &ActiveEventLoop,
        window: &Arc<Window>,
    ) -> bool {
        let Some(tab_id) = self.active_input_tab else {
            return false;
        };
        let Some(terminal) = self
            .input_endpoints
            .get(&tab_id)
            .map(|target| target.terminal.clone())
        else {
            return false;
        };

        let tab_titles = self.startup_tab_titles();
        let tabs: Vec<StartupTab<'_>> = tab_titles
            .iter()
            .map(|(label, active)| StartupTab {
                label,
                active: *active,
            })
            .collect();
        let (hovered_button, tab_context_menu, gutter_popup) = self.startup_interaction_snapshot();
        let maximized = window.is_maximized();
        let Some(presenter) = self.startup_presenter.as_mut() else {
            return false;
        };

        let delay = presenter.present(
            window,
            &terminal,
            &tabs,
            hovered_button,
            tab_context_menu.as_ref(),
            gutter_popup.as_ref(),
            maximized,
        );
        self.schedule_startup_redraw(event_loop, delay);
        true
    }

    fn layout_snapshot(&self) -> (u32, u32, u32, usize) {
        let state = self.input_state.lock();
        (
            state.cell_width,
            state.cell_height,
            state.gutter_width,
            state.tab_count,
        )
    }

    fn keybindings(&self) -> Keybindings {
        self.input_state.lock().keybindings.clone()
    }

    fn request_window_grid_size(
        &self,
        cols: u32,
        rows: u32,
    ) {
        let Some(window) = &self.window else {
            return;
        };
        let (_, _, gutter_width, _) = self.layout_snapshot();
        let width = cols
            .saturating_mul(self.cell_width)
            .saturating_add(gutter_width);
        let height = rows
            .saturating_mul(self.cell_height)
            .saturating_add(self.cell_height);
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(width, height));
    }

    fn request_window_size_for_tab(
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

    fn update_preedit(
        &mut self,
        preedit: Option<PreeditState>,
    ) {
        self.input_state.lock().preedit = preedit;
        self.notify_interaction_changed();
    }

    fn update_hovered_tab_bar_button(
        &mut self,
        hovered_button: Option<renderer::TabBarHover>,
    ) {
        self.input_state.lock().hovered_tab_bar_button = hovered_button;
    }

    fn update_tab_context_menu(
        &mut self,
        menu: Option<TabContextMenu>,
    ) {
        self.input_state.lock().tab_context_menu = menu;
    }

    fn update_gutter_popup(
        &mut self,
        popup: Option<renderer::GutterPopup>,
    ) {
        self.input_state.lock().gutter_popup = popup;
    }

    fn notify_interaction_changed(&mut self) {
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

    fn schedule_startup_redraw(
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

    fn request_due_startup_redraw(
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

    fn update_recording_popup_view(
        &mut self,
        popup: Option<RecordingPopupView>,
    ) {
        self.input_state.lock().recording_popup = popup;
        self.notify_interaction_changed();
    }

    fn update_toast_view(
        &mut self,
        toast: Option<ToastView>,
    ) {
        self.input_state.lock().toast = toast;
        self.notify_interaction_changed();
    }

    fn show_toast(
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

    fn show_recording_start_popup(
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

    fn show_recording_completed_popup(
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

    fn show_recording_error_popup(
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

    fn dismiss_recording_popup(&mut self) {
        self.recording_popup = None;
        self.update_recording_popup_view(None);
    }

    fn handle_recording_popup_key(
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

    fn write_host_bytes(
        &self,
        target: &InputEndpoint,
        host_bytes: Vec<u8>,
        reset_viewport: bool,
    ) {
        if host_bytes.is_empty() {
            return;
        }
        let _ = target.writer.borrow_mut().write(&host_bytes);
        if reset_viewport {
            view::reset_viewport(&mut target.terminal.lock().active);
        }
    }

    fn emit_host_input(
        &self,
        target: &InputEndpoint,
        input: HostInput<'_>,
        reset_viewport: bool,
    ) {
        let effects = {
            let mut terminal = target.terminal.lock();
            apply_host_input(&mut terminal, input)
        };
        self.write_host_bytes(target, effects.host_bytes, reset_viewport);
    }

    fn apply_terminal_effects(
        &mut self,
        tab_id: TabId,
        effects: TerminalEffects,
    ) {
        let Some(target) = self.input_endpoints.get(&tab_id) else {
            return;
        };
        let TerminalEffects {
            host_bytes,
            resize_request,
            bell,
        } = effects;
        self.write_host_bytes(target, host_bytes, false);
        if let Some((cols, rows)) = resize_request
            && self.active_input_tab == Some(tab_id)
        {
            self.request_window_grid_size(cols, rows);
        }
        if bell {
            self.send(RenderEvent::Bell(tab_id));
        }
    }

    fn handle_focus_event(
        &mut self,
        focused: bool,
    ) {
        {
            let Some(target) = self.active_input_target() else {
                return;
            };
            self.emit_host_input(target, HostInput::FocusChanged { focused }, true);
        }
        self.notify_interaction_changed();
    }

    fn handle_search_key(
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
    }

    fn run_local_action(
        &mut self,
        action: Action,
        tab_id: TabId,
    ) -> bool {
        let Some(target) = self.input_endpoints.get(&tab_id) else {
            return true;
        };
        match action {
            Action::ScrollPageUp => {
                let mut terminal = target.terminal.lock();
                let rows = terminal.viewport.rows;
                let viewport = terminal.viewport;
                view::scroll_viewport_up(&mut terminal.active, &viewport, rows);
                true
            }
            Action::ScrollPageDown => {
                let mut terminal = target.terminal.lock();
                let rows = terminal.viewport.rows;
                view::scroll_viewport_down(&mut terminal.active, rows);
                true
            }
            Action::Copy => {
                let mut guard = target.terminal.lock();
                let terminal = &mut *guard;
                if terminal.has_selection() {
                    copy_selection(
                        &mut terminal.clipboard,
                        terminal.selection.as_ref(),
                        &terminal.active,
                        ClipboardKind::Clipboard,
                    );
                }
                true
            }
            Action::Paste => {
                self.emit_host_input(
                    target,
                    HostInput::PasteFromClipboard {
                        kind: ClipboardKind::Clipboard,
                    },
                    true,
                );
                true
            }
            Action::OpenSearch => {
                open_search(&mut target.terminal.lock().search);
                true
            }
            Action::ScrollPrevPrompt => {
                let mut terminal = target.terminal.lock();
                let viewport = terminal.viewport;
                view::scroll_to_prev_prompt(&mut terminal.active, &viewport);
                true
            }
            Action::ScrollNextPrompt => {
                let mut terminal = target.terminal.lock();
                let viewport = terminal.viewport;
                view::scroll_to_next_prompt(&mut terminal.active, &viewport);
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
            | Action::ClearPastedBackground => false,
        }
    }

    fn handle_key_event(
        &mut self,
        key: Key,
        location: KeyLocation,
        physical: PhysicalKey,
    ) {
        if self.ime_preedit_active && matches!(key, Key::Character(_)) {
            return;
        }

        let Some(active_tab_id) = self.active_input_tab else {
            return;
        };

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

        if let Some(action) = self.keybindings().lookup(&key, self.modifiers) {
            if self.run_local_action(action, active_tab_id) {
                self.notify_interaction_changed();
            } else {
                self.send(RenderEvent::Action(action));
            }
            return;
        }

        let target = &self.input_endpoints[&active_tab_id];

        let (kitty_flags, c1_mode) = {
            let terminal = target.terminal.lock();
            (terminal.kitty_keyboard.current(), terminal.modes.c1_mode)
        };
        if let Some(bytes) = kitty_encode_input(&key, self.modifiers, kitty_flags, c1_mode) {
            view::reset_viewport(&mut target.terminal.lock().active);
            let _ = target.writer.borrow_mut().write(&bytes);
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
                view::reset_viewport(&mut target.terminal.lock().active);
                if self.modifiers.alt_key() {
                    let _ = target.writer.borrow_mut().write(&[0x1b, byte]);
                } else {
                    let _ = target.writer.borrow_mut().write(&[byte]);
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
            view::reset_viewport(&mut target.terminal.lock().active);
            let _ = target.writer.borrow_mut().write(&bytes);
            self.notify_interaction_changed();
        }
    }

    fn handle_ime_commit(
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
        view::reset_viewport(&mut target.terminal.lock().active);
        let _ = target.writer.borrow_mut().write(&bytes);
        self.notify_interaction_changed();
    }

    fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
    ) {
        self.mouse_pos = (x, y);
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

        let cell = self.cell_at(x, y);
        if self.forward_mouse_to_app() {
            if self.last_motion_cell == Some(cell) {
                return;
            }
            self.last_motion_cell = Some(cell);
            let Some(target) = self.active_input_target() else {
                return;
            };
            self.emit_host_input(
                target,
                HostInput::Mouse(HostMouse {
                    kind: MouseEventKind::Motion,
                    button: self.mouse_buttons.primary_held(),
                    col: cell.0,
                    row: cell.1,
                    mods: self.mouse_modifiers(),
                }),
                true,
            );
            self.notify_interaction_changed();
            return;
        }

        if self.left_drag_active
            && let Some(target) = self.active_input_target()
        {
            {
                let mut guard = target.terminal.lock();
                let terminal = &mut *guard;
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
            }
            self.notify_interaction_changed();
            return;
        }

        self.notify_interaction_changed();
    }

    fn handle_mouse_input(
        &mut self,
        pressed: bool,
        button: MouseButton,
    ) {
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

        if pressed {
            self.last_motion_cell = None;
        }

        if self.forward_mouse_to_app() {
            let (col, row) = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
            let kind = if pressed {
                MouseEventKind::Press
            } else {
                MouseEventKind::Release
            };
            let Some(target) = self.active_input_target() else {
                return;
            };
            self.emit_host_input(
                target,
                HostInput::Mouse(HostMouse {
                    kind,
                    button: term_button,
                    col,
                    row,
                    mods: self.mouse_modifiers(),
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
                }
                self.left_drag_active = true;
                self.notify_interaction_changed();
            }
            (MouseButton::Left, false) => {
                self.left_drag_active = false;
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
                    } else {
                        drop(guard);
                        self.emit_host_input(
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

    fn handle_mouse_wheel(
        &mut self,
        raw_x: f64,
        raw_y: f64,
        pixels: bool,
    ) {
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
            let (col, row) = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
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
                                col,
                                row,
                                mods: self.mouse_modifiers(),
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
                                col,
                                row,
                                mods: self.mouse_modifiers(),
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
                                col,
                                row,
                                mods: self.mouse_modifiers(),
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
                                col,
                                row,
                                mods: self.mouse_modifiers(),
                            }),
                        ));
                    }
                }
                effects
            };
            self.write_host_bytes(target, effects.host_bytes, true);
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
        }
        self.notify_interaction_changed();
    }

    fn execute_tab_menu_action(
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

    fn close_gutter_popup(&mut self) {
        let had_popup = self.input_state.lock().gutter_popup.take().is_some();
        if had_popup && let Some(target) = self.active_input_target() {
            target.terminal.lock().selection = None;
        }
    }

    fn open_gutter_popup(
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

    fn execute_popup_action(
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
                if let Some(cmd) = command_text_at(
                    popup.prompt_abs_row,
                    &terminal.metadata.command_metas,
                    &terminal.active,
                ) {
                    let cmd = cmd.trim().to_owned();
                    terminal.selection = None;
                    drop(guard);
                    let text = format!("{cmd}\r");
                    self.emit_host_input(target, HostInput::PasteText(&text), true);
                }
            }
            1 => {
                let mut guard = target.terminal.lock();
                let terminal = &mut *guard;
                if let Some(text) = command_text_at(
                    popup.prompt_abs_row,
                    &terminal.metadata.command_metas,
                    &terminal.active,
                ) {
                    copy_to_clipboard(&mut terminal.clipboard, text.trim());
                }
                terminal.selection = None;
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
            }
            _ => return,
        }
        self.notify_interaction_changed();
    }

    fn mouse_modifiers(&self) -> MouseModifiers {
        MouseModifiers {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            ctrl: self.modifiers.control_key(),
        }
    }

    fn forward_mouse_to_app(&self) -> bool {
        self.active_input_target().is_some_and(|target| {
            host::mouse_tracking_enabled(target.terminal.lock().modes.mouse_tracking)
                && !self.modifiers.shift_key()
        })
    }

    fn next_click_count(
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

    fn cell_at(
        &self,
        x: f64,
        y: f64,
    ) -> (u32, u32) {
        let (cell_w, cell_h, gutter_w, _) = self.layout_snapshot();
        let raw_x = x.max(0.0) as u32;
        let raw_y = y.max(0.0) as u32;
        let y = raw_y.saturating_sub(cell_h);
        let x = raw_x.saturating_sub(gutter_w);
        let Some(target) = self.active_input_target() else {
            return (0, 0);
        };
        let terminal = target.terminal.lock();
        let cols = terminal.viewport.cols.saturating_sub(1);
        let rows = terminal.viewport.rows.saturating_sub(1);
        ((x / cell_w).min(cols), (y / cell_h).min(rows))
    }

    fn is_in_tab_bar(&self) -> bool {
        let (_, cell_h, _, _) = self.layout_snapshot();
        (self.mouse_pos.1.max(0.0) as u32) < cell_h
    }

    fn window_button_at(&self) -> Option<WindowButton> {
        match self.tab_bar_hover_at() {
            Some(renderer::TabBarHover::Minimize) => Some(WindowButton::Minimize),
            Some(renderer::TabBarHover::Maximize) => Some(WindowButton::Maximize),
            Some(renderer::TabBarHover::Close) => Some(WindowButton::Close),
            _ => None,
        }
    }

    fn tab_at_mouse(&self) -> Option<usize> {
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

    fn is_on_new_tab_button(&self) -> bool {
        matches!(self.tab_bar_hover_at(), Some(renderer::TabBarHover::NewTab))
    }

    fn is_in_titlebar_drag_region(&self) -> bool {
        self.is_in_tab_bar() && self.tab_bar_hover_at().is_none()
    }

    fn tab_bar_hover_at(&self) -> Option<renderer::TabBarHover> {
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

    fn resize_direction_at(&self) -> Option<winit::window::ResizeDirection> {
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

    fn tab_menu_item_at(
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

    fn is_in_gutter(&self) -> bool {
        let (_, _, gutter_w, _) = self.layout_snapshot();
        gutter_w > 0 && (self.mouse_pos.0.max(0.0) as u32) < gutter_w
    }

    fn popup_item_at(
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

#[derive(Clone, Copy)]
enum WindowButton {
    Minimize = 0,
    Maximize = 1,
    Close = 2,
}

#[derive(Clone, Copy)]
enum TabMenuActionLocal {
    NewTab,
    CloseTab,
    CloseOtherTabs,
}

const RESIZE_BORDER: f32 = 5.0;
const TAB_MENU_WIDTH_CELLS: f32 = 16.0;
const POPUP_WIDTH_CELLS: f32 = 20.0;

fn popup_item_at(
    popup: Option<&renderer::GutterPopup>,
    x: f64,
    y: f64,
    cell_width: u32,
    cell_height: u32,
    gutter_width: u32,
    window_height: u32,
) -> Option<usize> {
    let popup = popup?;
    let cell_w = cell_width as f32;
    let cell_h = cell_height as f32;
    let total_rows = popup.duration_text.is_some() as usize + 4;
    let popup_w = cell_w * POPUP_WIDTH_CELLS;
    let popup_h = total_rows as f32 * cell_h;
    let popup_x = gutter_width as f32;
    let popup_y = (popup.screen_row as f32 * cell_h + cell_h).min(window_height as f32 - popup_h);
    let x = x as f32;
    let y = y as f32;
    if x < popup_x || x > popup_x + popup_w || y < popup_y || y > popup_y + popup_h {
        return None;
    }
    let row_in_popup = ((y - popup_y) / cell_h) as usize;
    let header = if popup.duration_text.is_some() { 1 } else { 0 };
    let item_idx = row_in_popup.checked_sub(header)?;
    (item_idx < 4).then_some(item_idx)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{:.0}ms", secs * 1000.0)
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else if secs < 3600.0 {
        let m = (secs / 60.0).floor();
        let s = secs - m * 60.0;
        format!("{m:.0}m {s:.0}s")
    } else {
        let h = (secs / 3600.0).floor();
        let m = ((secs - h * 3600.0) / 60.0).floor();
        format!("{h:.0}h {m:.0}m")
    }
}

/// Maximum time between clicks that still count as part of a sequence.
const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

impl ApplicationHandler<AppEvent> for WindowHost {
    fn about_to_wait(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        self.request_due_startup_redraw(event_loop);
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
                    let _ = tx.send(());
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
            } => {
                self.input_endpoints.insert(
                    tab_id,
                    InputEndpoint {
                        terminal,
                        writer: RefCell::new(writer),
                        recorder,
                    },
                );
            }
            AppEvent::RemoveInputEndpoint(tab_id) => {
                self.input_endpoints.remove(&tab_id);
            }
            AppEvent::SetActiveInputTab(tab_id) => {
                self.active_input_tab = tab_id;
                if let Some(tab_id) = tab_id {
                    self.request_window_size_for_tab(tab_id);
                }
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
                self.modifiers = mods.state();
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

#[derive(Default, Copy, Clone)]
struct MouseButtonState {
    left: bool,
    middle: bool,
    right: bool,
}

impl MouseButtonState {
    fn set(
        &mut self,
        button: MouseButton,
        pressed: bool,
    ) {
        match button {
            MouseButton::Left => self.left = pressed,
            MouseButton::Middle => self.middle = pressed,
            MouseButton::Right => self.right = pressed,
            _ => {}
        }
    }

    fn primary_held(&self) -> TermMouseButton {
        if self.left {
            TermMouseButton::Left
        } else if self.middle {
            TermMouseButton::Middle
        } else if self.right {
            TermMouseButton::Right
        } else {
            TermMouseButton::None
        }
    }
}

fn main() {
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let _ = APP_START_TIME.set(Instant::now());

    let directive = cfg_select! {
        debug_assertions => {
            "term41=debug"
        }
        not(debug_assertions) => {
            "term41=warn"
        }
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(directive.parse().expect("parse log filter directive"))
                .from_env_lossy(),
        )
        .with(tracing_subscriber::fmt::layer().with_span_events(FmtSpan::CLOSE))
        .with(LogToastLayer)
        .init();

    #[cfg(feature = "deadlock-detection")]
    {
        use parking_lot::deadlock;

        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(10));
                let deadlocks = deadlock::check_deadlock();
                if deadlocks.is_empty() {
                    continue;
                }

                error!("{} deadlocks detected", deadlocks.len());
                for (i, threads) in deadlocks.iter().enumerate() {
                    error!("Deadlock #{}", i);
                    for t in threads {
                        error!("Thread Id {:#?}", t.thread_id());
                        error!("{:#?}", t.backtrace());
                    }
                }
            }
        });
    }

    let command = parse_command_args();

    let event_loop: EventLoop<AppEvent> =
        tracing::debug_span!("create_event_loop").in_scope(|| {
            EventLoop::with_user_event()
                .build()
                .expect("create event loop")
        });
    // Channels.
    let (event_tx, event_rx) =
        cueue::cueue::<RenderEvent>(EVENT_QUEUE_SIZE).expect("create event queue");
    let (window_tx, window_rx) = mpsc::sync_channel(1);
    let (startup_release_tx, startup_release_rx) = mpsc::sync_channel(1);
    let (child_exit_tx, child_exit_rx) = mpsc::channel();
    let config_reload = Arc::new(AtomicBool::new(false));
    let render_thread_handle = Arc::new(OnceLock::new());

    let proxy = event_loop.create_proxy();
    install_log_toast_forwarder(proxy.clone());
    let startup_redraw_proxy = proxy.clone();

    let config_path = config::config_file_path();
    let config = tracing::debug_span!("load_config").in_scope(|| match config_path.as_deref() {
        Some(p) => config::load_from(p),
        None => Config::default(),
    });

    let font_system = tracing::debug_span!("init_font_system").in_scope(|| {
        FontSystem::new(
            config.fonts.clone(),
            config.font_size,
            config.font_supersampling,
        )
    });
    let cell_width = font_system.cell_width;
    let cell_height = font_system.cell_height;
    let opacity = config.opacity;
    let startup_fonts = config.fonts.clone();
    let startup_font_size = config.font_size;
    let startup_supersampling = config.font_supersampling;
    let startup_dpi_scale = config.dpi_scale;
    let startup_gutter = config.gutter;
    let startup_keybindings = config.keybindings.clone();

    // Create the terminal thread handle before spawning the PTY so the PTY
    // reader can unpark the terminal thread once it starts.
    let terminal_thread = TerminalThread::new();

    // Spawn the initial PTY early so the shell starts running immediately.
    let initial_status_rows =
        u32::from(config.status_line.display_kind() != terminal41::StatusDisplayKind::None);
    let initial_main_rows = INITIAL_ROWS.saturating_sub(initial_status_rows);
    let (pty, pty_writer, pty_reader) = tracing::debug_span!("spawn_pty").in_scope(|| {
        Pty::spawn(
            TabId(0),
            INITIAL_COLS as u16,
            initial_main_rows as u16,
            cell_width as u16,
            cell_height as u16,
            command,
            None,
            terminal_thread.thread_handle.clone(),
            child_exit_tx.clone(),
        )
        .expect("failed to spawn PTY")
    });
    let initial_recorder = RecorderControl::new();

    let mut terminal = Terminal::new(
        INITIAL_COLS,
        INITIAL_ROWS,
        config.scrollback_lines,
        config.status_line.display_kind(),
        config.feature_permissions.clone(),
        cell_height,
        cell_width,
        config.palette.clone(),
    );
    settings::set_default_cursor_style(&mut terminal.cursor_style, config.cursor_style);
    settings::set_emoji_compatibility_mode(
        &mut terminal.emoji_compatibility_mode,
        config.compatibility.emoji,
    );
    let terminal = Arc::new(Mutex::new(terminal));

    terminal_thread.spawn(
        "terminal-0".into(),
        terminal.clone(),
        pty_reader,
        render_thread_handle.clone(),
        Some(Box::new(move || {
            let _ = startup_redraw_proxy.send_event(AppEvent::RequestStartupRedraw);
        })),
        Box::new({
            let recorder = initial_recorder.clone();
            move |bytes| {
                crate::perf_ctrl_c::observe_pty_output(TabId(0), bytes);
                recorder.write_chunk(bytes);
            }
        }),
        Box::new({
            let proxy = proxy.clone();
            move |effects| {
                let _ = proxy.send_event(AppEvent::ApplyTerminalEffects {
                    tab_id: TabId(0),
                    effects,
                });
            }
        }),
    );

    let input_state = Arc::new(Mutex::new(InputState {
        keybindings: startup_keybindings,
        tab_count: 1,
        tab_order: vec![TabId(0)],
        cell_width,
        cell_height,
        gutter_width: if startup_gutter {
            compute_gutter_width(cell_width)
        } else {
            0
        },
        hovered_tab_bar_button: None,
        tab_context_menu: None,
        gutter_popup: None,
        recording_popup: None,
        toast: None,
        preedit: None,
    }));
    let tab = Tab {
        id: TabId(0),
        terminal: terminal.clone(),
        pty,
        window_sync_epoch: 0,
        _terminal_thread: terminal_thread,
    };

    // Clone config_path before moving it into the render thread closure —
    // the original is still needed by the config watcher below.
    let config_path_for_watcher = config_path.clone();

    // Spawn the render thread.
    let config_reload_ = config_reload.clone();
    let input_state_for_render = input_state.clone();
    let render_thread_handle_for_render = render_thread_handle.clone();
    let render_proxy = proxy.clone();
    thread::Builder::new()
        .name("renderer".into())
        .spawn(move || {
            render_thread_handle_for_render
                .set(thread::current())
                .expect("set render thread handle");
            let mut host = RenderHost::new(
                event_rx,
                child_exit_rx,
                child_exit_tx,
                config_reload_,
                render_proxy,
                font_system,
                tab,
                config,
                config_path,
                input_state_for_render,
                render_thread_handle_for_render.clone(),
            );
            host.run(window_rx, startup_release_rx);
        })
        .expect("spawn render thread");

    // Spawn the config file watcher.
    if let Some(ref path) = config_path_for_watcher {
        spawn_config_watcher(path.clone(), config_reload, render_thread_handle.clone());
    }

    let mut host = WindowHost {
        window: None,
        startup_presenter: None,
        startup_next_redraw: None,
        startup_release_tx: Some(startup_release_tx),
        input_endpoints: HashMap::from([(
            TabId(0),
            InputEndpoint {
                terminal: terminal.clone(),
                writer: RefCell::new(pty_writer),
                recorder: initial_recorder,
            },
        )]),
        active_input_tab: Some(TabId(0)),
        input_state,
        event_tx,
        window_tx: Some(window_tx),
        modifiers: ModifiersState::default(),
        ime_preedit_active: false,
        mouse_pos: (0.0, 0.0),
        mouse_buttons: MouseButtonState::default(),
        last_motion_cell: None,
        last_click_time: None,
        last_click_cell: None,
        click_count: 0,
        left_drag_active: false,
        window_size: (0, 0),
        opacity,
        cell_width,
        cell_height,
        startup_fonts,
        startup_font_size,
        startup_supersampling,
        startup_dpi_scale,
        startup_gutter,
        render_thread_handle,
        event_proxy: proxy,
        recording_popup: None,
        next_recording_popup_token: 1,
        next_toast_token: 1,
    };
    event_loop.run_app(&mut host).expect("run event loop");
}

fn spawn_config_watcher(
    config_path: PathBuf,
    config_reload: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<std::thread::Thread>>,
) {
    use notify::EventKind;
    use notify::RecursiveMode;
    use notify::Watcher;

    let Some(dir) = config_path.parent().map(PathBuf::from) else {
        return;
    };

    std::thread::Builder::new()
        .name("config-watcher".into())
        .spawn(move || {
            let target = config_path.clone();
            let config_reload_for_handler = config_reload.clone();
            let mut watcher = match notify::recommended_watcher(move |res| {
                let event: notify::Event = match res {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("config watcher error: {e}");
                        return;
                    }
                };
                let touches_config = event.paths.iter().any(|p| p == &target);
                if !touches_config {
                    return;
                }
                if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    return;
                }
                config_reload_for_handler.store(true, Ordering::Release);
                if let Some(thread) = render_thread_handle.get() {
                    thread.unpark();
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    warn!("failed to create config watcher: {e}");
                    return;
                }
            };

            if let Err(e) = watcher.watch(&dir, RecursiveMode::NonRecursive) {
                warn!("failed to watch config dir {}: {e}", dir.display());
                return;
            }
            std::thread::park();
        })
        .expect("spawn config watcher");
}

fn spawn_new_window(cwd: Option<PathBuf>) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!("open-new-window: cannot locate term41 binary: {e}");
            return;
        }
    };

    let mut cmd = std::process::Command::new(&exe);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::Builder::new()
                .name("new-window-waiter".into())
                .spawn(move || {
                    let _ = child.wait();
                })
                .ok();
        }
        Err(e) => {
            warn!("open-new-window: spawn failed: {e}");
        }
    }
}

fn parse_command_args() -> Option<Vec<String>> {
    let mut args = std::env::args();
    let _argv0 = args.next();
    let mut rest: Vec<String> = args.collect();
    if rest.first().map(String::as_str) == Some("--") {
        rest.remove(0);
    }
    if rest.is_empty() { None } else { Some(rest) }
}

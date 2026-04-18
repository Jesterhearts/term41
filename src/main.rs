#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod config;
mod keybindings;
mod renderer;

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use clip41::ClipboardKind;
use config::Config;
use font41::FontSystem;
use pty_pipe41::Pty;
use pty_pipe41::PtyWriter;
use renderer::RenderHost;
use renderer::startup::StartupPresenter;
use terminal41::MouseButton as TermMouseButton;
use terminal41::MouseEventKind;
use terminal41::MouseModifiers;
use terminal41::Terminal;
use terminal41::TerminalThread;
use terminal41::selection::SelectionMode;
use winit::application::ApplicationHandler;
use winit::event::ElementState;
use winit::event::Ime;
use winit::event::MouseButton;
use winit::event::MouseScrollDelta;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::event_loop::OwnedDisplayHandle;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::Window;
use winit::window::WindowId;

use crate::keybindings::Action;
use crate::keybindings::Keybindings;
use crate::renderer::PreeditState;
use crate::renderer::RenderEvent;
use crate::renderer::TabContextMenu;
use crate::renderer::compute_gutter_width;
use crate::renderer::ctrl_byte;
use crate::renderer::kitty_encode_ime_commit;
use crate::renderer::kitty_encode_input;
use crate::renderer::legacy_encode_named;

#[macro_use]
extern crate log;

static APP_START_TIME: OnceLock<Instant> = OnceLock::new();

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
    RegisterInputEndpoint {
        tab_id: TabId,
        terminal: Arc<Mutex<Terminal>>,
        writer: PtyWriter,
    },
    RemoveInputEndpoint(TabId),
    SetActiveInputTab(Option<TabId>),
}

struct Tab {
    id: TabId,
    terminal: Arc<Mutex<Terminal>>,
    pty: Pty,
    /// Kept alive for its Drop impl which signals the thread to stop.
    _terminal_thread: TerminalThread,
}

struct InputEndpoint {
    terminal: Arc<Mutex<Terminal>>,
    writer: RefCell<PtyWriter>,
}

pub(crate) struct InputState {
    keybindings: Keybindings,
    tab_count: usize,
    cell_width: u32,
    cell_height: u32,
    gutter_width: u32,
    hovered_button: Option<u8>,
    tab_context_menu: Option<TabContextMenu>,
    gutter_popup: Option<renderer::GutterPopup>,
    preedit: Option<PreeditState>,
}

struct WindowHost {
    window: Option<Arc<Window>>,
    startup_presenter: Option<StartupPresenter>,
    startup_release_tx: Option<mpsc::SyncSender<()>>,
    input_endpoints: HashMap<TabId, InputEndpoint>,
    active_input_tab: Option<TabId>,
    input_state: Arc<Mutex<InputState>>,
    event_tx: cueue::Writer<RenderEvent>,
    /// One-shot channel to deliver the window + display handle to the render
    /// thread after `resumed()` creates the window. Taken (set to `None`)
    /// after the first send.
    window_tx: Option<mpsc::SyncSender<(Arc<Window>, OwnedDisplayHandle)>>,
    render_thread: Thread,
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
    startup_supersampling: i32,
    startup_dpi_scale: Option<f32>,
    startup_gutter: bool,
}

impl WindowHost {
    fn send(
        &mut self,
        ev: RenderEvent,
    ) {
        let _ = self.event_tx.push(ev);
        self.render_thread.unpark();
    }

    fn active_input_target(&self) -> Option<&InputEndpoint> {
        let tab_id = self.active_input_tab?;
        self.input_endpoints.get(&tab_id)
    }

    fn layout_snapshot(&self) -> (u32, u32, u32, usize) {
        let state = self.input_state.lock().unwrap();
        (
            state.cell_width,
            state.cell_height,
            state.gutter_width,
            state.tab_count,
        )
    }

    fn keybindings(&self) -> Keybindings {
        self.input_state.lock().unwrap().keybindings.clone()
    }

    fn update_preedit(
        &mut self,
        preedit: Option<PreeditState>,
    ) {
        self.input_state.lock().unwrap().preedit = preedit;
        self.notify_interaction_changed();
    }

    fn update_hovered_button(
        &mut self,
        hovered_button: Option<u8>,
    ) {
        self.input_state.lock().unwrap().hovered_button = hovered_button;
    }

    fn update_tab_context_menu(
        &mut self,
        menu: Option<TabContextMenu>,
    ) {
        self.input_state.lock().unwrap().tab_context_menu = menu;
    }

    fn update_gutter_popup(
        &mut self,
        popup: Option<renderer::GutterPopup>,
    ) {
        self.input_state.lock().unwrap().gutter_popup = popup;
    }

    fn notify_interaction_changed(&self) {
        self.render_thread.unpark();
        if self.startup_presenter.is_some()
            && let Some(window) = &self.window
        {
            window.request_redraw();
        }
    }

    fn flush_target_output(
        &self,
        target: &InputEndpoint,
    ) {
        let pending = target.terminal.lock().unwrap().take_pending_output();
        if pending.is_empty() {
            return;
        }
        let _ = target.writer.borrow_mut().write(&pending);
        target.terminal.lock().unwrap().reset_viewport();
    }

    fn handle_focus_event(
        &mut self,
        focused: bool,
    ) {
        let Some(target) = self.active_input_target() else {
            return;
        };
        target.terminal.lock().unwrap().report_focus_change(focused);
        self.flush_target_output(target);
        self.notify_interaction_changed();
    }

    fn handle_search_key(
        &self,
        target: &InputEndpoint,
        key: &Key,
    ) {
        let shift = self.modifiers.shift_key();
        let mut terminal = target.terminal.lock().unwrap();
        match key {
            Key::Named(NamedKey::Escape) => terminal.close_search(),
            Key::Named(NamedKey::Backspace) => terminal.search_backspace(),
            Key::Named(NamedKey::Enter) => {
                if shift {
                    terminal.search_prev();
                } else {
                    terminal.search_next();
                }
            }
            Key::Named(NamedKey::Space) => terminal.search_append(" "),
            Key::Character(s) => terminal.search_append(s),
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
                let mut terminal = target.terminal.lock().unwrap();
                let rows = terminal.viewport.rows;
                terminal.scroll_viewport_up(rows);
                true
            }
            Action::ScrollPageDown => {
                let mut terminal = target.terminal.lock().unwrap();
                let rows = terminal.viewport.rows;
                terminal.scroll_viewport_down(rows);
                true
            }
            Action::Copy => {
                let mut terminal = target.terminal.lock().unwrap();
                if terminal.has_selection() {
                    terminal.copy_selection(clip41::ClipboardKind::Clipboard);
                }
                true
            }
            Action::Paste => {
                target
                    .terminal
                    .lock()
                    .unwrap()
                    .paste_from_clipboard(clip41::ClipboardKind::Clipboard);
                self.flush_target_output(target);
                true
            }
            Action::OpenSearch => {
                target.terminal.lock().unwrap().open_search();
                true
            }
            Action::ScrollPrevPrompt => {
                target.terminal.lock().unwrap().scroll_to_prev_prompt();
                true
            }
            Action::ScrollNextPrompt => {
                target.terminal.lock().unwrap().scroll_to_next_prompt();
                true
            }
            Action::OpenNewWindow => {
                let cwd = target.terminal.lock().unwrap().current_directory.clone();
                spawn_new_window(cwd);
                true
            }
            Action::NewTab
            | Action::CloseTab
            | Action::NextTab
            | Action::PrevTab
            | Action::PasteAsBackground
            | Action::ClearPastedBackground => false,
        }
    }

    fn handle_key_event(
        &mut self,
        key: Key,
    ) {
        if self.ime_preedit_active && matches!(key, Key::Character(_)) {
            return;
        }

        let Some(active_tab_id) = self.active_input_tab else {
            return;
        };

        if self.input_endpoints[&active_tab_id]
            .terminal
            .lock()
            .unwrap()
            .search_active()
        {
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
            let terminal = target.terminal.lock().unwrap();
            (terminal.kitty_keyboard.current(), terminal.modes.c1_mode)
        };
        if let Some(bytes) = kitty_encode_input(&key, self.modifiers, kitty_flags, c1_mode) {
            target.terminal.lock().unwrap().reset_viewport();
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
                target.terminal.lock().unwrap().reset_viewport();
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
            let terminal = target.terminal.lock().unwrap();
            (
                terminal.active.app_cursor_keys,
                terminal.active.app_keypad,
                terminal.modes.c1_mode,
            )
        };

        let bytes = match &key {
            Key::Character(c) => {
                if self.modifiers.alt_key() {
                    let mut v = vec![0x1b];
                    v.extend_from_slice(c.as_bytes());
                    Some(v)
                } else {
                    Some(c.as_bytes().to_vec())
                }
            }
            Key::Named(named) => {
                legacy_encode_named(*named, self.modifiers, app_cursor_keys, app_keypad, c1_mode)
            }
            _ => None,
        };

        if let Some(bytes) = bytes {
            target.terminal.lock().unwrap().reset_viewport();
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
            let terminal = target.terminal.lock().unwrap();
            (terminal.kitty_keyboard.current(), terminal.modes.c1_mode)
        };
        let bytes = if flags.contains(terminal41::KittyFlags::REPORT_ASSOCIATED_TEXT) {
            kitty_encode_ime_commit(text, c1_mode)
        } else {
            text.as_bytes().to_vec()
        };
        target.terminal.lock().unwrap().reset_viewport();
        let _ = target.writer.borrow_mut().write(&bytes);
        self.notify_interaction_changed();
    }

    fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
    ) {
        self.mouse_pos = (x, y);

        let hovered_button = self.window_button_at().map(|b| b as u8);
        self.update_hovered_button(hovered_button);

        let hovered_menu_item = self.tab_menu_item_at(x, y).map(|(_, idx)| idx);
        {
            let mut state = self.input_state.lock().unwrap();
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
            target.terminal.lock().unwrap().mouse_report(
                MouseEventKind::Motion,
                self.mouse_buttons.primary_held(),
                cell.0,
                cell.1,
                self.mouse_modifiers(),
            );
            self.flush_target_output(target);
            self.notify_interaction_changed();
            return;
        }

        if self.left_drag_active
            && let Some(target) = self.active_input_target()
        {
            target
                .terminal
                .lock()
                .unwrap()
                .extend_selection(cell.0, cell.1);
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
                WindowButton::Close => std::process::exit(0),
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

        if pressed && button == MouseButton::Right && self.is_in_tab_bar() {
            let has_menu = self.input_state.lock().unwrap().tab_context_menu.is_some();
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
                self.update_tab_context_menu(Some(TabContextMenu {
                    x: self.mouse_pos.0 as f32,
                    hovered_item: None,
                }));
            }
            self.notify_interaction_changed();
            return;
        }

        if pressed
            && button == MouseButton::Left
            && self.input_state.lock().unwrap().tab_context_menu.is_some()
        {
            if let Some((action, _)) = self.tab_menu_item_at(self.mouse_pos.0, self.mouse_pos.1) {
                self.execute_tab_menu_action(action);
            }
            self.update_tab_context_menu(None);
            self.notify_interaction_changed();
            return;
        }

        if pressed
            && button == MouseButton::Left
            && self.input_state.lock().unwrap().gutter_popup.is_some()
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
            target.terminal.lock().unwrap().mouse_report(
                kind,
                term_button,
                col,
                row,
                self.mouse_modifiers(),
            );
            self.flush_target_output(target);
            self.notify_interaction_changed();
            return;
        }

        let (col, row) = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
        match (button, pressed) {
            (MouseButton::Left, true) => {
                if self.modifiers.control_key()
                    && let Some(target) = self.active_input_target()
                {
                    let url = target
                        .terminal
                        .lock()
                        .unwrap()
                        .hyperlink_at(row, col)
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
                    target
                        .terminal
                        .lock()
                        .unwrap()
                        .start_selection(col, row, mode);
                }
                self.left_drag_active = true;
                self.notify_interaction_changed();
            }
            (MouseButton::Left, false) => {
                self.left_drag_active = false;
                if let Some(target) = self.active_input_target() {
                    let mut terminal = target.terminal.lock().unwrap();
                    if terminal.has_selection() {
                        terminal.copy_selection(ClipboardKind::Primary);
                    } else {
                        terminal.clear_selection();
                    }
                }
                self.notify_interaction_changed();
            }
            (MouseButton::Right, true) => {
                if let Some(target) = self.active_input_target() {
                    let mut terminal = target.terminal.lock().unwrap();
                    if terminal.has_selection() {
                        terminal.copy_selection(ClipboardKind::Clipboard);
                        terminal.clear_selection();
                    } else {
                        terminal.reset_viewport();
                        terminal.paste_from_clipboard(ClipboardKind::Clipboard);
                    }
                    drop(terminal);
                    self.flush_target_output(target);
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
            let mut terminal = target.terminal.lock().unwrap();
            if y_lines < 0 {
                for _ in 0..y_lines.unsigned_abs() {
                    terminal.mouse_report(
                        MouseEventKind::Press,
                        TermMouseButton::WheelUp,
                        col,
                        row,
                        self.mouse_modifiers(),
                    );
                }
            } else if y_lines > 0 {
                for _ in 0..y_lines as u32 {
                    terminal.mouse_report(
                        MouseEventKind::Press,
                        TermMouseButton::WheelDown,
                        col,
                        row,
                        self.mouse_modifiers(),
                    );
                }
            }
            if x_lines < 0 {
                for _ in 0..x_lines.unsigned_abs() {
                    terminal.mouse_report(
                        MouseEventKind::Press,
                        TermMouseButton::WheelLeft,
                        col,
                        row,
                        self.mouse_modifiers(),
                    );
                }
            } else if x_lines > 0 {
                for _ in 0..x_lines as u32 {
                    terminal.mouse_report(
                        MouseEventKind::Press,
                        TermMouseButton::WheelRight,
                        col,
                        row,
                        self.mouse_modifiers(),
                    );
                }
            }
            drop(terminal);
            self.flush_target_output(target);
            self.notify_interaction_changed();
            return;
        }

        if let Some(target) = self.active_input_target() {
            let mut terminal = target.terminal.lock().unwrap();
            if y_lines < 0 {
                terminal.scroll_viewport_up(y_lines.unsigned_abs());
            } else if y_lines > 0 {
                terminal.scroll_viewport_down(y_lines as u32);
            }
        }
        self.notify_interaction_changed();
    }

    fn execute_tab_menu_action(
        &mut self,
        action: TabMenuActionLocal,
    ) {
        match action {
            TabMenuActionLocal::NewTab => self.send(RenderEvent::Action(Action::NewTab)),
            TabMenuActionLocal::CloseTab => self.send(RenderEvent::Action(Action::CloseTab)),
            TabMenuActionLocal::CloseOtherTabs => self.send(RenderEvent::CloseOtherTabs),
        }
    }

    fn close_gutter_popup(&mut self) {
        let had_popup = self
            .input_state
            .lock()
            .unwrap()
            .gutter_popup
            .take()
            .is_some();
        if had_popup && let Some(target) = self.active_input_target() {
            target.terminal.lock().unwrap().clear_selection();
        }
    }

    fn open_gutter_popup(
        &mut self,
        screen_row: u32,
    ) {
        let Some(target) = self.active_input_target() else {
            return;
        };
        let mut terminal = target.terminal.lock().unwrap();
        let Some(prompt_abs) = terminal.find_prompt_for_screen_row(screen_row) else {
            return;
        };
        terminal.select_command_at(prompt_abs);
        let duration_text = terminal
            .command_duration_at(prompt_abs)
            .map(format_duration);
        drop(terminal);
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
        let popup = self.input_state.lock().unwrap().gutter_popup.take();
        let Some(popup) = popup else {
            return;
        };
        let Some(target) = self.active_input_target() else {
            return;
        };
        match item_idx {
            0 => {
                let mut terminal = target.terminal.lock().unwrap();
                if let Some(cmd) = terminal.command_text_at(popup.prompt_abs_row) {
                    let cmd = cmd.trim().to_owned();
                    terminal.clear_selection();
                    terminal.reset_viewport();
                    terminal.paste(&format!("{cmd}\r"));
                }
                drop(terminal);
                self.flush_target_output(target);
            }
            1 => {
                let mut terminal = target.terminal.lock().unwrap();
                if let Some(text) = terminal.command_text_at(popup.prompt_abs_row) {
                    terminal.copy_to_clipboard(text.trim());
                }
                terminal.clear_selection();
            }
            2 => {
                let mut terminal = target.terminal.lock().unwrap();
                if let Some(text) = terminal.command_and_output_text_at(popup.prompt_abs_row) {
                    terminal.copy_to_clipboard(&text);
                }
                terminal.clear_selection();
            }
            3 => {
                let mut terminal = target.terminal.lock().unwrap();
                if let Some(text) = terminal.output_text_at(popup.prompt_abs_row) {
                    terminal.copy_to_clipboard(&text);
                }
                terminal.clear_selection();
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
            target.terminal.lock().unwrap().mouse_tracking_enabled() && !self.modifiers.shift_key()
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
        let terminal = target.terminal.lock().unwrap();
        let cols = terminal.viewport.cols.saturating_sub(1);
        let rows = terminal.viewport.rows.saturating_sub(1);
        ((x / cell_w).min(cols), (y / cell_h).min(rows))
    }

    fn is_in_tab_bar(&self) -> bool {
        let (_, cell_h, _, _) = self.layout_snapshot();
        (self.mouse_pos.1.max(0.0) as u32) < cell_h
    }

    fn window_button_at(&self) -> Option<WindowButton> {
        if !self.is_in_tab_bar() {
            return None;
        }
        let (cell_w, _, _, _) = self.layout_snapshot();
        let cell_w = cell_w as f32;
        let surface_w = self.window_size.0 as f32;
        let region_w = cell_w * BUTTONS_REGION_CELLS;
        let buttons_x = surface_w - region_w;
        let mx = self.mouse_pos.0 as f32;
        if mx < buttons_x {
            return None;
        }
        let btn_w = cell_w * BUTTON_CELLS;
        let idx = ((mx - buttons_x) / btn_w) as usize;
        match idx {
            0 => Some(WindowButton::Minimize),
            1 => Some(WindowButton::Maximize),
            2 => Some(WindowButton::Close),
            _ => None,
        }
    }

    fn tab_at_mouse(&self) -> Option<usize> {
        let (cell_w, _, _, tab_count) = self.layout_snapshot();
        if tab_count == 0 {
            return None;
        }
        let cell_w = cell_w as f32;
        let surface_w = self.window_size.0 as f32;
        let region_w = cell_w * BUTTONS_REGION_CELLS;
        let tabs_w = surface_w - region_w;
        let max_tab_w = cell_w * 30.0;
        let tab_w = (tabs_w / tab_count as f32).min(max_tab_w);
        let mx = self.mouse_pos.0.max(0.0) as f32;
        if mx >= tabs_w {
            return None;
        }
        let idx = (mx / tab_w) as usize;
        (idx < tab_count).then_some(idx)
    }

    fn is_in_titlebar_drag_region(&self) -> bool {
        self.is_in_tab_bar() && self.window_button_at().is_none()
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
    ) -> Option<(TabMenuActionLocal, usize)> {
        let state = self.input_state.lock().unwrap();
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
        Some((action, idx))
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
        let state = self.input_state.lock().unwrap();
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
const BUTTON_CELLS: f32 = 3.0;
const BUTTONS_REGION_CELLS: f32 = BUTTON_CELLS * 3.0;
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
                if let Some(tx) = self.startup_release_tx.take() {
                    let _ = tx.send(());
                }
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            AppEvent::RegisterInputEndpoint {
                tab_id,
                terminal,
                writer,
            } => {
                self.input_endpoints.insert(
                    tab_id,
                    InputEndpoint {
                        terminal,
                        writer: RefCell::new(writer),
                    },
                );
            }
            AppEvent::RemoveInputEndpoint(tab_id) => {
                self.input_endpoints.remove(&tab_id);
            }
            AppEvent::SetActiveInputTab(tab_id) => {
                self.active_input_tab = tab_id;
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

        let scale_factor = self
            .startup_dpi_scale
            .map(|s| s as f64)
            .unwrap_or_else(|| window.scale_factor());
        self.startup_presenter = StartupPresenter::new(
            window.clone(),
            self.startup_fonts.clone(),
            self.startup_font_size,
            self.startup_supersampling,
            scale_factor,
            self.startup_gutter,
        );
        if let Some(tab_id) = self.active_input_tab
            && let Some(target) = self.input_endpoints.get(&tab_id)
            && let Some(presenter) = self.startup_presenter.as_mut()
        {
            presenter.present(&window, target);
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

        let size = window.inner_size();
        self.window_size = (size.width, size.height);
        self.window = Some(window);
        self.render_thread.unpark();

        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let ev = match event {
            WindowEvent::CloseRequested => {
                std::process::exit(0);
            }

            WindowEvent::Resized(size) => {
                self.window_size = (size.width, size.height);
                RenderEvent::Resized {
                    width: size.width,
                    height: size.height,
                }
            }

            WindowEvent::RedrawRequested => {
                if let Some(tab_id) = self.active_input_tab
                    && let Some(window) = self.window.as_ref()
                    && let Some(target) = self.input_endpoints.get(&tab_id)
                    && let Some(presenter) = self.startup_presenter.as_mut()
                {
                    presenter.present(window, target);
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
                    Key::Character(_) | Key::Named(_) => self.handle_key_event(event.logical_key),
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

    let _ = APP_START_TIME.set(Instant::now());

    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_span_events(FmtSpan::CLOSE)
        .init();

    let command = parse_command_args();

    let config_path = config::config_file_path();
    let config = tracing::debug_span!("load_config").in_scope(|| match config_path.as_deref() {
        Some(p) => config::load_from(p),
        None => Config::default(),
    });

    let event_loop: EventLoop<AppEvent> =
        tracing::debug_span!("create_event_loop").in_scope(|| {
            EventLoop::with_user_event()
                .build()
                .expect("create event loop")
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

    // Channels.
    let (event_tx, event_rx) =
        cueue::cueue::<RenderEvent>(EVENT_QUEUE_SIZE).expect("create event queue");
    let (window_tx, window_rx) = mpsc::sync_channel(1);
    let (startup_release_tx, startup_release_rx) = mpsc::sync_channel(1);
    let (child_exit_tx, child_exit_rx) = mpsc::channel();
    let config_reload = Arc::new(AtomicBool::new(false));
    let render_thread_handle: Arc<OnceLock<Thread>> = Arc::new(OnceLock::new());

    let proxy = event_loop.create_proxy();
    let startup_redraw_proxy = proxy.clone();

    // Create the terminal thread handle before spawning the PTY so the PTY
    // reader can unpark the terminal thread once it starts.
    let terminal_thread = TerminalThread::new();

    // Spawn the initial PTY early so the shell starts running immediately.
    let (pty, pty_writer, pty_reader) = tracing::debug_span!("spawn_pty").in_scope(|| {
        Pty::spawn(
            TabId(0),
            INITIAL_COLS as u16,
            INITIAL_ROWS as u16,
            cell_width as u16,
            cell_height as u16,
            command,
            None,
            terminal_thread.thread_handle.clone(),
            render_thread_handle.clone(),
            child_exit_tx.clone(),
        )
        .expect("failed to spawn PTY")
    });

    let mut terminal = Terminal::new(
        INITIAL_COLS,
        INITIAL_ROWS,
        config.scrollback_lines,
        cell_height,
        cell_width,
        config.palette.clone(),
    );
    terminal.set_default_cursor_style(config.cursor_style);
    let terminal = Arc::new(Mutex::new(terminal));

    terminal_thread.spawn(
        "terminal-0".into(),
        terminal.clone(),
        pty_reader,
        render_thread_handle.clone(),
        Some(Arc::new(move || {
            let _ = startup_redraw_proxy.send_event(AppEvent::RequestStartupRedraw);
        })),
    );

    let input_state = Arc::new(Mutex::new(InputState {
        keybindings: startup_keybindings,
        tab_count: 1,
        cell_width,
        cell_height,
        gutter_width: if startup_gutter {
            compute_gutter_width(cell_width)
        } else {
            0
        },
        hovered_button: None,
        tab_context_menu: None,
        gutter_popup: None,
        preedit: None,
    }));
    let tab = Tab {
        id: TabId(0),
        terminal: terminal.clone(),
        pty,
        _terminal_thread: terminal_thread,
    };

    // Clone config_path before moving it into the render thread closure —
    // the original is still needed by the config watcher below.
    let config_path_for_watcher = config_path.clone();

    let render_thread_handle_ = render_thread_handle.clone();
    // Spawn the render thread.
    let config_reload_ = config_reload.clone();
    let input_state_for_render = input_state.clone();
    thread::Builder::new()
        .name("renderer".into())
        .spawn(move || {
            render_thread_handle_
                .set(thread::current())
                .expect("set render thread handle");
            let mut host = RenderHost::new(
                event_rx,
                child_exit_rx,
                child_exit_tx,
                config_reload_,
                render_thread_handle_,
                proxy,
                font_system,
                tab,
                config,
                config_path,
                input_state_for_render,
            );
            host.run(window_rx, startup_release_rx);
        })
        .expect("spawn render thread");

    // Get the render thread's Thread handle for unparking.
    // The render thread sets it immediately, but we spin briefly just in case.
    let render_thread = render_thread_handle.wait().clone();

    // Spawn the config file watcher.
    if let Some(ref path) = config_path_for_watcher {
        spawn_config_watcher(path.clone(), config_reload, render_thread.clone());
    }

    let mut host = WindowHost {
        window: None,
        startup_presenter: None,
        startup_release_tx: Some(startup_release_tx),
        input_endpoints: HashMap::from([(
            TabId(0),
            InputEndpoint {
                terminal: terminal.clone(),
                writer: RefCell::new(pty_writer),
            },
        )]),
        active_input_tab: Some(TabId(0)),
        input_state,
        event_tx,
        window_tx: Some(window_tx),
        render_thread,
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
    };
    event_loop.run_app(&mut host).expect("run event loop");
}

fn spawn_config_watcher(
    config_path: PathBuf,
    config_reload: Arc<AtomicBool>,
    render_thread: Thread,
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
            let render_thread_for_handler = render_thread.clone();
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
                render_thread_for_handler.unpark();
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

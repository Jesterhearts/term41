pub mod glyph_atlas;
pub mod image_atlas;
mod r#impl;
mod shelf;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use winit::event::MouseButton;
use winit::event_loop::EventLoopProxy;
use winit::event_loop::OwnedDisplayHandle;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::window::Window;

use crate::AppEvent;
use crate::INITIAL_COLS;
use crate::INITIAL_ROWS;
use crate::MULTI_CLICK_WINDOW;
use crate::MouseButtonState;
use crate::Tab;
use crate::TabId;
use crate::clipboard::ClipboardKind;
use crate::config::BellMode;
use crate::config::Config;
use crate::config::DEFAULT_SCROLLBACK;
use crate::font::FontSystem;
use crate::keybindings::Action;
use crate::pty::MAX_READ_CHUNK;
use crate::pty::Pty;
use crate::renderer::r#impl::Renderer;
use crate::renderer::r#impl::TabInfo;
use crate::selection::SelectionMode;
use crate::terminal::KittyFlags;
use crate::terminal::KittyKeys;
use crate::terminal::MouseButton as TermMouseButton;
use crate::terminal::MouseEventKind;
use crate::terminal::MouseModifiers;
use crate::terminal::Terminal;

// ---------------------------------------------------------------------------
// Gutter popup — shown on click of a shell-integration gutter marker
// ---------------------------------------------------------------------------

/// Action the user can pick from the gutter popup menu.
#[derive(Clone, Copy)]
enum GutterMenuAction {
    Rerun,
    CopyCommand,
    CopyCommandAndOutput,
    CopyOutput,
}

/// A single item in the popup menu.
struct GutterMenuItem {
    label: &'static str,
    action: GutterMenuAction,
}

const GUTTER_MENU_ITEMS: &[GutterMenuItem] = &[
    GutterMenuItem {
        label: "Rerun",
        action: GutterMenuAction::Rerun,
    },
    GutterMenuItem {
        label: "Copy command",
        action: GutterMenuAction::CopyCommand,
    },
    GutterMenuItem {
        label: "Copy cmd+output",
        action: GutterMenuAction::CopyCommandAndOutput,
    },
    GutterMenuItem {
        label: "Copy output",
        action: GutterMenuAction::CopyOutput,
    },
];

/// Width of the popup in cell units.
const POPUP_WIDTH_CELLS: f32 = 20.0;

/// State of the gutter popup while it is open.
pub(crate) struct GutterPopup {
    /// Absolute row of the prompt whose marker was clicked.
    pub prompt_abs_row: u64,
    /// Screen row (viewport-relative) where the marker sits.
    pub screen_row: u32,
    /// Duration formatted as a human-readable string, if available.
    pub duration_text: Option<String>,
    /// Currently hovered menu-item index (0..GUTTER_MENU_ITEMS.len()).
    pub hovered_item: Option<usize>,
}

impl GutterPopup {
    /// Number of rows the popup occupies (header + items).
    fn total_rows(&self) -> usize {
        let header = if self.duration_text.is_some() { 1 } else { 0 };
        header + GUTTER_MENU_ITEMS.len()
    }
}

// ---------------------------------------------------------------------------
// RenderEvent — window thread → render thread (via cueue ring buffer)
// ---------------------------------------------------------------------------

/// Event sent from the window thread to the render thread through a cueue
/// SPSC ring buffer. Must be `Default` (for cueue slot initialization).
/// Only contains types that are small or cheap to clone — the heavyweight
/// `(Arc<Window>, OwnedDisplayHandle)` for renderer init is sent through a
/// separate one-shot mpsc channel.
#[derive(Clone, Default)]
pub enum RenderEvent {
    #[default]
    None,
    Resized {
        width: u32,
        height: u32,
    },
    Focused(bool),
    /// Key press — carries the full `Key` value from winit so the render
    /// thread can match keybindings and encode PTY input without any
    /// SmolStr version wrangling.
    KeyInput(Key),
    ModifiersChanged(ModifiersState),
    CursorMoved {
        x: f64,
        y: f64,
    },
    MouseInput {
        pressed: bool,
        button: MouseButton,
    },
    /// Raw scroll delta. `pixels == false` means line units (LineDelta),
    /// `pixels == true` means physical pixels (PixelDelta). The render
    /// thread converts to lines using its font metrics.
    MouseWheel {
        x: f64,
        y: f64,
        pixels: bool,
    },
    /// The window's DPI scale factor changed (e.g. moved to a different
    /// monitor). The render thread rescales font metrics and re-rasterizes
    /// glyphs.
    ScaleFactorChanged {
        scale_factor: f64,
    },
    CloseRequested,
}

pub struct RenderHost {
    renderer: Option<Renderer>,
    event_rx: cueue::Reader<RenderEvent>,
    child_exit_rx: mpsc::Receiver<TabId>,
    child_exit_tx: mpsc::Sender<TabId>,
    config_reload: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<Thread>>,
    proxy: EventLoopProxy<AppEvent>,

    tabs: Vec<Tab>,
    active_tab_id: TabId,
    next_tab_id: u64,
    font_system: FontSystem,

    config_path: Option<PathBuf>,
    config: Config,

    applied_title: Option<String>,
    modifiers: ModifiersState,
    mouse_pos: (f64, f64),
    mouse_buttons: MouseButtonState,
    last_motion_cell: Option<(u32, u32)>,
    last_click_time: Option<Instant>,
    last_click_cell: Option<(u32, u32)>,
    click_count: u32,
    left_drag_active: bool,

    /// Last known window size in physical pixels. Updated on Resized events.
    window_size: (u32, u32),

    /// Gutter popup menu, shown when a shell-integration marker is clicked.
    gutter_popup: Option<GutterPopup>,

    should_exit: bool,
}

impl RenderHost {
    pub fn new(
        event_rx: cueue::Reader<RenderEvent>,
        child_exit_rx: mpsc::Receiver<TabId>,
        child_exit_tx: mpsc::Sender<TabId>,
        config_reload: Arc<AtomicBool>,
        render_thread_handle: Arc<OnceLock<Thread>>,
        proxy: EventLoopProxy<AppEvent>,
        font_system: FontSystem,
        tab: Tab,
        config: Config,
        config_path: Option<PathBuf>,
    ) -> Self {
        Self {
            renderer: None,
            event_rx,
            child_exit_rx,
            child_exit_tx,
            config_reload,
            render_thread_handle,
            proxy,
            tabs: vec![tab],
            active_tab_id: TabId(0),
            next_tab_id: 1,
            font_system,
            config_path,
            config,
            applied_title: None,
            modifiers: ModifiersState::default(),
            mouse_pos: (0.0, 0.0),
            mouse_buttons: MouseButtonState::default(),
            last_motion_cell: None,
            last_click_time: None,
            last_click_cell: None,
            click_count: 0,
            left_drag_active: false,
            window_size: (0, 0),
            gutter_popup: None,
            should_exit: false,
        }
    }

    // -- Main loop ----------------------------------------------------------

    pub fn run(
        &mut self,
        window_rx: mpsc::Receiver<(Arc<Window>, OwnedDisplayHandle)>,
    ) {
        let mut frames = 0u64;
        let runtime = Instant::now();

        // Phase 1: wait for the window and initialize the renderer.
        let (window, display) = match window_rx.recv() {
            Ok(wd) => wd,
            Err(_) => return,
        };

        // Apply DPI scale factor: honour the config override if set,
        // otherwise use the monitor's native scale.
        let scale = self
            .config
            .dpi_scale
            .map(|s| s as f64)
            .unwrap_or_else(|| window.scale_factor());
        if scale != 1.0 {
            self.font_system.set_scale_factor(scale as f32);
        }

        let initial_size = window.inner_size();
        self.window_size = (initial_size.width, initial_size.height);
        self.handle_resize(initial_size.width, initial_size.height);

        let mut frame_time = Instant::now();
        // Phase 2: frame loop.
        loop {
            if let Some(duration) = Duration::from_millis(8).checked_sub(frame_time.elapsed()) {
                std::thread::park_timeout(duration);
            }
            frame_time = Instant::now();

            // Batch-drain all pending input events from the window thread.
            // Clone into a local buffer so we can commit() (freeing ring
            // buffer slots for the writer) before processing, which also
            // avoids a borrow conflict with &mut self in handle_render_event.
            let events: Vec<RenderEvent> = self.event_rx.read_chunk().to_vec();
            self.event_rx.commit();
            for event in &events {
                self.handle_render_event(event);
            }

            // Drain child-exit notifications.
            while let Ok(tab_id) = self.child_exit_rx.try_recv() {
                self.handle_child_exited(tab_id);
            }

            // Hot-reload config if the watcher flagged a change.
            if self.config_reload.swap(false, Ordering::Acquire) {
                self.reload_config();
            }

            // Catch-all: flush any pending terminal output that individual
            // event handlers didn't flush.
            self.flush_pending();

            if self.should_exit || self.event_rx.is_abandoned() {
                break;
            }

            // Drain PTY data for every tab and render a frame.
            self.drain_all_ptys();

            if self.renderer.is_none() {
                self.renderer = Some(pollster::block_on(Renderer::new(
                    window.clone(),
                    display.clone(),
                    self.config.opacity,
                    self.config.gutter,
                    self.config.power_preference,
                    self.config.vsync,
                )));
            }
            self.render_frame();

            frames += 1;
            if frames.is_multiple_of(100) {
                debug!(
                    "rendering at {:0.0} fps",
                    frames as f64 / runtime.elapsed().as_secs_f64()
                );
            }
        }

        let _ = self.proxy.send_event(AppEvent::Exit);
    }

    // -- Event dispatch -----------------------------------------------------

    fn handle_render_event(
        &mut self,
        event: &RenderEvent,
    ) {
        match event {
            RenderEvent::None => {}
            RenderEvent::CloseRequested => {
                self.should_exit = true;
            }
            RenderEvent::Resized { width, height } => {
                self.window_size = (*width, *height);
                self.handle_resize(*width, *height);
            }
            RenderEvent::Focused(focused) => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.report_focus_change(*focused);
                }
                self.flush_pending();
            }
            RenderEvent::KeyInput(key) => {
                self.handle_key_input(key.clone());
            }
            RenderEvent::ModifiersChanged(mods) => {
                self.modifiers = *mods;
            }
            RenderEvent::CursorMoved { x, y } => {
                self.handle_cursor_moved(*x, *y);
            }
            RenderEvent::MouseInput { pressed, button } => {
                self.handle_mouse_input(*pressed, *button);
            }
            RenderEvent::MouseWheel { x, y, pixels } => {
                self.handle_mouse_wheel(*x, *y, *pixels);
            }
            RenderEvent::ScaleFactorChanged { scale_factor } => {
                self.handle_scale_factor_changed(*scale_factor);
            }
        }
    }

    // -- Keyboard -----------------------------------------------------------

    fn handle_key_input(
        &mut self,
        key: Key,
    ) {
        // Dismiss gutter popup on any keypress.
        if self.gutter_popup.is_some() {
            self.close_gutter_popup();
            // Escape is consumed; other keys fall through to their normal
            // action so the user isn't forced to press twice.
            if matches!(key, Key::Named(NamedKey::Escape)) {
                return;
            }
        }

        // Search-bar routing runs ahead of keybindings and PTY encoding.
        if let Some(tab) = self.active_tab()
            && tab.terminal.search_active()
        {
            self.handle_search_key(&key);
            return;
        }

        if let Some(action) = self.config.keybindings.lookup(&key, self.modifiers) {
            self.run_action(action);
            return;
        }

        if let Some(tab) = self.active_tab() {
            let kitty_flags = tab.terminal.kitty_keyboard.current();
            if let Some(bytes) = kitty_encode_input(&key, self.modifiers, kitty_flags) {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.reset_viewport();
                    let _ = tab.pty.write(&bytes);
                }
                return;
            }
        }

        if self.modifiers.control_key() {
            let byte = match &key {
                Key::Character(c) => ctrl_byte(c),
                Key::Named(NamedKey::Space) => Some(0x00),
                _ => None,
            };

            if let Some(byte) = byte {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.reset_viewport();
                    let _ = tab.pty.write(&[byte]);
                }
                return;
            }
        }

        let bytes = match &key {
            Key::Character(c) => Some(c.as_bytes().to_vec()),
            Key::Named(named) => named_key_to_bytes(*named),
            _ => None,
        };

        if let Some(bytes) = bytes
            && let Some(tab) = self.active_tab_mut()
        {
            tab.terminal.reset_viewport();
            let _ = tab.pty.write(&bytes);
        }
    }

    fn handle_search_key(
        &mut self,
        key: &Key,
    ) {
        let shift = self.modifiers.shift_key();
        if let Some(terminal) = self.active_tab_mut().map(|tab| &mut tab.terminal) {
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
    }

    // -- Mouse --------------------------------------------------------------

    fn handle_cursor_moved(
        &mut self,
        x: f64,
        y: f64,
    ) {
        self.mouse_pos = (x, y);

        // Update popup hover state.
        if self.gutter_popup.is_some() {
            let item = self.popup_item_at(x, y);
            if let Some(popup) = self.gutter_popup.as_mut() {
                popup.hovered_item = item;
            }
        }

        let cell = self.cell_at(x, y);

        if self.forward_mouse_to_app() {
            if self.last_motion_cell == Some(cell) {
                return;
            }
            self.last_motion_cell = Some(cell);
            let button = self.mouse_buttons.primary_held();
            let mods = self.mouse_modifiers();
            if let Some(tab) = self.active_tab_mut() {
                tab.terminal
                    .mouse_report(MouseEventKind::Motion, button, cell.0, cell.1, mods);
            }
            self.flush_pending();
            return;
        }

        if self.left_drag_active
            && let Some(tab) = self.active_tab_mut()
        {
            tab.terminal.extend_selection(cell.0, cell.1);
        }
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

        // Clicks in the tab bar switch tabs instead of reaching the terminal.
        if pressed && button == MouseButton::Left && self.is_in_tab_bar() {
            self.close_gutter_popup();
            self.handle_tab_bar_click();
            return;
        }

        // Gutter popup interaction: clicks inside the popup fire the action;
        // clicks outside dismiss it.
        if pressed && button == MouseButton::Left && self.gutter_popup.is_some() {
            if let Some(item) = self.popup_item_at(self.mouse_pos.0, self.mouse_pos.1) {
                self.execute_popup_action(item);
                return;
            }
            // Click was outside the popup — dismiss it.
            self.close_gutter_popup();
            // If the click was in the gutter again, fall through to open a
            // new popup below; otherwise let the normal path handle it.
            if !self.is_in_gutter() {
                return;
            }
        }

        // Left-click in the gutter opens the popup for the clicked row.
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
            let mods = self.mouse_modifiers();
            if let Some(tab) = self.active_tab_mut() {
                tab.terminal.mouse_report(kind, term_button, col, row, mods);
            }
            self.flush_pending();
            return;
        }

        let (col, row) = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
        match (button, pressed) {
            (MouseButton::Left, true) => {
                if self.modifiers.control_key()
                    && let Some(tab) = self.active_tab()
                    && let Some(url) = tab.terminal.hyperlink_at(row, col)
                {
                    if let Err(e) = open::that_detached(url) {
                        warn!("failed to open hyperlink {url:?}: {e}");
                    }
                    return;
                }
                self.click_count = self.next_click_count((col, row));
                self.last_click_cell = Some((col, row));
                self.last_click_time = Some(Instant::now());
                let mode = match self.click_count {
                    2 => SelectionMode::Word,
                    3 => SelectionMode::Line,
                    _ => SelectionMode::Char,
                };
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.start_selection(col, row, mode);
                }
                self.left_drag_active = true;
            }
            (MouseButton::Left, false) => {
                self.left_drag_active = false;
                if let Some(tab) = self.active_tab_mut() {
                    if tab.terminal.has_selection() {
                        tab.terminal.copy_selection(ClipboardKind::Primary);
                    } else {
                        tab.terminal.clear_selection();
                    }
                }
            }
            (MouseButton::Right, true) => {
                if let Some(tab) = self.active_tab_mut() {
                    if tab.terminal.has_selection() {
                        tab.terminal.copy_selection(ClipboardKind::Clipboard);
                        tab.terminal.clear_selection();
                    } else {
                        tab.terminal.reset_viewport();
                        tab.terminal.paste_from_clipboard(ClipboardKind::Clipboard);
                    }
                    self.flush_pending();
                }
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
        let (x_lines, y_lines) = if pixels {
            let cw = self.font_system.cell_width as i32;
            let ch = self.font_system.cell_height as i32;
            ((raw_x as i32) / cw, -(raw_y as i32) / ch)
        } else {
            (raw_x as i32, -(raw_y as i32))
        };

        if let Some(tab) = self.active_tab()
            && tab.terminal.mouse_tracking_enabled()
            && !self.modifiers.shift_key()
        {
            let (col, row) = self.cell_at(self.mouse_pos.0, self.mouse_pos.1);
            let mods = self.mouse_modifiers();
            if let Some(tab) = self.active_tab_mut() {
                let terminal = &mut tab.terminal;
                if y_lines < 0 {
                    for _ in 0..y_lines.unsigned_abs() {
                        terminal.mouse_report(
                            MouseEventKind::Press,
                            TermMouseButton::WheelUp,
                            col,
                            row,
                            mods,
                        );
                    }
                } else if y_lines > 0 {
                    for _ in 0..y_lines as u32 {
                        terminal.mouse_report(
                            MouseEventKind::Press,
                            TermMouseButton::WheelDown,
                            col,
                            row,
                            mods,
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
                            mods,
                        );
                    }
                } else if x_lines > 0 {
                    for _ in 0..x_lines as u32 {
                        terminal.mouse_report(
                            MouseEventKind::Press,
                            TermMouseButton::WheelRight,
                            col,
                            row,
                            mods,
                        );
                    }
                }
            }
            self.flush_pending();
            return;
        }

        if let Some(tab) = self.active_tab_mut() {
            let terminal = &mut tab.terminal;
            if y_lines < 0 {
                terminal.scroll_viewport_up(y_lines.unsigned_abs());
            } else if y_lines > 0 {
                terminal.scroll_viewport_down(y_lines as u32);
            }
        }
    }

    // -- Actions ------------------------------------------------------------

    fn run_action(
        &mut self,
        action: Action,
    ) {
        match action {
            Action::ScrollPageUp => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.scroll_viewport_up(tab.terminal.viewport.rows);
                }
            }
            Action::ScrollPageDown => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal
                        .scroll_viewport_down(tab.terminal.viewport.rows);
                }
            }
            Action::Copy => {
                if let Some(tab) = self.active_tab_mut()
                    && tab.terminal.has_selection()
                {
                    tab.terminal.copy_selection(ClipboardKind::Clipboard);
                }
            }
            Action::Paste => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.reset_viewport();
                    tab.terminal.paste_from_clipboard(ClipboardKind::Clipboard);
                }
                self.flush_pending();
            }
            Action::OpenSearch => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.open_search();
                }
            }
            Action::ScrollPrevPrompt => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.scroll_to_prev_prompt();
                }
            }
            Action::ScrollNextPrompt => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.terminal.scroll_to_next_prompt();
                }
            }
            Action::OpenNewWindow => {
                if let Some(tab) = self.active_tab() {
                    let cwd = tab.terminal.current_directory.clone();
                    let _ = self.proxy.send_event(AppEvent::SpawnNewWindow { cwd });
                }
            }
            Action::NewTab => {
                self.spawn_new_tab();
            }
            Action::CloseTab => {
                self.close_active_tab();
            }
            Action::NextTab => {
                self.switch_tab(1);
            }
            Action::PrevTab => {
                self.switch_tab(-1);
            }
        }
    }

    // -- DPI scaling ---------------------------------------------------------

    fn handle_scale_factor_changed(
        &mut self,
        scale_factor: f64,
    ) {
        // When the user has a fixed dpi_scale, ignore system scale changes.
        if self.config.dpi_scale.is_some() {
            return;
        }
        self.font_system.set_scale_factor(scale_factor as f32);
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.reset_glyph_atlas();
        }
        // The compositor will follow up with a Resized event carrying the
        // new physical dimensions, which triggers handle_resize and
        // recalculates the grid with the updated cell metrics.
    }

    // -- Resize -------------------------------------------------------------

    fn handle_resize(
        &mut self,
        width: u32,
        height: u32,
    ) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(winit::dpi::PhysicalSize::new(width, height));
            let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
            let usable_width = width.saturating_sub(gutter_px);
            let tab_bar_px = if self.tab_bar_visible() {
                self.font_system.cell_height
            } else {
                0
            };
            let usable_height = height.saturating_sub(tab_bar_px);
            let (cols, rows) = self
                .font_system
                .grid_dimensions(usable_width, usable_height);
            for tab in &mut self.tabs {
                tab.terminal.resize(cols, rows);
                tab.pty.resize(cols as u16, rows as u16);
            }
        }
    }

    fn recalculate_grid_size(&mut self) {
        let Some(ref mut renderer) = self.renderer else {
            return;
        };
        let (width, height) = self.window_size;
        let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
        let usable_width = width.saturating_sub(gutter_px);
        let tab_bar_px = if self.tab_bar_visible() {
            self.font_system.cell_height
        } else {
            0
        };
        let usable_height = height.saturating_sub(tab_bar_px);
        let (cols, rows) = self
            .font_system
            .grid_dimensions(usable_width, usable_height);
        for tab in &mut self.tabs {
            tab.terminal.resize(cols, rows);
            tab.pty.resize(cols as u16, rows as u16);
        }
    }

    // -- Tab management -----------------------------------------------------

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active_tab_id)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == self.active_tab_id)
    }

    fn tab_bar_visible(&self) -> bool {
        self.tabs.len() >= 2
    }

    fn spawn_new_tab(&mut self) {
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;

        let cwd = if let Some(tab) = self.active_tab() {
            tab.terminal.current_directory.clone()
        } else {
            Default::default()
        };
        let was_single = self.tabs.len() == 1;

        let (cols, rows) = if let Some(renderer) = &self.renderer {
            let (width, height) = self.window_size;
            let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
            let usable_width = width.saturating_sub(gutter_px);
            // The tab bar will now be visible (2+ tabs).
            let tab_bar_px = self.font_system.cell_height;
            let usable_height = height.saturating_sub(tab_bar_px);
            self.font_system
                .grid_dimensions(usable_width, usable_height)
        } else {
            (INITIAL_COLS, INITIAL_ROWS)
        };

        let scrollback = if let Some(tab) = self.active_tab() {
            tab.terminal.active.grid.scrollback_limit
        } else {
            DEFAULT_SCROLLBACK
        };
        let mut terminal = Terminal::new(
            cols,
            rows,
            scrollback,
            self.font_system.cell_height,
            self.font_system.cell_width,
        );
        if let Some(tab) = self.active_tab() {
            terminal.set_default_cursor_style(tab.terminal.cursor_style);
        }

        let pty = match Pty::spawn(
            id,
            cols as u16,
            rows as u16,
            self.font_system.cell_width as u16,
            self.font_system.cell_height as u16,
            None,
            cwd,
            self.render_thread_handle.clone(),
            self.child_exit_tx.clone(),
        ) {
            Ok(pty) => pty,
            Err(e) => {
                warn!("failed to spawn new tab: {e}");
                return;
            }
        };

        self.tabs.push(Tab { id, terminal, pty });
        self.active_tab_id = id;

        if was_single {
            self.recalculate_grid_size();
        }
    }

    fn close_active_tab(&mut self) {
        let tab_id = self.active_tab_id;
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.should_exit = true;
            return;
        }
        let new_idx = idx.min(self.tabs.len() - 1);
        self.active_tab_id = self.tabs[new_idx].id;
        self.recalculate_grid_size();
    }

    fn switch_tab(
        &mut self,
        delta: i32,
    ) {
        if self.tabs.len() <= 1 {
            return;
        }
        let idx = self
            .tabs
            .iter()
            .position(|t| t.id == self.active_tab_id)
            .unwrap_or(0);
        let n = self.tabs.len() as i32;
        let new_idx = ((idx as i32 + delta).rem_euclid(n)) as usize;
        self.active_tab_id = self.tabs[new_idx].id;
    }

    fn handle_child_exited(
        &mut self,
        tab_id: TabId,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let was_active = self.active_tab_id == tab_id;
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.should_exit = true;
            return;
        }
        if was_active {
            let new_idx = idx.min(self.tabs.len() - 1);
            self.active_tab_id = self.tabs[new_idx].id;
        }
        self.recalculate_grid_size();
    }

    fn handle_tab_bar_click(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let cell_w = self.font_system.cell_width as f32;
        let surface_w = self.window_size.0 as f32;
        let max_tab_w = cell_w * 30.0;
        let tab_w = (surface_w / self.tabs.len() as f32).min(max_tab_w);
        let clicked_idx = (self.mouse_pos.0.max(0.0) as f32 / tab_w) as usize;
        if let Some(tab) = self.tabs.get(clicked_idx) {
            self.active_tab_id = tab.id;
        }
    }

    // -- Config -------------------------------------------------------------

    fn reload_config(&mut self) {
        let Some(path) = self.config_path.as_ref() else {
            return;
        };
        let cfg = crate::config::load_from(path);

        for tab in &mut self.tabs {
            tab.terminal.set_default_cursor_style(cfg.cursor_style);
            tab.terminal.set_scrollback_limit(cfg.scrollback_lines);
        }
        self.config.keybindings = cfg.keybindings;
        self.config.bell = cfg.bell;

        if cfg.gutter != self.config.gutter {
            self.config.gutter = cfg.gutter;
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.set_gutter_enabled(cfg.gutter);
                self.recalculate_grid_size();
            }
        }

        if cfg.dpi_scale != self.config.dpi_scale {
            warn!(
                "config: dpi_scale changed ({:?} → {:?}); restart to apply",
                self.config.dpi_scale, cfg.dpi_scale
            );
        }

        if cfg.fonts != self.config.fonts {
            warn!(
                "config: fonts changed (was {:?}, now {:?}); restart to apply",
                self.config.fonts, cfg.fonts
            );
        }
        if (cfg.font_size - self.config.font_size).abs() > f32::EPSILON {
            warn!(
                "config: font_size changed ({} → {}); restart to apply",
                self.config.font_size, cfg.font_size
            );
        }
        if (cfg.opacity - self.config.opacity).abs() > f32::EPSILON {
            warn!(
                "config: opacity changed ({} → {}); restart to apply",
                self.config.opacity, cfg.opacity
            );
        }
        if self.config.power_preference != cfg.power_preference {
            warn!(
                "config: power_preference changed ({:?} → {:?}); restart to apply",
                self.config.power_preference, cfg.power_preference
            );
        }
    }

    // -- PTY draining -------------------------------------------------------

    fn drain_all_ptys(&mut self) {
        let time_slice = Instant::now() + Duration::from_millis(5);
        let tab_ids: Vec<TabId> = self.tabs.iter().map(|t| t.id).collect();
        for tab_id in tab_ids {
            self.read_pty_output(tab_id, time_slice);
        }
    }

    fn read_pty_output(
        &mut self,
        tab_id: TabId,
        time_slice: Instant,
    ) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        tab.pty.clear_pending();
        let mut buf = [0u8; MAX_READ_CHUNK];
        loop {
            let read = tab.pty.read(&mut buf);
            if read == 0 {
                break;
            }
            tab.terminal.process(&buf[..read]);
            if Instant::now() >= time_slice {
                break;
            }
        }
    }

    // -- Per-frame rendering ------------------------------------------------

    fn render_frame(&mut self) {
        if self.tabs.is_empty() {
            return;
        }

        self.sync_window_title();
        self.dispatch_bell();

        let active_idx = self
            .tabs
            .iter()
            .position(|t| t.id == self.active_tab_id)
            .expect("active tab must exist");

        let synced = self.tabs[active_idx]
            .terminal
            .is_synchronized_update_active();
        if synced {
            return;
        }

        let tab_infos: Vec<TabInfo> = if self.tab_bar_visible() {
            self.tabs
                .iter()
                .map(|t| TabInfo {
                    label: t.terminal.current_title.as_deref().unwrap_or("Shell"),
                    active: t.id == self.active_tab_id,
                })
                .collect()
        } else {
            Vec::new()
        };
        if let Some(ref mut renderer) = self.renderer {
            renderer.render(
                &mut self.font_system,
                &self.tabs[active_idx].terminal,
                &tab_infos,
                self.gutter_popup.as_ref(),
            );
        }
    }

    fn sync_window_title(&mut self) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let base = tab.terminal.current_title.as_deref();
        let want = if self.tabs.len() > 1 {
            let idx = self
                .tabs
                .iter()
                .position(|t| t.id == self.active_tab_id)
                .unwrap_or(0);
            Some(format!(
                "[{}/{}] {}",
                idx + 1,
                self.tabs.len(),
                base.unwrap_or("term41")
            ))
        } else {
            base.map(str::to_owned)
        };
        if self.applied_title.as_deref() == want.as_deref() {
            return;
        }
        let title = want.clone().unwrap_or_else(|| "term41".to_owned());
        let _ = self.proxy.send_event(AppEvent::SetTitle(title));
        self.applied_title = want;
    }

    fn dispatch_bell(&mut self) {
        if let Some(tab) = self.active_tab_mut()
            && !tab.terminal.take_bell_pending()
        {
            return;
        }
        match self.config.bell {
            BellMode::Off => {}
            BellMode::Visual => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.notify_bell();
                }
            }
            BellMode::Urgent => {
                let _ = self.proxy.send_event(AppEvent::RequestUserAttention);
            }
        }
    }

    // -- Helpers ------------------------------------------------------------

    fn cell_at(
        &self,
        x: f64,
        y: f64,
    ) -> (u32, u32) {
        let raw_x = x.max(0.0) as u32;
        let raw_y = y.max(0.0) as u32;
        let tab_bar_px = if self.tab_bar_visible() {
            self.font_system.cell_height
        } else {
            0
        };
        let y = raw_y.saturating_sub(tab_bar_px);
        let gutter_px = self
            .renderer
            .as_ref()
            .map(|r| r.gutter_width_px(self.font_system.cell_width))
            .unwrap_or(0);
        let x = raw_x.saturating_sub(gutter_px);
        let Some(tab) = self.active_tab() else {
            return (0, 0);
        };
        let cols = tab.terminal.viewport.cols.saturating_sub(1);
        let rows = tab.terminal.viewport.rows.saturating_sub(1);
        (
            (x / self.font_system.cell_width).min(cols),
            (y / self.font_system.cell_height).min(rows),
        )
    }

    fn is_in_tab_bar(&self) -> bool {
        self.tab_bar_visible() && (self.mouse_pos.1.max(0.0) as u32) < self.font_system.cell_height
    }

    fn mouse_modifiers(&self) -> MouseModifiers {
        MouseModifiers {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            ctrl: self.modifiers.control_key(),
        }
    }

    fn flush_pending(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };

        let bytes = tab.terminal.take_pending_output();
        if !bytes.is_empty() {
            let _ = tab.pty.write(&bytes);
            tab.terminal.reset_viewport();
        }
    }

    fn forward_mouse_to_app(&self) -> bool {
        if let Some(tab) = self.active_tab() {
            tab.terminal.mouse_tracking_enabled() && !self.modifiers.shift_key()
        } else {
            false
        }
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

    // -- Gutter popup -------------------------------------------------------

    /// True when the mouse is in the gutter strip (left of col 0).
    fn is_in_gutter(&self) -> bool {
        let gutter_px = self
            .renderer
            .as_ref()
            .map(|r| r.gutter_width_px(self.font_system.cell_width))
            .unwrap_or(0);
        gutter_px > 0 && (self.mouse_pos.0.max(0.0) as u32) < gutter_px
    }

    /// Compute the popup's pixel bounds: `(x, y, w, h)`.
    fn popup_bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let popup = self.gutter_popup.as_ref()?;
        let cell_w = self.font_system.cell_width as f32;
        let cell_h = self.font_system.cell_height as f32;
        let gutter_px = self
            .renderer
            .as_ref()
            .map(|r| r.gutter_width_px(self.font_system.cell_width))
            .unwrap_or(0) as f32;
        let tab_bar_h = if self.tab_bar_visible() { cell_h } else { 0.0 };

        let total_rows = popup.total_rows() as f32;
        let popup_w = cell_w * POPUP_WIDTH_CELLS;
        let popup_h = total_rows * cell_h;
        let popup_x = gutter_px;
        let popup_y =
            (popup.screen_row as f32 * cell_h + tab_bar_h).min(self.window_size.1 as f32 - popup_h);
        Some((popup_x, popup_y, popup_w, popup_h))
    }

    /// Which menu-item index (if any) the given pixel position hits.
    fn popup_item_at(
        &self,
        x: f64,
        y: f64,
    ) -> Option<usize> {
        let (px, py, pw, ph) = self.popup_bounds()?;
        let popup = self.gutter_popup.as_ref()?;
        let cell_h = self.font_system.cell_height as f32;
        let x = x as f32;
        let y = y as f32;
        if x < px || x > px + pw || y < py || y > py + ph {
            return None;
        }
        let row_in_popup = ((y - py) / cell_h) as usize;
        let header = if popup.duration_text.is_some() { 1 } else { 0 };
        let item_idx = row_in_popup.checked_sub(header)?;
        (item_idx < GUTTER_MENU_ITEMS.len()).then_some(item_idx)
    }

    /// Open the gutter popup for `screen_row`. Finds the owning prompt,
    /// selects the command, and builds the popup state.
    fn open_gutter_popup(
        &mut self,
        screen_row: u32,
    ) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let Some(prompt_abs) = tab.terminal.find_prompt_for_screen_row(screen_row) else {
            return;
        };
        tab.terminal.select_command_at(prompt_abs);
        let duration_text = tab
            .terminal
            .command_duration_at(prompt_abs)
            .map(format_duration);
        self.gutter_popup = Some(GutterPopup {
            prompt_abs_row: prompt_abs,
            screen_row,
            duration_text,
            hovered_item: None,
        });
    }

    /// Dismiss the popup (if open).
    fn close_gutter_popup(&mut self) {
        if self.gutter_popup.take().is_some()
            && let Some(tab) = self.active_tab_mut()
        {
            tab.terminal.clear_selection();
        }
    }

    /// Execute the action from the popup at `item_idx` and close it.
    fn execute_popup_action(
        &mut self,
        item_idx: usize,
    ) {
        let Some(popup) = self.gutter_popup.take() else {
            return;
        };
        let action = GUTTER_MENU_ITEMS[item_idx].action;
        match action {
            GutterMenuAction::Rerun => {
                if let Some(tab) = self.active_tab_mut()
                    && let Some(cmd) = tab.terminal.command_text_at(popup.prompt_abs_row)
                {
                    let cmd = cmd.trim().to_owned();
                    tab.terminal.clear_selection();
                    tab.terminal.reset_viewport();
                    tab.terminal.paste(&format!("{cmd}\r"));
                }
                self.flush_pending();
            }
            GutterMenuAction::CopyCommand => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(text) = tab.terminal.command_text_at(popup.prompt_abs_row) {
                        tab.terminal.copy_to_clipboard(text.trim());
                    }
                    tab.terminal.clear_selection();
                }
            }
            GutterMenuAction::CopyCommandAndOutput => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(text) = tab
                        .terminal
                        .command_and_output_text_at(popup.prompt_abs_row)
                    {
                        tab.terminal.copy_to_clipboard(&text);
                    }
                    tab.terminal.clear_selection();
                }
            }
            GutterMenuAction::CopyOutput => {
                if let Some(tab) = self.active_tab_mut() {
                    if let Some(text) = tab.terminal.output_text_at(popup.prompt_abs_row) {
                        tab.terminal.copy_to_clipboard(&text);
                    }
                    tab.terminal.clear_selection();
                }
            }
        }
    }
}

/// Format a Duration as a human-readable string.
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

fn ctrl_byte(c: &str) -> Option<u8> {
    match c.as_bytes() {
        [b @ b'a'..=b'z'] => Some(b - b'a' + 1),
        [b @ b'A'..=b'Z'] => Some(b - b'A' + 1),
        [b'@'] => Some(0x00),
        [b'['] => Some(0x1B),
        [b'\\'] => Some(0x1C),
        [b']'] => Some(0x1D),
        [b'^'] => Some(0x1E),
        [b'_'] => Some(0x1F),
        _ => None,
    }
}

fn kitty_modifier_bits(mods: ModifiersState) -> u8 {
    let mut b = 0;
    if mods.shift_key() {
        b |= KittyKeys::SHIFT.bits();
    }
    if mods.alt_key() {
        b |= KittyKeys::ALT.bits();
    }
    if mods.control_key() {
        b |= KittyKeys::CTRL.bits();
    }
    if mods.super_key() {
        b |= KittyKeys::SUPER.bits();
    }
    b
}

fn kitty_encode_input(
    key: &Key,
    mods: ModifiersState,
    flags: KittyFlags,
) -> Option<Vec<u8>> {
    if !flags.contains(KittyFlags::DISAMBIGUATE_ESCAPE_CODES) {
        return None;
    }

    let mod_bits = kitty_modifier_bits(mods);
    let only_shift_or_none = (mod_bits & !1) == 0;
    let mod_param = mod_bits + 1;

    match key {
        Key::Character(s) => {
            if only_shift_or_none {
                return None;
            }
            let lower = s.to_lowercase();
            let cp = lower.chars().next()? as u32;
            Some(format!("\x1b[{cp};{mod_param}u").into_bytes())
        }
        Key::Named(named) => kitty_encode_named(*named, mod_bits, mod_param),
        _ => None,
    }
}

fn kitty_encode_named(
    named: NamedKey,
    mod_bits: u8,
    mod_param: u8,
) -> Option<Vec<u8>> {
    let direct_code = match named {
        NamedKey::Enter => Some(13u32),
        NamedKey::Tab => Some(9),
        NamedKey::Backspace => Some(127),
        NamedKey::Escape => Some(27),
        NamedKey::Space => Some(32),
        _ => None,
    };
    if let Some(cp) = direct_code {
        if (mod_bits & !1) == 0 && mod_bits == 0 {
            return None;
        }
        return Some(format!("\x1b[{cp};{mod_param}u").into_bytes());
    }

    if mod_bits == 0 {
        return None;
    }

    let arrow_action = match named {
        NamedKey::ArrowUp => Some('A'),
        NamedKey::ArrowDown => Some('B'),
        NamedKey::ArrowRight => Some('C'),
        NamedKey::ArrowLeft => Some('D'),
        NamedKey::Home => Some('H'),
        NamedKey::End => Some('F'),
        _ => None,
    };
    if let Some(action) = arrow_action {
        return Some(format!("\x1b[1;{mod_param}{action}").into_bytes());
    }

    let tilde_code = match named {
        NamedKey::Insert => Some(2u32),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        _ => None,
    };
    if let Some(code) = tilde_code {
        return Some(format!("\x1b[{code};{mod_param}~").into_bytes());
    }

    None
}

fn named_key_to_bytes(key: NamedKey) -> Option<Vec<u8>> {
    match key {
        NamedKey::Enter => Some(b"\r".to_vec()),
        NamedKey::Backspace => Some(b"\x7f".to_vec()),
        NamedKey::Tab => Some(b"\t".to_vec()),
        NamedKey::Escape => Some(b"\x1b".to_vec()),
        NamedKey::ArrowUp => Some(b"\x1b[A".to_vec()),
        NamedKey::ArrowDown => Some(b"\x1b[B".to_vec()),
        NamedKey::ArrowRight => Some(b"\x1b[C".to_vec()),
        NamedKey::ArrowLeft => Some(b"\x1b[D".to_vec()),
        NamedKey::Home => Some(b"\x1b[H".to_vec()),
        NamedKey::End => Some(b"\x1b[F".to_vec()),
        NamedKey::Delete => Some(b"\x1b[3~".to_vec()),
        NamedKey::PageUp => Some(b"\x1b[5~".to_vec()),
        NamedKey::PageDown => Some(b"\x1b[6~".to_vec()),
        NamedKey::Space => Some(b" ".to_vec()),
        _ => None,
    }
}

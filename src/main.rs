#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod clipboard;
mod config;
mod font;
mod pty;
mod renderer;
mod selection;
mod sixel;
mod terminal;
mod vte;

use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use clipboard::ClipboardKind;
use font::FontSystem;
use pty::Pty;
use renderer::Renderer;
use selection::SelectionMode;
use terminal::MouseButton as TermMouseButton;
use terminal::MouseEventKind;
use terminal::MouseModifiers;
use terminal::Terminal;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalPosition;
use winit::event::ElementState;
use winit::event::MouseButton;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::keyboard::Key;
use winit::keyboard::NamedKey;
use winit::window::Window;
use winit::window::WindowId;

#[macro_use]
extern crate log;

const INITIAL_COLS: u32 = 80;
const INITIAL_ROWS: u32 = 24;

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    terminal: Terminal,
    font_system: FontSystem,
    pty: Pty,
    opacity: f32,
    modifiers: winit::keyboard::ModifiersState,

    /// Last known pointer position in physical pixels. Updated on every
    /// CursorMoved; click/release events fall back to this because winit
    /// doesn't embed position in MouseInput.
    mouse_pos: PhysicalPosition<f64>,

    /// Buttons currently held, used to pick the reported button code on
    /// motion events under tracking mode 1002.
    mouse_buttons: MouseButtonState,

    /// Last cell a motion event was reported for. Motion is suppressed
    /// until the pointer crosses into a new cell, so apps don't drown in
    /// per-pixel events.
    last_motion_cell: Option<(u32, u32)>,

    /// Timestamp of the last left-button press — combined with
    /// `last_click_cell` to detect double/triple clicks.
    last_click_time: Option<Instant>,
    last_click_cell: Option<(u32, u32)>,
    /// Consecutive-click counter. 1 = single, 2 = double, 3 = triple.
    /// Resets after the click-expiry window or on a cell change.
    click_count: u32,

    /// True while the left button is held in selection mode, so motion
    /// events extend the selection rather than being dropped.
    left_drag_active: bool,
}

/// Maximum time between clicks that still count as part of a sequence.
const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

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

    /// Pick a single button to report in motion events. xterm reports the
    /// lowest-numbered held button.
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

impl App {
    fn new(
        pty: Pty,
        opacity: f32,
        fonts_config: Option<&str>,
        scrollback_lines: u32,
        font_size: f32,
    ) -> Self {
        let font_system = FontSystem::new(fonts_config, font_size);
        Self {
            window: None,
            renderer: None,
            terminal: Terminal::new(
                INITIAL_COLS,
                INITIAL_ROWS,
                scrollback_lines,
                font_system.cell_height,
            ),
            font_system,
            pty,
            opacity,
            modifiers: winit::keyboard::ModifiersState::default(),
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            mouse_buttons: MouseButtonState::default(),
            last_motion_cell: None,
            last_click_time: None,
            last_click_cell: None,
            click_count: 0,
            left_drag_active: false,
        }
    }

    fn cell_at(
        &self,
        pos: PhysicalPosition<f64>,
    ) -> (u32, u32) {
        let x = pos.x.max(0.0) as u32;
        let y = pos.y.max(0.0) as u32;
        let cols = self.terminal.viewport.cols.saturating_sub(1);
        let rows = self.terminal.viewport.rows.saturating_sub(1);
        (
            (x / self.font_system.cell_width).min(cols),
            (y / self.font_system.cell_height).min(rows),
        )
    }

    fn mouse_modifiers(&self) -> MouseModifiers {
        MouseModifiers {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            ctrl: self.modifiers.control_key(),
        }
    }

    fn flush_pending(&mut self) {
        let bytes = self.terminal.take_pending_output();
        if !bytes.is_empty() {
            let _ = self.pty.write(&bytes);
        }
    }

    /// Mouse events are forwarded to the foreground app when it has
    /// requested tracking and the shift bypass isn't active.
    fn forward_mouse_to_app(&self) -> bool {
        self.terminal.mouse_tracking_enabled() && !self.modifiers.shift_key()
    }

    /// Compute the click-sequence count for a new left-button press at
    /// `cell`. Same cell + within the multi-click window + not exceeding
    /// triple-click yields the next number; anything else restarts at 1.
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

    fn read_pty_output(&mut self) {
        let mut buf = [0u8; 128 * 1024];
        while let Ok(n) = self.pty.read(&mut buf) {
            if n == 0 {
                break;
            }
            self.terminal.process(&buf[..n]);
        }
        // Drain any bytes the terminal itself queued for the PTY (OSC 52
        // query responses and similar). Do this after the read loop so we
        // batch replies across a whole input chunk.
        let reply = self.terminal.take_pending_output();
        if !reply.is_empty() {
            let _ = self.pty.write(&reply);
        }
    }

    fn request_redraw(&self) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) {
        if self.window.is_some() {
            return;
        }

        let (pixel_width, pixel_height) = self.font_system.grid_size(INITIAL_COLS, INITIAL_ROWS);
        let transparent = self.opacity < 1.0;
        let attrs = Window::default_attributes()
            .with_title("term41")
            .with_transparent(transparent)
            .with_inner_size(winit::dpi::PhysicalSize::new(pixel_width, pixel_height));

        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let renderer = pollster::block_on(Renderer::new(
            Arc::clone(&window),
            &mut self.font_system,
            &self.terminal,
            self.opacity,
        ));

        self.window = Some(window);
        self.renderer = Some(renderer);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.resize(size);
                    let (cols, rows) = self.font_system.grid_dimensions(size.width, size.height);
                    self.terminal.resize(cols, rows);
                    self.pty.resize(cols as u16, rows as u16);
                }
            }

            WindowEvent::MouseWheel { delta, .. } => {
                let (x_lines, y_lines) = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => (x as i32, -y as i32),
                    winit::event::MouseScrollDelta::PixelDelta(pos) => (
                        (pos.x as i32) / self.font_system.cell_width as i32,
                        -(pos.y as i32) / self.font_system.cell_height as i32,
                    ),
                };

                // When an app has mouse tracking enabled we forward wheel
                // events as button presses so it can page/scroll its own
                // viewport; shift bypasses the app so the user can still
                // navigate scrollback.
                if self.terminal.mouse_tracking_enabled() && !self.modifiers.shift_key() {
                    let (col, row) = self.cell_at(self.mouse_pos);
                    let mods = self.mouse_modifiers();
                    let mut report = |button: TermMouseButton, steps: u32| {
                        for _ in 0..steps {
                            self.terminal.mouse_report(
                                MouseEventKind::Press,
                                button,
                                col,
                                row,
                                mods,
                            );
                        }
                    };
                    if y_lines < 0 {
                        report(TermMouseButton::WheelUp, y_lines.unsigned_abs());
                    } else if y_lines > 0 {
                        report(TermMouseButton::WheelDown, y_lines as u32);
                    }
                    if x_lines < 0 {
                        report(TermMouseButton::WheelLeft, x_lines.unsigned_abs());
                    } else if x_lines > 0 {
                        report(TermMouseButton::WheelRight, x_lines as u32);
                    }
                    self.flush_pending();
                    return;
                }

                if y_lines < 0 {
                    self.terminal.scroll_viewport_up(y_lines.unsigned_abs());
                } else if y_lines > 0 {
                    self.terminal.scroll_viewport_down(y_lines as u32);
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_pos = position;
                let cell = self.cell_at(position);

                if self.forward_mouse_to_app() {
                    if self.last_motion_cell == Some(cell) {
                        return;
                    }
                    self.last_motion_cell = Some(cell);
                    let button = self.mouse_buttons.primary_held();
                    let mods = self.mouse_modifiers();
                    self.terminal.mouse_report(
                        MouseEventKind::Motion,
                        button,
                        cell.0,
                        cell.1,
                        mods,
                    );
                    self.flush_pending();
                    return;
                }

                // Local selection path: extend the active drag.
                if self.left_drag_active {
                    self.terminal.extend_selection(cell.0, cell.1);
                }
            }

            WindowEvent::MouseInput { state, button, .. } => {
                let term_button = match button {
                    MouseButton::Left => TermMouseButton::Left,
                    MouseButton::Middle => TermMouseButton::Middle,
                    MouseButton::Right => TermMouseButton::Right,
                    _ => return,
                };
                let pressed = state == ElementState::Pressed;
                self.mouse_buttons.set(button, pressed);
                if pressed {
                    self.last_motion_cell = None;
                }

                if self.forward_mouse_to_app() {
                    let (col, row) = self.cell_at(self.mouse_pos);
                    let kind = if pressed {
                        MouseEventKind::Press
                    } else {
                        MouseEventKind::Release
                    };
                    let mods = self.mouse_modifiers();
                    self.terminal
                        .mouse_report(kind, term_button, col, row, mods);
                    self.flush_pending();
                    return;
                }

                let (col, row) = self.cell_at(self.mouse_pos);
                match (button, pressed) {
                    (MouseButton::Left, true) => {
                        self.click_count = self.next_click_count((col, row));
                        self.last_click_cell = Some((col, row));
                        self.last_click_time = Some(Instant::now());
                        let mode = match self.click_count {
                            2 => SelectionMode::Word,
                            3 => SelectionMode::Line,
                            _ => SelectionMode::Char,
                        };
                        self.terminal.start_selection(col, row, mode);
                        self.left_drag_active = true;
                    }
                    (MouseButton::Left, false) => {
                        self.left_drag_active = false;
                        if self.terminal.has_selection() {
                            // Drag released with real content — mirror
                            // xterm/Linux convention and stage it on the
                            // primary selection so middle-click elsewhere
                            // picks it up without an explicit copy.
                            self.terminal.copy_selection(ClipboardKind::Primary);
                        } else {
                            self.terminal.clear_selection();
                        }
                    }
                    (MouseButton::Right, true) => {
                        if self.terminal.has_selection() {
                            self.terminal.copy_selection(ClipboardKind::Clipboard);
                            self.terminal.clear_selection();
                        } else {
                            self.terminal.reset_viewport();
                            self.terminal.paste_from_clipboard(ClipboardKind::Clipboard);
                            self.flush_pending();
                        }
                    }
                    _ => {}
                }
            }

            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                // Shift+PageUp/Down for scrollback navigation.
                if self.modifiers.shift_key()
                    && let Key::Named(named) = &event.logical_key
                {
                    match named {
                        NamedKey::PageUp => {
                            self.terminal
                                .scroll_viewport_up(self.terminal.viewport.rows);
                            return;
                        }
                        NamedKey::PageDown => {
                            self.terminal
                                .scroll_viewport_down(self.terminal.viewport.rows);
                            return;
                        }
                        _ => {}
                    }
                }

                // Ctrl+Shift+V / Ctrl+Shift+C → clipboard paste / copy.
                // Caught before the Ctrl+key → control-byte path so plain
                // Ctrl-V / Ctrl-C still emit 0x16 / 0x03.
                if self.modifiers.control_key()
                    && self.modifiers.shift_key()
                    && let Key::Character(c) = &event.logical_key
                {
                    if c.eq_ignore_ascii_case("v") {
                        self.terminal.reset_viewport();
                        self.terminal.paste_from_clipboard(ClipboardKind::Clipboard);
                        self.flush_pending();
                        return;
                    }
                    if c.eq_ignore_ascii_case("c") && self.terminal.has_selection() {
                        self.terminal.copy_selection(ClipboardKind::Clipboard);
                        return;
                    }
                }

                // Ctrl+key → control character byte (0x00–0x1F).
                if self.modifiers.control_key() {
                    let byte = match &event.logical_key {
                        Key::Character(c) => ctrl_byte(c),
                        Key::Named(NamedKey::Space) => Some(0x00),
                        _ => None,
                    };

                    if let Some(byte) = byte {
                        self.terminal.reset_viewport();
                        let _ = self.pty.write(&[byte]);
                        return;
                    }
                }

                let bytes = match &event.logical_key {
                    Key::Character(c) => Some(c.as_bytes().to_vec()),
                    Key::Named(named) => named_key_to_bytes(*named),
                    _ => None,
                };

                if let Some(bytes) = bytes {
                    self.terminal.reset_viewport();
                    let _ = self.pty.write(&bytes);
                }
            }

            WindowEvent::RedrawRequested => {
                self.read_pty_output();
                if let Some(renderer) = &mut self.renderer {
                    renderer.render(&mut self.font_system, &self.terminal);
                }

                self.request_redraw();
            }

            _ => {}
        }
    }

    fn about_to_wait(
        &mut self,
        _event_loop: &ActiveEventLoop,
    ) {
        self.request_redraw();
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

fn main() {
    env_logger::init();

    let config = config::load();
    let pty = Pty::spawn(INITIAL_COLS as u16, INITIAL_ROWS as u16).expect("failed to spawn PTY");

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(
        pty,
        config.opacity,
        config.fonts.as_deref(),
        config.scrollback_lines,
        config.font_size,
    );
    event_loop.run_app(&mut app).expect("run event loop");
}

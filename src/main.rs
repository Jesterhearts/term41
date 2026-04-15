#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod clipboard;
mod config;
mod font;
mod keybindings;
mod pty;
mod renderer;
mod search;
mod selection;
mod sixel;
mod terminal;
mod vte;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;

use clipboard::ClipboardKind;
use config::BellMode;
use config::Config;
use font::FontSystem;
use keybindings::Action;
use pty::Pty;
use renderer::Renderer;
use selection::SelectionMode;
use terminal::KittyFlags;
use terminal::KittyKeys;
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
use winit::event_loop::EventLoopProxy;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::window::Window;
use winit::window::WindowId;

#[macro_use]
extern crate log;

const INITIAL_COLS: u32 = 80;
const INITIAL_ROWS: u32 = 24;

/// Custom event type for the winit event loop. Keeps cross-thread plumbing
/// (config watcher, future async tasks) out of the main `WindowEvent` flow.
#[derive(Debug, Clone)]
enum AppEvent {
    /// The config file changed on disk and should be re-read + applied.
    ReloadConfig,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    terminal: Terminal,
    font_system: FontSystem,
    pty: Pty,
    opacity: f32,
    /// Path to the config file we'll reload on `AppEvent::ReloadConfig`.
    /// `None` means we couldn't even resolve a config dir at startup, so
    /// reloads are silently disabled.
    config_path: Option<PathBuf>,
    /// Snapshot of the config knobs that *can't* be hot-reloaded — fonts,
    /// font_size, opacity. Kept so `apply_config` can warn when the user
    /// changes one (signalling that a restart is needed).
    boot_fonts: Option<String>,
    boot_font_size: f32,
    boot_opacity: f32,
    keybindings: keybindings::Keybindings,
    bell_mode: BellMode,
    /// Last title we pushed to the OS via `Window::set_title`. Compared
    /// against `terminal.current_title` each frame so we only call
    /// `set_title` when something actually changed (the call hops to the
    /// compositor on Wayland and isn't free).
    applied_title: Option<String>,
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
        config: Config,
        config_path: Option<PathBuf>,
    ) -> Self {
        let font_system = FontSystem::new(config.fonts.as_deref(), config.font_size);
        let mut terminal = Terminal::new(
            INITIAL_COLS,
            INITIAL_ROWS,
            config.scrollback_lines,
            font_system.cell_height,
        );
        terminal.set_default_cursor_style(config.cursor_style);
        Self {
            window: None,
            renderer: None,
            terminal,
            font_system,
            pty,
            opacity: config.opacity,
            config_path,
            boot_fonts: config.fonts.clone(),
            boot_font_size: config.font_size,
            boot_opacity: config.opacity,
            keybindings: config.keybindings,
            bell_mode: config.bell,
            applied_title: None,
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

    /// Re-read the watched config file and apply the axes that can change
    /// at runtime (cursor style, scrollback budget, keybindings). Knobs
    /// that need a fresh GPU surface or atlas (opacity, fonts, font size)
    /// log a "needs restart" warning when they change so the user knows
    /// to relaunch — silent ignoring would be worse than the small noise.
    fn reload_config(&mut self) {
        let Some(path) = self.config_path.as_ref() else {
            return;
        };
        let cfg = config::load_from(path);

        self.terminal.set_default_cursor_style(cfg.cursor_style);
        self.terminal.set_scrollback_limit(cfg.scrollback_lines);
        self.keybindings = cfg.keybindings;
        self.bell_mode = cfg.bell;

        if cfg.fonts != self.boot_fonts {
            warn!(
                "config: fonts changed (was {:?}, now {:?}); restart to apply",
                self.boot_fonts, cfg.fonts
            );
        }
        if (cfg.font_size - self.boot_font_size).abs() > f32::EPSILON {
            warn!(
                "config: font_size changed ({} → {}); restart to apply",
                self.boot_font_size, cfg.font_size
            );
        }
        if (cfg.opacity - self.boot_opacity).abs() > f32::EPSILON {
            warn!(
                "config: opacity changed ({} → {}); restart to apply",
                self.boot_opacity, cfg.opacity
            );
        }
    }

    /// Push the foreground app's OSC 0 / OSC 2 title onto the OS window
    /// when it changes. Falls back to `term41` when the app clears the
    /// title (or never set one) so the user always has a recognisable
    /// label in their window list.
    fn sync_window_title(&mut self) {
        let want = self.terminal.current_title.as_deref();
        if self.applied_title.as_deref() == want {
            return;
        }
        let Some(window) = &self.window else {
            return;
        };
        match want {
            Some(t) => window.set_title(t),
            None => window.set_title("term41"),
        }
        self.applied_title = want.map(str::to_owned);
    }

    /// Drain the bell flag and act on it according to the configured
    /// [`BellMode`]. Polled once per frame so a tight loop of BELs only
    /// produces one user-visible reaction per frame, not a queue of
    /// stacked flashes / urgency requests.
    fn dispatch_bell(&mut self) {
        if !self.terminal.take_bell_pending() {
            return;
        }
        match self.bell_mode {
            BellMode::Off => {}
            BellMode::Visual => {
                if let Some(renderer) = &mut self.renderer {
                    renderer.notify_bell();
                }
            }
            BellMode::Urgent => {
                if let Some(window) = &self.window {
                    // Informational rather than Critical: critical bobs
                    // the dock indefinitely on macOS, which is overkill
                    // for a routine bell. Informational is more "look at
                    // me when you have a moment".
                    window.request_user_attention(Some(
                        winit::window::UserAttentionType::Informational,
                    ));
                }
            }
        }
    }

    /// Run a configurable [`Action`] in response to a keybinding match.
    fn run_action(
        &mut self,
        action: Action,
    ) {
        match action {
            Action::ScrollPageUp => {
                self.terminal
                    .scroll_viewport_up(self.terminal.viewport.rows);
            }
            Action::ScrollPageDown => {
                self.terminal
                    .scroll_viewport_down(self.terminal.viewport.rows);
            }
            Action::Copy => {
                if self.terminal.has_selection() {
                    self.terminal.copy_selection(ClipboardKind::Clipboard);
                }
            }
            Action::Paste => {
                self.terminal.reset_viewport();
                self.terminal.paste_from_clipboard(ClipboardKind::Clipboard);
                self.flush_pending();
            }
            Action::OpenSearch => {
                self.terminal.open_search();
            }
            Action::ScrollPrevPrompt => {
                self.terminal.scroll_to_prev_prompt();
            }
            Action::ScrollNextPrompt => {
                self.terminal.scroll_to_next_prompt();
            }
        }
    }

    /// Route a keystroke into the search bar. Escape closes, Backspace
    /// trims the query, Enter / Shift+Enter navigate matches, and printable
    /// characters are appended to the query. Every other key is swallowed
    /// so the host doesn't accidentally write the key to the PTY behind
    /// the bar's back.
    fn handle_search_input(
        &mut self,
        event: &winit::event::KeyEvent,
    ) {
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                self.terminal.close_search();
            }
            Key::Named(NamedKey::Backspace) => {
                self.terminal.search_backspace();
            }
            Key::Named(NamedKey::Enter) => {
                if self.modifiers.shift_key() {
                    self.terminal.search_prev();
                } else {
                    self.terminal.search_next();
                }
            }
            Key::Named(NamedKey::Space) => {
                // Space arrives as a named key on most winit backends, not
                // as `Key::Character(" ")`, so multi-word queries need this
                // explicit arm or the space is silently swallowed.
                self.terminal.search_append(" ");
            }
            Key::Character(s) => {
                // Feed the text verbatim — winit has already applied
                // Shift/AltGr to produce the character the user sees.
                self.terminal.search_append(s);
            }
            _ => {}
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
        let time_slice = Instant::now() + Duration::from_millis(5);
        let mut buf = [0u8; 128 * 1024];
        while let Ok(n) = self.pty.read(&mut buf) {
            if n == 0 {
                break;
            }
            self.terminal.process(&buf[..n]);
            if Instant::now() >= time_slice {
                break;
            }
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

impl ApplicationHandler<AppEvent> for App {
    fn user_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        event: AppEvent,
    ) {
        match event {
            AppEvent::ReloadConfig => self.reload_config(),
        }
    }

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
        event_loop.set_control_flow(ControlFlow::WaitUntil(
            Instant::now() + Duration::from_millis(8),
        ));

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

            WindowEvent::Focused(focused) => {
                self.terminal.report_focus_change(focused);
                self.flush_pending();
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
                        // Ctrl+click on a hyperlink opens the target — checked
                        // before selection so the click doesn't also drop a
                        // single-cell anchor that would block the next drag.
                        if self.modifiers.control_key()
                            && let Some(url) = self.terminal.hyperlink_at(row, col)
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

                // Search-bar routing runs ahead of keybindings and PTY
                // encoding so every keystroke while the bar is open lands
                // in the query buffer — including plain letters that would
                // otherwise produce PTY input. Escape closes; Enter /
                // Shift+Enter step through matches.
                if self.terminal.search_active() {
                    self.handle_search_input(&event);
                    return;
                }

                // Configurable keybindings run before any of the legacy
                // input encoders so the user's overrides win — including
                // disabling the previous defaults by omitting them.
                if let Some(action) = self.keybindings.lookup(&event.logical_key, self.modifiers) {
                    self.run_action(action);
                    return;
                }

                // Kitty keyboard protocol takes precedence: when the app has
                // pushed flags onto the stack, use the disambiguating
                // encoding so combos like Ctrl+Enter and Ctrl+I survive
                // round-tripping. Returns None to fall through to legacy.
                let kitty_flags = self.terminal.kitty_keyboard.current();
                if let Some(bytes) =
                    kitty_encode_input(&event.logical_key, self.modifiers, kitty_flags)
                {
                    self.terminal.reset_viewport();
                    let _ = self.pty.write(&bytes);
                    return;
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
                self.sync_window_title();
                self.dispatch_bell();
                // Mode 2026: while a synchronized update is open, parse PTY
                // bytes but skip presenting so apps never show a half-drawn
                // frame. The timeout inside `is_synchronized_update_active`
                // keeps us from freezing if the app never sends ESU.
                if !self.terminal.is_synchronized_update_active()
                    && let Some(renderer) = &mut self.renderer
                {
                    renderer.render(&mut self.font_system, &self.terminal);
                }

                self.request_redraw();
            }

            _ => {}
        }
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

/// Pack winit modifier state into the kitty wire format: bit 0 = shift,
/// bit 1 = alt, bit 2 = ctrl, bit 3 = super. Encoded values are this byte
/// plus 1 (so a key with no modifiers reports `1`, not `0`).
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

/// Encode a key event in the kitty keyboard protocol format if the active
/// `flags` call for it. Returns `None` to fall through to the legacy
/// xterm-style encoding (plain text, single control bytes, traditional CSI
/// arrows, …) — that's the right behaviour both when no flags are active
/// and when the key+modifier combo isn't ambiguous under DISAMBIGUATE.
fn kitty_encode_input(
    key: &Key,
    mods: ModifiersState,
    flags: KittyFlags,
) -> Option<Vec<u8>> {
    if !flags.contains(KittyFlags::DISAMBIGUATE_ESCAPE_CODES) {
        return None;
    }

    let mod_bits = kitty_modifier_bits(mods);
    // Shift alone is never disambiguating — capital letters and shifted
    // punctuation already produce distinct bytes via the OS layout.
    let only_shift_or_none = (mod_bits & !1) == 0;
    let mod_param = mod_bits + 1;

    match key {
        Key::Character(s) => {
            if only_shift_or_none {
                return None;
            }
            // Per spec, the key code is the codepoint of the unmodified key
            // — i.e. the lowercased form. Pick the first char; combining
            // sequences (extremely rare for a single keypress) fall through
            // to legacy.
            let lower = s.to_lowercase();
            let cp = lower.chars().next()? as u32;
            Some(format!("\x1b[{cp};{mod_param}u").into_bytes())
        }
        Key::Named(named) => kitty_encode_named(*named, mod_bits, mod_param),
        _ => None,
    }
}

/// Functional-key encoding under the kitty protocol. Plain (no-mod) presses
/// keep the legacy bytes — apps without DISAMBIGUATE knowledge still need
/// `\r` for Enter — but any modifier triggers a CSI form so combos like
/// Ctrl+Enter and Alt+ArrowLeft become unambiguous.
fn kitty_encode_named(
    named: NamedKey,
    mod_bits: u8,
    mod_param: u8,
) -> Option<Vec<u8>> {
    // Keys whose legacy encoding is a single byte get the `CSI codepoint ; mods u`
    // shape. The codepoint is the C0 byte the legacy form would produce.
    let direct_code = match named {
        NamedKey::Enter => Some(13u32),
        NamedKey::Tab => Some(9),
        NamedKey::Backspace => Some(127),
        NamedKey::Escape => Some(27),
        NamedKey::Space => Some(32),
        _ => None,
    };
    if let Some(cp) = direct_code {
        // Shift alone passes through to legacy — Shift+Tab is the one
        // exception apps actually want as CSI Z, but the spec lets
        // disambiguate-only mode emit it as `CSI 9;2u` and TUIs handle that.
        if (mod_bits & !1) == 0 && mod_bits == 0 {
            return None;
        }
        return Some(format!("\x1b[{cp};{mod_param}u").into_bytes());
    }

    // Cursor / nav keys with modifiers: use the long-standing xterm modifier
    // form (`CSI 1 ; mods <letter>` for arrows + Home/End, `CSI N ; mods ~`
    // for the tilde family). It pre-dates kitty by decades and TUIs that
    // request kitty mode also handle these correctly.
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

fn main() {
    env_logger::init();

    let config_path = config::config_file_path();
    let config = match config_path.as_deref() {
        Some(p) => config::load_from(p),
        None => Config::default(),
    };
    let pty = Pty::spawn(INITIAL_COLS as u16, INITIAL_ROWS as u16).expect("failed to spawn PTY");

    let event_loop: EventLoop<AppEvent> = EventLoop::with_user_event()
        .build()
        .expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    // Spawn the file watcher before running the loop so we don't miss a
    // save during startup. The watcher owns its own thread (via notify)
    // and only needs the proxy to forward reload notifications back.
    if let Some(ref path) = config_path {
        spawn_config_watcher(path.clone(), event_loop.create_proxy());
    }

    let mut app = App::new(pty, config, config_path);
    event_loop.run_app(&mut app).expect("run event loop");
}

/// Watch the config file for modifications and post `AppEvent::ReloadConfig`
/// onto the winit event loop whenever it changes. We watch the *parent*
/// directory rather than the file itself because many editors save by
/// writing to a temp file and renaming over the original, which would
/// invalidate a file-level watch on the first save.
fn spawn_config_watcher(
    config_path: PathBuf,
    proxy: EventLoopProxy<AppEvent>,
) {
    use notify::EventKind;
    use notify::RecursiveMode;
    use notify::Watcher;

    let Some(dir) = config_path.parent().map(PathBuf::from) else {
        return;
    };

    // The watcher needs to outlive this function; stash it on the spawned
    // thread so its `Drop` runs only when the process exits.
    std::thread::Builder::new()
        .name("config-watcher".into())
        .spawn(move || {
            let target = config_path.clone();
            let proxy_for_handler = proxy.clone();
            let mut watcher = match notify::recommended_watcher(move |res| {
                let event: notify::Event = match res {
                    Ok(e) => e,
                    Err(e) => {
                        warn!("config watcher error: {e}");
                        return;
                    }
                };
                // Only react to events touching the config file itself.
                // Atomic-rename saves can show up as `Create` or `Modify`
                // depending on the editor; both are reload triggers.
                let touches_config = event.paths.iter().any(|p| p == &target);
                if !touches_config {
                    return;
                }
                if !matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                    return;
                }
                if proxy_for_handler
                    .send_event(AppEvent::ReloadConfig)
                    .is_err()
                {
                    // Event loop is gone — process is shutting down.
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
            // Park the thread; the watcher uses its own internal thread to
            // drive the callback. Dropping `watcher` here would unsubscribe
            // immediately, so we keep it alive by parking forever.
            std::thread::park();
            drop(watcher);
        })
        .expect("spawn config watcher");
}

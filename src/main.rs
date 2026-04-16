#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod clipboard;
mod config;
mod font;
mod image;
mod keybindings;
mod pty;
mod renderer;
mod search;
mod selection;
mod terminal;
mod vte;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::LazyLock;
use std::time::Duration;
use std::time::Instant;

use clipboard::ClipboardKind;
use config::BellMode;
use config::Config;
use font::FontSystem;
use keybindings::Action;
use pty::Pty;
use renderer::Renderer;
use resvg::tiny_skia::Pixmap;
use resvg::usvg;
use resvg::usvg::Transform;
use selection::SelectionMode;
use terminal::KittyFlags;
use terminal::KittyKeys;
use terminal::MouseButton as TermMouseButton;
use terminal::MouseEventKind;
use terminal::MouseModifiers;
use terminal::Terminal;
use wgpu::PowerPreference;
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
use winit::window::Icon;
use winit::window::Window;
use winit::window::WindowId;

#[macro_use]
extern crate log;

const INITIAL_COLS: u32 = 80;
const INITIAL_ROWS: u32 = 24;

const ICON_BYTES: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/resources/icon.svg"));
static ICON: LazyLock<Icon> = LazyLock::new(|| {
    let opts = usvg::Options::default();
    let tree = usvg::Tree::from_str(ICON_BYTES, &opts).expect("failed to parse icon SVG");
    let mut pixmap = Pixmap::new(256, 256).expect("failed to create pixmap for icon");
    resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());

    let width = pixmap.width();
    let height = pixmap.height();

    Icon::from_rgba(pixmap.take(), width, height).expect("failed to create icon")
});

/// Stable identifier for a tab. Monotonically increasing; never reused, so
/// background threads that race with a tab close can't accidentally address
/// the wrong session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

/// Custom event type for the winit event loop. Keeps cross-thread plumbing
/// (config watcher, future async tasks) out of the main `WindowEvent` flow.
#[derive(Debug, Clone)]
enum AppEvent {
    /// The config file changed on disk and should be re-read + applied.
    ReloadConfig,
    DataReady(TabId),
    ChildExited(TabId),
}

struct Tab {
    id: TabId,
    terminal: Terminal,
    pty: Pty,
}

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    tabs: Vec<Tab>,
    active_tab_id: TabId,
    next_tab_id: u64,
    font_system: FontSystem,
    proxy: EventLoopProxy<AppEvent>,

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
    /// Whether the shell-integration gutter is currently enabled. Mirrored
    /// on [`Renderer`] but also kept here so `reload_config` can detect
    /// changes without having to probe the renderer (which may not exist
    /// yet before `resumed` runs).
    gutter_enabled: bool,
    power_preference: PowerPreference,
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

    /// Set to true when the last tab is closed manually. Checked on the
    /// next event loop iteration to call `event_loop.exit()`.
    exit_requested: bool,
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
        font_system: FontSystem,
        proxy: EventLoopProxy<AppEvent>,
        config_path: Option<PathBuf>,
    ) -> Self {
        let mut terminal = Terminal::new(
            INITIAL_COLS,
            INITIAL_ROWS,
            config.scrollback_lines,
            font_system.cell_height,
            font_system.cell_width,
        );
        terminal.set_default_cursor_style(config.cursor_style);
        let first_id = TabId(0);
        Self {
            window: None,
            renderer: None,
            tabs: vec![Tab {
                id: first_id,
                terminal,
                pty,
            }],
            active_tab_id: first_id,
            next_tab_id: 1,
            font_system,
            proxy,
            opacity: config.opacity,
            config_path,
            boot_fonts: config.fonts.clone(),
            boot_font_size: config.font_size,
            boot_opacity: config.opacity,
            keybindings: config.keybindings,
            bell_mode: config.bell,
            gutter_enabled: config.gutter,
            power_preference: config.power_preference,
            applied_title: None,
            modifiers: winit::keyboard::ModifiersState::default(),
            mouse_pos: PhysicalPosition::new(0.0, 0.0),
            mouse_buttons: MouseButtonState::default(),
            last_motion_cell: None,
            last_click_time: None,
            last_click_cell: None,
            click_count: 0,
            left_drag_active: false,
            exit_requested: false,
        }
    }

    fn active_tab(&self) -> &Tab {
        self.tabs
            .iter()
            .find(|t| t.id == self.active_tab_id)
            .expect("active tab must exist")
    }

    fn active_tab_mut(&mut self) -> &mut Tab {
        self.tabs
            .iter_mut()
            .find(|t| t.id == self.active_tab_id)
            .expect("active tab must exist")
    }

    fn tab_bar_visible(&self) -> bool {
        self.tabs.len() >= 2
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

        for tab in &mut self.tabs {
            tab.terminal.set_default_cursor_style(cfg.cursor_style);
            tab.terminal.set_scrollback_limit(cfg.scrollback_lines);
        }
        self.keybindings = cfg.keybindings;
        self.bell_mode = cfg.bell;

        // Toggling the gutter changes how many text columns fit in the
        // window, so after flipping the flag on the renderer we replay
        // the resize path. Skipped silently if the renderer hasn't been
        // created yet (pre-`resumed` reloads).
        if cfg.gutter != self.gutter_enabled {
            self.gutter_enabled = cfg.gutter;
            if let (Some(renderer), Some(window)) = (self.renderer.as_mut(), &self.window) {
                renderer.set_gutter_enabled(cfg.gutter);
                let size = window.inner_size();
                let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
                let usable_width = size.width.saturating_sub(gutter_px);
                let tab_bar_px = if self.tab_bar_visible() {
                    self.font_system.cell_height
                } else {
                    0
                };
                let usable_height = size.height.saturating_sub(tab_bar_px);
                let (cols, rows) = self
                    .font_system
                    .grid_dimensions(usable_width, usable_height);
                for tab in &mut self.tabs {
                    tab.terminal.resize(cols, rows);
                    tab.pty.resize(cols as u16, rows as u16);
                }
            }
            self.request_redraw();
        }

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
        if self.power_preference != cfg.power_preference {
            warn!(
                "config: power_preference changed ({:?} → {:?}); restart to apply",
                self.power_preference, cfg.power_preference
            );
        }
    }

    /// Push the foreground app's OSC 0 / OSC 2 title onto the OS window
    /// when it changes. Falls back to `term41` when the app clears the
    /// title (or never set one) so the user always has a recognisable
    /// label in their window list.
    fn sync_window_title(&mut self) {
        let tab = self.active_tab();
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
        let Some(window) = &self.window else {
            return;
        };
        match want.as_deref() {
            Some(t) => {
                debug!("sync_window_title: set to {t:?}");
                window.set_title(t);
            }
            None => {
                debug!("sync_window_title: cleared to default");
                window.set_title("term41");
            }
        }
        self.applied_title = want;
    }

    /// Drain the bell flag and act on it according to the configured
    /// [`BellMode`]. Polled once per frame so a tight loop of BELs only
    /// produces one user-visible reaction per frame, not a queue of
    /// stacked flashes / urgency requests.
    fn dispatch_bell(&mut self) {
        if !self.active_tab_mut().terminal.take_bell_pending() {
            return;
        }
        match self.bell_mode {
            BellMode::Off => {}
            BellMode::Visual => {
                if let Some(renderer) = self.renderer.as_mut() {
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
                let tab = self.active_tab_mut();
                tab.terminal.scroll_viewport_up(tab.terminal.viewport.rows);
            }
            Action::ScrollPageDown => {
                let tab = self.active_tab_mut();
                tab.terminal
                    .scroll_viewport_down(tab.terminal.viewport.rows);
            }
            Action::Copy => {
                let tab = self.active_tab_mut();
                if tab.terminal.has_selection() {
                    tab.terminal.copy_selection(ClipboardKind::Clipboard);
                }
            }
            Action::Paste => {
                let tab = self.active_tab_mut();
                tab.terminal.reset_viewport();
                tab.terminal.paste_from_clipboard(ClipboardKind::Clipboard);
                self.flush_pending();
            }
            Action::OpenSearch => {
                self.active_tab_mut().terminal.open_search();
            }
            Action::ScrollPrevPrompt => {
                self.active_tab_mut().terminal.scroll_to_prev_prompt();
            }
            Action::ScrollNextPrompt => {
                self.active_tab_mut().terminal.scroll_to_next_prompt();
            }
            Action::OpenNewWindow => {
                self.spawn_new_window();
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

    /// Fork a detached copy of this binary, seeded with the current
    /// session's working directory. Relies on OSC 7 to know the shell's
    /// cwd; falls back to inheriting this process's cwd when the shell
    /// never reported one.
    fn spawn_new_window(&self) {
        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                warn!("open-new-window: cannot locate term41 binary: {e}");
                return;
            }
        };

        let mut cmd = std::process::Command::new(&exe);
        match self.active_tab().terminal.current_directory.as_ref() {
            Some(cwd) => {
                cmd.current_dir(cwd);
            }
            None => {
                debug!("open-new-window: no OSC 7 cwd available, inheriting parent");
            }
        }

        match cmd.spawn() {
            Ok(child) => {
                // Detach from the child without leaving a zombie: a
                // watcher thread reaps the exit status and drops it.
                // Losing the PID isn't a problem — new windows are
                // independent by design.
                let mut child = child;
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

    /// Route a keystroke into the search bar. Escape closes, Backspace
    /// trims the query, Enter / Shift+Enter navigate matches, and printable
    /// characters are appended to the query. Every other key is swallowed
    /// so the host doesn't accidentally write the key to the PTY behind
    /// the bar's back.
    fn handle_search_input(
        &mut self,
        event: &winit::event::KeyEvent,
    ) {
        let shift = self.modifiers.shift_key();
        let terminal = &mut self.active_tab_mut().terminal;
        match &event.logical_key {
            Key::Named(NamedKey::Escape) => {
                terminal.close_search();
            }
            Key::Named(NamedKey::Backspace) => {
                terminal.search_backspace();
            }
            Key::Named(NamedKey::Enter) => {
                if shift {
                    terminal.search_prev();
                } else {
                    terminal.search_next();
                }
            }
            Key::Named(NamedKey::Space) => {
                terminal.search_append(" ");
            }
            Key::Character(s) => {
                terminal.search_append(s);
            }
            _ => {}
        }
    }

    fn cell_at(
        &self,
        pos: PhysicalPosition<f64>,
    ) -> (u32, u32) {
        let raw_x = pos.x.max(0.0) as u32;
        let raw_y = pos.y.max(0.0) as u32;
        // When the tab bar is visible, subtract its height so terminal
        // row 0 starts just below it.
        let tab_bar_px = if self.tab_bar_visible() {
            self.font_system.cell_height
        } else {
            0
        };
        let y = raw_y.saturating_sub(tab_bar_px);
        // Clicks inside the gutter map to col 0 (clamp-to-left) so users
        // don't accidentally start selections on the gutter strip.
        let gutter_px = self
            .renderer
            .as_ref()
            .map(|r| r.gutter_width_px(self.font_system.cell_width))
            .unwrap_or(0);
        let x = raw_x.saturating_sub(gutter_px);
        let tab = self.active_tab();
        let cols = tab.terminal.viewport.cols.saturating_sub(1);
        let rows = tab.terminal.viewport.rows.saturating_sub(1);
        (
            (x / self.font_system.cell_width).min(cols),
            (y / self.font_system.cell_height).min(rows),
        )
    }

    /// Returns true when the mouse position is inside the tab bar area.
    fn is_in_tab_bar(
        &self,
        pos: PhysicalPosition<f64>,
    ) -> bool {
        self.tab_bar_visible() && (pos.y.max(0.0) as u32) < self.font_system.cell_height
    }

    fn mouse_modifiers(&self) -> MouseModifiers {
        MouseModifiers {
            shift: self.modifiers.shift_key(),
            alt: self.modifiers.alt_key(),
            ctrl: self.modifiers.control_key(),
        }
    }

    fn flush_pending(&mut self) {
        let tab = self.active_tab_mut();
        let bytes = tab.terminal.take_pending_output();
        if !bytes.is_empty() {
            let _ = tab.pty.write(&bytes);
            tab.terminal.reset_viewport();
        }
    }

    /// Mouse events are forwarded to the foreground app when it has
    /// requested tracking and the shift bypass isn't active.
    fn forward_mouse_to_app(&self) -> bool {
        self.active_tab().terminal.mouse_tracking_enabled() && !self.modifiers.shift_key()
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

    /// Map a click position in the tab bar to the clicked tab and switch
    /// to it. Uses the same equal-width layout as `render_tab_bar`.
    fn handle_tab_bar_click(
        &mut self,
        pos: PhysicalPosition<f64>,
    ) {
        if self.tabs.is_empty() {
            return;
        }
        let cell_w = self.font_system.cell_width as f32;
        let surface_w = self
            .renderer
            .as_ref()
            .map(|_| {
                self.window
                    .as_ref()
                    .map(|w| w.inner_size().width as f32)
                    .unwrap_or(0.0)
            })
            .unwrap_or(0.0);
        let max_tab_w = cell_w * 30.0;
        let tab_w = (surface_w / self.tabs.len() as f32).min(max_tab_w);
        let clicked_idx = (pos.x.max(0.0) as f32 / tab_w) as usize;
        if let Some(tab) = self.tabs.get(clicked_idx) {
            self.active_tab_id = tab.id;
            self.request_redraw();
        }
    }

    fn spawn_new_tab(&mut self) {
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;

        // Inherit the active tab's working directory when available.
        let cwd = self.active_tab().terminal.current_directory.clone();

        // Derive grid dimensions accounting for a tab bar that may not
        // have been visible before (going from 1 → 2 tabs).
        let was_single = self.tabs.len() == 1;

        let (cols, rows) = if let (Some(window), Some(renderer)) = (&self.window, &self.renderer) {
            let size = window.inner_size();
            let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
            let usable_width = size.width.saturating_sub(gutter_px);
            // The tab bar will now be visible (2+ tabs).
            let tab_bar_px = self.font_system.cell_height;
            let usable_height = size.height.saturating_sub(tab_bar_px);
            self.font_system
                .grid_dimensions(usable_width, usable_height)
        } else {
            (INITIAL_COLS, INITIAL_ROWS)
        };

        let scrollback = self.active_tab().terminal.active.grid.scrollback_limit;
        let mut terminal = Terminal::new(
            cols,
            rows,
            scrollback,
            self.font_system.cell_height,
            self.font_system.cell_width,
        );
        terminal.set_default_cursor_style(self.active_tab().terminal.cursor_style);

        let pty = match Pty::spawn(
            id,
            cols as u16,
            rows as u16,
            self.font_system.cell_width as u16,
            self.font_system.cell_height as u16,
            None,
            cwd,
            self.proxy.clone(),
        ) {
            Ok(pty) => pty,
            Err(e) => {
                warn!("failed to spawn new tab: {e}");
                return;
            }
        };

        self.tabs.push(Tab { id, terminal, pty });
        self.active_tab_id = id;

        // Tab bar just appeared — all terminals need to shrink by one row.
        if was_single {
            self.recalculate_grid_size();
        }
        self.request_redraw();
    }

    fn close_active_tab(&mut self) {
        let tab_id = self.active_tab_id;
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            self.exit_requested = true;
            return;
        }
        let new_idx = idx.min(self.tabs.len() - 1);
        self.active_tab_id = self.tabs[new_idx].id;
        self.recalculate_grid_size();
        self.request_redraw();
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
        self.request_redraw();
    }

    fn handle_child_exited(
        &mut self,
        tab_id: TabId,
        event_loop: &ActiveEventLoop,
    ) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == tab_id) else {
            return;
        };
        let was_active = self.active_tab_id == tab_id;
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            event_loop.exit();
            return;
        }
        if was_active {
            let new_idx = idx.min(self.tabs.len() - 1);
            self.active_tab_id = self.tabs[new_idx].id;
        }
        // Tab bar visibility may have changed (2 -> 1), trigger resize.
        self.recalculate_grid_size();
        self.request_redraw();
    }

    /// Re-derive grid dimensions from the current window size and resize all
    /// tabs. Called when tab count crosses the 1/2 boundary (the tab bar
    /// appears or disappears, changing usable height).
    fn recalculate_grid_size(&mut self) {
        let Some(window) = &self.window else { return };
        let Some(ref mut renderer) = self.renderer else {
            return;
        };
        let size = window.inner_size();
        let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
        let usable_width = size.width.saturating_sub(gutter_px);
        let tab_bar_px = if self.tab_bar_visible() {
            self.font_system.cell_height
        } else {
            0
        };
        let usable_height = size.height.saturating_sub(tab_bar_px);
        let (cols, rows) = self
            .font_system
            .grid_dimensions(usable_width, usable_height);
        for tab in &mut self.tabs {
            tab.terminal.resize(cols, rows);
            tab.pty.resize(cols as u16, rows as u16);
        }
    }

    fn read_pty_output(
        &mut self,
        tab_id: TabId,
    ) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) else {
            return;
        };
        // Clear the coalesce flag before draining so any write the
        // reader does while we're here posts a fresh DataReady, rather
        // than being silently absorbed into the event we're already
        // servicing.
        tab.pty.clear_pending();
        let time_slice = Instant::now() + Duration::from_millis(5);
        let mut buf = [0u8; 128 * 1024];
        let mut bailed = false;
        loop {
            let read = tab.pty.read(&mut buf);
            if read == 0 {
                break;
            }
            tab.terminal.process(&buf[..read]);
            if Instant::now() >= time_slice {
                bailed = true;
                break;
            }
        }

        // If we stopped on the time slice (not an empty ring) there may
        // still be bytes waiting. The reader's coalesce will swallow its
        // own wakeup whenever `pending` is already set, so we re-arm +
        // re-post ourselves; otherwise a final partial drain would sit
        // stale until the child writes again.
        if bailed && tab.pty.arm_pending() {
            let _ = self.proxy.send_event(AppEvent::DataReady(tab_id));
        }

        self.request_redraw();
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
        event_loop: &ActiveEventLoop,
        event: AppEvent,
    ) {
        match event {
            AppEvent::ReloadConfig => self.reload_config(),
            AppEvent::DataReady(tab_id) => self.read_pty_output(tab_id),
            AppEvent::ChildExited(tab_id) => {
                self.handle_child_exited(tab_id, event_loop);
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

        let (pixel_width, pixel_height) = self.font_system.grid_size(INITIAL_COLS, INITIAL_ROWS);
        let transparent = self.opacity < 1.0;
        let attrs = Window::default_attributes()
            .with_title("term41")
            .with_transparent(transparent)
            .with_window_icon(Some(ICON.clone()))
            .with_inner_size(winit::dpi::PhysicalSize::new(pixel_width, pixel_height));

        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        let window_ = window.clone();

        self.renderer = Some(pollster::block_on(Renderer::new(
            window_,
            &mut self.font_system,
            self.opacity,
            self.gutter_enabled,
            self.power_preference,
        )));
        self.window = Some(window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        if self.exit_requested {
            event_loop.exit();
            return;
        }

        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }

            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size);
                    let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
                    let usable_width = size.width.saturating_sub(gutter_px);
                    let tab_bar_px = if self.tab_bar_visible() {
                        self.font_system.cell_height
                    } else {
                        0
                    };
                    let usable_height = size.height.saturating_sub(tab_bar_px);
                    let (cols, rows) = self
                        .font_system
                        .grid_dimensions(usable_width, usable_height);
                    for tab in &mut self.tabs {
                        tab.terminal.resize(cols, rows);
                        tab.pty.resize(cols as u16, rows as u16);
                    }
                }
            }

            WindowEvent::Focused(focused) => {
                self.active_tab_mut().terminal.report_focus_change(focused);
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
                if self.active_tab().terminal.mouse_tracking_enabled()
                    && !self.modifiers.shift_key()
                {
                    let (col, row) = self.cell_at(self.mouse_pos);
                    let mods = self.mouse_modifiers();
                    let terminal = &mut self.active_tab_mut().terminal;
                    let report = |term: &mut Terminal, button: TermMouseButton, steps: u32| {
                        for _ in 0..steps {
                            term.mouse_report(MouseEventKind::Press, button, col, row, mods);
                        }
                    };
                    if y_lines < 0 {
                        report(terminal, TermMouseButton::WheelUp, y_lines.unsigned_abs());
                    } else if y_lines > 0 {
                        report(terminal, TermMouseButton::WheelDown, y_lines as u32);
                    }
                    if x_lines < 0 {
                        report(terminal, TermMouseButton::WheelLeft, x_lines.unsigned_abs());
                    } else if x_lines > 0 {
                        report(terminal, TermMouseButton::WheelRight, x_lines as u32);
                    }
                    self.flush_pending();
                    return;
                }

                let terminal = &mut self.active_tab_mut().terminal;
                if y_lines < 0 {
                    terminal.scroll_viewport_up(y_lines.unsigned_abs());
                } else if y_lines > 0 {
                    terminal.scroll_viewport_down(y_lines as u32);
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
                    self.active_tab_mut().terminal.mouse_report(
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
                    self.active_tab_mut()
                        .terminal
                        .extend_selection(cell.0, cell.1);
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

                // Clicks in the tab bar switch tabs instead of reaching
                // the terminal. Only react on press, not release.
                if pressed && button == MouseButton::Left && self.is_in_tab_bar(self.mouse_pos) {
                    self.handle_tab_bar_click(self.mouse_pos);
                    return;
                }

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
                    self.active_tab_mut()
                        .terminal
                        .mouse_report(kind, term_button, col, row, mods);
                    self.flush_pending();
                    return;
                }

                let (col, row) = self.cell_at(self.mouse_pos);
                match (button, pressed) {
                    (MouseButton::Left, true) => {
                        if self.modifiers.control_key()
                            && let Some(url) = self.active_tab().terminal.hyperlink_at(row, col)
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
                        self.active_tab_mut()
                            .terminal
                            .start_selection(col, row, mode);
                        self.left_drag_active = true;
                    }
                    (MouseButton::Left, false) => {
                        self.left_drag_active = false;
                        let tab = self.active_tab_mut();
                        if tab.terminal.has_selection() {
                            tab.terminal.copy_selection(ClipboardKind::Primary);
                        } else {
                            tab.terminal.clear_selection();
                        }
                    }
                    (MouseButton::Right, true) => {
                        let tab = self.active_tab_mut();
                        if tab.terminal.has_selection() {
                            tab.terminal.copy_selection(ClipboardKind::Clipboard);
                            tab.terminal.clear_selection();
                        } else {
                            tab.terminal.reset_viewport();
                            tab.terminal.paste_from_clipboard(ClipboardKind::Clipboard);
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
                if self.active_tab().terminal.search_active() {
                    self.handle_search_input(&event);
                    return;
                }

                if let Some(action) = self.keybindings.lookup(&event.logical_key, self.modifiers) {
                    self.run_action(action);
                    return;
                }

                let kitty_flags = self.active_tab().terminal.kitty_keyboard.current();
                if let Some(bytes) =
                    kitty_encode_input(&event.logical_key, self.modifiers, kitty_flags)
                {
                    let tab = self.active_tab_mut();
                    tab.terminal.reset_viewport();
                    let _ = tab.pty.write(&bytes);
                    return;
                }

                if self.modifiers.control_key() {
                    let byte = match &event.logical_key {
                        Key::Character(c) => ctrl_byte(c),
                        Key::Named(NamedKey::Space) => Some(0x00),
                        _ => None,
                    };

                    if let Some(byte) = byte {
                        let tab = self.active_tab_mut();
                        tab.terminal.reset_viewport();
                        let _ = tab.pty.write(&[byte]);
                        return;
                    }
                }

                let bytes = match &event.logical_key {
                    Key::Character(c) => Some(c.as_bytes().to_vec()),
                    Key::Named(named) => named_key_to_bytes(*named),
                    _ => None,
                };

                if let Some(bytes) = bytes {
                    let tab = self.active_tab_mut();
                    tab.terminal.reset_viewport();
                    let _ = tab.pty.write(&bytes);
                }
            }

            WindowEvent::RedrawRequested => {
                self.sync_window_title();
                self.dispatch_bell();
                // Mode 2026: while a synchronized update is open, parse PTY
                // bytes but skip presenting so apps never show a half-drawn
                // frame. The timeout inside `is_synchronized_update_active`
                // keeps us from freezing if the app never sends ESU.
                let active_idx = self
                    .tabs
                    .iter()
                    .position(|t| t.id == self.active_tab_id)
                    .expect("active tab must exist");
                let synced = self.tabs[active_idx]
                    .terminal
                    .is_synchronized_update_active();
                if !synced {
                    let tab_infos: Vec<renderer::TabInfo> = if self.tab_bar_visible() {
                        self.tabs
                            .iter()
                            .map(|t| renderer::TabInfo {
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
                        );
                    }
                }

                event_loop.set_control_flow(ControlFlow::WaitUntil(
                    Instant::now() + Duration::from_millis(8),
                ));
                self.request_redraw();
            }

            _ => {}
        }

        self.flush_pending();
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

/// Parses positional CLI arguments into a command to run in the PTY.
///
/// `term41 <program> [args...]` runs the given program in place of the
/// default shell. A leading `--` is consumed as an optional separator so
/// that future term41 flags can coexist with commands whose arguments
/// start with a dash. Returns `None` when no command was supplied.
fn parse_command_args() -> Option<Vec<String>> {
    let mut args = std::env::args();
    let _argv0 = args.next();
    let mut rest: Vec<String> = args.collect();
    if rest.first().map(String::as_str) == Some("--") {
        rest.remove(0);
    }
    if rest.is_empty() { None } else { Some(rest) }
}

fn main() {
    env_logger::init();

    let command = parse_command_args();

    let config_path = config::config_file_path();
    let config = match config_path.as_deref() {
        Some(p) => config::load_from(p),
        None => Config::default(),
    };

    let event_loop: EventLoop<AppEvent> = EventLoop::with_user_event()
        .build()
        .expect("create event loop");

    let font_system = FontSystem::new(config.fonts.clone(), config.font_size);

    let pty = Pty::spawn(
        TabId(0),
        INITIAL_COLS as u16,
        INITIAL_ROWS as u16,
        font_system.cell_width as u16,
        font_system.cell_height as u16,
        command,
        None,
        event_loop.create_proxy(),
    )
    .expect("failed to spawn PTY");

    // Spawn the file watcher before running the loop so we don't miss a
    // save during startup. The watcher owns its own thread (via notify)
    // and only needs the proxy to forward reload notifications back.
    if let Some(ref path) = config_path {
        spawn_config_watcher(path.clone(), event_loop.create_proxy());
    }

    let mut app = App::new(
        pty,
        config,
        font_system,
        event_loop.create_proxy(),
        config_path,
    );
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
        })
        .expect("spawn config watcher");
}

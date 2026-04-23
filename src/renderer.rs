pub mod background;
pub mod glyph_atlas;
pub mod image_atlas;
mod r#impl;
pub(crate) mod paint;
mod shelf;
pub(crate) mod startup;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use clip41::Clipboard;
use font41::FontSystem;
use parking_lot::Mutex;
use pty_pipe41::Pty;
use terminal41::C1Mode;
use terminal41::KittyFlags;
use terminal41::KittyKeys;
use terminal41::Terminal;
use terminal41::TerminalThread;
use terminal41::host;
use terminal41::settings;
use winit::event_loop::EventLoopProxy;
use winit::event_loop::OwnedDisplayHandle;
use winit::keyboard::Key;
use winit::keyboard::KeyCode;
use winit::keyboard::KeyLocation;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::keyboard::PhysicalKey;
use winit::window::Window;

use crate::APP_START_TIME;
use crate::AppEvent;
use crate::INITIAL_COLS;
use crate::INITIAL_ROWS;
use crate::InputState;
use crate::Tab;
use crate::TabId;
use crate::config::BellMode;
use crate::config::Config;
use crate::config::DEFAULT_SCROLLBACK;
use crate::keybindings::Action;
use crate::output_recording::RecorderControl;
use crate::renderer::r#impl::Renderer;
use crate::renderer::r#impl::TabInfo;
use crate::renderer::r#impl::WindowControls;
pub use crate::renderer::r#impl::compute_gutter_width;

// ---------------------------------------------------------------------------
// Gutter popup — shown on click of a shell-integration gutter marker
// ---------------------------------------------------------------------------

pub(crate) struct GutterMenuItem {
    pub label: &'static str,
}

pub(crate) const GUTTER_MENU_ITEMS: &[GutterMenuItem] = &[
    GutterMenuItem { label: "Rerun" },
    GutterMenuItem {
        label: "Copy command",
    },
    GutterMenuItem {
        label: "Copy cmd+output",
    },
    GutterMenuItem {
        label: "Copy output",
    },
];

pub(crate) const POPUP_WIDTH_CELLS: f32 = 20.0;
const FRAME_DURATION: Duration = Duration::from_millis(1000 / 60);

/// State of the gutter popup while it is open.
#[derive(Clone)]
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

// ---------------------------------------------------------------------------
// CSD — client-side window decoration state
// ---------------------------------------------------------------------------

/// Number of cell-widths reserved for each window control button.
pub(crate) const BUTTON_CELLS: f32 = 3.0;

/// Total width of the window-control button region in cell-width units.
pub(crate) const BUTTONS_REGION_CELLS: f32 = BUTTON_CELLS * 3.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TabBarHover {
    NewTab,
    Minimize,
    Maximize,
    Close,
}

// ---------------------------------------------------------------------------
// Tab context menu — right-click on a tab in the tab bar
// ---------------------------------------------------------------------------

pub(crate) struct TabMenuItem {
    pub label: &'static str,
}

pub(crate) const TAB_MENU_ITEMS: &[TabMenuItem] = &[
    TabMenuItem { label: "New tab" },
    TabMenuItem { label: "Close tab" },
    TabMenuItem {
        label: "Close others",
    },
];

pub(crate) const TAB_MENU_WIDTH_CELLS: f32 = 16.0;

/// State of the tab context popup while it is open.
#[derive(Clone)]
pub(crate) struct TabContextMenu {
    pub tab_idx: usize,
    /// Pixel position where the popup was opened (used for placement).
    pub x: f32,
    /// Currently hovered menu-item index.
    pub hovered_item: Option<usize>,
}

#[derive(Clone)]
pub(crate) struct RecordingPopup {
    pub lines: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionChoice {
    Allow,
    Deny,
}

#[derive(Clone)]
pub(crate) struct PermissionModal {
    pub feature: String,
    pub hovered: Option<PermissionChoice>,
}

#[derive(Clone)]
pub(crate) struct Toast {
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PermissionButtonLayout {
    pub yes: (f32, f32, f32, f32),
    pub no: (f32, f32, f32, f32),
}

pub(crate) fn permission_modal_button_at(
    feature: &str,
    x: f32,
    y: f32,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    tab_bar_h: f32,
) -> Option<PermissionChoice> {
    let layout = permission_button_layout(feature, cell_w, cell_h, surface_w, surface_h, tab_bar_h);
    if point_in_rect(x, y, layout.yes) {
        return Some(PermissionChoice::Allow);
    }
    if point_in_rect(x, y, layout.no) {
        return Some(PermissionChoice::Deny);
    }
    None
}

pub(crate) fn permission_button_layout(
    feature: &str,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    tab_bar_h: f32,
) -> PermissionButtonLayout {
    let panel = permission_panel_rect(feature, cell_w, cell_h, surface_w, surface_h, tab_bar_h);
    let button_y = panel.1 + 4.0 * cell_h;
    let yes_w = 7.0 * cell_w;
    let no_w = 6.0 * cell_w;
    let gap = 2.0 * cell_w;
    let buttons_w = yes_w + gap + no_w;
    let yes_x = panel.0 + (panel.2 - buttons_w) * 0.5;
    let no_x = yes_x + yes_w + gap;
    PermissionButtonLayout {
        yes: (yes_x, button_y, yes_w, cell_h),
        no: (no_x, button_y, no_w, cell_h),
    }
}

pub(crate) fn permission_panel_rect(
    feature: &str,
    cell_w: f32,
    cell_h: f32,
    surface_w: f32,
    surface_h: f32,
    tab_bar_h: f32,
) -> (f32, f32, f32, f32) {
    let feature_line = permission_feature_line(feature);
    let max_chars = feature_line
        .chars()
        .count()
        .max("Would you like to allow this?".chars().count())
        .max("[y]es   [n]o".chars().count());
    let panel_w = (max_chars as f32 + 4.0) * cell_w;
    let panel_h = 6.0 * cell_h;
    let panel_x = ((surface_w - panel_w) * 0.5).max(0.0);
    let panel_y = ((surface_h - panel_h + tab_bar_h) * 0.5).max(tab_bar_h);
    (panel_x, panel_y, panel_w, panel_h)
}

pub(crate) fn permission_feature_line(feature: &str) -> String {
    format!(
        "A program would like to use {}.",
        permission_feature_label(feature)
    )
}

fn permission_feature_label(feature: &str) -> String {
    let mut label = String::new();
    for (len, ch) in feature.chars().enumerate() {
        if len >= 32 {
            label.push_str("...");
            break;
        }
        if ch.is_control() {
            label.push(' ');
        } else {
            label.push(ch);
        }
    }
    label
}

fn point_in_rect(
    x: f32,
    y: f32,
    rect: (f32, f32, f32, f32),
) -> bool {
    let (rx, ry, rw, rh) = rect;
    x >= rx && x < rx + rw && y >= ry && y < ry + rh
}

fn resize_tab_to_grid(
    tab: &mut Tab,
    cols: u32,
    rows: u32,
) {
    let pty_rows = {
        let mut terminal = tab.terminal.lock();
        terminal.resize(cols, rows);
        terminal.viewport.rows
    };
    tab.pty.resize(cols as u16, pty_rows as u16);
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
    Bell(TabId),
    Resized {
        width: u32,
        height: u32,
    },
    Action(Action),
    SetActiveTab(usize),
    CloseTab(usize),
    CloseOtherTabs(usize),
    /// The window's DPI scale factor changed (e.g. moved to a different
    /// monitor). The render thread rescales font metrics and re-rasterizes
    /// glyphs.
    ScaleFactorChanged {
        scale_factor: f64,
    },
}

pub struct RenderHost {
    renderer: Option<Renderer>,
    event_rx: cueue::Reader<RenderEvent>,
    child_exit_rx: mpsc::Receiver<TabId>,
    child_exit_tx: mpsc::Sender<TabId>,
    config_reload: Arc<AtomicBool>,
    proxy: EventLoopProxy<AppEvent>,

    tabs: Vec<Tab>,
    active_tab_id: TabId,
    next_tab_id: u64,
    font_system: FontSystem,

    config_path: Option<PathBuf>,
    config: Config,

    applied_title: Option<String>,

    /// Last known window size in physical pixels. Updated on Resized events.
    window_size: (u32, u32),
    /// Monotonic counter for real window/grid changes. Tabs record the last
    /// epoch they were resized to so activation can reconcile stale tabs.
    window_resize_epoch: u64,

    /// Window handle, persisted after the first frame so IME requests
    /// (`set_ime_cursor_area`) can be issued from event handlers.
    window: Option<Arc<Window>>,
    render_thread_handle: Arc<OnceLock<Thread>>,

    /// Last pixel position/size we handed to `set_ime_cursor_area`. Used to
    /// skip redundant calls — winit queues each one to the main thread, so
    /// hammering it every frame would churn without value.
    ime_cursor_area: Option<(f32, f32, f32, f32)>,

    /// Window-level clipboard, separate from the per-tab terminal
    /// clipboards (which exist so OSC 52 sets are scoped per-tab). Used
    /// by `PasteAsBackground` to read image data from the system
    /// clipboard regardless of which tab is active.
    clipboard: Clipboard,
    input_state: Arc<Mutex<InputState>>,

    should_exit: bool,
}

/// Snapshot of the IME's current composition.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreeditState {
    pub text: String,
    pub cursor: Option<(usize, usize)>,
}

// ---------------------------------------------------------------------------
// Terminal thread — processes PTY data on its own thread so rendering and
// terminal updates are decoupled. The parser runs outside the terminal lock;
// only per-action state mutations hold the lock.
// ---------------------------------------------------------------------------

impl RenderHost {
    pub fn new(
        event_rx: cueue::Reader<RenderEvent>,
        child_exit_rx: mpsc::Receiver<TabId>,
        child_exit_tx: mpsc::Sender<TabId>,
        config_reload: Arc<AtomicBool>,
        proxy: EventLoopProxy<AppEvent>,
        font_system: FontSystem,
        tab: Tab,
        config: Config,
        config_path: Option<PathBuf>,
        input_state: Arc<Mutex<InputState>>,
        render_thread_handle: Arc<OnceLock<Thread>>,
    ) -> Self {
        Self {
            renderer: None,
            event_rx,
            child_exit_rx,
            child_exit_tx,
            config_reload,
            proxy,
            tabs: vec![tab],
            active_tab_id: TabId(0),
            next_tab_id: 1,
            font_system,
            config_path,
            config,
            applied_title: None,
            window_size: (0, 0),
            window_resize_epoch: 0,
            window: None,
            render_thread_handle,
            ime_cursor_area: None,
            clipboard: Clipboard::new(),
            input_state,
            should_exit: false,
        }
    }

    // -- Main loop ----------------------------------------------------------

    #[cfg_attr(feature = "software-only", allow(unused_variables, unreachable_code))]
    pub fn run(
        &mut self,
        window_rx: mpsc::Receiver<(Arc<Window>, OwnedDisplayHandle)>,
        startup_release_rx: mpsc::Receiver<()>,
    ) {
        #[cfg(feature = "software-only")]
        {
            info!("software-only rendering mode enabled; GPU features will be disabled");
            self.run_software_only(window_rx);
            return;
        }

        let mut frames = 0u64;
        let mut first_frame = true;

        // Phase 1: wait for the window and initialize the renderer.
        let (window, display) = match window_rx.recv() {
            Ok(wd) => wd,
            Err(_) => return,
        };
        self.window = Some(window.clone());
        self.sync_input_state();

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

        let mut last_frame_time = Instant::now();

        // Phase 2: frame loop.
        loop {
            if !first_frame {
                match self.next_render_wait(last_frame_time.elapsed()) {
                    Some(duration) => std::thread::park_timeout(duration),
                    None => std::thread::park(),
                }
            }
            last_frame_time = Instant::now();
            first_frame = false;

            // Batch-drain all pending input events from the window thread.
            // Clone into a local buffer so we can commit() (freeing ring
            // buffer slots for the writer) before processing, which also
            // avoids a borrow conflict with &mut self in handle_render_event.
            self.drain_render_events();

            // Drain child-exit notifications.
            self.drain_child_exit_notifications();

            // Hot-reload config if the watcher flagged a change.
            self.reload_config_if_requested();

            if self.should_exit || self.event_rx.is_abandoned() {
                break;
            }

            // Keep the IME's candidate popup anchored to the text cursor as
            // it moves (normal typing, cursor-movement escapes, etc.). The
            // call dedupes against the last position, so idle frames cost
            // one comparison and nothing else.
            self.update_ime_cursor_area();

            if self.renderer.is_none() {
                let prepared_renderer = tracing::debug_span!("prepare_renderer").in_scope(|| {
                    pollster::block_on(Renderer::prepare(
                        display.clone(),
                        self.config.power_preference,
                        effective_bg_path(&self.config),
                        self.config.background_opacity,
                        self.startup_snapshot_size(),
                        window.inner_size(),
                    ))
                });

                let _ = self.proxy.send_event(AppEvent::ReleaseStartupSurface);
                let _ = startup_release_rx.recv();
                // Surface the precedence rule once at startup so the user
                // isn't confused why their config edit appears to do
                // nothing — the pasted bg overrides until cleared.
                if let Some(pasted) = find_pasted_background()
                    && self.config.background_image.is_some()
                {
                    info!(
                        "background: pasted image at {} overrides config background_image; clear \
                         it via Ctrl+Shift+Backspace to revert",
                        pasted.display()
                    );
                }
                self.renderer = Some(tracing::debug_span!("create_renderer").in_scope(|| {
                    Renderer::from_prepared(
                        prepared_renderer,
                        window.clone(),
                        self.config.opacity,
                        self.config.gutter,
                        self.config.vsync,
                    )
                }));
            }

            self.renderer.as_mut().unwrap().advance_background_frame();
            self.render_frame();

            frames += 1;
            if frames.is_multiple_of(100) {
                debug!(
                    "rendering at {:0.0} fps",
                    frames as f64 / APP_START_TIME.get().unwrap().elapsed().as_secs_f64()
                );
            }
        }

        std::process::exit(0);
    }

    #[cfg(feature = "software-only")]
    fn run_software_only(
        &mut self,
        window_rx: mpsc::Receiver<(Arc<Window>, OwnedDisplayHandle)>,
    ) {
        let (window, _) = match window_rx.recv() {
            Ok(wd) => wd,
            Err(_) => return,
        };
        self.window = Some(window.clone());

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

        loop {
            std::thread::park();
            self.drain_render_events();
            self.drain_child_exit_notifications();
            self.reload_config_if_requested();

            if self.should_exit || self.event_rx.is_abandoned() {
                break;
            }

            self.update_ime_cursor_area();
        }

        std::process::exit(0);
    }

    fn drain_render_events(&mut self) {
        let events: Vec<RenderEvent> = self.event_rx.read_chunk().to_vec();
        self.event_rx.commit();
        for event in &events {
            self.handle_render_event(event);
        }
    }

    fn drain_child_exit_notifications(&mut self) {
        while let Ok(tab_id) = self.child_exit_rx.try_recv() {
            self.handle_child_exited(tab_id);
        }
    }

    fn reload_config_if_requested(&mut self) {
        if self.config_reload.swap(false, Ordering::Acquire) {
            self.reload_config();
        }
    }

    fn next_render_wait(
        &self,
        last_frame_duration: Duration,
    ) -> Option<Duration> {
        let Some(renderer) = self.renderer.as_ref() else {
            return Some(Duration::ZERO);
        };
        if renderer.has_animated_background() || renderer.visual_bell_active() {
            return Some(FRAME_DURATION.saturating_sub(last_frame_duration));
        }
        let tab = self.active_tab()?;
        let terminal = tab.terminal.lock();
        if terminal.active.offset == 0
            && terminal.active.cursor_visible
            && terminal.cursor_style.blink
        {
            Some(r#impl::CURSOR_BLINK_HALF_PERIOD.saturating_sub(last_frame_duration))
        } else {
            None
        }
    }

    // -- Event dispatch -----------------------------------------------------

    fn handle_render_event(
        &mut self,
        event: &RenderEvent,
    ) {
        match event {
            RenderEvent::None => {}
            RenderEvent::Bell(tab_id) => self.handle_bell(*tab_id),
            RenderEvent::Resized { width, height } => {
                self.window_size = (*width, *height);
                self.handle_resize(*width, *height);
            }
            RenderEvent::Action(action) => {
                self.run_action(*action);
            }
            RenderEvent::SetActiveTab(tab_idx) => self.set_active_tab(*tab_idx),
            RenderEvent::CloseOtherTabs(tab_idx) => self.close_other_tabs(*tab_idx),
            RenderEvent::CloseTab(tab_idx) => self.close_tab(*tab_idx),
            RenderEvent::ScaleFactorChanged { scale_factor } => {
                self.handle_scale_factor_changed(*scale_factor);
            }
        }
    }

    /// Tell winit where the IME should anchor its candidate popup: the
    /// pixel rect of the terminal's current cursor cell. Skipped when the
    /// cursor is scrolled off-screen or hidden, and deduplicated against
    /// the last value so we don't queue a request every frame.
    fn update_ime_cursor_area(&mut self) {
        let Some(window) = self.window.clone() else {
            return;
        };
        let Some(tab) = self.active_tab() else {
            return;
        };
        let terminal = tab.terminal.lock();
        // The compositor doesn't care about IME positioning when the user
        // has scrolled away from live output, and we don't want to signal
        // one — clear it so the popup doesn't stick to stale coordinates.
        if terminal.active.offset != 0 || !terminal.active.cursor_visible {
            return;
        }

        let cell_w = self.font_system.cell_width as f32;
        let cell_h = self.font_system.cell_height as f32;
        let gutter_px = self
            .renderer
            .as_ref()
            .map(|r| r.gutter_width_px(self.font_system.cell_width))
            .unwrap_or(0) as f32;
        let tab_bar_h = if self.tab_bar_visible() { cell_h } else { 0.0 };

        let cursor = terminal.active.cursor;
        drop(terminal);
        // Place the area at the row *below* the cursor so the popup doesn't
        // cover the cell the user is about to type into.
        let x = cursor.col as f32 * cell_w + gutter_px;
        let y = cursor.row as f32 * cell_h + tab_bar_h;

        let new_area = (x, y, cell_w, cell_h);
        if self.ime_cursor_area == Some(new_area) {
            return;
        }
        self.ime_cursor_area = Some(new_area);

        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(x as f64, y as f64),
            winit::dpi::PhysicalSize::new(cell_w as f64, cell_h as f64),
        );
    }

    // -- Actions ------------------------------------------------------------

    fn run_action(
        &mut self,
        action: Action,
    ) {
        match action {
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::Copy
            | Action::Paste
            | Action::OpenSearch
            | Action::ScrollPrevPrompt
            | Action::ScrollNextPrompt
            | Action::OpenNewWindow => {}
            Action::NewTab => {
                self.spawn_new_tab();
            }
            Action::CloseActiveTab => {
                self.close_active_tab();
            }
            Action::CloseWindow => {
                for _ in 0..self.tabs.len() {
                    self.close_tab(0);
                }
            }
            Action::NextTab => {
                self.switch_tab(1);
            }
            Action::PrevTab => {
                self.switch_tab(-1);
            }
            Action::PasteAsBackground => {
                self.handle_paste_as_background();
            }
            Action::ClearPastedBackground => {
                self.handle_clear_pasted_background();
            }
            Action::ToggleOutputRecording | Action::CycleEmojiCompatibility => {}
        }
    }

    // -- Background-image actions ------------------------------------------

    fn handle_paste_as_background(&mut self) {
        let Some(dir) = pasted_background_dir() else {
            warn!("paste-as-background: no data directory available on this platform");
            self.fire_ui_bell();
            return;
        };
        if let Err(e) = std::fs::create_dir_all(&dir) {
            warn!(
                "paste-as-background: failed to create {}: {e}",
                dir.display()
            );
            self.fire_ui_bell();
            return;
        }

        // Try raw clipboard bytes first — preserves GIF animation that
        // arboard's decoded-RGBA path would flatten to a single frame.
        if let Some(bytes) = clip41::get_raw_image_bytes()
            && let Some(kind) = infer::get(&bytes)
        {
            let ext = kind.extension();
            let path = dir.join(format!("pasted_background.{ext}"));
            clear_pasted_backgrounds();
            if let Err(e) = std::fs::write(&path, &bytes) {
                warn!(
                    "paste-as-background: failed to write {}: {e}",
                    path.display()
                );
                self.fire_ui_bell();
                return;
            }
            info!(
                "background: pasted {} saved to {} ({} bytes)",
                kind.mime_type(),
                path.display(),
                bytes.len()
            );
            let startup_snapshot_size = self.startup_snapshot_size();
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.set_background(
                    Some(&path),
                    self.config.background_opacity,
                    startup_snapshot_size,
                );
            }
            return;
        }

        // Fallback: arboard decoded RGBA → PNG. Handles cases where the
        // raw-bytes path isn't available (non-Linux, tools not installed,
        // or the clipboard holds a bitmap with no encoded-format version).
        let Some(img) = self.clipboard.get_image() else {
            warn!("paste-as-background: clipboard does not hold image data");
            self.fire_ui_bell();
            return;
        };
        let Some(path) = pasted_background_path("png") else {
            self.fire_ui_bell();
            return;
        };
        clear_pasted_backgrounds();
        if let Err(e) = encode_png_rgba(&path, img.width, img.height, &img.rgba) {
            warn!(
                "paste-as-background: failed to write {}: {e}",
                path.display()
            );
            self.fire_ui_bell();
            return;
        }
        info!(
            "background: pasted image saved to {} ({}x{})",
            path.display(),
            img.width,
            img.height
        );
        let startup_snapshot_size = self.startup_snapshot_size();
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_background(
                Some(&path),
                self.config.background_opacity,
                startup_snapshot_size,
            );
        }
    }

    fn handle_clear_pasted_background(&mut self) {
        let Some(path) = find_pasted_background() else {
            return;
        };
        clear_pasted_backgrounds();
        info!(
            "background: pasted image at {} cleared, reverting to config",
            path.display()
        );
        let startup_snapshot_size = self.startup_snapshot_size();
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_background(
                self.config.background_image.as_deref(),
                self.config.background_opacity,
                startup_snapshot_size,
            );
        }
    }

    /// Trigger the visual bell + urgent-attention paths that the configured
    /// bell mode would normally only fire for app-emitted BELs. UI-driven
    /// failures (paste-as-background with no image, etc.) get this
    /// regardless of `bell = "off"`: the user clicked something and got no
    /// feedback otherwise. The terminal bell config governs *app* bells,
    /// not UI errors that the user themselves initiated.
    fn fire_ui_bell(&mut self) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.notify_bell();
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
        self.sync_input_state();
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
        }
        if let Some((cols, rows)) = self.current_window_grid_size() {
            self.window_resize_epoch += 1;
            let epoch = self.window_resize_epoch;
            if let Some(tab) = self.active_tab_mut() {
                resize_tab_to_grid(tab, cols, rows);
                tab.window_sync_epoch = epoch;
            }
        }
        self.sync_input_state();
    }

    fn recalculate_grid_size(&mut self) {
        if let Some((cols, rows)) = self.current_window_grid_size() {
            self.window_resize_epoch += 1;
            let epoch = self.window_resize_epoch;
            if let Some(tab) = self.active_tab_mut() {
                resize_tab_to_grid(tab, cols, rows);
                tab.window_sync_epoch = epoch;
            }
        }
        self.sync_input_state();
    }

    fn current_window_grid_size(&self) -> Option<(u32, u32)> {
        let (width, height) = self.window_size;
        if width == 0 || height == 0 {
            return None;
        }
        let gutter_px = if self.config.gutter {
            self.renderer
                .as_ref()
                .map(|renderer| renderer.gutter_width_px(self.font_system.cell_width))
                .unwrap_or_else(|| compute_gutter_width(self.font_system.cell_width))
        } else {
            0
        };
        let usable_width = width.saturating_sub(gutter_px);
        let tab_bar_px = if self.tab_bar_visible() {
            self.font_system.cell_height
        } else {
            0
        };
        let usable_height = height.saturating_sub(tab_bar_px);
        Some(
            self.font_system
                .grid_dimensions(usable_width, usable_height),
        )
    }

    // -- Tab management -----------------------------------------------------

    fn active_tab(&self) -> Option<&Tab> {
        self.tabs.iter().find(|t| t.id == self.active_tab_id)
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.iter_mut().find(|t| t.id == self.active_tab_id)
    }

    fn tab_bar_visible(&self) -> bool {
        true
    }

    fn startup_snapshot_size(&self) -> (u32, u32) {
        let gutter = if self.config.gutter {
            compute_gutter_width(self.font_system.cell_width)
        } else {
            0
        };
        (
            INITIAL_COLS * self.font_system.cell_width + gutter,
            INITIAL_ROWS * self.font_system.cell_height + self.font_system.cell_height,
        )
    }

    fn spawn_new_tab(&mut self) {
        let id = TabId(self.next_tab_id);
        self.next_tab_id += 1;

        let cwd = if let Some(tab) = self.active_tab() {
            tab.terminal.lock().metadata.current_directory.clone()
        } else {
            Default::default()
        };
        let (cols, rows) = if let Some(renderer) = &self.renderer {
            let (width, height) = self.window_size;
            let gutter_px = renderer.gutter_width_px(self.font_system.cell_width);
            let usable_width = width.saturating_sub(gutter_px);
            let tab_bar_px = self.font_system.cell_height;
            let usable_height = height.saturating_sub(tab_bar_px);
            self.font_system
                .grid_dimensions(usable_width, usable_height)
        } else {
            (INITIAL_COLS, INITIAL_ROWS)
        };

        let scrollback = if let Some(tab) = self.active_tab() {
            tab.terminal.lock().active.grid.scrollback_limit
        } else {
            DEFAULT_SCROLLBACK
        };
        let mut terminal = Terminal::new(
            cols,
            rows,
            scrollback,
            self.config.status_line.display_kind(),
            self.config.feature_permissions.clone(),
            self.font_system.cell_height,
            self.font_system.cell_width,
            self.config.palette.clone(),
        );
        settings::set_emoji_compatibility_mode(
            &mut terminal.emoji_compatibility_mode,
            self.config.compatibility.emoji,
        );
        if let Some(tab) = self.active_tab() {
            settings::set_default_cursor_style(
                &mut terminal.cursor_style,
                tab.terminal.lock().cursor_style,
            );
        }

        let terminal_thread = TerminalThread::new();
        let pty_rows = terminal.viewport.rows;

        let (pty, writer, pty_reader) = match Pty::spawn(
            id,
            cols as u16,
            pty_rows as u16,
            self.font_system.cell_width as u16,
            self.font_system.cell_height as u16,
            None,
            cwd,
            terminal_thread.thread_handle.clone(),
            self.child_exit_tx.clone(),
        ) {
            Ok(pair) => pair,
            Err(e) => {
                warn!("failed to spawn new tab: {e}");
                return;
            }
        };
        let recorder = RecorderControl::new();

        let terminal = Arc::new(Mutex::new(terminal));
        terminal_thread.spawn(
            format!("terminal-{}", id.0),
            terminal.clone(),
            pty_reader,
            self.render_thread_handle.clone(),
            None,
            Box::new({
                let recorder = recorder.clone();
                move |bytes| {
                    #[cfg(feature = "testonly-perf-ctrl-c")]
                    crate::perf_ctrl_c::observe_pty_output(id, bytes);
                    recorder.write_chunk(bytes);
                }
            }),
            Box::new({
                let proxy = self.proxy.clone();
                move |effects| {
                    let _ = proxy.send_event(AppEvent::ApplyTerminalEffects {
                        tab_id: id,
                        effects,
                    });
                }
            }),
        );
        let _ = self.proxy.send_event(AppEvent::RegisterInputEndpoint {
            tab_id: id,
            terminal: terminal.clone(),
            writer,
            recorder,
        });

        self.tabs.push(Tab {
            id,
            terminal,
            pty,
            window_sync_epoch: self.window_resize_epoch,
            _terminal_thread: terminal_thread,
        });
        self.active_tab_id = id;
        self.sync_input_state();
        self.sync_active_input_tab();
    }

    fn close_active_tab(&mut self) {
        let Some(idx) = self.tabs.iter().position(|t| t.id == self.active_tab_id) else {
            return;
        };

        self.close_tab(idx);
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
        self.activate_tab_idx(new_idx);
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
        let _ = self.proxy.send_event(AppEvent::RemoveInputEndpoint(tab_id));
        if self.tabs.is_empty() {
            self.sync_input_state();
            self.sync_active_input_tab();
            self.should_exit = true;
            return;
        }
        if was_active {
            let new_idx = idx.min(self.tabs.len() - 1);
            self.active_tab_id = self.tabs[new_idx].id;
        }
        self.recalculate_grid_size();
        self.sync_input_state();
        self.sync_active_input_tab();
    }

    // -- Config -------------------------------------------------------------

    fn reload_config(&mut self) {
        let Some(path) = self.config_path.as_ref() else {
            return;
        };
        let cfg = crate::config::load_from(path);

        for tab in &mut self.tabs {
            let mut terminal = tab.terminal.lock();
            let terminal = &mut *terminal;
            let terminal41::Terminal {
                active,
                stash,
                viewport,
                cursor_style,
                palette,
                base_palette,
                dec_color,
                default_status_display,
                emoji_compatibility_mode,
                protocol,
                ..
            } = terminal;
            settings::set_default_cursor_style(cursor_style, cfg.cursor_style);
            settings::set_emoji_compatibility_mode(
                emoji_compatibility_mode,
                cfg.compatibility.emoji,
            );
            settings::set_default_status_display(
                active,
                stash,
                viewport,
                palette,
                default_status_display,
                cfg.status_line.display_kind(),
            );
            settings::set_scrollback_policy(active, viewport, cfg.scrollback_lines);
            settings::set_feature_permissions(protocol, cfg.feature_permissions.clone());
            settings::set_palette(
                active,
                stash,
                palette,
                base_palette,
                dec_color,
                cfg.palette.clone(),
            );
        }
        self.config.keybindings = cfg.keybindings;
        self.sync_input_state();
        self.config.bell = cfg.bell;
        self.config.scrollback_lines = cfg.scrollback_lines;
        let status_line_changed = cfg.status_line != self.config.status_line;
        self.config.status_line = cfg.status_line;
        self.config.palette = cfg.palette.clone();
        self.config.feature_permissions = cfg.feature_permissions.clone();
        self.config.compatibility = cfg.compatibility;

        if cfg.gutter != self.config.gutter {
            self.config.gutter = cfg.gutter;
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.set_gutter_enabled(cfg.gutter);
                self.recalculate_grid_size();
            }
            self.sync_input_state();
        }

        if cfg.dpi_scale != self.config.dpi_scale {
            warn!(
                "config: dpi_scale changed ({:?} → {:?}); restart to apply",
                self.config.dpi_scale, cfg.dpi_scale
            );
        }

        let fonts_changed = cfg.fonts != self.config.fonts;
        let size_changed = (cfg.font_size - self.config.font_size).abs() > f32::EPSILON;
        let ss_changed = cfg.font_supersampling != self.config.font_supersampling;
        if fonts_changed || size_changed || ss_changed {
            self.font_system
                .reload(cfg.fonts.clone(), cfg.font_size, cfg.font_supersampling);
            if let Some(renderer) = self.renderer.as_mut() {
                renderer.reset_glyph_atlas();
            }
            for tab in &self.tabs {
                let mut terminal = tab.terminal.lock();
                let terminal = &mut *terminal;
                let terminal41::Terminal {
                    cell_width,
                    cell_height,
                    ..
                } = terminal;
                settings::set_cell_dimensions(
                    cell_width,
                    cell_height,
                    self.font_system.cell_width,
                    self.font_system.cell_height,
                );
            }
            self.recalculate_grid_size();
            self.config.fonts = cfg.fonts.clone();
            self.config.font_size = cfg.font_size;
            self.config.font_supersampling = cfg.font_supersampling;
            self.sync_input_state();
        }
        if status_line_changed {
            self.recalculate_grid_size();
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

        // Background hot-reload. Path: pasted-on-disk always wins
        // (Ctrl+Shift+B sets it; Ctrl+Shift+Backspace clears) — config
        // edits below show through only when no pasted image exists.
        // Opacity is unconditional because it's a UI dim factor, not
        // bound to a specific image.
        let bg_path_changed = cfg.background_image != self.config.background_image;
        let bg_opacity_changed =
            (cfg.background_opacity - self.config.background_opacity).abs() > f32::EPSILON;
        if bg_path_changed || bg_opacity_changed {
            self.config.background_image = cfg.background_image.clone();
            self.config.background_opacity = cfg.background_opacity;
            let startup_snapshot_size = self.startup_snapshot_size();
            if let Some(renderer) = self.renderer.as_mut() {
                let path = effective_bg_path(&self.config);
                renderer.set_background(
                    path.as_deref(),
                    self.config.background_opacity,
                    startup_snapshot_size,
                );
            }
        }
    }

    // -- Per-frame rendering ------------------------------------------------

    fn render_frame(&mut self) {
        if self.tabs.is_empty() {
            return;
        }

        self.sync_window_title();

        let active_idx = self
            .tabs
            .iter()
            .position(|t| t.id == self.active_tab_id)
            .expect("active tab must exist");

        {
            let guard = self.tabs[active_idx].terminal.lock();
            let synced = host::synchronized_update_active(guard.modes.synchronized_update_since);
            if synced {
                return;
            }
        }

        // Collect owned tab titles under brief per-tab locks before
        // borrowing the renderer. Two-pass so the MutexGuards are dropped
        // before we enter the render call.
        let tab_titles: Vec<(String, bool)> = if self.tab_bar_visible() {
            self.tabs
                .iter()
                .map(|t| {
                    let title = t
                        .terminal
                        .lock()
                        .metadata
                        .current_title
                        .clone()
                        .unwrap_or_else(|| "Shell".to_owned());
                    (title, t.id == self.active_tab_id)
                })
                .collect()
        } else {
            Vec::new()
        };
        let tab_infos: Vec<TabInfo> = tab_titles
            .iter()
            .map(|(title, active)| TabInfo {
                label: title,
                active: *active,
            })
            .collect();

        let Some(ref mut renderer) = self.renderer else {
            return;
        };

        // Acquire the swapchain image BEFORE locking the terminal. This is
        // where vsync blocks — keeping the terminal unlocked here lets the
        // terminal thread continue processing PTY data during the wait.
        let Some(acquired) = renderer.acquire_frame() else {
            return;
        };

        let (
            hovered_button,
            tab_context_menu,
            gutter_popup,
            recording_popup,
            permission_modal,
            toast,
            preedit,
        ) = {
            let input_state = self.input_state.lock();
            (
                input_state.hovered_tab_bar_button,
                input_state.tab_context_menu.clone(),
                input_state.gutter_popup.clone(),
                input_state.recording_popup.clone(),
                input_state.permission_modal.clone(),
                input_state.toast.clone(),
                input_state.preedit.clone(),
            )
        };
        let recording_popup = recording_popup.map(|popup| RecordingPopup { lines: popup.lines });
        let toast = toast.map(|toast| Toast { text: toast.text });

        let controls = WindowControls {
            hovered: hovered_button,
            maximized: self.window.as_ref().is_some_and(|w| w.is_maximized()),
            tab_menu: tab_context_menu.as_ref().map(|m| (m.x, m.hovered_item)),
        };

        // Snapshot terminal state under a brief lock, then release it so
        // the terminal thread can continue processing PTY data while the
        // renderer does shaping, glyph caching, and image-atlas work.
        let (snap, visible_images) = {
            let terminal = self.tabs[active_idx].terminal.lock();
            let snap = r#impl::snapshot_terminal(&terminal);
            let visible_images = terminal41::view::visible_images(
                &terminal.active,
                &terminal.viewport,
                terminal.cell_height(),
                Instant::now(),
            )
            .collect::<Vec<_>>();
            (snap, visible_images)
        };
        renderer.render(
            acquired,
            &mut self.font_system,
            &visible_images,
            &snap,
            &tab_infos,
            &controls,
            gutter_popup.as_ref(),
            recording_popup.as_ref(),
            permission_modal.as_ref(),
            toast.as_ref(),
            preedit.as_ref(),
        );
    }

    fn sync_window_title(&mut self) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let base = tab.terminal.lock().metadata.current_title.clone();
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
                base.as_deref().unwrap_or("term41")
            ))
        } else {
            base
        };
        if self.applied_title.as_deref() == want.as_deref() {
            return;
        }
        let title = want.clone().unwrap_or_else(|| "term41".to_owned());
        let _ = self.proxy.send_event(AppEvent::SetTitle(title));
        self.applied_title = want;
    }

    fn handle_bell(
        &mut self,
        tab_id: TabId,
    ) {
        if self.active_tab_id != tab_id {
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

    fn sync_input_state(&mut self) {
        let mut input_state = self.input_state.lock();
        input_state.keybindings = self.config.keybindings.clone();
        input_state.tab_count = self.tabs.len();
        input_state.tab_order = self.tabs.iter().map(|tab| tab.id).collect();
        input_state.cell_width = self.font_system.cell_width;
        input_state.cell_height = self.font_system.cell_height;
        input_state.gutter_width = if self.config.gutter {
            self.renderer
                .as_ref()
                .map(|r| r.gutter_width_px(self.font_system.cell_width))
                .unwrap_or_else(|| compute_gutter_width(self.font_system.cell_width))
        } else {
            0
        };
    }

    fn sync_active_input_tab(&self) {
        let _ = self.proxy.send_event(AppEvent::SetActiveInputTab(
            self.active_tab().map(|tab| tab.id),
        ));
    }

    fn set_active_tab(
        &mut self,
        tab_idx: usize,
    ) {
        self.activate_tab_idx(tab_idx);
    }

    fn activate_tab_idx(
        &mut self,
        tab_idx: usize,
    ) {
        let Some((cols, rows)) = self.current_window_grid_size() else {
            if let Some(tab) = self.tabs.get(tab_idx) {
                self.active_tab_id = tab.id;
                self.sync_input_state();
                self.sync_active_input_tab();
            }
            return;
        };

        let epoch = self.window_resize_epoch;
        let Some(tab) = self.tabs.get_mut(tab_idx) else {
            return;
        };
        if tab.window_sync_epoch < epoch {
            resize_tab_to_grid(tab, cols, rows);
            tab.window_sync_epoch = epoch;
        }
        self.active_tab_id = tab.id;
        self.sync_input_state();
        self.sync_active_input_tab();
    }

    fn close_other_tabs(
        &mut self,
        keep: usize,
    ) {
        for (idx, tab) in self.tabs.iter().enumerate() {
            if idx != keep {
                let _ = self.proxy.send_event(AppEvent::RemoveInputEndpoint(tab.id));
            }
        }

        let keep = if let Some(tab) = self.tabs.get(keep) {
            tab.id
        } else {
            return;
        };

        self.tabs.retain(|t| t.id == keep);
        self.recalculate_grid_size();
        self.sync_input_state();
        self.sync_active_input_tab();
    }

    fn close_tab(
        &mut self,
        tab_idx: usize,
    ) {
        let Some(tab_id) = self.tabs.get(tab_idx).map(|t| t.id) else {
            return;
        };
        self.tabs.remove(tab_idx);
        let _ = self.proxy.send_event(AppEvent::RemoveInputEndpoint(tab_id));
        if self.tabs.is_empty() {
            self.sync_input_state();
            self.sync_active_input_tab();
            self.should_exit = true;
            return;
        }
        self.activate_tab_idx(tab_idx.min(self.tabs.len() - 1));
    }
}

pub(crate) fn ctrl_byte(c: &str) -> Option<u8> {
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

pub(crate) fn term41_data_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("term41"))
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

fn encode_csi_bytes(
    args: std::fmt::Arguments<'_>,
    c1_mode: C1Mode,
) -> Vec<u8> {
    let mut out = Vec::new();
    if c1_mode == C1Mode::EightBit {
        out.push(0x9B);
    } else {
        out.extend_from_slice(b"\x1b[");
    }
    use std::io::Write as _;
    out.write_fmt(args).expect("write to Vec is infallible");
    out
}

fn encode_ss3_bytes(
    final_byte: char,
    c1_mode: C1Mode,
) -> Vec<u8> {
    let mut out = Vec::new();
    if c1_mode == C1Mode::EightBit {
        out.push(0x8F);
    } else {
        out.extend_from_slice(b"\x1bO");
    }
    out.push(final_byte as u8);
    out
}

pub(crate) fn kitty_encode_input(
    key: &Key,
    mods: ModifiersState,
    flags: KittyFlags,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    if !flags.contains(KittyFlags::DISAMBIGUATE_ESCAPE_CODES) {
        return None;
    }

    let mod_bits = kitty_modifier_bits(mods);
    let only_shift_or_none = (mod_bits & !1) == 0;
    let mod_param = mod_bits + 1;
    let report_text = flags.contains(KittyFlags::REPORT_ASSOCIATED_TEXT);
    let all_as_escape = flags.contains(KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES);

    match key {
        Key::Character(s) => {
            // Pure text input (no modifiers beyond shift) is normally left as
            // the raw byte. REPORT_ALL_KEYS_AS_ESCAPE_CODES forces it into
            // CSI u form too so apps can tell key events apart from pastes.
            if only_shift_or_none && !all_as_escape {
                return None;
            }
            let lower = s.to_lowercase();
            let cp = lower.chars().next()? as u32;
            let text = report_text.then_some(s.as_str());
            Some(format_csi_u(cp, mod_param, text, c1_mode))
        }
        Key::Named(named) => kitty_encode_named(*named, mod_bits, mod_param, report_text, c1_mode),
        _ => None,
    }
}

/// Emit a CSI u sequence. `text`, when `Some` and non-empty, becomes the third
/// parameter as `cp1:cp2:...` — the associated text the key produced. Apps
/// with `REPORT_ASSOCIATED_TEXT` on use this to distinguish "user typed A"
/// from "user typed shift+a then Caps got hit"; the raw CSI u form alone
/// only carries the unmodified key code and the modifiers.
fn format_csi_u(
    cp: u32,
    mod_param: u8,
    text: Option<&str>,
    c1_mode: C1Mode,
) -> Vec<u8> {
    let mut out = encode_csi_bytes(format_args!(""), c1_mode);
    match text {
        Some(t) if !t.is_empty() => {
            use std::io::Write as _;
            out.write_fmt(format_args!("{cp};{mod_param};"))
                .expect("write to Vec is infallible");
            let mut first = true;
            for ch in t.chars() {
                if !first {
                    out.push(b':');
                }
                first = false;
                out.write_fmt(format_args!("{}", ch as u32))
                    .expect("write to Vec is infallible");
            }
            out.push(b'u');
            out
        }
        _ => {
            use std::io::Write as _;
            out.write_fmt(format_args!("{cp};{mod_param}u"))
                .expect("write to Vec is infallible");
            out
        }
    }
}

/// Encode an IME commit as a synthetic key event under the kitty protocol.
/// Key code 0 is the spec's sentinel for "this wasn't a physical key" —
/// editors read that plus the text param and can treat the string as a
/// single input block instead of N individual keystrokes. Callers should
/// only route through here when `REPORT_ASSOCIATED_TEXT` is set; without it,
/// the bytes go straight to the PTY unchanged.
pub(crate) fn kitty_encode_ime_commit(
    text: &str,
    c1_mode: C1Mode,
) -> Vec<u8> {
    format_csi_u(0, 0, Some(text), c1_mode)
}

fn kitty_encode_named(
    named: NamedKey,
    mod_bits: u8,
    mod_param: u8,
    report_text: bool,
    c1_mode: C1Mode,
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
        // Enter/Tab/Space genuinely produce text ("\r", "\t", " "); Backspace
        // and Escape don't — they're control actions, no text param for them.
        let text: Option<&str> = if report_text {
            match named {
                NamedKey::Enter => Some("\r"),
                NamedKey::Tab => Some("\t"),
                NamedKey::Space => Some(" "),
                _ => None,
            }
        } else {
            None
        };
        return Some(format_csi_u(cp, mod_param, text, c1_mode));
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
        return Some(encode_csi_bytes(
            format_args!("1;{mod_param}{action}"),
            c1_mode,
        ));
    }

    let tilde_code = match named {
        NamedKey::Insert => Some(2u32),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        _ => None,
    };
    if let Some(code) = tilde_code {
        return Some(encode_csi_bytes(
            format_args!("{code};{mod_param}~"),
            c1_mode,
        ));
    }

    None
}

/// Encode a named key for legacy (non-Kitty) mode, using xterm-style
/// modifier encoding. Plain keys use standard VT/xterm sequences;
/// modified keys use the `CSI 1;mod X` (arrows/Home/End) or
/// `CSI code;mod ~` (F-keys/Ins/Del/PgUp/PgDn) format where
/// mod = 1 + Shift(1) + Alt(2) + Ctrl(4).
pub(crate) fn legacy_encode_named(
    key: NamedKey,
    location: KeyLocation,
    mods: ModifiersState,
    app_cursor_keys: bool,
    app_keypad: bool,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    let mod_param = legacy_modifier_param(mods);

    if mod_param == 0
        && app_keypad
        && location == KeyLocation::Numpad
        && let Some(ch) = application_keypad_final(key)
    {
        return Some(encode_ss3_bytes(ch, c1_mode));
    }

    // Simple keys that don't take modifier parameters.
    if mod_param == 0 {
        let plain = match key {
            NamedKey::Enter => Some(&b"\r"[..]),
            NamedKey::Backspace => Some(&b"\x7f"[..]),
            NamedKey::Tab => Some(&b"\t"[..]),
            NamedKey::Escape => Some(&b"\x1b"[..]),
            NamedKey::Space => Some(&b" "[..]),
            _ => None,
        };
        if let Some(bytes) = plain {
            return Some(bytes.to_vec());
        }
    }

    // Shift+Tab → CSI Z (backtab).
    if key == NamedKey::Tab && mods.shift_key() {
        return Some(encode_csi_bytes(format_args!("Z"), c1_mode));
    }

    // Arrow-style keys: CSI [1;mod] X
    // In DECCKM (app cursor keys) mode, unmodified arrows/Home/End send
    // SS3 form (ESC O X) instead of CSI form (ESC [ X).
    let arrow_final = match key {
        NamedKey::ArrowUp => Some('A'),
        NamedKey::ArrowDown => Some('B'),
        NamedKey::ArrowRight => Some('C'),
        NamedKey::ArrowLeft => Some('D'),
        NamedKey::Home => Some('H'),
        NamedKey::End => Some('F'),
        _ => None,
    };
    if let Some(ch) = arrow_final {
        return if mod_param > 0 {
            Some(encode_csi_bytes(format_args!("1;{mod_param}{ch}"), c1_mode))
        } else if app_cursor_keys {
            Some(encode_ss3_bytes(ch, c1_mode))
        } else {
            Some(encode_csi_bytes(format_args!("{ch}"), c1_mode))
        };
    }

    // Tilde-style keys: CSI code [;mod] ~
    let tilde_code = match key {
        NamedKey::Insert => Some(2),
        NamedKey::Delete => Some(3),
        NamedKey::PageUp => Some(5),
        NamedKey::PageDown => Some(6),
        _ => None,
    };
    if let Some(code) = tilde_code {
        return if mod_param > 0 {
            Some(encode_csi_bytes(
                format_args!("{code};{mod_param}~"),
                c1_mode,
            ))
        } else {
            Some(encode_csi_bytes(format_args!("{code}~"), c1_mode))
        };
    }

    // F1-F4 use SS3 unmodified, CSI 1;mod P/Q/R/S with modifiers.
    let f1_4_final = match key {
        NamedKey::F1 => Some('P'),
        NamedKey::F2 => Some('Q'),
        NamedKey::F3 => Some('R'),
        NamedKey::F4 => Some('S'),
        _ => None,
    };
    if let Some(ch) = f1_4_final {
        return if mod_param > 0 {
            Some(encode_csi_bytes(format_args!("1;{mod_param}{ch}"), c1_mode))
        } else {
            Some(encode_ss3_bytes(ch, c1_mode))
        };
    }

    // F5-F20 use tilde-style: CSI code [;mod] ~. DEC skips 22, 27, and 30.
    let fkey_code = match key {
        NamedKey::F5 => Some(15),
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        NamedKey::F13 => Some(25),
        NamedKey::F14 => Some(26),
        NamedKey::F15 => Some(28),
        NamedKey::F16 => Some(29),
        NamedKey::F17 => Some(31),
        NamedKey::F18 => Some(32),
        NamedKey::F19 => Some(33),
        NamedKey::F20 => Some(34),
        _ => None,
    };
    if let Some(code) = fkey_code {
        return if mod_param > 0 {
            Some(encode_csi_bytes(
                format_args!("{code};{mod_param}~"),
                c1_mode,
            ))
        } else {
            Some(encode_csi_bytes(format_args!("{code}~"), c1_mode))
        };
    }

    None
}

fn application_keypad_final(key: NamedKey) -> Option<char> {
    match key {
        NamedKey::Enter => Some('M'),
        NamedKey::ArrowUp => Some('A'),
        NamedKey::ArrowDown => Some('B'),
        NamedKey::ArrowRight => Some('C'),
        NamedKey::ArrowLeft => Some('D'),
        NamedKey::PageUp => Some('I'),
        NamedKey::PageDown => Some('G'),
        NamedKey::Home => Some('H'),
        NamedKey::End => Some('F'),
        NamedKey::Insert => Some('L'),
        NamedKey::Delete => Some('N'),
        _ => None,
    }
}

pub(crate) fn legacy_encode_numpad_character(
    text: &str,
    location: KeyLocation,
    physical: PhysicalKey,
    mods: ModifiersState,
    app_keypad: bool,
    c1_mode: C1Mode,
) -> Option<Vec<u8>> {
    if location != KeyLocation::Numpad || legacy_modifier_param(mods) != 0 {
        return None;
    }

    let code = match physical {
        PhysicalKey::Code(code) => code,
        _ => return None,
    };

    if app_keypad {
        let ch = match code {
            KeyCode::Numpad0 => 'p',
            KeyCode::Numpad1 => 'q',
            KeyCode::Numpad2 => 'r',
            KeyCode::Numpad3 => 's',
            KeyCode::Numpad4 => 't',
            KeyCode::Numpad5 => 'u',
            KeyCode::Numpad6 => 'v',
            KeyCode::Numpad7 => 'w',
            KeyCode::Numpad8 => 'x',
            KeyCode::Numpad9 => 'y',
            KeyCode::NumpadDecimal => 'n',
            KeyCode::NumpadComma => 'l',
            KeyCode::NumpadDivide => 'o',
            KeyCode::NumpadMultiply => 'j',
            KeyCode::NumpadSubtract => 'm',
            KeyCode::NumpadAdd => 'k',
            _ => return None,
        };
        Some(encode_ss3_bytes(ch, c1_mode))
    } else {
        let bytes = match code {
            KeyCode::Numpad0 => b"0".to_vec(),
            KeyCode::Numpad1 => b"1".to_vec(),
            KeyCode::Numpad2 => b"2".to_vec(),
            KeyCode::Numpad3 => b"3".to_vec(),
            KeyCode::Numpad4 => b"4".to_vec(),
            KeyCode::Numpad5 => b"5".to_vec(),
            KeyCode::Numpad6 => b"6".to_vec(),
            KeyCode::Numpad7 => b"7".to_vec(),
            KeyCode::Numpad8 => b"8".to_vec(),
            KeyCode::Numpad9 => b"9".to_vec(),
            KeyCode::NumpadDecimal => b".".to_vec(),
            KeyCode::NumpadComma => b",".to_vec(),
            KeyCode::NumpadDivide => b"/".to_vec(),
            KeyCode::NumpadMultiply => b"*".to_vec(),
            KeyCode::NumpadSubtract => b"-".to_vec(),
            KeyCode::NumpadAdd => b"+".to_vec(),
            _ => text.as_bytes().to_vec(),
        };
        Some(bytes)
    }
}

/// Compute the xterm modifier parameter: 1 + (shift | alt | ctrl).
/// Returns 0 when no modifiers are held, meaning the plain (unmodified)
/// sequence should be used.
fn legacy_modifier_param(mods: ModifiersState) -> u8 {
    let mut bits: u8 = 0;
    if mods.shift_key() {
        bits |= 1;
    }
    if mods.alt_key() {
        bits |= 2;
    }
    if mods.control_key() {
        bits |= 4;
    }
    if bits == 0 { 0 } else { bits + 1 }
}

/// Directory where `PasteAsBackground` persists images.
/// `~/.local/share/term41/` on Linux, `~/Library/Application Support/term41/`
/// on macOS, `%APPDATA%\term41\` on Windows. Returns `None` on platforms
/// where `dirs` can't resolve a data dir (rare — usually broken environment).
fn pasted_background_dir() -> Option<PathBuf> {
    term41_data_dir()
}

/// Build the full pasted-background path for a given file extension.
fn pasted_background_path(ext: &str) -> Option<PathBuf> {
    pasted_background_dir().map(|d| d.join(format!("pasted_background.{ext}")))
}

/// Find an existing pasted-background file, regardless of extension.
/// Returns the first match found; there should only ever be one because
/// `clear_pasted_backgrounds` deletes all variants before a new save.
fn find_pasted_background() -> Option<PathBuf> {
    let dir = pasted_background_dir()?;
    let entries = std::fs::read_dir(&dir).ok()?;
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("pasted_background."))
        {
            return Some(entry.path());
        }
    }
    None
}

/// Delete every `pasted_background.*` file in the data directory so a
/// fresh paste doesn't leave a stale file from a previous format.
fn clear_pasted_backgrounds() {
    let Some(dir) = pasted_background_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("pasted_background."))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Resolve which background image to actually load: pasted-image-on-disk
/// always wins over the config-supplied path. The "pasted always wins
/// until cleared" rule keeps the precedence one-line debuggable —
/// "does a pasted file exist?" is the whole question.
pub(crate) fn effective_bg_path(config: &Config) -> Option<PathBuf> {
    find_pasted_background().or_else(|| config.background_image.clone())
}

/// Encode an RGBA byte buffer to PNG at `path`. Always RGBA8 — the
/// clipboard hands us pixels in that layout and the renderer reads them
/// back the same way, so there's no need for a more flexible encoder.
fn encode_png_rgba(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba: &[u8],
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer
        .write_image_data(rgba)
        .map_err(std::io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod kitty_encode_tests {
    use winit::keyboard::Key;
    use winit::keyboard::ModifiersState;
    use winit::keyboard::NamedKey;
    use winit::keyboard::SmolStr;

    use super::*;

    fn char_key(s: &str) -> Key {
        Key::Character(SmolStr::new(s))
    }

    #[test]
    fn ctrl_letter_without_text_flag() {
        let bytes = kitty_encode_input(
            &char_key("a"),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[97;5u");
    }

    #[test]
    fn ctrl_letter_with_text_flag_appends_text_param() {
        let bytes = kitty_encode_input(
            &char_key("a"),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        // text param is the codepoint of the produced char ("a" = 97)
        assert_eq!(bytes, b"\x1b[97;5;97u");
    }

    #[test]
    fn shift_a_with_all_as_escape_and_text() {
        // Plain "A" (shift+a) normally emits no CSI u. With REPORT_ALL_KEYS
        // the key code is the unmodified base ("a" = 97), modifier param is
        // 2 (shift = bit 0 + 1), text param carries the actual produced
        // character so apps can distinguish a true "A" from a synth one.
        let bytes = kitty_encode_input(
            &char_key("A"),
            ModifiersState::SHIFT,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES
                | KittyFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
                | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[97;2;65u");
    }

    #[test]
    fn plain_text_without_all_as_escape_is_not_encoded() {
        // Just REPORT_ASSOCIATED_TEXT shouldn't force plain text into CSI u;
        // the raw byte path still handles it.
        assert!(
            kitty_encode_input(
                &char_key("a"),
                ModifiersState::empty(),
                KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
                C1Mode::SevenBit,
            )
            .is_none()
        );
    }

    #[test]
    fn enter_with_text_flag_reports_cr_as_text() {
        let bytes = kitty_encode_input(
            &Key::Named(NamedKey::Enter),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        // Enter's associated text is "\r" (13).
        assert_eq!(bytes, b"\x1b[13;5;13u");
    }

    #[test]
    fn escape_with_text_flag_has_no_text_param() {
        // Escape is a control action, not a text-producing key.
        let bytes = kitty_encode_input(
            &Key::Named(NamedKey::Escape),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_ASSOCIATED_TEXT,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1b[27;5u");
    }

    #[test]
    fn ime_commit_uses_zero_key_and_zero_mods() {
        // Spec sentinel: key code 0 + modifier param 0 means "not a physical
        // key". Codepoints join with ':'. 啊 = U+554A (0x554A = 21834),
        // 不 = U+4E0D (0x4E0D = 19981).
        let bytes = kitty_encode_ime_commit("啊不", C1Mode::SevenBit);
        assert_eq!(bytes, b"\x1b[0;0;21834:19981u");
    }

    #[test]
    fn ime_commit_single_codepoint() {
        let bytes = kitty_encode_ime_commit("é", C1Mode::SevenBit);
        // é = U+00E9 = 233
        assert_eq!(bytes, b"\x1b[0;0;233u");
    }

    #[test]
    fn kitty_encode_uses_8bit_csi_when_requested() {
        let bytes = kitty_encode_input(
            &char_key("a"),
            ModifiersState::CONTROL,
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES,
            C1Mode::EightBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x9b97;5u");
    }

    #[test]
    fn legacy_app_cursor_keys_use_8bit_ss3_when_requested() {
        let bytes = legacy_encode_named(
            NamedKey::ArrowUp,
            KeyLocation::Standard,
            ModifiersState::empty(),
            true,
            false,
            C1Mode::EightBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x8fA");
    }

    #[test]
    fn legacy_app_keypad_encodes_numpad_named_keys_as_ss3() {
        let bytes = legacy_encode_named(
            NamedKey::Enter,
            KeyLocation::Numpad,
            ModifiersState::empty(),
            false,
            true,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1bOM");
    }

    #[test]
    fn legacy_app_keypad_encodes_numpad_digits_as_ss3() {
        let bytes = legacy_encode_numpad_character(
            "7",
            KeyLocation::Numpad,
            PhysicalKey::Code(KeyCode::Numpad7),
            ModifiersState::empty(),
            true,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"\x1bOw");
    }

    #[test]
    fn legacy_numeric_keypad_uses_physical_numpad_digit_even_if_logical_key_varies() {
        let bytes = legacy_encode_numpad_character(
            "Home",
            KeyLocation::Numpad,
            PhysicalKey::Code(KeyCode::Numpad7),
            ModifiersState::empty(),
            false,
            C1Mode::SevenBit,
        )
        .expect("encoded");
        assert_eq!(bytes, b"7");
    }
}

#[cfg(test)]
mod permission_modal_tests {
    use super::*;

    #[test]
    fn permission_buttons_are_centered_in_panel() {
        let panel = permission_panel_rect("the clipboard", 10.0, 20.0, 800.0, 600.0, 20.0);
        let buttons = permission_button_layout("the clipboard", 10.0, 20.0, 800.0, 600.0, 20.0);
        let left_gap = buttons.yes.0 - panel.0;
        let right_gap = panel.0 + panel.2 - (buttons.no.0 + buttons.no.2);
        assert!((left_gap - right_gap).abs() < 0.01);
    }

    #[test]
    fn permission_button_hit_testing_distinguishes_yes_and_no() {
        let buttons = permission_button_layout("the clipboard", 10.0, 20.0, 800.0, 600.0, 20.0);
        let yes = permission_modal_button_at(
            "the clipboard",
            buttons.yes.0 + 1.0,
            buttons.yes.1 + 1.0,
            10.0,
            20.0,
            800.0,
            600.0,
            20.0,
        );
        let no = permission_modal_button_at(
            "the clipboard",
            buttons.no.0 + 1.0,
            buttons.no.1 + 1.0,
            10.0,
            20.0,
            800.0,
            600.0,
            20.0,
        );
        assert_eq!(yes, Some(PermissionChoice::Allow));
        assert_eq!(no, Some(PermissionChoice::Deny));
    }

    #[test]
    fn permission_feature_line_sanitizes_untrusted_label() {
        let line = permission_feature_line("clipboard\nread");
        assert_eq!(line, "A program would like to use clipboard read.");

        let long = permission_feature_line("abcdefghijklmnopqrstuvwxyz0123456789");
        assert!(long.contains("abcdefghijklmnopqrstuvwxyz012345..."));
    }
}

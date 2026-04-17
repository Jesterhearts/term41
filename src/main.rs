#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod clipboard;
mod config;
mod image;
mod keybindings;
mod pty;
mod renderer;
mod search;
mod selection;
mod terminal;

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

use config::Config;
use font41::FontSystem;
use pty::Pty;
use renderer::RenderHost;
use terminal::MouseButton as TermMouseButton;
use terminal::Terminal;
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
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::Window;
use winit::window::WindowId;

use crate::renderer::RenderEvent;
use crate::terminal::TerminalThread;

#[macro_use]
extern crate log;

const INITIAL_COLS: u32 = 80;
const INITIAL_ROWS: u32 = 24;

/// Size of the cueue ring buffer for window→renderer events (in elements).
const EVENT_QUEUE_SIZE: usize = 4096;

/// Stable identifier for a tab. Monotonically increasing; never reused, so
/// background threads that race with a tab close can't accidentally address
/// the wrong session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

/// Commands sent from the render thread back to the window thread.
#[derive(Debug, Clone)]
enum AppEvent {
    SetTitle(String),
    RequestUserAttention,
    SpawnNewWindow { cwd: Option<PathBuf> },
    Exit,
}

struct Tab {
    id: TabId,
    terminal: Arc<Mutex<Terminal>>,
    pty: Pty,
    /// Kept alive for its Drop impl which signals the thread to stop.
    _terminal_thread: TerminalThread,
}

struct WindowHost {
    window: Option<Arc<Window>>,
    event_tx: cueue::Writer<RenderEvent>,
    /// One-shot channel to deliver the window + display handle to the render
    /// thread after `resumed()` creates the window. Taken (set to `None`)
    /// after the first send.
    window_tx: Option<mpsc::SyncSender<(Arc<Window>, OwnedDisplayHandle)>>,
    render_thread: Thread,
    opacity: f32,
    cell_width: u32,
    cell_height: u32,
}

impl WindowHost {
    fn send(
        &mut self,
        ev: RenderEvent,
    ) {
        let _ = self.event_tx.push(ev);
        self.render_thread.unpark();
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
            AppEvent::SpawnNewWindow { cwd } => {
                spawn_new_window(cwd);
            }
            AppEvent::Exit => {
                event_loop.exit();
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

        let pixel_width = INITIAL_COLS * self.cell_width;
        let pixel_height = INITIAL_ROWS * self.cell_height;
        let transparent = self.opacity < 1.0;
        // LogicalSize so the window occupies the same visual area regardless
        // of the monitor's DPI scale factor. Cell metrics are computed at 1x
        // here; the render thread rescales them once it knows the actual
        // scale factor.
        let attrs = Window::default_attributes()
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

        // Opt into IME events. `ImePurpose::Terminal` is a hint some
        // Wayland compositors and Android IMEs use to expose extra keys
        // (arrows, Tab, etc.) that wouldn't normally appear on a text-input
        // OSK; on platforms that don't understand it, it's a no-op.
        window.set_ime_allowed(true);
        window.set_ime_purpose(winit::window::ImePurpose::Terminal);

        if let Some(tx) = self.window_tx.take() {
            let _ = tx.send((window.clone(), event_loop.owned_display_handle()));
        }

        self.window = Some(window);
        self.render_thread.unpark();

        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        let ev = match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
                RenderEvent::CloseRequested
            }

            WindowEvent::Resized(size) => RenderEvent::Resized {
                width: size.width,
                height: size.height,
            },

            WindowEvent::Focused(f) => RenderEvent::Focused(f),

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }
                match &event.logical_key {
                    Key::Character(_) | Key::Named(_) => {
                        RenderEvent::KeyInput(event.logical_key.clone())
                    }
                    _ => return,
                }
            }

            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                RenderEvent::ScaleFactorChanged { scale_factor }
            }

            WindowEvent::ModifiersChanged(mods) => RenderEvent::ModifiersChanged(mods.state()),

            WindowEvent::CursorMoved { position, .. } => RenderEvent::CursorMoved {
                x: position.x,
                y: position.y,
            },

            WindowEvent::MouseInput { state, button, .. } => RenderEvent::MouseInput {
                pressed: state == ElementState::Pressed,
                button,
            },

            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y, pixels) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => (x as f64, y as f64, false),
                    MouseScrollDelta::PixelDelta(pos) => (pos.x, pos.y, true),
                };
                RenderEvent::MouseWheel { x, y, pixels }
            }

            WindowEvent::Ime(ime) => match ime {
                Ime::Enabled => RenderEvent::ImeEnabled(true),
                Ime::Disabled => RenderEvent::ImeEnabled(false),
                Ime::Preedit(text, cursor) => RenderEvent::ImePreedit { text, cursor },
                Ime::Commit(text) => RenderEvent::ImeCommit(text),
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
    let cell_width = font_system.cell_width;
    let cell_height = font_system.cell_height;
    let opacity = config.opacity;

    // Channels.
    let (event_tx, event_rx) =
        cueue::cueue::<RenderEvent>(EVENT_QUEUE_SIZE).expect("create event queue");
    let (window_tx, window_rx) = mpsc::sync_channel(1);
    let (child_exit_tx, child_exit_rx) = mpsc::channel();
    let config_reload = Arc::new(AtomicBool::new(false));
    let render_thread_handle: Arc<OnceLock<Thread>> = Arc::new(OnceLock::new());

    let proxy = event_loop.create_proxy();

    // Create the terminal thread handle before spawning the PTY so the PTY
    // reader can unpark the terminal thread once it starts.
    let terminal_thread = TerminalThread::new();

    // Spawn the initial PTY early so the shell starts running immediately.
    let (pty, pty_reader) = Pty::spawn(
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
    .expect("failed to spawn PTY");

    let mut terminal = Terminal::new(
        INITIAL_COLS,
        INITIAL_ROWS,
        config.scrollback_lines,
        cell_height,
        cell_width,
    );
    terminal.set_default_cursor_style(config.cursor_style);
    let terminal = Arc::new(Mutex::new(terminal));

    terminal_thread.spawn(
        "terminal-0".into(),
        terminal.clone(),
        pty_reader,
        render_thread_handle.clone(),
    );

    let tab = Tab {
        id: TabId(0),
        terminal,
        pty,
        _terminal_thread: terminal_thread,
    };

    // Clone config_path before moving it into the render thread closure —
    // the original is still needed by the config watcher below.
    let config_path_for_watcher = config_path.clone();

    let render_thread_handle_ = render_thread_handle.clone();
    // Spawn the render thread.
    let config_reload_ = config_reload.clone();
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
            );
            host.run(window_rx);
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
        event_tx,
        window_tx: Some(window_tx),
        render_thread,
        opacity,
        cell_width,
        cell_height,
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

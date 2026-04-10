mod config;
mod font;
mod pty;
mod renderer;
mod sixel;
mod terminal;

use std::sync::Arc;

use font::FontSystem;
use pty::Pty;
use renderer::Renderer;
use terminal::Terminal;
use winit::application::ApplicationHandler;
use winit::event::ElementState;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::keyboard::Key;
use winit::keyboard::NamedKey;
use winit::window::Window;
use winit::window::WindowId;

const INITIAL_COLS: u16 = 80;
const INITIAL_ROWS: u16 = 24;

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    terminal: Terminal,
    font_system: FontSystem,
    pty: Pty,
    opacity: f32,
    modifiers: winit::keyboard::ModifiersState,
}

impl App {
    fn new(
        pty: Pty,
        opacity: f32,
        fonts_config: Option<&str>,
        scrollback_lines: u32,
    ) -> Self {
        let font_system = FontSystem::new(fonts_config);
        Self {
            window: None,
            renderer: None,
            terminal: Terminal::new(
                INITIAL_COLS,
                INITIAL_ROWS,
                font_system.cell_height,
                scrollback_lines,
            ),
            font_system,
            pty,
            opacity,
            modifiers: winit::keyboard::ModifiersState::default(),
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
                    if cols != self.terminal.cols || rows != self.terminal.rows {
                        self.terminal.resize(cols, rows);
                        self.pty.resize(cols, rows);
                    }
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
                if self.modifiers.shift_key() {
                    if let Key::Named(named) = &event.logical_key {
                        match named {
                            NamedKey::PageUp => {
                                self.terminal.scroll_viewport_up(self.terminal.rows as u32);
                                return;
                            }
                            NamedKey::PageDown => {
                                self.terminal
                                    .scroll_viewport_down(self.terminal.rows as u32);
                                return;
                            }
                            _ => {}
                        }
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
                self.terminal
                    .prune_offscreen_images(self.font_system.cell_height);

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
    let pty = Pty::spawn(INITIAL_COLS, INITIAL_ROWS).expect("failed to spawn PTY");

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(
        pty,
        config.opacity,
        config.fonts.as_deref(),
        config.scrollback_lines,
    );
    event_loop.run_app(&mut app).expect("run event loop");
}

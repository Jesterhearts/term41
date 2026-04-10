mod font;
mod pty;
mod renderer;
mod terminal;

use std::sync::Arc;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use font::FontSystem;
use pty::Pty;
use renderer::Renderer;
use terminal::Terminal;

const INITIAL_COLS: u16 = 80;
const INITIAL_ROWS: u16 = 24;

struct App {
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    terminal: Terminal,
    font_system: FontSystem,
    pty: Pty,
}

impl App {
    fn new(pty: Pty) -> Self {
        let font_system = FontSystem::new();
        Self {
            window: None,
            renderer: None,
            terminal: Terminal::new(INITIAL_COLS, INITIAL_ROWS),
            font_system,
            pty,
        }
    }

    fn read_pty_output(&mut self) {
        let mut buf = [0u8; 4096];
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
        let attrs = Window::default_attributes()
            .with_title("term41")
            .with_inner_size(winit::dpi::PhysicalSize::new(pixel_width, pixel_height));

        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));
        let renderer = pollster::block_on(Renderer::new(
            Arc::clone(&window),
            &self.font_system,
            &self.terminal,
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
                    self.pty.resize(cols, rows);
                }
            }

            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                let bytes = match &event.logical_key {
                    Key::Character(c) => Some(c.as_bytes().to_vec()),
                    Key::Named(named) => named_key_to_bytes(*named),
                    _ => None,
                };

                if let Some(bytes) = bytes {
                    let _ = self.pty.write(&bytes);
                }
            }

            WindowEvent::RedrawRequested => {
                self.read_pty_output();

                if let Some(renderer) = &mut self.renderer {
                    renderer.render(&self.font_system, &self.terminal);
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

    let pty = Pty::spawn(INITIAL_COLS, INITIAL_ROWS).expect("failed to spawn PTY");

    let event_loop = EventLoop::new().expect("create event loop");
    event_loop.set_control_flow(ControlFlow::Wait);

    let mut app = App::new(pty);
    event_loop.run_app(&mut app).expect("run event loop");
}

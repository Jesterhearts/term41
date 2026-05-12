#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod command_catalog;
mod history_runtime;
mod output_recording;
mod perf_ctrl_c;
mod renderer;
mod scripting;
mod window_host;

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use command_catalog::CommandCatalog;
use commands41::CommandEditor;
use config41::StatusLineMode;
use font41::FontSystem;
use parking_lot::Mutex;
use pty_pipe41::Pty;
use renderer::RenderHost;
use terminal41::PasteMode;
use terminal41::Terminal;
use terminal41::TerminalThread;
use terminal41::prompt::CommandBlockCommand;
use terminal41::prompt::CommandTextSource;
use terminal41::prompt::PromptRef;
use terminal41::prompt::command_block_for_prompt;
use terminal41::settings;
use terminal41::view;
use winit::event_loop::EventLoop;
use winit::event_loop::EventLoopProxy;
use winit::keyboard::ModifiersState;

use crate::output_recording::RecorderControl;
use crate::renderer::RenderEvent;
use crate::renderer::compute_gutter_width;
use crate::window_host::AppEvent;
use crate::window_host::CommandRuntime;
use crate::window_host::InputEndpoint;
use crate::window_host::InputRuntime;
use crate::window_host::InputState;
use crate::window_host::KeyboardRuntime;
use crate::window_host::ModalRuntime;
use crate::window_host::MouseButtonState;
use crate::window_host::MouseReportPosition;
use crate::window_host::MouseRuntime;
use crate::window_host::PhysicalModifierState;
use crate::window_host::PopupRerunPasteTarget;
use crate::window_host::RenderRuntime;
use crate::window_host::SelectionAutoscroll;
use crate::window_host::SelectionCopySource;
use crate::window_host::StartupState;
use crate::window_host::Tab;
use crate::window_host::TabId;
use crate::window_host::WindowHost;
use crate::window_host::WindowMetrics;

#[macro_use]
extern crate log;

pub(crate) fn unpark_thread_if_started(thread_handle: &OnceLock<Thread>) {
    if let Some(thread) = thread_handle.get() {
        thread.unpark();
    }
}

static APP_START_TIME: OnceLock<Instant> = OnceLock::new();
static LOG_TOAST_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();

const INITIAL_COLS: u32 = 80;
const INITIAL_ROWS: u32 = 24;
const COMMAND_EDITOR_BOX_ROWS: u32 = 3;

/// Size of the cueue ring buffer for window→renderer events (in elements).
const EVENT_QUEUE_SIZE: usize = 4096;

const SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(45);

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

fn reset_viewport_and_invalidate(terminal: &mut Terminal) {
    let offset = terminal.active.offset;
    view::reset_viewport(&mut terminal.active);
    if terminal.active.offset != offset {
        terminal.invalidate_snapshot_rows();
    }
}

fn reset_tab_viewport_and_invalidate(
    input_endpoints: &HashMap<TabId, InputEndpoint>,
    tab_id: TabId,
) {
    let Some(target) = input_endpoints.get(&tab_id) else {
        return;
    };
    reset_viewport_and_invalidate(&mut target.terminal.lock());
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
    window_width: u32,
    window_height: u32,
) -> Option<usize> {
    let popup = popup?;
    let cell_w = cell_width as f32;
    let cell_h = cell_height as f32;
    let total_rows = popup.duration_text.is_some() as usize + 4;
    let popup_w = cell_w * POPUP_WIDTH_CELLS;
    let popup_h = total_rows as f32 * cell_h;
    let (popup_x, popup_y) = renderer::gutter_popup_origin(
        popup,
        popup_w,
        popup_h,
        cell_w,
        cell_h,
        gutter_width as f32,
        window_width as f32,
        window_height as f32,
    );
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

fn popup_command_text(
    document: &terminal41::prompt::CommandBlockDocument,
    prompt: PromptRef,
) -> Option<CommandBlockCommand> {
    command_block_for_prompt(document, prompt)?.command.clone()
}

fn popup_rerun_command_text(command: CommandBlockCommand) -> String {
    match command.source {
        CommandTextSource::Observed => command.text.trim().to_owned(),
        CommandTextSource::UntrustedMetadata => command.text,
    }
}

fn popup_rerun_paste(
    command: CommandBlockCommand,
    editor_available: bool,
    bracketed_paste_enabled: bool,
) -> Option<(String, PopupRerunPasteTarget)> {
    let text = popup_rerun_command_text(command);

    if editor_available {
        return Some((text, PopupRerunPasteTarget::Editor));
    }

    if bracketed_paste_enabled {
        return Some((text, PopupRerunPasteTarget::Terminal(PasteMode::Bracketed)));
    }

    if text.contains(['\r', '\n']) {
        return None;
    }

    Some((text, PopupRerunPasteTarget::Terminal(PasteMode::Terminal)))
}

/// Maximum time between clicks that still count as part of a sequence.
const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

fn selection_autoscroll_direction(
    mouse_y: f64,
    cell_height: u32,
    viewport_rows: u32,
) -> Option<SelectionAutoscroll> {
    if cell_height == 0 || viewport_rows == 0 {
        return None;
    }

    let cell_height = cell_height as f64;
    let terminal_top = cell_height;
    let terminal_bottom = terminal_top + viewport_rows as f64 * cell_height;
    let top_edge = terminal_top + cell_height;
    let bottom_edge = terminal_bottom - cell_height;

    if mouse_y < top_edge {
        Some(SelectionAutoscroll::Up)
    } else if mouse_y >= bottom_edge {
        Some(SelectionAutoscroll::Down)
    } else {
        None
    }
}

fn selection_copy_source(
    terminal_has_selection: bool,
    editor_has_selection: bool,
    editor_open: bool,
) -> Option<SelectionCopySource> {
    if terminal_has_selection {
        Some(SelectionCopySource::Terminal)
    } else if editor_open && editor_has_selection {
        Some(SelectionCopySource::Editor)
    } else {
        None
    }
}

fn mouse_report_position_from_pixels(
    raw_x: u32,
    raw_y: u32,
    cell_w: u32,
    cell_h: u32,
    gutter_w: u32,
    cols: u32,
    rows: u32,
    terminal_row_offset: u32,
) -> MouseReportPosition {
    let cell_w = cell_w.max(1);
    let cell_h = cell_h.max(1);
    let cols = cols.max(1);
    let rows = rows.max(1);
    let pixel_x = raw_x
        .saturating_sub(gutter_w)
        .min(cols.saturating_mul(cell_w).saturating_sub(1));
    let pixel_y = raw_y
        .saturating_sub(cell_h)
        .saturating_add(terminal_row_offset.saturating_mul(cell_h))
        .min(rows.saturating_mul(cell_h).saturating_sub(1));

    MouseReportPosition {
        col: (pixel_x / cell_w).min(cols.saturating_sub(1)),
        row: (pixel_y / cell_h).min(rows.saturating_sub(1)),
        pixel_x,
        pixel_y,
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

    let config = config41::init_config(config_reload.clone(), render_thread_handle.clone());

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
    let startup_command_editor = config.command_editor.clone();
    let command_history_store =
        history_runtime::history_db_path().and_then(|path| match history41::open(&path) {
            Ok(store) => Some(store),
            Err(error) => {
                warn!(
                    "persistent command history: failed to open {}: {error}",
                    path.display()
                );
                None
            }
        });
    let command_history_writer = command_history_store
        .as_ref()
        .cloned()
        .and_then(history_runtime::spawn_history_writer);

    // Create the terminal thread handle before spawning the PTY so the PTY
    // reader can unpark the terminal thread once it starts.
    let terminal_thread = TerminalThread::new();
    let term_thread_handle = terminal_thread.thread_handle.clone();

    // Spawn the initial PTY early so the shell starts running immediately.
    let initial_status_rows = u32::from(config.status_line != StatusLineMode::Off);
    let initial_main_rows = INITIAL_ROWS.saturating_sub(initial_status_rows);
    let (pty, pty_writer, pty_reader) = tracing::debug_span!("spawn_pty").in_scope(|| {
        let term_features = terminal41::iterm_features::term_features(&config.feature_permissions);
        Pty::spawn(
            TabId(0),
            INITIAL_COLS as u16,
            initial_main_rows as u16,
            cell_width as u16,
            cell_height as u16,
            Some(term_features),
            command,
            config.shell_integration.hooks,
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
        config.status_line,
        config.feature_permissions.clone(),
        config.limits,
        cell_height,
        cell_width,
        config.palette.clone(),
    );
    settings::set_default_cursor_style(
        &mut terminal.default_cursor_style,
        &mut terminal.cursor_style,
        config.cursor_style,
    );
    settings::set_emoji_compatibility_mode(
        &mut terminal.emoji_compatibility_mode,
        config.compatibility.emoji,
    );
    let (snapshot_publisher, snapshot_output) = terminal41::terminal_snapshot_buffer(&mut terminal);
    let terminal = Arc::new(Mutex::new(terminal));

    terminal_thread.spawn(
        "terminal-0".into(),
        terminal.clone(),
        pty_reader,
        render_thread_handle.clone(),
        snapshot_publisher,
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
        command_editor_config: startup_command_editor.clone(),
        command_editor_views: HashMap::new(),
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
        permission_modal: None,
        command_palette: None,
        history_confirmation: None,
        history_deletion: None,
        toast: None,
        preedit: None,
    }));
    let tab = Tab {
        id: TabId(0),
        terminal: terminal.clone(),
        snapshot_output,
        pty,
        window_sync_epoch: 0,
        terminal_thread,
    };

    // Spawn the render thread.
    let config_reload_ = config_reload.clone();
    let input_state_for_render = input_state.clone();
    let render_thread_handle_for_render = render_thread_handle.clone();
    let render_proxy = proxy.clone();
    let new_tab_text = config.new_tab_text.clone();
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
                config,
                input_state_for_render,
                render_thread_handle_for_render.clone(),
            );
            host.run(window_rx, startup_release_rx);
        })
        .expect("spawn render thread");

    let mut host = WindowHost {
        window: None,
        startup: StartupState {
            presenter: None,
            tabs: vec![tab],
            next_redraw: None,
            release_tx: Some(startup_release_tx),
        },
        input: InputRuntime {
            endpoints: HashMap::from([(
                TabId(0),
                InputEndpoint {
                    terminal: terminal.clone(),
                    terminal_thread: term_thread_handle,
                    writer: pty_writer,
                    recorder: initial_recorder,
                    command_editor: CommandEditor::new(),
                },
            )]),
            active_tab: Some(TabId(0)),
        },
        command: CommandRuntime {
            catalog: CommandCatalog::from_config(&startup_command_editor),
            history_store: command_history_store,
            history_writer: command_history_writer,
            shell_history_entries: Vec::new(),
            shell_history_loaded: false,
            shell_history_enabled: startup_command_editor.deep_history_integration,
        },
        render: RenderRuntime {
            input_state,
            event_tx,
            window_tx: Some(window_tx),
            thread_handle: render_thread_handle,
            event_proxy: proxy,
        },
        keyboard: KeyboardRuntime {
            modifiers: ModifiersState::default(),
            physical_modifiers: PhysicalModifierState::default(),
            ime_preedit_active: false,
        },
        mouse: MouseRuntime {
            pos: (0.0, 0.0),
            mouse_buttons: MouseButtonState::default(),
            last_motion_position: None,
            last_click_time: None,
            last_click_cell: None,
            click_count: 0,
            left_drag_active: false,
            selection_drag_moved: false,
            command_editor_drag_anchor: None,
            selection_autoscroll_direction: None,
            selection_autoscroll_next: None,
        },
        metrics: WindowMetrics {
            window_size: (0, 0),
            new_tab_text,
            opacity,
            cell_width,
            cell_height,
            startup_fonts,
            startup_font_size,
            startup_supersampling,
            startup_dpi_scale,
            startup_gutter,
        },
        modals: ModalRuntime {
            recording_popup: None,
            permission_modal: None,
            history_confirmation: None,
            queued_permission_requests: VecDeque::new(),
            next_recording_popup_token: 1,
            next_toast_token: 1,
        },
    };
    event_loop.run_app(&mut host).expect("run event loop");
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

fn resolve_command_palette_working_directory(
    current_dir: Option<PathBuf>,
    argument: &str,
) -> Option<PathBuf> {
    let argument = argument.trim();
    if argument.is_empty() {
        return None;
    }
    let path = expand_home_path(argument).unwrap_or_else(|| PathBuf::from(argument));
    if path.is_absolute() {
        Some(path)
    } else {
        Some(current_dir.map_or(path.clone(), |dir| dir.join(path)))
    }
}

fn expand_home_path(argument: &str) -> Option<PathBuf> {
    if argument == "~" {
        return dirs::home_dir();
    }
    let rest = argument
        .strip_prefix("~/")
        .or_else(|| argument.strip_prefix("~\\"))?;
    Some(dirs::home_dir()?.join(rest))
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

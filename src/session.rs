//! Terminal session construction.
//!
//! A session is one PTY, one [`Terminal`], one terminal thread, and the two
//! halves the application keeps them in: a [`Tab`], owned by the render
//! thread, and an [`InputEndpoint`], owned by the window thread. The initial
//! session created in `main` and every session the render thread opens later
//! both come from here, so per-session setup only exists once.

use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread::Thread;

use commands41::CommandEditor;
use config41::Config;
use config41::StatusLineMode;
use parking_lot::Mutex;
use pty_pipe41::Pty;
use terminal41::Terminal;
use terminal41::TerminalThread;
use terminal41::settings;
use winit::event_loop::EventLoopProxy;

use crate::output_recording::RecorderControl;
use crate::window_host::AppEvent;
use crate::window_host::InputEndpoint;
use crate::window_host::Tab;
use crate::window_host::TabId;

/// Everything that differs between one session and the next.
pub(crate) struct SessionRequest {
    pub(crate) id: TabId,
    /// Terminal grid width in cells.
    pub(crate) cols: u32,
    /// Terminal grid height in cells, including the status row when one is
    /// configured. The PTY is sized to the main area only.
    pub(crate) rows: u32,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
    pub(crate) scrollback_lines: u32,
    /// Working directory for the child process. `None` inherits ours.
    pub(crate) cwd: Option<PathBuf>,
    /// Program to run in place of the user's shell, from the command line.
    pub(crate) command: Option<Vec<String>>,
    pub(crate) window_sync_epoch: u64,
    /// Invoked from the terminal thread when output arrives, so the startup
    /// presenter can redraw before the GPU renderer exists.
    pub(crate) startup_redraw: Option<Box<dyn Fn() + Send + Sync>>,
}

/// A live session, split by the thread that owns each half.
pub(crate) struct Session {
    pub(crate) tab: Tab,
    pub(crate) endpoint: InputEndpoint,
}

/// Rows available to the child process, which is the grid minus whatever the
/// status line reserves.
///
/// `Terminal::new` derives the same number into `viewport.rows`, but the PTY is
/// spawned first so the shell starts running while the terminal is still being
/// built. A debug assertion below keeps the two derivations honest.
fn main_area_rows(
    rows: u32,
    status_line: StatusLineMode,
) -> u32 {
    rows.saturating_sub(u32::from(status_line != StatusLineMode::Off))
}

/// Build a complete terminal session: spawn the PTY and its child, construct
/// the terminal state, and start the thread that pumps one into the other.
pub(crate) fn spawn_session(
    request: SessionRequest,
    config: &Config,
    render_thread_handle: Arc<OnceLock<Thread>>,
    proxy: EventLoopProxy<AppEvent>,
    child_exit_tx: mpsc::Sender<TabId>,
) -> io::Result<Session> {
    let SessionRequest {
        id,
        cols,
        rows,
        cell_width,
        cell_height,
        scrollback_lines,
        cwd,
        command,
        window_sync_epoch,
        startup_redraw,
    } = request;

    // Create the terminal thread handle before spawning the PTY so the PTY
    // reader can unpark the terminal thread once it starts.
    let terminal_thread = TerminalThread::new();
    let terminal_thread_handle = terminal_thread.thread_handle.clone();

    let (pty, writer, pty_reader) = tracing::debug_span!("spawn_pty").in_scope(|| {
        let term_features = terminal41::iterm_features::term_features(&config.feature_permissions);
        Pty::spawn(
            id,
            cols as u16,
            main_area_rows(rows, config.status_line) as u16,
            cell_width as u16,
            cell_height as u16,
            Some(term_features),
            command,
            config.shell_integration.hooks || config.command_editor.enabled,
            cwd,
            terminal_thread.thread_handle.clone(),
            child_exit_tx,
        )
    })?;

    let mut terminal = Terminal::new(
        cols,
        rows,
        scrollback_lines,
        config.status_line,
        config.feature_permissions.clone(),
        config.limits,
        cell_height,
        cell_width,
        config.palette.clone(),
    );
    debug_assert_eq!(
        terminal.viewport.rows,
        main_area_rows(rows, config.status_line),
        "PTY row count and terminal viewport disagree"
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
    let recorder = RecorderControl::new();

    terminal_thread.spawn(
        format!("terminal-{}", id.0),
        terminal.clone(),
        pty_reader,
        render_thread_handle,
        snapshot_publisher,
        startup_redraw,
        Box::new({
            let recorder = recorder.clone();
            move |bytes| {
                crate::perf_ctrl_c::observe_pty_output(id, bytes);
                recorder.write_chunk(bytes);
            }
        }),
        Box::new(move |effects| {
            let _ = proxy.send_event(AppEvent::ApplyTerminalEffects {
                tab_id: id,
                effects,
            });
        }),
    );

    Ok(Session {
        endpoint: InputEndpoint {
            terminal: terminal.clone(),
            terminal_thread: terminal_thread_handle,
            writer,
            recorder,
            command_editor: CommandEditor::new(),
            shell_editing: Default::default(),
        },
        tab: Tab {
            id,
            terminal,
            snapshot_output,
            pty,
            window_sync_epoch,
            terminal_thread,
        },
    })
}

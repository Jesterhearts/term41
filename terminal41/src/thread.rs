#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::thread::Thread;

use parking_lot::Mutex;
use pty_pipe41::PtyReader;

use crate::TermSnapshotPublisher;
use crate::Terminal;
use crate::TerminalEffects;
use crate::runtime;

/// Handle to a running terminal thread. Signals the thread to stop on drop.
pub struct TerminalThread {
    stop: Arc<AtomicBool>,
    /// Thread handle populated by the terminal thread after it starts.
    pub thread_handle: Arc<OnceLock<Thread>>,
}

impl Default for TerminalThread {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalThread {
    /// Create a fresh `OnceLock` that the terminal thread will populate with
    /// its `Thread` handle. Pass a clone to `Pty::spawn` so the PTY reader
    /// can unpark the terminal thread.
    pub fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            thread_handle: Arc::new(OnceLock::new()),
        }
    }

    /// Spawn the terminal thread. `thread_handle` must be the same `OnceLock`
    /// that was passed to `Pty::spawn` for this tab.
    pub fn spawn(
        &self,
        name: String,
        terminal: Arc<Mutex<Terminal>>,
        pty_reader: PtyReader,
        render_thread_handle: Arc<OnceLock<Thread>>,
        snapshot_publisher: TermSnapshotPublisher,
        startup_redraw: Option<Box<dyn Fn() + Send + Sync>>,
        tee_read: Box<dyn Fn(&[u8]) + Send + Sync>,
        deliver_effects: Box<dyn Fn(TerminalEffects) + Send + Sync>,
    ) {
        if self.thread_handle.get().is_some() {
            error!("terminal thread already running");
            return;
        }

        let stop = self.stop.clone();
        let handle_ = self.thread_handle.clone();

        thread::Builder::new()
            .name(name)
            .spawn(move || {
                handle_
                    .set(thread::current())
                    .expect("set terminal thread handle");
                runtime::run_terminal_thread(
                    terminal,
                    pty_reader,
                    stop,
                    render_thread_handle,
                    snapshot_publisher,
                    startup_redraw,
                    tee_read,
                    deliver_effects,
                );
            })
            .expect("spawn terminal thread");
    }
}

impl Drop for TerminalThread {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread_handle.get() {
            t.unpark();
        }
    }
}

use std::io;
use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;

use cueue::cueue;
use portable_pty::ChildKiller;
use portable_pty::CommandBuilder;
use portable_pty::MasterPty;
use portable_pty::PtySize;
use portable_pty::native_pty_system;
use winit::event_loop::EventLoopProxy;

use crate::AppEvent;
use crate::TabId;

pub const MAX_BUFFER: usize = 128 * 1024 * 1024;
pub const MAX_READ_CHUNK: usize = 128 * 1024;

/// A pseudo-terminal connected to a child shell process.
///
/// Wraps `portable-pty` so the same code path handles forkpty on Unix and
/// ConPTY on Windows. portable-pty exposes a blocking reader, so a worker
/// thread pumps bytes into a channel and `read` drains it without ever
/// blocking the UI loop.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child_killer: Box<dyn ChildKiller>,
    writer: Box<dyn Write + Send>,
    rx: cueue::Reader<u8>,
    /// Coalesce flag shared with the pty-reader thread. The reader
    /// only posts `DataReady` on the false→true transition, so a burst
    /// of reads queues a single event instead of one-per-read — which
    /// otherwise let the reader flood the event loop and starve redraw.
    /// The main thread clears it at the top of its drain; if the drain
    /// bails on its time slice with bytes still in the ring, the main
    /// thread re-arms and re-posts so the leftover doesn't sit stale
    /// waiting for the child to write again.
    pending: Arc<AtomicBool>,
}

impl Pty {
    /// Spawns a child process in a new PTY with the given grid size. When
    /// `command` is `Some`, the first element is the program and the rest are
    /// its arguments; otherwise the user's default shell is launched.
    pub fn spawn(
        tab_id: TabId,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
        command: Option<Vec<String>>,
        cwd: Option<std::path::PathBuf>,
        event_loop: EventLoopProxy<AppEvent>,
    ) -> io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: cell_width,
                pixel_height: cell_height,
            })
            .map_err(io::Error::other)?;

        let mut cmd = match command {
            Some(argv) if !argv.is_empty() => {
                let mut iter = argv.into_iter();
                let program = iter.next().expect("argv non-empty");
                let mut builder = CommandBuilder::new(program);
                builder.args(iter);
                builder
            }
            // new_default_prog resolves to $SHELL (or the passwd entry) on
            // Unix and to %ComSpec%/cmd.exe on Windows, and arranges
            // login-shell argv0 semantics where applicable.
            _ => CommandBuilder::new_default_prog(),
        };
        cmd.env("TERM", "xterm-256color");
        // Advertise iTerm2 in TERM_PROGRAM so clients that gate inline-image
        // output on a hardcoded allowlist (viu, chafa, rich, etc.) emit the
        // iTerm2 OSC 1337 protocol — which we now implement. The app
        // would otherwise fall back to half-blocks even though we could
        // render full images.
        cmd.env("TERM_PROGRAM", "iTerm.app");
        cmd.env("TERM_PROGRAM_VERSION", "3.5.0");
        match cwd {
            Some(dir) => cmd.cwd(dir),
            None => {
                if let Ok(dir) = std::env::current_dir() {
                    cmd.cwd(dir);
                }
            }
        }

        let mut child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        // Drop our handle on the slave so the child is the only side keeping
        // it open; that way closing the master at shutdown delivers SIGHUP
        // (or the ConPTY equivalent) cleanly.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = pair.master.take_writer().map_err(io::Error::other)?;

        let (read_tx, rx) = cueue(MAX_BUFFER)?;
        let pending = Arc::new(AtomicBool::new(false));
        let pending_reader = pending.clone();
        let event_loop_ = event_loop.clone();
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || pump_reader(tab_id, reader, read_tx, event_loop_, pending_reader))
            .map_err(io::Error::other)?;

        let child_killer = child.clone_killer();
        thread::Builder::new()
            .name("child-watcher".into())
            .spawn(move || {
                let _ = child.wait();
                let _ = event_loop.send_event(AppEvent::ChildExited(tab_id));
            })
            .map_err(io::Error::other)?;

        Ok(Self {
            master: pair.master,
            child_killer,
            writer,
            rx,
            pending,
        })
    }

    /// Release the coalesce flag so the reader is free to post a fresh
    /// `DataReady` if it writes more data. Call at the top of a drain —
    /// if the reader races us during the drain, we see its data in the
    /// ring this pass, and its event re-enters us cleanly next pass.
    pub fn clear_pending(&self) {
        self.pending.store(false, Ordering::Release);
    }

    /// Mark a `DataReady` as in flight. Returns `true` when the caller
    /// is responsible for actually posting the event (flag was false),
    /// `false` when one is already pending (reader beat us to it).
    pub fn arm_pending(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    /// Non-blocking read of bytes received from the PTY. Returns 0 when no
    /// data is currently available so callers can poll in the event loop.
    pub fn read(
        &mut self,
        buf: &mut [u8],
    ) -> usize {
        let data = self
            .rx
            .limited_read_chunk(buf.len().min(MAX_READ_CHUNK) as u64);
        let read_len = data.len();
        buf[..read_len].copy_from_slice(data);
        self.rx.commit();
        read_len
    }

    /// Write bytes to the PTY (sends input to the shell).
    pub fn write(
        &mut self,
        data: &[u8],
    ) -> io::Result<()> {
        self.writer.write_all(data)
    }

    /// Notify the PTY of a terminal resize.
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
    ) {
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child_killer.kill();
    }
}

fn pump_reader(
    tab_id: TabId,
    mut reader: Box<dyn Read + Send>,
    mut tx: cueue::Writer<u8>,
    event_loop: EventLoopProxy<AppEvent>,
    pending: Arc<AtomicBool>,
) {
    let mut buf = [0u8; MAX_READ_CHUNK];

    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut written = 0;
                loop {
                    match tx.write_chunk().write(&buf[written..n]) {
                        Ok(m) => {
                            written += m;
                            tx.commit(m);
                            if written >= n {
                                break;
                            }
                            thread::yield_now();
                        }
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return,
                    }
                }
                if !pending.swap(true, Ordering::AcqRel) {
                    let _ = event_loop.send_event(AppEvent::DataReady(tab_id));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

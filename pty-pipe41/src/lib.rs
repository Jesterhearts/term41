#![allow(clippy::too_many_arguments)]

use std::io;
use std::io::Read;
use std::io::Write;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread;
use std::thread::Thread;

use cueue::cueue;
use portable_pty::ChildKiller;
use portable_pty::CommandBuilder;
use portable_pty::MasterPty;
use portable_pty::PtySize;
use portable_pty::native_pty_system;

#[macro_use]
extern crate log;

pub const MAX_READ_CHUNK: usize = 128 * 1024;
// Keep PTY read-ahead modest so interactive control input (Ctrl+C, etc.)
// doesn't end up visually stuck behind tens of megabytes of already-buffered
// output during huge bursts like `cat bigfile`.
pub const MAX_BUFFER: usize = MAX_READ_CHUNK * 8; // 1 MB

/// Read half of a PTY connection. Owns the cueue ring-buffer consumer and the
/// coalesce flag shared with the pump thread. Lives on the terminal thread so
/// PTY data can be drained and parsed without touching the render thread.
pub struct PtyReader {
    rx: cueue::Reader<u8>,
    /// Coalesce flag shared with the pty-reader thread. The reader
    /// only unparks the consumer thread on the false→true transition, so
    /// a burst of reads produces a single wakeup instead of one per read.
    /// The consumer clears it at the top of its drain.
    pending_read: Arc<AtomicBool>,
}

impl PtyReader {
    /// Release the coalesce flag so the reader thread is free to unpark us
    /// again. Call at the top of a drain — if the reader races us during the
    /// drain, we see its data in the ring this pass, and its wakeup
    /// re-enters us cleanly next pass.
    pub fn clear_pending(&self) {
        self.pending_read.store(false, Ordering::Release);
    }

    /// Non-blocking read of bytes received from the PTY. Returns 0 when no
    /// data is currently available.
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
}

/// Write half of a PTY connection. Keeps the master fd (for resize), the
/// child killer (for cleanup). Lives on the render thread.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    child_killer: Box<dyn ChildKiller>,
}

/// Input half of a PTY connection. Lives on the window thread so user input
/// can be forwarded to the child process without cross-thread locking.
pub struct PtyWriter {
    writer: Box<dyn Write + Send>,
}

impl Pty {
    /// Spawns a child process in a new PTY with the given grid size. Returns
    /// the resize/child-control half (`Pty`), the write half (`PtyWriter`),
    /// and the read half (`PtyReader`).
    ///
    /// When `command` is `Some`, the first element is the program and the rest
    /// are its arguments; otherwise the user's default shell is launched.
    ///
    /// `data_thread` is the thread that consumes PTY output (the terminal
    /// thread). The PTY pump thread unparks it when new data arrives.
    ///
    /// `render_thread` is used by the child-watcher thread to unpark the
    /// render loop when the child process exits.
    pub fn spawn<TabId>(
        tab_id: TabId,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
        command: Option<Vec<String>>,
        cwd: Option<std::path::PathBuf>,
        data_thread: Arc<OnceLock<Thread>>,
        render_thread: Arc<OnceLock<Thread>>,
        child_exit_tx: mpsc::Sender<TabId>,
    ) -> io::Result<(Self, PtyWriter, PtyReader)>
    where
        TabId: Send + 'static + Into<u64>,
    {
        let start_time = std::time::Instant::now();

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
        let pending_read = Arc::new(AtomicBool::new(false));
        let pending_read_ = pending_read.clone();

        // The pump thread unparks the data consumer (terminal thread) when
        // new bytes arrive.
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || pump_reader(start_time, reader, read_tx, data_thread, pending_read_))
            .map_err(io::Error::other)?;

        // The child watcher unparks the render thread so it can handle the
        // tab close.
        let child_killer = child.clone_killer();
        thread::Builder::new()
            .name("child-watcher".into())
            .spawn(move || {
                let _ = child.wait();
                let _ = child_exit_tx.send(tab_id);
                if let Some(t) = render_thread.get() {
                    t.unpark();
                }
            })
            .map_err(io::Error::other)?;

        Ok((
            Self {
                master: pair.master,
                child_killer,
            },
            PtyWriter { writer },
            PtyReader { rx, pending_read },
        ))
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

impl PtyWriter {
    /// Write bytes to the PTY (sends input to the shell).
    pub fn write(
        &mut self,
        data: &[u8],
    ) -> io::Result<()> {
        self.writer.write_all(data)
    }
}

fn pump_reader(
    start_time: std::time::Instant,
    mut reader: Box<dyn Read + Send>,
    mut tx: cueue::Writer<u8>,
    consumer_thread: Arc<OnceLock<Thread>>,
    pending_read: Arc<AtomicBool>,
) {
    let mut buf = [0u8; MAX_READ_CHUNK];
    let mut read = false;

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
                            if !pending_read.swap(true, Ordering::AcqRel)
                                && let Some(t) = consumer_thread.get()
                            {
                                if !read {
                                    info!("TTFR: {} ms", start_time.elapsed().as_millis());
                                    read = true;
                                }
                                t.unpark();
                            }
                            if written >= n {
                                break;
                            }
                            thread::yield_now();
                        }
                        Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                        Err(_) => return,
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

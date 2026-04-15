use std::collections::VecDeque;
use std::io;
use std::io::Read;
use std::io::Write;
use std::sync::mpsc;
use std::thread;

use portable_pty::Child;
use portable_pty::CommandBuilder;
use portable_pty::MasterPty;
use portable_pty::PtySize;
use portable_pty::native_pty_system;

/// A pseudo-terminal connected to a child shell process.
///
/// Wraps `portable-pty` so the same code path handles forkpty on Unix and
/// ConPTY on Windows. portable-pty exposes a blocking reader, so a worker
/// thread pumps bytes into a channel and `read` drains it without ever
/// blocking the UI loop.
pub struct Pty {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn Child + Send + Sync>,
    rx: mpsc::Receiver<Vec<u8>>,
    pending: VecDeque<u8>,
}

impl Pty {
    /// Spawns the user's default shell in a new PTY with the given grid size.
    pub fn spawn(
        cols: u16,
        rows: u16,
    ) -> io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(io::Error::other)?;

        // new_default_prog resolves to $SHELL (or the passwd entry) on Unix
        // and to %ComSpec%/cmd.exe on Windows, and arranges login-shell argv0
        // semantics where applicable.
        let mut cmd = CommandBuilder::new_default_prog();
        cmd.env("TERM", "xterm-256color");

        let child = pair.slave.spawn_command(cmd).map_err(io::Error::other)?;
        // Drop our handle on the slave so the child is the only side keeping
        // it open; that way closing the master at shutdown delivers SIGHUP
        // (or the ConPTY equivalent) cleanly.
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().map_err(io::Error::other)?;
        let writer = pair.master.take_writer().map_err(io::Error::other)?;

        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("pty-reader".into())
            .spawn(move || pump_reader(reader, tx))
            .map_err(io::Error::other)?;

        Ok(Self {
            master: pair.master,
            writer,
            child,
            rx,
            pending: VecDeque::new(),
        })
    }

    /// Non-blocking read of bytes received from the PTY. Returns 0 when no
    /// data is currently available so callers can poll in the event loop.
    pub fn read(
        &mut self,
        buf: &mut [u8],
    ) -> io::Result<usize> {
        if self.pending.is_empty() {
            match self.rx.try_recv() {
                Ok(chunk) => self.pending.extend(chunk),
                // Empty: nothing pending right now. Disconnected: reader
                // thread exited (child closed its end); treat as EOF — keep
                // returning 0 so the UI loop stays responsive until the user
                // closes the window.
                Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => return Ok(0),
            }
        }

        let n = buf.len().min(self.pending.len());
        let (head, tail) = self.pending.as_slices();
        let from_head = n.min(head.len());
        buf[..from_head].copy_from_slice(&head[..from_head]);
        if n > from_head {
            buf[from_head..n].copy_from_slice(&tail[..n - from_head]);
        }
        self.pending.drain(..n);
        Ok(n)
    }

    /// Write bytes to the PTY (sends input to the shell).
    pub fn write(
        &mut self,
        data: &[u8],
    ) -> io::Result<usize> {
        self.writer.write(data)
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
        let _ = self.child.kill();
    }
}

fn pump_reader(
    mut reader: Box<dyn Read + Send>,
    tx: mpsc::Sender<Vec<u8>>,
) {
    let mut buf = vec![0u8; 128 * 1024];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

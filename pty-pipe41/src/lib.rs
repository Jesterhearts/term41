#![allow(clippy::too_many_arguments)]

use std::io;
use std::io::Read;
use std::io::Write;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::fd::FromRawFd;
#[cfg(target_os = "macos")]
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProgram {
    pub exe_path: PathBuf,
    pub exe_name: String,
}

impl ForegroundProgram {
    pub fn from_exe_path(exe_path: PathBuf) -> Option<Self> {
        let exe_name = exe_path.file_name()?.to_string_lossy().into_owned();
        Some(Self { exe_path, exe_name })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ForegroundProcessSet {
    pub programs: Vec<ForegroundProgram>,
}

impl ForegroundProcessSet {
    pub fn is_empty(&self) -> bool {
        self.programs.is_empty()
    }
}

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
    #[cfg(unix)]
    foreground_probe: Option<ForegroundProbe>,
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

    pub fn foreground_processes(&mut self) -> Option<ForegroundProcessSet> {
        #[cfg(unix)]
        {
            self.foreground_probe
                .as_mut()
                .and_then(ForegroundProbe::resolve)
        }
        #[cfg(not(unix))]
        {
            None
        }
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
        #[cfg(unix)]
        let foreground_probe = pair
            .master
            .as_raw_fd()
            .and_then(|fd| ForegroundProbe::new(fd, pair.master.tty_name()));
        debug!(
            "Spawned child with PID {:?}, PTY master fd {:?}, foreground probe: \
             {foreground_probe:?}",
            child.process_id(),
            pair.master.as_raw_fd(),
        );

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
            })
            .map_err(io::Error::other)?;

        Ok((
            Self {
                master: pair.master,
                child_killer,
            },
            PtyWriter { writer },
            PtyReader {
                rx,
                pending_read,
                #[cfg(unix)]
                foreground_probe,
            },
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

#[cfg(unix)]
#[derive(Debug)]
struct ForegroundProbe {
    master_fd: std::os::fd::OwnedFd,
    tty_path: Option<PathBuf>,
    cached_pgrp: Option<libc::pid_t>,
    cached_processes: Option<ForegroundProcessSet>,
}

#[cfg(unix)]
impl ForegroundProbe {
    fn new(
        raw_fd: std::os::unix::io::RawFd,
        tty_path: Option<PathBuf>,
    ) -> Option<Self> {
        debug!(
            "Creating ForegroundProbe for fd {raw_fd} (tty path: {:?})",
            tty_path
        );

        let dup_fd = unsafe { libc::dup(raw_fd) };
        (dup_fd >= 0).then(|| Self {
            // SAFETY: dup() returned a fresh owned fd on success.
            master_fd: unsafe { std::os::fd::OwnedFd::from_raw_fd(dup_fd) },
            tty_path,
            cached_pgrp: None,
            cached_processes: None,
        })
    }

    fn resolve(&mut self) -> Option<ForegroundProcessSet> {
        let maybe_pgrp = current_foreground_pgrp(self.master_fd.as_raw_fd()).or_else(|| {
            self.tty_path
                .as_ref()
                .and_then(current_foreground_pgrp_from_tty_path)
        });

        trace!("ForegroundProbe: current foreground pgrp: {maybe_pgrp:?}");
        let pgrp = maybe_pgrp?;

        if self.cached_pgrp == Some(pgrp) && self.cached_processes.is_some() {
            return self.cached_processes.clone();
        }
        let processes = resolve_foreground_processes(pgrp);
        if let Some(processes) = processes {
            self.cached_pgrp = Some(pgrp);
            self.cached_processes = Some(processes.clone());
            Some(processes)
        } else {
            self.cached_pgrp = None;
            self.cached_processes = None;
            None
        }
    }
}

#[cfg(unix)]
fn current_foreground_pgrp(fd: std::os::unix::io::RawFd) -> Option<libc::pid_t> {
    match unsafe { libc::tcgetpgrp(fd) } {
        pid if pid > 0 => Some(pid),
        e => {
            trace!("tcgetpgrp failed with {e}");
            None
        }
    }
}

#[cfg(target_os = "linux")]
fn resolve_foreground_processes(pgrp: libc::pid_t) -> Option<ForegroundProcessSet> {
    let mut programs = vec![];
    for entry in std::fs::read_dir("/proc").ok()? {
        let Ok(entry) = entry else {
            trace!("Failed to read /proc entry: {entry:?}");
            continue;
        };
        let file_name = entry.file_name();
        let Ok(pid) = file_name.to_string_lossy().parse::<libc::pid_t>() else {
            trace!("Non-numeric /proc entry: {file_name:?}");
            continue;
        };
        let Some(member_pgrp) = process_group_for_pid(pid) else {
            trace!("Failed to get process group for PID {pid}");
            continue;
        };
        if member_pgrp != pgrp {
            trace!("PID {pid} is in process group {member_pgrp}, not {pgrp}");
            continue;
        }
        trace!("PID {pid} is in foreground process group {pgrp}");
        let Some(exe) = std::fs::read_link(format!("/proc/{pid}/exe")).ok() else {
            trace!("Failed to read /proc/{pid}/exe");
            continue;
        };
        let Some(program) = ForegroundProgram::from_exe_path(exe) else {
            trace!("Failed to parse executable path for PID {pid}");
            continue;
        };
        if !programs.contains(&program) {
            programs.push(program);
        }
    }
    (!programs.is_empty()).then_some(ForegroundProcessSet { programs })
}

#[cfg(unix)]
fn current_foreground_pgrp_from_tty_path(path: &std::path::PathBuf) -> Option<libc::pid_t> {
    use std::os::fd::AsRawFd;

    let tty = std::fs::OpenOptions::new().read(true).open(path).ok()?;
    current_foreground_pgrp(tty.as_raw_fd())
}

#[cfg(target_os = "macos")]
fn resolve_foreground_processes(pgrp: libc::pid_t) -> Option<ForegroundProcessSet> {
    let mut programs = vec![];
    for pid in list_process_group_members(pgrp)? {
        let Some(exe) = executable_path_for_pid(pid) else {
            trace!("Failed to get executable path for PID {pid}");
            continue;
        };
        let Some(program) = ForegroundProgram::from_exe_path(exe) else {
            trace!("Failed to parse executable path for PID {pid}");
            continue;
        };
        if !programs.contains(&program) {
            programs.push(program);
        }
    }
    (!programs.is_empty()).then_some(ForegroundProcessSet { programs })
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn resolve_foreground_processes(_pgrp: libc::pid_t) -> Option<ForegroundProcessSet> {
    None
}

#[cfg(target_os = "linux")]
fn process_group_for_pid(pid: libc::pid_t) -> Option<libc::pid_t> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    trace!("Read /proc/{pid}/stat: {stat}");
    let after_comm = stat.rsplit_once(") ")?.1;
    let mut fields = after_comm.split_ascii_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    fields.next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn process_group_parser_skips_ppid_field() {
        let stat = "1234 (selftest41) S 4321 5678 5678 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0";
        let after_comm = stat.rsplit_once(") ").unwrap().1;
        let mut fields = after_comm.split_ascii_whitespace();
        let _state = fields.next().unwrap();
        let _ppid = fields.next().unwrap();
        let pgrp: libc::pid_t = fields.next().unwrap().parse().unwrap();
        assert_eq!(pgrp, 5678);
    }
}

#[cfg(target_os = "macos")]
fn list_process_group_members(pgrp: libc::pid_t) -> Option<Vec<libc::pid_t>> {
    let count = unsafe { libc::proc_listpgrppids(pgrp, std::ptr::null_mut(), 0) };
    if count <= 0 {
        return None;
    }
    let mut pids = vec![0i32; count as usize];
    let buffer_size = (pids.len() * std::mem::size_of::<libc::pid_t>()) as i32;
    let written = unsafe {
        libc::proc_listpgrppids(pgrp, pids.as_mut_ptr().cast::<libc::c_void>(), buffer_size)
    };
    if written <= 0 {
        return None;
    }
    pids.truncate(written as usize);
    Some(pids.into_iter().filter(|pid| *pid > 0).collect())
}

#[cfg(target_os = "macos")]
fn executable_path_for_pid(pid: libc::pid_t) -> Option<PathBuf> {
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let written = unsafe {
        libc::proc_pidpath(
            pid,
            path.as_mut_ptr().cast::<libc::c_void>(),
            path.len() as u32,
        )
    };
    if written <= 0 {
        return None;
    }
    path.truncate(written as usize);
    Some(PathBuf::from(std::ffi::OsString::from_vec(path)))
}

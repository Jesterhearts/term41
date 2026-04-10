use std::ffi::CString;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// A pseudo-terminal connected to a child shell process.
pub struct Pty {
    master: OwnedFd,
    child_pid: libc::pid_t,
}

impl Pty {
    /// Spawns the user's default shell in a new PTY with the given grid size.
    pub fn spawn(
        cols: u16,
        rows: u16,
    ) -> io::Result<Self> {
        let winsize = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };

        let mut master_fd: RawFd = -1;

        // Safety: forkpty is a well-defined POSIX call. We immediately exec in the child.
        let pid = unsafe {
            libc::forkpty(
                &mut master_fd,
                std::ptr::null_mut(),
                std::ptr::null(),
                &winsize,
            )
        };

        if pid < 0 {
            return Err(io::Error::last_os_error());
        }

        if pid == 0 {
            // Child process: exec the user's shell.
            exec_shell();
        }

        // Parent process.
        let master = unsafe { OwnedFd::from_raw_fd(master_fd) };
        set_nonblocking(&master)?;

        Ok(Self {
            master,
            child_pid: pid,
        })
    }

    /// Non-blocking read from the PTY master fd.
    pub fn read(
        &self,
        buf: &mut [u8],
    ) -> io::Result<usize> {
        let fd = fd_raw(&self.master);
        // Safety: buf is a valid slice, fd is owned by us.
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::WouldBlock {
                return Ok(0);
            }
            return Err(err);
        }
        Ok(n as usize)
    }

    /// Write bytes to the PTY (sends input to the shell).
    pub fn write(
        &self,
        data: &[u8],
    ) -> io::Result<usize> {
        let fd = fd_raw(&self.master);
        // Safety: data is a valid slice, fd is owned by us.
        let n = unsafe { libc::write(fd, data.as_ptr().cast(), data.len()) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }

    /// Notify the PTY of a terminal resize.
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
    ) {
        let winsize = libc::winsize {
            ws_col: cols,
            ws_row: rows,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        let fd = fd_raw(&self.master);
        // Safety: fd is valid, winsize is on the stack.
        unsafe {
            libc::ioctl(fd, libc::TIOCSWINSZ, &winsize);
        }
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        unsafe {
            libc::kill(self.child_pid, libc::SIGHUP);
        }
    }
}

fn fd_raw(fd: &OwnedFd) -> RawFd {
    use std::os::fd::AsRawFd;
    fd.as_raw_fd()
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    let raw = fd_raw(fd);
    // Safety: standard fcntl usage on a valid fd.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn exec_shell() -> ! {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let shell_c = CString::new(shell.clone()).expect("shell path");

    // Set TERM so programs know our capabilities.
    let term = CString::new("TERM=xterm-256color").expect("TERM env");
    unsafe {
        libc::putenv(term.into_raw());
    }

    // Exec as a login shell by convention (argv[0] starts with '-').
    let shell_name = std::path::Path::new(&shell)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy();
    let argv0 = CString::new(format!("-{shell_name}")).expect("argv0");
    let argv: [*const libc::c_char; 2] = [argv0.as_ptr(), std::ptr::null()];

    unsafe {
        libc::execvp(shell_c.as_ptr(), argv.as_ptr());
    }

    // If execvp returns, something went wrong.
    eprintln!("term41: failed to exec shell: {shell}");
    unsafe { libc::_exit(1) }
}

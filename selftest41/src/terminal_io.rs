use std::io;
use std::io::Stdout;
use std::io::Write;
#[cfg(unix)]
use std::os::fd::AsFd;
use std::time::Duration;
use std::time::Instant;

use crossterm::cursor;
use crossterm::event;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::execute;
use crossterm::terminal;
use crossterm::terminal::EnterAlternateScreen;
use crossterm::terminal::LeaveAlternateScreen;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
#[cfg(unix)]
use rustix::event::PollFd;
#[cfg(unix)]
use rustix::event::PollFlags;
#[cfg(unix)]
use rustix::event::Timespec;

use crate::capabilities;
use crate::capabilities::CapabilityReport;
use crate::demo;
use crate::demo::DemoId;

pub struct TerminalSession {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalSession {
    pub fn enter() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableMouseCapture,
            cursor::Hide
        )?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    pub fn terminal_mut(&mut self) -> &mut Terminal<CrosstermBackend<Stdout>> {
        &mut self.terminal
    }

    pub fn poll_event(
        &mut self,
        timeout: Duration,
    ) -> io::Result<Option<Event>> {
        if event::poll(timeout)? {
            return Ok(Some(event::read()?));
        }
        Ok(None)
    }

    pub fn probe_capabilities(&mut self) -> io::Result<CapabilityReport> {
        let out = self.terminal.backend_mut();
        write!(out, "\x1b[c")?;
        out.flush()?;

        #[cfg(unix)]
        if let Some(report) =
            probe_capabilities_from_stdin(Instant::now() + Duration::from_millis(250))?
        {
            return Ok(report);
        }

        let deadline = Instant::now() + Duration::from_millis(250);
        let mut bytes = Vec::new();
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !event::poll(remaining.min(Duration::from_millis(25)))? {
                continue;
            }
            let event = event::read()?;
            collect_reply_bytes(&mut bytes, &event);
            if let Some(report) = capabilities::parse_da1_reply(&bytes) {
                return Ok(report);
            }
        }
        Ok(capabilities::fallback_report())
    }

    pub fn run_demo(
        &mut self,
        demo_id: DemoId,
        capabilities: &CapabilityReport,
    ) -> io::Result<()> {
        self.suspend_tui()?;
        {
            let out = self.terminal.backend_mut();
            demo::run_demo(out, demo_id, capabilities)?;
        }
        wait_for_keypress()?;
        {
            let out = self.terminal.backend_mut();
            write!(out, "\x1b[0m\x1bc")?;
            out.flush()?;
        }
        self.resume_tui()
    }

    fn suspend_tui(&mut self) -> io::Result<()> {
        let out = self.terminal.backend_mut();
        execute!(out, DisableMouseCapture, LeaveAlternateScreen, cursor::Show)?;
        Ok(())
    }

    fn resume_tui(&mut self) -> io::Result<()> {
        let out = self.terminal.backend_mut();
        execute!(out, EnterAlternateScreen, EnableMouseCapture, cursor::Hide)?;
        self.terminal.autoresize()?;
        self.terminal.clear()?;
        Ok(())
    }
}

#[cfg(unix)]
fn probe_capabilities_from_stdin(deadline: Instant) -> io::Result<Option<CapabilityReport>> {
    let stdin = io::stdin();
    let stdin = stdin.lock();
    let fd = stdin.as_fd();
    let mut bytes = Vec::new();
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = remaining.min(Duration::from_millis(25));
        let mut poll_fds = [PollFd::from_borrowed_fd(fd, PollFlags::IN)];
        let timeout = Timespec::try_from(timeout).map_err(io::Error::other)?;
        let ready = rustix::event::poll(&mut poll_fds, Some(&timeout)).map_err(io::Error::other)?;
        if ready == 0 {
            continue;
        }

        let mut buf = [0u8; 256];
        let read = rustix::io::read(fd, &mut buf).map_err(io::Error::other)?;
        if read == 0 {
            continue;
        }
        bytes.extend_from_slice(&buf[..read]);
        if let Some(report) = capabilities::parse_da1_reply(&bytes) {
            return Ok(Some(report));
        }
    }
    Ok(None)
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            cursor::Show,
            DisableMouseCapture,
            LeaveAlternateScreen
        );
    }
}

fn wait_for_keypress() -> io::Result<()> {
    loop {
        match event::read()? {
            Event::Key(_) | Event::Mouse(_) => return Ok(()),
            _ => {}
        }
    }
}

fn collect_reply_bytes(
    bytes: &mut Vec<u8>,
    event: &Event,
) {
    let Event::Key(key) = event else {
        return;
    };
    match key.code {
        KeyCode::Esc => bytes.push(0x1b),
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Char(ch) => {
            let mut buf = [0; 4];
            bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        }
        _ => {}
    }
}

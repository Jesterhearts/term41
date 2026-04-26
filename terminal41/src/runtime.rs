use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread::Thread;

use parking_lot::Mutex;
use pty_pipe41::MAX_READ_CHUNK;
use pty_pipe41::PtyReader;

use crate::TermSnapshotPublisher;
use crate::Terminal;
use crate::TerminalEffects;
use crate::TerminalProcessor;
use crate::publish_terminal_snapshot;

pub(crate) const TERMINAL_BATCH_TIME_BUDGET: std::time::Duration =
    std::time::Duration::from_millis(4);

pub(crate) fn run_terminal_thread(
    terminal: Arc<Mutex<Terminal>>,
    mut pty_reader: PtyReader,
    stop: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<Thread>>,
    snapshot_publisher: TermSnapshotPublisher,
    startup_redraw: Option<Box<dyn Fn() + Send + Sync>>,
    tee_read: Box<dyn Fn(&[u8]) + Send + Sync>,
    deliver_effects: Box<dyn Fn(TerminalEffects) + Send + Sync>,
) {
    let mut processor = TerminalProcessor::new();
    let mut buf = [0u8; MAX_READ_CHUNK];

    loop {
        pty_reader.clear_pending();
        let mut did_work = false;
        let mut hit_budget = false;
        let mut batch_effects = TerminalEffects::default();
        let batch_start = std::time::Instant::now();
        loop {
            let n = pty_reader.read(&mut buf);
            if n == 0 {
                break;
            }
            did_work = true;
            trace!("Read {n} bytes from PTY");
            tee_read(&buf[..n]);

            let effects = {
                let mut terminal = terminal.lock();
                processor.process_bytes(&mut terminal, &buf[..n])
            };
            batch_effects.extend(effects);
            if terminal_batch_budget_exhausted(batch_start) {
                hit_budget = true;
                break;
            }
        }

        if did_work && !batch_effects.is_empty() {
            deliver_effects(batch_effects);
        }
        if did_work {
            {
                let mut terminal = terminal.lock();
                publish_terminal_snapshot(&mut terminal, &snapshot_publisher);
            }
            if let Some(request_redraw) = startup_redraw.as_ref() {
                request_redraw();
            }
            if let Some(thread) = render_thread_handle.get() {
                thread.unpark();
            }
        }

        if stop.load(Ordering::Acquire) {
            break;
        }

        if hit_budget {
            std::thread::yield_now();
            continue;
        }

        std::thread::park();
        if stop.load(Ordering::Acquire) {
            break;
        }
    }
}

pub(crate) fn terminal_batch_budget_exhausted(batch_start: std::time::Instant) -> bool {
    batch_start.elapsed() >= TERMINAL_BATCH_TIME_BUDGET
}

#[cfg(test)]
mod tests {

    #[test]
    fn terminal_batch_budget_trips_on_time_limit() {
        let start = std::time::Instant::now() - super::TERMINAL_BATCH_TIME_BUDGET;
        assert!(super::terminal_batch_budget_exhausted(start));
    }
}

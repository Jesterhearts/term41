use super::*;

pub(crate) const TERMINAL_BATCH_TIME_BUDGET: std::time::Duration =
    std::time::Duration::from_millis(2);

pub(crate) fn run_terminal_thread(
    terminal: Arc<Mutex<Terminal>>,
    mut pty_reader: PtyReader,
    stop: Arc<AtomicBool>,
    render_thread_handle: Arc<OnceLock<Thread>>,
    startup_redraw: Option<Box<dyn Fn() + Send + Sync>>,
    tee_read: Box<dyn Fn(&[u8]) + Send + Sync>,
    output_ready: Box<dyn Fn() + Send + Sync>,
    host_resize: Box<dyn Fn(u32, u32) + Send + Sync>,
) {
    let mut parser = vtepp::Parser::new();
    let mut hooks: Vec<dcs::HookState> = vec![];
    let mut buf = [0u8; MAX_READ_CHUNK];

    loop {
        pty_reader.clear_pending();
        let mut did_work = false;
        let mut hit_budget = false;
        let batch_start = std::time::Instant::now();
        loop {
            let n = pty_reader.read(&mut buf);
            if n == 0 {
                break;
            }
            did_work = true;
            let foreground_processes = pty_reader.foreground_processes();
            trace!("Read {n} bytes from PTY, foreground processes: {foreground_processes:?}");
            tee_read(&buf[..n]);

            terminal
                .lock()
                .unwrap()
                .set_foreground_processes(foreground_processes);
            for action in parser.parse(&buf[..n]) {
                match action {
                    vtepp::Action::Hook {
                        params,
                        intermediates,
                        action,
                    } => dcs::push_hook_state(&mut hooks, params, intermediates, action),
                    vtepp::Action::Put(bytes) => dcs::append_hook_bytes(&mut hooks, bytes),
                    vtepp::Action::Unhook => {
                        let hook = hooks.pop().unwrap();
                        dcs::dispatch_hook(hook, &mut terminal.lock().unwrap());
                    }
                    action => {
                        terminal.lock().unwrap().apply(action);
                    }
                }
                if let Some((cols, rows)) =
                    host::take_pending_host_resize(&mut terminal.lock().unwrap().output)
                {
                    host_resize(cols, rows);
                }
            }
            if terminal_batch_budget_exhausted(batch_start) {
                hit_budget = true;
                break;
            }
        }

        if did_work && let Some(request_redraw) = startup_redraw.as_ref() {
            request_redraw();
        }
        if did_work {
            output_ready();
        }
        if did_work && let Some(thread) = render_thread_handle.get() {
            thread.unpark();
        }

        if stop.load(Ordering::Acquire) {
            break;
        }

        if hit_budget {
            thread::yield_now();
            continue;
        }

        thread::park();
        if stop.load(Ordering::Acquire) {
            break;
        }
    }
}

pub(crate) fn terminal_batch_budget_exhausted(batch_start: std::time::Instant) -> bool {
    batch_start.elapsed() >= TERMINAL_BATCH_TIME_BUDGET
}

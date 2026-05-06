use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::thread;
use std::thread::JoinHandle;
use std::thread::Thread;
use std::time::Duration;

use config41::CommandEditorConfig;
use history41::HistoryRetention;
use history41::HistoryStore;
use history41::StoreCommandRequest;

const HISTORY_WRITE_QUEUE_SIZE: usize = 1024;
const HISTORY_WRITER_IDLE_POLL: Duration = Duration::from_millis(250);
const GLOBAL_HISTORY_RETENTION_MULTIPLIER: usize = 20;

pub(crate) struct HistoryWriter {
    tx: Option<cueue::Writer<StoreCommandRequest>>,
    thread: Arc<OnceLock<Thread>>,
    join_handle: Option<JoinHandle<()>>,
    dropped_writes: u64,
}

impl HistoryWriter {
    pub(crate) fn enqueue(
        &mut self,
        request: StoreCommandRequest,
    ) {
        let Some(tx) = self.tx.as_mut() else {
            self.record_drop();
            return;
        };
        if tx.push(request).is_err() {
            self.record_drop();
            return;
        }
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    pub(crate) fn finish(mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
        if let Some(handle) = self.join_handle.take()
            && let Err(error) = handle.join()
        {
            debug!("persistent command history writer join failed: {error:?}");
        }
    }

    fn record_drop(&mut self) {
        self.dropped_writes = self.dropped_writes.saturating_add(1);
        if self.dropped_writes == 1 || self.dropped_writes.is_multiple_of(100) {
            debug!(
                "persistent command history dropped {} write request(s)",
                self.dropped_writes
            );
        }
    }
}

impl Drop for HistoryWriter {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
        if let Some(handle) = self.join_handle.take()
            && let Err(error) = handle.join()
        {
            debug!("persistent command history writer join failed: {error:?}");
        }
    }
}

pub(crate) fn spawn_history_writer(store: HistoryStore) -> Option<HistoryWriter> {
    let (tx, rx) = match cueue::cueue::<StoreCommandRequest>(HISTORY_WRITE_QUEUE_SIZE) {
        Ok(queue) => queue,
        Err(error) => {
            warn!("persistent command history: failed to create write queue: {error}");
            return None;
        }
    };
    let thread = Arc::new(OnceLock::new());
    let writer_thread = thread.clone();
    let join_handle = thread::Builder::new()
        .name("history-writer".into())
        .spawn(move || {
            writer_thread
                .set(thread::current())
                .expect("set history writer thread handle");
            run_history_writer(store, rx);
        })
        .map_err(|error| {
            warn!("persistent command history: failed to spawn writer thread: {error}");
            error
        })
        .ok()?;
    Some(HistoryWriter {
        tx: Some(tx),
        thread,
        join_handle: Some(join_handle),
        dropped_writes: 0,
    })
}

pub(crate) fn history_db_path() -> Option<PathBuf> {
    dirs::data_dir().map(|dir| dir.join("term41").join("history.sqlite3"))
}

pub(crate) fn store_request(
    command: String,
    cwd: PathBuf,
    config: &CommandEditorConfig,
) -> StoreCommandRequest {
    StoreCommandRequest {
        command,
        cwd,
        submitted_at: std::time::SystemTime::now(),
        retention: HistoryRetention {
            max_entries_per_cwd: config.max_persistent_history_per_dir.max(1),
            max_global_entries: global_retention_limit(config.max_persistent_history_per_dir),
        },
        ignore_leading_space: true,
    }
}

fn run_history_writer(
    store: HistoryStore,
    mut rx: cueue::Reader<StoreCommandRequest>,
) {
    loop {
        drain_history_writes(&store, &mut rx);
        if rx.is_abandoned() {
            drain_history_writes(&store, &mut rx);
            break;
        }
        thread::park_timeout(HISTORY_WRITER_IDLE_POLL);
    }
}

fn drain_history_writes(
    store: &HistoryStore,
    rx: &mut cueue::Reader<StoreCommandRequest>,
) {
    let requests = rx.read_chunk().to_vec();
    rx.commit();
    for request in requests {
        if let Err(error) = history41::store_command(store, request) {
            debug!("persistent command history write failed: {error}");
        }
    }
}

fn global_retention_limit(max_entries_per_cwd: usize) -> usize {
    max_entries_per_cwd
        .max(1)
        .saturating_mul(GLOBAL_HISTORY_RETENTION_MULTIPLIER)
        .max(max_entries_per_cwd.max(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn global_retention_scales_from_per_directory_limit() {
        assert_eq!(global_retention_limit(0), 20);
        assert_eq!(global_retention_limit(5), 100);
    }
}

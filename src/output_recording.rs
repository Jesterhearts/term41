use std::fs::File;
use std::io;
use std::io::BufWriter;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

use time::OffsetDateTime;
use time::format_description::FormatItem;
use time::macros::format_description;

static RECORDING_FILENAME_FORMAT: &[FormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]-[minute]-[second].[subsecond digits:3]");

#[derive(Clone)]
pub(crate) struct RecorderControl {
    state: Arc<Mutex<RecorderState>>,
}

struct RecorderState {
    active: Option<ActiveRecorder>,
}

struct ActiveRecorder {
    path: PathBuf,
    file: BufWriter<File>,
}

impl RecorderControl {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(RecorderState { active: None })),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        self.state.lock().unwrap().active.is_some()
    }

    pub(crate) fn start(
        &self,
        path: PathBuf,
    ) -> io::Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("recording path has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let file = File::options().create_new(true).write(true).open(&path)?;
        self.state.lock().unwrap().active = Some(ActiveRecorder {
            path,
            file: BufWriter::new(file),
        });
        Ok(())
    }

    pub(crate) fn stop(&self) -> Option<PathBuf> {
        let mut state = self.state.lock().unwrap();
        let mut active = state.active.take()?;
        if let Err(e) = active.file.flush() {
            warn!("failed to flush recording {}: {e}", active.path.display());
        }
        Some(active.path)
    }

    pub(crate) fn write_chunk(
        &self,
        bytes: &[u8],
    ) {
        let mut state = self.state.lock().unwrap();
        let Some(active) = state.active.as_mut() else {
            return;
        };
        if let Err(e) = active.file.write_all(bytes) {
            warn!("failed to write recording {}: {e}", active.path.display());
            state.active = None;
        }
    }
}

pub(crate) fn next_recording_path() -> PathBuf {
    let ts = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    let stamp = ts
        .format(RECORDING_FILENAME_FORMAT)
        .expect("static recording timestamp format must be valid");
    PathBuf::from("/tmp/term41").join(format!("{stamp}.rec"))
}

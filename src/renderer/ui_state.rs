#[derive(Clone)]
pub(crate) struct RecordingPopup {
    pub lines: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct HistoryConfirmationModal {
    pub title: String,
    pub message: String,
}

#[derive(Clone)]
pub(crate) struct Toast {
    pub text: String,
}

/// Snapshot of the IME's current composition.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreeditState {
    pub text: String,
    pub cursor: Option<(usize, usize)>,
}

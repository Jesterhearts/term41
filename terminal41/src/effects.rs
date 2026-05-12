use crate::graphics::KittyFileRequest;
use crate::io::clipboard::ClipboardRequest;

/// Host-facing side effects produced while applying terminal input.
#[derive(Debug, Default)]
pub struct TerminalEffects {
    /// Bytes that must be written back to the PTY, such as query replies.
    pub host_bytes: Vec<u8>,
    /// True when terminal state changed in a way that can affect host-side
    /// input routing or cached editor UI.
    pub input_context_changed: bool,
    /// Latest host-driven geometry request emitted by VT controls such as
    /// DECSNLS / DECSCPP.
    pub resize_request: Option<(u32, u32)>,
    /// True if at least one BEL was seen while producing this batch.
    pub bell: bool,
    /// Host-driven OSC 52 clipboard requests that need app-level approval.
    pub clipboard_requests: Vec<ClipboardRequest>,
    /// Host-driven kitty graphics file reads that need app-level approval.
    pub kitty_file_requests: Vec<KittyFileRequest>,
}

impl TerminalEffects {
    /// Return whether this batch produced no host-visible side effects.
    pub fn is_empty(&self) -> bool {
        self.host_bytes.is_empty()
            && !self.input_context_changed
            && self.resize_request.is_none()
            && !self.bell
            && self.clipboard_requests.is_empty()
            && self.kitty_file_requests.is_empty()
    }

    /// Merge another batch into this one, preserving the latest resize
    /// request and OR-ing bell state.
    pub fn extend(
        &mut self,
        other: Self,
    ) {
        self.host_bytes.extend(other.host_bytes);
        self.input_context_changed |= other.input_context_changed;
        if other.resize_request.is_some() {
            self.resize_request = other.resize_request;
        }
        self.bell |= other.bell;
        self.clipboard_requests.extend(other.clipboard_requests);
        self.kitty_file_requests.extend(other.kitty_file_requests);
    }
}

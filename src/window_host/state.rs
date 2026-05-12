use std::collections::HashMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::thread::Thread;
use std::time::Instant;

use commands41::CommandEditor;
use commands41::CommandLineView;
use commands41::HistoryEntry;
use config41::CommandEditorConfig;
use config41::keybindings::Keybindings;
use history41::HistoryStore;
use parking_lot::Mutex;
use pty_pipe41::Pty;
use pty_pipe41::PtyWriter;
use smol_str::SmolStr;
use terminal41::ClipboardRequest;
use terminal41::KittyFileRequest;
use terminal41::MouseButton as TermMouseButton;
use terminal41::TermSnapshotOutput;
use terminal41::Terminal;
use terminal41::TerminalEffects;
use terminal41::TerminalThread;
use winit::event::MouseButton;
use winit::event_loop::EventLoopProxy;
use winit::event_loop::OwnedDisplayHandle;
use winit::keyboard::Key;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::window::Window;

use crate::command_catalog::CommandCatalog;
use crate::history_runtime::HistoryWriter;
use crate::output_recording::RecorderControl;
use crate::renderer;
use crate::renderer::PermissionModal;
use crate::renderer::PreeditState;
use crate::renderer::RenderEvent;
use crate::renderer::TabContextMenu;
use crate::renderer::startup::StartupPresenter;

/// Stable identifier for a tab. Monotonically increasing; never reused, so
/// background threads that race with a tab close can't accidentally address
/// the wrong session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct TabId(pub(crate) u64);

impl From<TabId> for u64 {
    fn from(val: TabId) -> Self {
        val.0
    }
}

/// Commands sent from the render thread back to the window thread.
pub(crate) enum AppEvent {
    SetTitle(String),
    RequestUserAttention,
    RequestStartupRedraw,
    ReleaseStartupSurface,
    ApplyTerminalEffects {
        tab_id: TabId,
        effects: TerminalEffects,
    },
    RegisterInputEndpoint {
        tab_id: TabId,
        terminal: Arc<Mutex<Terminal>>,
        terminal_thread: Arc<OnceLock<Thread>>,
        writer: PtyWriter,
        recorder: RecorderControl,
    },
    RemoveInputEndpoint(TabId),
    SetActiveInputTab(Option<TabId>),
    ResolveClipboardRequest {
        tab_id: TabId,
        request: ClipboardRequest,
        decision: PermissionDecision,
    },
    ResolveKittyFileRequest {
        tab_id: TabId,
        request: KittyFileRequest,
        decision: PermissionDecision,
    },
    DismissRecordingPopup(u64),
    ShowToast(String),
    DismissToast(u64),
}

pub(crate) struct Tab {
    pub(crate) id: TabId,
    pub(crate) terminal: Arc<Mutex<Terminal>>,
    pub(crate) snapshot_output: TermSnapshotOutput,
    pub(crate) pty: Pty,
    pub(crate) window_sync_epoch: u64,
    /// Kept alive for its Drop impl which signals the thread to stop.
    pub(crate) terminal_thread: TerminalThread,
}

pub(crate) struct InputEndpoint {
    pub(crate) terminal: Arc<Mutex<Terminal>>,
    pub(crate) terminal_thread: Arc<OnceLock<Thread>>,
    pub(crate) writer: PtyWriter,
    pub(crate) recorder: RecorderControl,
    pub(crate) command_editor: CommandEditor,
}

#[derive(Clone)]
pub(crate) struct RecordingPopupView {
    pub(crate) lines: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct ToastView {
    pub(crate) text: String,
}

#[derive(Clone)]
pub(crate) struct HistoryConfirmationView {
    pub(crate) title: String,
    pub(crate) message: String,
}

pub(crate) enum HistoryConfirmation {
    ClearAll,
    ClearDirectory(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PermissionDecision {
    Allow,
    Deny,
}

pub(crate) struct PermissionRequest {
    pub(crate) feature: String,
    pub(crate) response_tx: mpsc::Sender<PermissionDecision>,
}

pub(crate) struct PermissionModalState {
    pub(crate) response_tx: mpsc::Sender<PermissionDecision>,
}

pub(crate) enum RecordingPopupState {
    PendingStart { path: PathBuf },
    Completed { token: u64 },
}

pub(crate) struct InputState {
    pub(crate) keybindings: Keybindings,
    pub(crate) command_editor_config: CommandEditorConfig,
    pub(crate) command_editor_views: HashMap<TabId, CommandLineView>,
    pub(crate) tab_count: usize,
    pub(crate) tab_order: Vec<TabId>,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
    pub(crate) gutter_width: u32,
    pub(crate) hovered_tab_bar_button: Option<renderer::TabBarHover>,
    pub(crate) tab_context_menu: Option<TabContextMenu>,
    pub(crate) gutter_popup: Option<renderer::GutterPopup>,
    pub(crate) recording_popup: Option<RecordingPopupView>,
    pub(crate) permission_modal: Option<PermissionModal>,
    pub(crate) command_palette: Option<super::CommandPaletteView>,
    pub(crate) history_confirmation: Option<HistoryConfirmationView>,
    pub(crate) history_deletion: Option<super::HistoryDeletionView>,
    pub(crate) toast: Option<ToastView>,
    pub(crate) preedit: Option<PreeditState>,
}

pub(crate) struct WindowHost {
    pub(crate) window: Option<Arc<Window>>,
    pub(crate) startup: StartupState,
    pub(crate) input: InputRuntime,
    pub(crate) command: CommandRuntime,
    pub(crate) render: RenderRuntime,
    pub(crate) keyboard: KeyboardRuntime,
    pub(crate) mouse: MouseRuntime,
    pub(crate) metrics: WindowMetrics,
    pub(crate) modals: ModalRuntime,
}

pub(crate) struct StartupState {
    pub(crate) presenter: Option<StartupPresenter>,
    pub(crate) tabs: Vec<Tab>,
    pub(crate) next_redraw: Option<Instant>,
    pub(crate) release_tx: Option<mpsc::SyncSender<Vec<Tab>>>,
}

pub(crate) struct InputRuntime {
    pub(crate) endpoints: HashMap<TabId, InputEndpoint>,
    pub(crate) active_tab: Option<TabId>,
}

pub(crate) struct CommandRuntime {
    pub(crate) catalog: CommandCatalog,
    pub(crate) history_store: Option<HistoryStore>,
    pub(crate) history_writer: Option<HistoryWriter>,
    pub(crate) shell_history_entries: Vec<HistoryEntry>,
    pub(crate) shell_history_loaded: bool,
    pub(crate) shell_history_enabled: bool,
}

pub(crate) struct RenderRuntime {
    pub(crate) input_state: Arc<Mutex<InputState>>,
    pub(crate) event_tx: cueue::Writer<RenderEvent>,
    /// One-shot channel to deliver the window + display handle to the render
    /// thread after `resumed()` creates the window. Taken (set to `None`)
    /// after the first send.
    pub(crate) window_tx: Option<mpsc::SyncSender<(Arc<Window>, OwnedDisplayHandle)>>,
    pub(crate) thread_handle: Arc<OnceLock<std::thread::Thread>>,
    pub(crate) event_proxy: EventLoopProxy<AppEvent>,
}

pub(crate) struct KeyboardRuntime {
    pub(crate) modifiers: ModifiersState,
    pub(crate) physical_modifiers: PhysicalModifierState,
    pub(crate) ime_preedit_active: bool,
}

pub(crate) struct MouseRuntime {
    pub(crate) pos: (f64, f64),
    pub(crate) mouse_buttons: MouseButtonState,
    pub(crate) last_motion_position: Option<(u32, u32)>,
    pub(crate) last_click_time: Option<Instant>,
    pub(crate) last_click_cell: Option<(u32, u32)>,
    pub(crate) click_count: u32,
    pub(crate) left_drag_active: bool,
    pub(crate) selection_drag_moved: bool,
    pub(crate) command_editor_drag_anchor: Option<usize>,
    pub(crate) selection_autoscroll_direction: Option<SelectionAutoscroll>,
    pub(crate) selection_autoscroll_next: Option<Instant>,
}

pub(crate) struct WindowMetrics {
    pub(crate) window_size: (u32, u32),
    pub(crate) new_tab_text: SmolStr,
    pub(crate) opacity: f32,
    pub(crate) cell_width: u32,
    pub(crate) cell_height: u32,
    pub(crate) startup_fonts: Option<String>,
    pub(crate) startup_font_size: f32,
    pub(crate) startup_supersampling: u32,
    pub(crate) startup_dpi_scale: Option<f32>,
    pub(crate) startup_gutter: bool,
}

pub(crate) struct ModalRuntime {
    pub(crate) recording_popup: Option<RecordingPopupState>,
    pub(crate) permission_modal: Option<PermissionModalState>,
    pub(crate) history_confirmation: Option<HistoryConfirmation>,
    pub(crate) queued_permission_requests: VecDeque<PermissionRequest>,
    pub(crate) next_recording_popup_token: u64,
    pub(crate) next_toast_token: u64,
}

pub(crate) fn permission_key_decision(key: &Key) -> Option<PermissionDecision> {
    match key {
        Key::Character(text) if text.eq_ignore_ascii_case("y") => Some(PermissionDecision::Allow),
        Key::Character(text) if text.eq_ignore_ascii_case("n") => Some(PermissionDecision::Deny),
        Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Escape) => {
            Some(PermissionDecision::Deny)
        }
        _ => None,
    }
}

#[derive(Clone, Copy)]
pub(crate) enum WindowButton {
    Minimize = 0,
    Maximize = 1,
    Close = 2,
}

#[derive(Clone, Copy)]
pub(crate) enum TabMenuActionLocal {
    NewTab,
    CloseTab,
    CloseOtherTabs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PopupRerunPasteTarget {
    Editor,
    Terminal(terminal41::PasteMode),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionAutoscroll {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectionCopySource {
    Terminal,
    Editor,
}

#[derive(Default, Copy, Clone)]
pub(crate) struct PhysicalModifierState {
    pub(crate) shift_left: bool,
    pub(crate) shift_right: bool,
    pub(crate) control_left: bool,
    pub(crate) control_right: bool,
    pub(crate) alt_left: bool,
    pub(crate) alt_right: bool,
    pub(crate) super_left: bool,
    pub(crate) super_right: bool,
}

#[derive(Default, Copy, Clone)]
pub(crate) struct MouseButtonState {
    pub(crate) left: bool,
    pub(crate) middle: bool,
    pub(crate) right: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MouseReportPosition {
    pub(crate) col: u32,
    pub(crate) row: u32,
    pub(crate) pixel_x: u32,
    pub(crate) pixel_y: u32,
}

impl MouseButtonState {
    pub(crate) fn set(
        &mut self,
        button: MouseButton,
        pressed: bool,
    ) {
        match button {
            MouseButton::Left => self.left = pressed,
            MouseButton::Middle => self.middle = pressed,
            MouseButton::Right => self.right = pressed,
            _ => {}
        }
    }

    pub(crate) fn primary_held(&self) -> TermMouseButton {
        if self.left {
            TermMouseButton::Left
        } else if self.middle {
            TermMouseButton::Middle
        } else if self.right {
            TermMouseButton::Right
        } else {
            TermMouseButton::None
        }
    }
}

impl PhysicalModifierState {
    pub(crate) fn modifiers(self) -> ModifiersState {
        let mut mods = ModifiersState::empty();
        mods.set(ModifiersState::SHIFT, self.shift_left || self.shift_right);
        mods.set(
            ModifiersState::CONTROL,
            self.control_left || self.control_right,
        );
        mods.set(ModifiersState::ALT, self.alt_left || self.alt_right);
        mods.set(ModifiersState::SUPER, self.super_left || self.super_right);
        mods
    }
}

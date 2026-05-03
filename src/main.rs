#![allow(clippy::too_many_arguments)]
#![allow(clippy::type_complexity)]

mod command_catalog;
mod history_runtime;
mod output_recording;
mod perf_ctrl_c;
mod renderer;
mod scripting;

mod window_host {
    use super::*;

    mod events;
    mod input;
    mod mouse;
    mod startup;

    #[cfg(test)]
    mod tests;
}

use std::collections::HashMap;
use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::thread;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use clip41::ClipboardKind;
use command_catalog::CommandCatalog;
use commands41::CommandEditor;
use commands41::CommandEditorCursorStyle;
use commands41::CommandLineView;
use commands41::EditOutcome;
use commands41::EditorInput;
use commands41::EditorSettings;
use commands41::HistoryEntry;
use commands41::VimKey;
use commands41::apply_input;
use commands41::clear_selection as clear_editor_selection;
use commands41::select_range;
use commands41::selected_text;
use commands41::set_cursor;
use config41::CommandEditorConfig;
use config41::StatusLineMode;
use config41::keybindings::Action;
use config41::keybindings::Keybindings;
use font41::FontSystem;
use history41::HistoryStore;
use nucleo_matcher::Config as NucleoConfig;
use nucleo_matcher::Matcher;
use nucleo_matcher::Utf32Str;
use nucleo_matcher::pattern::AtomKind;
use nucleo_matcher::pattern::CaseMatching;
use nucleo_matcher::pattern::Normalization;
use nucleo_matcher::pattern::Pattern;
use parking_lot::Mutex;
use pty_pipe41::Pty;
use pty_pipe41::PtyWriter;
use renderer::RenderHost;
use renderer::startup::StartupPresenter;
use smol_str::SmolStr;
use terminal41::ClipboardRequest;
use terminal41::HostInput;
use terminal41::HostInputEffects;
use terminal41::HostMouse;
use terminal41::KittyFileRequest;
use terminal41::MouseButton as TermMouseButton;
use terminal41::MouseEventKind;
use terminal41::MouseModifiers;
use terminal41::PasteMode;
use terminal41::TermSnapshotOutput;
use terminal41::Terminal;
use terminal41::TerminalEffects;
use terminal41::TerminalThread;
use terminal41::apply_host_input;
use terminal41::host;
use terminal41::io::clipboard::copy_to_clipboard;
use terminal41::prompt::command_and_output_text_at;
use terminal41::prompt::command_duration_at;
use terminal41::prompt::command_text_at;
use terminal41::prompt::find_prompt_for_screen_row;
use terminal41::prompt::output_text_at;
use terminal41::prompt::select_command_at;
use terminal41::prompt::untrusted_command_line_at;
use terminal41::selection::SelectionMode;
use terminal41::selection::close_search;
use terminal41::selection::copy_selection;
use terminal41::selection::extend_selection;
use terminal41::selection::extend_selection_from_start;
use terminal41::selection::open_search;
use terminal41::selection::search_active;
use terminal41::selection::search_append;
use terminal41::selection::search_backspace;
use terminal41::selection::search_step_next;
use terminal41::selection::search_step_prev;
use terminal41::selection::selection_text;
use terminal41::selection::start_selection;
use terminal41::settings;
use terminal41::view;
use unicode_segmentation::UnicodeSegmentation;
use winit::application::ApplicationHandler;
use winit::event::ElementState;
use winit::event::Ime;
use winit::event::MouseButton;
use winit::event::MouseScrollDelta;
use winit::event::WindowEvent;
use winit::event_loop::ActiveEventLoop;
use winit::event_loop::ControlFlow;
use winit::event_loop::EventLoop;
use winit::event_loop::EventLoopProxy;
use winit::event_loop::OwnedDisplayHandle;
use winit::keyboard::Key;
use winit::keyboard::KeyLocation;
use winit::keyboard::ModifiersState;
use winit::keyboard::NamedKey;
use winit::keyboard::PhysicalKey;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::Window;
use winit::window::WindowId;

use crate::history_runtime::HistoryWriter;
use crate::output_recording::RecorderControl;
use crate::output_recording::next_recording_path;
use crate::renderer::PermissionChoice;
use crate::renderer::PermissionModal;
use crate::renderer::PreeditState;
use crate::renderer::RenderEvent;
use crate::renderer::TabContextMenu;
use crate::renderer::background::startup_snapshot_path;
use crate::renderer::compute_gutter_width;
use crate::renderer::ctrl_byte;
use crate::renderer::kitty_encode_ime_commit;
use crate::renderer::kitty_encode_input;
use crate::renderer::legacy_encode_named;
use crate::renderer::legacy_encode_numpad_character;
use crate::renderer::paint::build_tab_bar_layout;

#[macro_use]
extern crate log;

pub(crate) fn unpark_thread_if_started(thread_handle: &OnceLock<Thread>) {
    if let Some(thread) = thread_handle.get() {
        thread.unpark();
    }
}

static APP_START_TIME: OnceLock<Instant> = OnceLock::new();
static LOG_TOAST_TX: OnceLock<mpsc::Sender<String>> = OnceLock::new();

const INITIAL_COLS: u32 = 80;
const INITIAL_ROWS: u32 = 24;
const COMMAND_EDITOR_BOX_ROWS: u32 = 3;

/// Size of the cueue ring buffer for window→renderer events (in elements).
const EVENT_QUEUE_SIZE: usize = 4096;

const SELECTION_AUTOSCROLL_INTERVAL: Duration = Duration::from_millis(45);

/// Stable identifier for a tab. Monotonically increasing; never reused, so
/// background threads that race with a tab close can't accidentally address
/// the wrong session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

impl From<TabId> for u64 {
    fn from(val: TabId) -> Self {
        val.0
    }
}

/// Commands sent from the render thread back to the window thread.
enum AppEvent {
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

#[derive(Default)]
struct LogToastVisitor {
    message: Option<String>,
    fields: Vec<String>,
}

struct LogToastLayer;

impl tracing::field::Visit for LogToastVisitor {
    fn record_debug(
        &mut self,
        field: &tracing::field::Field,
        value: &dyn fmt::Debug,
    ) {
        let value = clean_log_field_value(format!("{value:?}"));
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.fields.push(format!("{}={value}", field.name()));
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for LogToastLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let level = *event.metadata().level();
        if !matches!(level, tracing::Level::WARN | tracing::Level::ERROR) {
            return;
        }

        let mut visitor = LogToastVisitor::default();
        event.record(&mut visitor);
        let message = log_toast_message(visitor, event.metadata().target());
        enqueue_log_toast(format!("{level}: {message}"));
    }
}

fn clean_log_field_value(value: String) -> String {
    let Some(trimmed) = value.strip_prefix('"').and_then(|v| v.strip_suffix('"')) else {
        return value;
    };
    trimmed.to_string()
}

fn log_toast_message(
    visitor: LogToastVisitor,
    target: &str,
) -> String {
    if let Some(message) = visitor.message {
        return message;
    }
    if !visitor.fields.is_empty() {
        return visitor.fields.join(" ");
    }
    target.to_string()
}

fn enqueue_log_toast(message: String) {
    if let Some(tx) = LOG_TOAST_TX.get() {
        let _ = tx.send(message);
    }
}

fn install_log_toast_forwarder(proxy: EventLoopProxy<AppEvent>) {
    let (tx, rx) = mpsc::channel();
    if LOG_TOAST_TX.set(tx).is_err() {
        return;
    }

    thread::Builder::new()
        .name("log-toast-forwarder".into())
        .spawn(move || {
            for message in rx {
                let _ = proxy.send_event(AppEvent::ShowToast(message));
            }
        })
        .expect("spawn log toast forwarder");
}

struct Tab {
    id: TabId,
    terminal: Arc<Mutex<Terminal>>,
    snapshot_output: TermSnapshotOutput,
    pty: Pty,
    window_sync_epoch: u64,
    /// Kept alive for its Drop impl which signals the thread to stop.
    terminal_thread: TerminalThread,
}

struct InputEndpoint {
    terminal: Arc<Mutex<Terminal>>,
    terminal_thread: Arc<OnceLock<Thread>>,
    writer: PtyWriter,
    recorder: RecorderControl,
    command_editor: CommandEditor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandEditorContext {
    current_dir: Option<PathBuf>,
}

fn command_editor_view_context(terminal: &Terminal) -> Option<CommandEditorContext> {
    if terminal.on_alt_screen || command_editor_hidden_by_foreground_app(terminal) {
        return None;
    }
    Some(CommandEditorContext {
        current_dir: terminal.metadata.current_directory.clone(),
    })
}

fn command_editor_view_for_input_tab(
    input_state: &InputState,
    tab_id: TabId,
) -> Option<&CommandLineView> {
    if !input_state.command_editor_config.enabled {
        return None;
    }
    command_editor_view_for_tab_state(&input_state.command_editor_views, tab_id)
}

fn command_editor_view_for_tab_state(
    view_state: &HashMap<TabId, CommandLineView>,
    tab_id: TabId,
) -> Option<&CommandLineView> {
    view_state.get(&tab_id)
}

fn command_editor_view_open_for_input_tab(
    input_state: &InputState,
    tab_id: Option<TabId>,
) -> bool {
    tab_id
        .and_then(|tab_id| command_editor_view_for_input_tab(input_state, tab_id))
        .is_some()
}

fn command_editor_hidden_by_foreground_app(terminal: &Terminal) -> bool {
    if terminal.metadata.shell_integration_phase != terminal41::ShellIntegrationPhase::Output {
        return false;
    }
    host::mouse_tracking_enabled(terminal.modes.mouse_tracking)
        || terminal.active.app_cursor_keys
        || terminal.active.app_keypad
}

fn command_editor_input_context(
    terminal: &Terminal,
    command_editor_open: bool,
) -> Option<CommandEditorContext> {
    let context = command_editor_view_context(terminal)?;
    if command_editor_open
        || terminal.metadata.shell_integration_phase == terminal41::ShellIntegrationPhase::Command
    {
        Some(context)
    } else {
        None
    }
}

fn reset_viewport_and_invalidate(terminal: &mut Terminal) {
    let offset = terminal.active.offset;
    view::reset_viewport(&mut terminal.active);
    if terminal.active.offset != offset {
        terminal.invalidate_snapshot_rows();
    }
}

fn reset_tab_viewport_and_invalidate(
    input_endpoints: &HashMap<TabId, InputEndpoint>,
    tab_id: TabId,
) {
    let Some(target) = input_endpoints.get(&tab_id) else {
        return;
    };
    reset_viewport_and_invalidate(&mut target.terminal.lock());
}

#[derive(Clone)]
struct RecordingPopupView {
    lines: Vec<String>,
}

#[derive(Clone)]
struct ToastView {
    text: String,
}

#[derive(Clone)]
pub(crate) struct CommandPaletteItem {
    pub(crate) label: String,
    pub(crate) action: Action,
    argument: Option<CommandPaletteArgumentKind>,
}

struct CommandPaletteMatch {
    item: CommandPaletteItem,
    score: u32,
}

#[derive(Clone)]
pub(crate) struct CommandPaletteView {
    pub(crate) query: String,
    pub(crate) items: Vec<CommandPaletteItem>,
    pub(crate) selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CommandPaletteArgumentKind {
    WorkingDirectory,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CommandPaletteArgument {
    WorkingDirectory(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CommandPaletteInvocation {
    pub(crate) action: Action,
    pub(crate) argument: Option<CommandPaletteArgument>,
}

#[derive(Debug, PartialEq, Eq)]
enum CommandPaletteAccept {
    Ready(CommandPaletteInvocation),
    NeedsArgument,
}

#[derive(Debug, PartialEq, Eq)]
struct CommandPaletteInput<'a> {
    command: &'a str,
    argument: Option<&'a str>,
}

fn command_palette_view() -> CommandPaletteView {
    CommandPaletteView {
        query: String::new(),
        items: command_palette_items(""),
        selected: 0,
    }
}

fn command_palette_items(query: &str) -> Vec<CommandPaletteItem> {
    let input = parse_command_palette_input(query);
    let items = Action::command_palette_actions()
        .iter()
        .flat_map(|action| command_palette_items_for_action(*action))
        .filter(|item| input.argument.is_none() || item.argument.is_some());
    if input.command.is_empty() {
        return sorted_command_palette_items(items);
    }

    fuzzy_command_palette_items(input.command, items)
}

fn parse_command_palette_input(query: &str) -> CommandPaletteInput<'_> {
    if let Some((command, argument)) = query.split_once(':') {
        CommandPaletteInput {
            command: command.trim_end(),
            argument: Some(argument.trim_start()),
        }
    } else {
        CommandPaletteInput {
            command: query,
            argument: None,
        }
    }
}

fn command_palette_items_for_action(action: Action) -> Vec<CommandPaletteItem> {
    let mut items = vec![command_palette_item(action, action.palette_label(), None)];
    if action == Action::OpenNewWindow {
        items.push(command_palette_item(
            action,
            "Open new window in dir:",
            Some(CommandPaletteArgumentKind::WorkingDirectory),
        ));
    }
    if action == Action::NewTab {
        items.push(command_palette_item(
            action,
            "Open new tab in dir:",
            Some(CommandPaletteArgumentKind::WorkingDirectory),
        ));
    }
    items
}

fn command_palette_item(
    action: Action,
    label: &str,
    argument: Option<CommandPaletteArgumentKind>,
) -> CommandPaletteItem {
    CommandPaletteItem {
        label: label.to_owned(),
        action,
        argument,
    }
}

fn sorted_command_palette_items(
    items: impl IntoIterator<Item = CommandPaletteItem>
) -> Vec<CommandPaletteItem> {
    let mut items: Vec<_> = items.into_iter().collect();
    items.sort_by_key(|item| item.label.to_ascii_lowercase());
    items
}

fn fuzzy_command_palette_items(
    query: &str,
    items: impl IntoIterator<Item = CommandPaletteItem>,
) -> Vec<CommandPaletteItem> {
    let pattern = Pattern::new(
        query,
        CaseMatching::Ignore,
        Normalization::Smart,
        AtomKind::Fuzzy,
    );
    let mut matcher = command_palette_matcher();
    let mut utf32_buf = Vec::new();
    let mut matches: Vec<_> = items
        .into_iter()
        .filter_map(|item| {
            pattern
                .score(
                    Utf32Str::new(item.label.as_str(), &mut utf32_buf),
                    &mut matcher,
                )
                .map(|score| CommandPaletteMatch { item, score })
        })
        .collect();
    matches.sort_by(command_palette_match_order);
    matches.into_iter().map(|matched| matched.item).collect()
}

fn command_palette_matcher() -> Matcher {
    let mut config = NucleoConfig::DEFAULT;
    config.prefer_prefix = true;
    Matcher::new(config)
}

fn command_palette_match_order(
    left: &CommandPaletteMatch,
    right: &CommandPaletteMatch,
) -> std::cmp::Ordering {
    right.score.cmp(&left.score).then_with(|| {
        left.item
            .label
            .to_ascii_lowercase()
            .cmp(&right.item.label.to_ascii_lowercase())
    })
}

fn move_command_palette_selection(
    view: &mut CommandPaletteView,
    delta: isize,
) {
    if view.items.is_empty() {
        view.selected = 0;
        return;
    }
    let len = view.items.len();
    view.selected = if delta < 0 {
        (view.selected + len - 1) % len
    } else {
        (view.selected + 1) % len
    };
}

fn set_command_palette_query(
    view: &mut CommandPaletteView,
    query: String,
) {
    view.query = query;
    view.items = command_palette_items(&view.query);
    view.selected = 0;
}

fn complete_command_palette_selection(view: &mut CommandPaletteView) -> bool {
    let Some(query) = command_palette_completion_text(view) else {
        return false;
    };
    set_command_palette_query(view, query);
    true
}

fn command_palette_completion_text(view: &CommandPaletteView) -> Option<String> {
    let item = view.items.get(view.selected)?;
    let input = parse_command_palette_input(&view.query);
    if item.argument.is_none() {
        return Some(item.label.clone());
    }
    match input.argument {
        Some(argument) if !argument.is_empty() => Some(format!("{} {}", item.label, argument)),
        _ => Some(format!("{} ", item.label)),
    }
}

fn command_palette_selected_invocation(view: &CommandPaletteView) -> Option<CommandPaletteAccept> {
    let item = view.items.get(view.selected)?;
    let input = parse_command_palette_input(&view.query);
    let argument = match item.argument {
        Some(CommandPaletteArgumentKind::WorkingDirectory) => {
            let Some(argument) = input.argument else {
                return Some(CommandPaletteAccept::NeedsArgument);
            };
            let argument = argument.trim();
            if argument.is_empty() {
                return Some(CommandPaletteAccept::NeedsArgument);
            }
            Some(CommandPaletteArgument::WorkingDirectory(
                argument.to_owned(),
            ))
        }
        None => None,
    };
    Some(CommandPaletteAccept::Ready(CommandPaletteInvocation {
        action: item.action,
        argument,
    }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PermissionDecision {
    Allow,
    Deny,
}

struct PermissionRequest {
    feature: String,
    response_tx: mpsc::Sender<PermissionDecision>,
}

struct PermissionModalState {
    response_tx: mpsc::Sender<PermissionDecision>,
}

enum RecordingPopupState {
    PendingStart { path: PathBuf },
    Completed { token: u64 },
}

pub(crate) struct InputState {
    keybindings: Keybindings,
    command_editor_config: CommandEditorConfig,
    command_editor_views: HashMap<TabId, CommandLineView>,
    tab_count: usize,
    tab_order: Vec<TabId>,
    cell_width: u32,
    cell_height: u32,
    gutter_width: u32,
    hovered_tab_bar_button: Option<renderer::TabBarHover>,
    tab_context_menu: Option<TabContextMenu>,
    gutter_popup: Option<renderer::GutterPopup>,
    recording_popup: Option<RecordingPopupView>,
    permission_modal: Option<PermissionModal>,
    command_palette: Option<CommandPaletteView>,
    toast: Option<ToastView>,
    preedit: Option<PreeditState>,
}

struct WindowHost {
    window: Option<Arc<Window>>,
    startup_presenter: Option<StartupPresenter>,
    startup_tabs: Vec<Tab>,
    startup_next_redraw: Option<Instant>,
    startup_release_tx: Option<mpsc::SyncSender<Vec<Tab>>>,
    input_endpoints: HashMap<TabId, InputEndpoint>,
    command_catalog: CommandCatalog,
    command_history_store: Option<HistoryStore>,
    command_history_writer: Option<HistoryWriter>,
    shell_history_entries: Vec<HistoryEntry>,
    shell_history_loaded: bool,
    shell_history_enabled: bool,
    active_input_tab: Option<TabId>,
    input_state: Arc<Mutex<InputState>>,
    event_tx: cueue::Writer<RenderEvent>,
    /// One-shot channel to deliver the window + display handle to the render
    /// thread after `resumed()` creates the window. Taken (set to `None`)
    /// after the first send.
    window_tx: Option<mpsc::SyncSender<(Arc<Window>, OwnedDisplayHandle)>>,
    modifiers: ModifiersState,
    ime_preedit_active: bool,
    mouse_pos: (f64, f64),
    mouse_buttons: MouseButtonState,
    last_motion_position: Option<(u32, u32)>,
    last_click_time: Option<Instant>,
    last_click_cell: Option<(u32, u32)>,
    click_count: u32,
    left_drag_active: bool,
    selection_drag_moved: bool,
    command_editor_drag_anchor: Option<usize>,
    selection_autoscroll_direction: Option<SelectionAutoscroll>,
    selection_autoscroll_next: Option<Instant>,
    window_size: (u32, u32),
    new_tab_text: SmolStr,
    opacity: f32,
    cell_width: u32,
    cell_height: u32,
    startup_fonts: Option<String>,
    startup_font_size: f32,
    startup_supersampling: u32,
    startup_dpi_scale: Option<f32>,
    startup_gutter: bool,
    render_thread_handle: Arc<OnceLock<std::thread::Thread>>,
    event_proxy: EventLoopProxy<AppEvent>,
    recording_popup: Option<RecordingPopupState>,
    permission_modal: Option<PermissionModalState>,
    queued_permission_requests: VecDeque<PermissionRequest>,
    next_recording_popup_token: u64,
    next_toast_token: u64,
}

fn permission_key_decision(key: &Key) -> Option<PermissionDecision> {
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
enum WindowButton {
    Minimize = 0,
    Maximize = 1,
    Close = 2,
}

#[derive(Clone, Copy)]
enum TabMenuActionLocal {
    NewTab,
    CloseTab,
    CloseOtherTabs,
}

const RESIZE_BORDER: f32 = 5.0;
const TAB_MENU_WIDTH_CELLS: f32 = 16.0;
const POPUP_WIDTH_CELLS: f32 = 20.0;

enum PopupCommandText {
    Observed(String),
    Untrusted(String),
}

fn popup_item_at(
    popup: Option<&renderer::GutterPopup>,
    x: f64,
    y: f64,
    cell_width: u32,
    cell_height: u32,
    gutter_width: u32,
    window_height: u32,
) -> Option<usize> {
    let popup = popup?;
    let cell_w = cell_width as f32;
    let cell_h = cell_height as f32;
    let total_rows = popup.duration_text.is_some() as usize + 4;
    let popup_w = cell_w * POPUP_WIDTH_CELLS;
    let popup_h = total_rows as f32 * cell_h;
    let popup_x = gutter_width as f32;
    let popup_y = (popup.screen_row as f32 * cell_h + cell_h).min(window_height as f32 - popup_h);
    let x = x as f32;
    let y = y as f32;
    if x < popup_x || x > popup_x + popup_w || y < popup_y || y > popup_y + popup_h {
        return None;
    }
    let row_in_popup = ((y - popup_y) / cell_h) as usize;
    let header = if popup.duration_text.is_some() { 1 } else { 0 };
    let item_idx = row_in_popup.checked_sub(header)?;
    (item_idx < 4).then_some(item_idx)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{:.0}ms", secs * 1000.0)
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else if secs < 3600.0 {
        let m = (secs / 60.0).floor();
        let s = secs - m * 60.0;
        format!("{m:.0}m {s:.0}s")
    } else {
        let h = (secs / 3600.0).floor();
        let m = ((secs - h * 3600.0) / 60.0).floor();
        format!("{h:.0}h {m:.0}m")
    }
}

fn popup_command_text(
    prompt_abs: u64,
    command_metas: &HashMap<u64, terminal41::CommandMeta>,
    screen: &terminal41::Screen,
) -> Option<PopupCommandText> {
    if let Some(command) = command_text_at(prompt_abs, command_metas, screen) {
        return Some(PopupCommandText::Observed(command));
    }
    untrusted_command_line_at(prompt_abs, command_metas)
        .map(|command| PopupCommandText::Untrusted(command.to_owned()))
}

fn popup_rerun_command_text(command: PopupCommandText) -> String {
    match command {
        PopupCommandText::Observed(command) => command.trim().to_owned(),
        PopupCommandText::Untrusted(command) => command,
    }
}

fn popup_rerun_paste(
    command: PopupCommandText,
    bracketed_paste_enabled: bool,
) -> Option<(String, PasteMode)> {
    let text = popup_rerun_command_text(command);

    if bracketed_paste_enabled {
        return Some((text, PasteMode::Bracketed));
    }

    if text.contains(['\r', '\n']) {
        return None;
    }

    Some((text, PasteMode::Terminal))
}

/// Maximum time between clicks that still count as part of a sequence.
const MULTI_CLICK_WINDOW: Duration = Duration::from_millis(400);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionAutoscroll {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionCopySource {
    Terminal,
    Editor,
}

fn selection_autoscroll_direction(
    mouse_y: f64,
    cell_height: u32,
    viewport_rows: u32,
) -> Option<SelectionAutoscroll> {
    if cell_height == 0 || viewport_rows == 0 {
        return None;
    }

    let cell_height = cell_height as f64;
    let terminal_top = cell_height;
    let terminal_bottom = terminal_top + viewport_rows as f64 * cell_height;
    let top_edge = terminal_top + cell_height;
    let bottom_edge = terminal_bottom - cell_height;

    if mouse_y < top_edge {
        Some(SelectionAutoscroll::Up)
    } else if mouse_y >= bottom_edge {
        Some(SelectionAutoscroll::Down)
    } else {
        None
    }
}

fn selection_copy_source(
    terminal_has_selection: bool,
    editor_has_selection: bool,
    editor_open: bool,
) -> Option<SelectionCopySource> {
    if terminal_has_selection {
        Some(SelectionCopySource::Terminal)
    } else if editor_open && editor_has_selection {
        Some(SelectionCopySource::Editor)
    } else {
        None
    }
}

fn command_editor_terminal_row_offset(
    terminal: &Terminal,
    command_editor_view_present: bool,
) -> u32 {
    if command_editor_view_present
        && !search_active(&terminal.search)
        && command_editor_view_context(terminal).is_some()
    {
        command_editor_terminal_row_offset_for_cursor(
            terminal.active.cursor.row,
            terminal.viewport.rows,
        )
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CommandEditorPopupSide {
    Above,
    Below,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandEditorPlacement {
    pub(crate) top_row: u32,
    pub(crate) rows: u32,
    pub(crate) terminal_row_offset: u32,
}

pub(crate) fn command_editor_placement_for_cursor(
    cursor_row: u32,
    viewport_rows: u32,
) -> CommandEditorPlacement {
    let viewport_rows = viewport_rows.max(1);
    let cursor_row = cursor_row.min(viewport_rows - 1);
    let terminal_row_offset =
        command_editor_terminal_row_offset_for_cursor(cursor_row, viewport_rows);
    let screen_cursor_row = cursor_row.saturating_sub(terminal_row_offset);
    let top_row = screen_cursor_row.saturating_add(1).min(viewport_rows - 1);
    CommandEditorPlacement {
        top_row,
        rows: viewport_rows.saturating_sub(top_row).max(1),
        terminal_row_offset,
    }
}

pub(crate) fn command_editor_popup_side_for_row(
    screen_row: u32,
    viewport_rows: u32,
) -> CommandEditorPopupSide {
    if screen_row < viewport_rows.max(1) / 2 {
        CommandEditorPopupSide::Below
    } else {
        CommandEditorPopupSide::Above
    }
}

fn command_editor_terminal_row_offset_for_cursor(
    cursor_row: u32,
    viewport_rows: u32,
) -> u32 {
    let viewport_rows = viewport_rows.max(1);
    let cursor_row = cursor_row.min(viewport_rows - 1);
    let desired_rows = COMMAND_EDITOR_BOX_ROWS.min(viewport_rows.saturating_sub(1));
    cursor_row
        .saturating_add(1)
        .saturating_add(desired_rows)
        .saturating_sub(viewport_rows)
        .min(desired_rows)
}

fn mouse_report_position_from_pixels(
    raw_x: u32,
    raw_y: u32,
    cell_w: u32,
    cell_h: u32,
    gutter_w: u32,
    cols: u32,
    rows: u32,
    terminal_row_offset: u32,
) -> MouseReportPosition {
    let cell_w = cell_w.max(1);
    let cell_h = cell_h.max(1);
    let cols = cols.max(1);
    let rows = rows.max(1);
    let pixel_x = raw_x
        .saturating_sub(gutter_w)
        .min(cols.saturating_mul(cell_w).saturating_sub(1));
    let pixel_y = raw_y
        .saturating_sub(cell_h)
        .saturating_add(terminal_row_offset.saturating_mul(cell_h))
        .min(rows.saturating_mul(cell_h).saturating_sub(1));

    MouseReportPosition {
        col: (pixel_x / cell_w).min(cols.saturating_sub(1)),
        row: (pixel_y / cell_h).min(rows.saturating_sub(1)),
        pixel_x,
        pixel_y,
    }
}

fn command_editor_mouse_paste_kind(
    command_editor_open: bool,
    pressed: bool,
    button: MouseButton,
) -> Option<ClipboardKind> {
    if !command_editor_open || !pressed {
        return None;
    }
    match button {
        MouseButton::Right => Some(ClipboardKind::Clipboard),
        MouseButton::Middle => Some(ClipboardKind::Primary),
        _ => None,
    }
}

fn dec_udk_selector(
    key: &Key,
    mods: ModifiersState,
) -> Option<u16> {
    if !mods.shift_key() {
        return None;
    }
    match key {
        Key::Named(named) => dec_function_key_selector(*named),
        _ => None,
    }
}

fn command_editor_input(
    key: &Key,
    mods: ModifiersState,
    vim_mode: bool,
) -> Option<EditorInput> {
    if vim_mode {
        return vim_command_editor_input(key, mods);
    }
    if mods.super_key() {
        return None;
    }
    if let Some(input) = modified_command_editor_input(key, mods) {
        return Some(input);
    }
    if mods.control_key() || mods.alt_key() {
        return None;
    }
    match key {
        Key::Character(text) => Some(EditorInput::Insert(text.to_string())),
        Key::Named(NamedKey::Space) => Some(EditorInput::Insert(" ".to_owned())),
        Key::Named(NamedKey::Enter) if mods.shift_key() => Some(EditorInput::Insert("\n".into())),
        Key::Named(NamedKey::Enter) if !mods.shift_key() => Some(EditorInput::Enter),
        Key::Named(NamedKey::Backspace) if !mods.shift_key() => Some(EditorInput::Backspace),
        Key::Named(NamedKey::Delete) if !mods.shift_key() => Some(EditorInput::Delete),
        Key::Named(NamedKey::ArrowLeft) if !mods.shift_key() => Some(EditorInput::MoveLeft),
        Key::Named(NamedKey::ArrowRight) if !mods.shift_key() => Some(EditorInput::MoveRight),
        Key::Named(NamedKey::Home) if !mods.shift_key() => Some(EditorInput::MoveHome),
        Key::Named(NamedKey::End) if !mods.shift_key() => Some(EditorInput::MoveEnd),
        Key::Named(NamedKey::ArrowUp) if !mods.shift_key() => Some(EditorInput::HistoryPrevious),
        Key::Named(NamedKey::ArrowDown) if !mods.shift_key() => Some(EditorInput::HistoryNext),
        Key::Named(NamedKey::Tab) if !mods.shift_key() => Some(EditorInput::Complete),
        Key::Named(NamedKey::Escape) if !mods.shift_key() => Some(EditorInput::Cancel),
        _ => None,
    }
}

fn vim_command_editor_input(
    key: &Key,
    mods: ModifiersState,
) -> Option<EditorInput> {
    if !mods.shift_key()
        && mods.control_key()
        && !mods.alt_key()
        && !mods.super_key()
        && matches!(key, Key::Character(text) if text.eq_ignore_ascii_case("r"))
    {
        return Some(EditorInput::Redo);
    }
    if plain_control_character_key(key, mods, "c") {
        return Some(EditorInput::Cancel);
    }
    if mods.super_key() || mods.control_key() || mods.alt_key() {
        return None;
    }
    let key = match key {
        Key::Character(text) if !mods.shift_key() || text.chars().count() == 1 => {
            VimKey::Text(text.to_string())
        }
        Key::Named(NamedKey::Space) => VimKey::Text(" ".to_owned()),
        Key::Named(NamedKey::Escape) => VimKey::Escape,
        Key::Named(NamedKey::Enter) if mods.shift_key() => VimKey::ShiftEnter,
        Key::Named(NamedKey::Enter) if !mods.shift_key() => VimKey::Enter,
        Key::Named(NamedKey::Backspace) if !mods.shift_key() => VimKey::Backspace,
        Key::Named(NamedKey::Delete) if !mods.shift_key() => VimKey::Delete,
        Key::Named(NamedKey::ArrowLeft) if !mods.shift_key() => VimKey::ArrowLeft,
        Key::Named(NamedKey::ArrowRight) if !mods.shift_key() => VimKey::ArrowRight,
        Key::Named(NamedKey::ArrowUp) if !mods.shift_key() => VimKey::ArrowUp,
        Key::Named(NamedKey::ArrowDown) if !mods.shift_key() => VimKey::ArrowDown,
        Key::Named(NamedKey::Home) if !mods.shift_key() => VimKey::Home,
        Key::Named(NamedKey::End) if !mods.shift_key() => VimKey::End,
        Key::Named(NamedKey::Tab) if !mods.shift_key() => VimKey::Tab,
        _ => return None,
    };
    Some(EditorInput::Vim(key))
}

fn modified_command_editor_input(
    key: &Key,
    mods: ModifiersState,
) -> Option<EditorInput> {
    if mods.shift_key() {
        return None;
    }
    match key {
        Key::Character(text) if mods.control_key() && !mods.alt_key() => {
            control_command_editor_input(text)
        }
        Key::Character(text) if mods.alt_key() && !mods.control_key() => {
            alt_command_editor_input(text)
        }
        Key::Named(NamedKey::ArrowLeft) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::MoveWordLeft)
        }
        Key::Named(NamedKey::ArrowRight) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::MoveWordRight)
        }
        Key::Named(NamedKey::Backspace) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::DeleteWordLeft)
        }
        Key::Named(NamedKey::Delete) if mods.control_key() && !mods.alt_key() => {
            Some(EditorInput::DeleteWordRight)
        }
        Key::Named(NamedKey::ArrowLeft) if mods.alt_key() && !mods.control_key() => {
            Some(EditorInput::MoveWordLeft)
        }
        Key::Named(NamedKey::ArrowRight) if mods.alt_key() && !mods.control_key() => {
            Some(EditorInput::MoveWordRight)
        }
        Key::Named(NamedKey::Backspace) if mods.alt_key() && !mods.control_key() => {
            Some(EditorInput::DeleteWordLeft)
        }
        _ => None,
    }
}

fn control_command_editor_input(text: &str) -> Option<EditorInput> {
    match text {
        "a" | "A" => Some(EditorInput::MoveHome),
        "c" | "C" => Some(EditorInput::Cancel),
        "d" | "D" => Some(EditorInput::Delete),
        "e" | "E" => Some(EditorInput::MoveEnd),
        "k" | "K" => Some(EditorInput::KillToEnd),
        "u" | "U" => Some(EditorInput::KillToStart),
        "w" | "W" => Some(EditorInput::DeleteWordLeft),
        "y" | "Y" => Some(EditorInput::Yank),
        "r" | "R" => Some(EditorInput::Redo),
        _ => None,
    }
}

fn ignored_command_editor_input_falls_through(
    input: &EditorInput,
    key: &Key,
    mods: ModifiersState,
    editor_was_empty: bool,
) -> bool {
    *input == EditorInput::Cancel
        || (editor_was_empty
            && *input == EditorInput::Delete
            && plain_control_character_key(key, mods, "d"))
}

fn plain_control_character_key(
    key: &Key,
    mods: ModifiersState,
    text: &str,
) -> bool {
    !mods.shift_key()
        && mods.control_key()
        && !mods.alt_key()
        && !mods.super_key()
        && matches!(key, Key::Character(actual) if actual.eq_ignore_ascii_case(text))
}

fn alt_command_editor_input(text: &str) -> Option<EditorInput> {
    match text {
        "b" | "B" => Some(EditorInput::MoveWordLeft),
        "f" | "F" => Some(EditorInput::MoveWordRight),
        "d" | "D" => Some(EditorInput::DeleteWordRight),
        _ => None,
    }
}

fn command_editor_view(
    editor: &CommandEditor,
    settings: &EditorSettings,
    vim_mode: bool,
) -> Option<CommandLineView> {
    let mut view = editor.view(settings);
    if !vim_mode {
        view.cursor_style = CommandEditorCursorStyle::Beam;
    }
    Some(view)
}

fn command_editor_line_ranges(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    for (idx, ch) in text.char_indices() {
        if ch == '\n' {
            ranges.push((start, idx));
            start = idx + ch.len_utf8();
        }
    }
    ranges.push((start, text.len()));
    ranges
}

fn command_editor_cursor_line(
    lines: &[(usize, usize)],
    cursor: usize,
) -> usize {
    for (idx, &(_, end)) in lines.iter().enumerate() {
        if cursor <= end {
            return idx;
        }
    }
    lines.len().saturating_sub(1)
}

fn command_editor_visible_line_start(
    line_count: usize,
    cursor_line: usize,
    visible_rows: usize,
) -> usize {
    let visible = visible_rows.max(1);
    if line_count <= visible {
        return 0;
    }
    cursor_line.saturating_add(1).saturating_sub(visible)
}

fn command_editor_byte_index_at_cell(
    view: &CommandLineView,
    viewport_cols: u32,
    visible_rows: u32,
    visible_row: u32,
    col: u32,
) -> usize {
    let lines = command_editor_line_ranges(&view.text);
    let cursor = view.cursor.min(view.text.len());
    if !view.text.is_char_boundary(cursor) {
        return view.text.len();
    }
    let cursor_line = command_editor_cursor_line(&lines, cursor);
    let visible_rows = visible_rows.max(1) as usize;
    let visible_start = command_editor_visible_line_start(lines.len(), cursor_line, visible_rows);
    let line_idx = (visible_start
        + visible_row.min(visible_rows.saturating_sub(1) as u32) as usize)
        .min(lines.len().saturating_sub(1));
    let has_overflow = lines.len() > visible_rows;
    let scrollbar_cols = u32::from(has_overflow);
    let content_cols = viewport_cols.saturating_sub(1 + scrollbar_cols).max(1);
    let text_col = col.min(content_cols);
    let (line_start, line_end) = lines[line_idx];
    view.text[line_start..line_end]
        .grapheme_indices(true)
        .nth(text_col as usize)
        .map_or(line_end, |(idx, _)| line_start + idx)
}

fn dec_local_function_key_selector(
    key: &Key,
    mods: ModifiersState,
) -> Option<u16> {
    if mods.shift_key() || mods.control_key() || mods.alt_key() || mods.super_key() {
        return None;
    }
    match key {
        Key::Named(NamedKey::F1) => Some(1),
        Key::Named(NamedKey::F2) => Some(2),
        Key::Named(NamedKey::F3) => Some(3),
        Key::Named(NamedKey::F4) => Some(4),
        _ => None,
    }
}

fn dec_function_key_selector(named: NamedKey) -> Option<u16> {
    match named {
        NamedKey::F6 => Some(17),
        NamedKey::F7 => Some(18),
        NamedKey::F8 => Some(19),
        NamedKey::F9 => Some(20),
        NamedKey::F10 => Some(21),
        NamedKey::F11 => Some(23),
        NamedKey::F12 => Some(24),
        NamedKey::F13 => Some(25),
        NamedKey::F14 => Some(26),
        NamedKey::F15 => Some(28),
        NamedKey::F16 => Some(29),
        NamedKey::F17 => Some(31),
        NamedKey::F18 => Some(32),
        NamedKey::F19 => Some(33),
        NamedKey::F20 => Some(34),
        _ => None,
    }
}

#[derive(Default, Copy, Clone)]
struct MouseButtonState {
    left: bool,
    middle: bool,
    right: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MouseReportPosition {
    col: u32,
    row: u32,
    pixel_x: u32,
    pixel_y: u32,
}

impl MouseButtonState {
    fn set(
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

    fn primary_held(&self) -> TermMouseButton {
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

fn main() {
    use tracing_subscriber::fmt::format::FmtSpan;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let _ = APP_START_TIME.set(Instant::now());

    let directive = cfg_select! {
        debug_assertions => {
            "term41=debug"
        }
        not(debug_assertions) => {
            "term41=warn"
        }
    };

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::builder()
                .with_default_directive(directive.parse().expect("parse log filter directive"))
                .from_env_lossy(),
        )
        .with(tracing_subscriber::fmt::layer().with_span_events(FmtSpan::CLOSE))
        .with(LogToastLayer)
        .init();

    #[cfg(feature = "deadlock-detection")]
    {
        use parking_lot::deadlock;

        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(10));
                let deadlocks = deadlock::check_deadlock();
                if deadlocks.is_empty() {
                    continue;
                }

                error!("{} deadlocks detected", deadlocks.len());
                for (i, threads) in deadlocks.iter().enumerate() {
                    error!("Deadlock #{}", i);
                    for t in threads {
                        error!("Thread Id {:#?}", t.thread_id());
                        error!("{:#?}", t.backtrace());
                    }
                }
            }
        });
    }

    let command = parse_command_args();

    let event_loop: EventLoop<AppEvent> =
        tracing::debug_span!("create_event_loop").in_scope(|| {
            EventLoop::with_user_event()
                .build()
                .expect("create event loop")
        });
    // Channels.
    let (event_tx, event_rx) =
        cueue::cueue::<RenderEvent>(EVENT_QUEUE_SIZE).expect("create event queue");
    let (window_tx, window_rx) = mpsc::sync_channel(1);
    let (startup_release_tx, startup_release_rx) = mpsc::sync_channel(1);
    let (child_exit_tx, child_exit_rx) = mpsc::channel();
    let config_reload = Arc::new(AtomicBool::new(false));
    let render_thread_handle = Arc::new(OnceLock::new());

    let proxy = event_loop.create_proxy();
    install_log_toast_forwarder(proxy.clone());
    let startup_redraw_proxy = proxy.clone();

    let config = config41::init_config(config_reload.clone(), render_thread_handle.clone());

    let font_system = tracing::debug_span!("init_font_system").in_scope(|| {
        FontSystem::new(
            config.fonts.clone(),
            config.font_size,
            config.font_supersampling,
        )
    });
    let cell_width = font_system.cell_width;
    let cell_height = font_system.cell_height;
    let opacity = config.opacity;
    let startup_fonts = config.fonts.clone();
    let startup_font_size = config.font_size;
    let startup_supersampling = config.font_supersampling;
    let startup_dpi_scale = config.dpi_scale;
    let startup_gutter = config.gutter;
    let startup_keybindings = config.keybindings.clone();
    let startup_command_editor = config.command_editor.clone();
    let command_history_store =
        history_runtime::history_db_path().and_then(|path| match history41::open(&path) {
            Ok(store) => Some(store),
            Err(error) => {
                warn!(
                    "persistent command history: failed to open {}: {error}",
                    path.display()
                );
                None
            }
        });
    let command_history_writer = command_history_store
        .as_ref()
        .cloned()
        .and_then(history_runtime::spawn_history_writer);

    // Create the terminal thread handle before spawning the PTY so the PTY
    // reader can unpark the terminal thread once it starts.
    let terminal_thread = TerminalThread::new();
    let term_thread_handle = terminal_thread.thread_handle.clone();

    // Spawn the initial PTY early so the shell starts running immediately.
    let initial_status_rows = u32::from(config.status_line != StatusLineMode::Off);
    let initial_main_rows = INITIAL_ROWS.saturating_sub(initial_status_rows);
    let (pty, pty_writer, pty_reader) = tracing::debug_span!("spawn_pty").in_scope(|| {
        let term_features = terminal41::iterm_features::term_features(&config.feature_permissions);
        Pty::spawn(
            TabId(0),
            INITIAL_COLS as u16,
            initial_main_rows as u16,
            cell_width as u16,
            cell_height as u16,
            Some(term_features),
            command,
            None,
            terminal_thread.thread_handle.clone(),
            child_exit_tx.clone(),
        )
        .expect("failed to spawn PTY")
    });
    let initial_recorder = RecorderControl::new();

    let mut terminal = Terminal::new(
        INITIAL_COLS,
        INITIAL_ROWS,
        config.scrollback_lines,
        config.status_line,
        config.feature_permissions.clone(),
        config.limits,
        cell_height,
        cell_width,
        config.palette.clone(),
    );
    settings::set_default_cursor_style(
        &mut terminal.default_cursor_style,
        &mut terminal.cursor_style,
        config.cursor_style,
    );
    settings::set_emoji_compatibility_mode(
        &mut terminal.emoji_compatibility_mode,
        config.compatibility.emoji,
    );
    let (snapshot_publisher, snapshot_output) = terminal41::terminal_snapshot_buffer(&mut terminal);
    let terminal = Arc::new(Mutex::new(terminal));

    terminal_thread.spawn(
        "terminal-0".into(),
        terminal.clone(),
        pty_reader,
        render_thread_handle.clone(),
        snapshot_publisher,
        Some(Box::new(move || {
            let _ = startup_redraw_proxy.send_event(AppEvent::RequestStartupRedraw);
        })),
        Box::new({
            let recorder = initial_recorder.clone();
            move |bytes| {
                crate::perf_ctrl_c::observe_pty_output(TabId(0), bytes);
                recorder.write_chunk(bytes);
            }
        }),
        Box::new({
            let proxy = proxy.clone();
            move |effects| {
                let _ = proxy.send_event(AppEvent::ApplyTerminalEffects {
                    tab_id: TabId(0),
                    effects,
                });
            }
        }),
    );

    let input_state = Arc::new(Mutex::new(InputState {
        keybindings: startup_keybindings,
        command_editor_config: startup_command_editor.clone(),
        command_editor_views: HashMap::new(),
        tab_count: 1,
        tab_order: vec![TabId(0)],
        cell_width,
        cell_height,
        gutter_width: if startup_gutter {
            compute_gutter_width(cell_width)
        } else {
            0
        },
        hovered_tab_bar_button: None,
        tab_context_menu: None,
        gutter_popup: None,
        recording_popup: None,
        permission_modal: None,
        command_palette: None,
        toast: None,
        preedit: None,
    }));
    let tab = Tab {
        id: TabId(0),
        terminal: terminal.clone(),
        snapshot_output,
        pty,
        window_sync_epoch: 0,
        terminal_thread,
    };

    // Spawn the render thread.
    let config_reload_ = config_reload.clone();
    let input_state_for_render = input_state.clone();
    let render_thread_handle_for_render = render_thread_handle.clone();
    let render_proxy = proxy.clone();
    let new_tab_text = config.new_tab_text.clone();
    thread::Builder::new()
        .name("renderer".into())
        .spawn(move || {
            render_thread_handle_for_render
                .set(thread::current())
                .expect("set render thread handle");
            let mut host = RenderHost::new(
                event_rx,
                child_exit_rx,
                child_exit_tx,
                config_reload_,
                render_proxy,
                font_system,
                config,
                input_state_for_render,
                render_thread_handle_for_render.clone(),
            );
            host.run(window_rx, startup_release_rx);
        })
        .expect("spawn render thread");

    let mut host = WindowHost {
        window: None,
        startup_presenter: None,
        startup_tabs: vec![tab],
        startup_next_redraw: None,
        startup_release_tx: Some(startup_release_tx),
        input_endpoints: HashMap::from([(
            TabId(0),
            InputEndpoint {
                terminal: terminal.clone(),
                terminal_thread: term_thread_handle,
                writer: pty_writer,
                recorder: initial_recorder,
                command_editor: CommandEditor::new(),
            },
        )]),
        command_catalog: CommandCatalog::from_config(&startup_command_editor),
        command_history_store,
        command_history_writer,
        shell_history_entries: Vec::new(),
        shell_history_loaded: false,
        shell_history_enabled: startup_command_editor.deep_history_integration,
        active_input_tab: Some(TabId(0)),
        input_state,
        event_tx,
        window_tx: Some(window_tx),
        modifiers: ModifiersState::default(),
        ime_preedit_active: false,
        mouse_pos: (0.0, 0.0),
        mouse_buttons: MouseButtonState::default(),
        last_motion_position: None,
        last_click_time: None,
        last_click_cell: None,
        click_count: 0,
        left_drag_active: false,
        selection_drag_moved: false,
        command_editor_drag_anchor: None,
        selection_autoscroll_direction: None,
        selection_autoscroll_next: None,
        window_size: (0, 0),
        opacity,
        cell_width,
        cell_height,
        startup_fonts,
        startup_font_size,
        startup_supersampling,
        startup_dpi_scale,
        startup_gutter,
        render_thread_handle,
        event_proxy: proxy,
        recording_popup: None,
        permission_modal: None,
        queued_permission_requests: VecDeque::new(),
        next_recording_popup_token: 1,
        next_toast_token: 1,
        new_tab_text,
    };
    event_loop.run_app(&mut host).expect("run event loop");
}

fn spawn_new_window(cwd: Option<PathBuf>) {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            warn!("open-new-window: cannot locate term41 binary: {e}");
            return;
        }
    };

    let mut cmd = std::process::Command::new(&exe);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }

    match cmd.spawn() {
        Ok(mut child) => {
            std::thread::Builder::new()
                .name("new-window-waiter".into())
                .spawn(move || {
                    let _ = child.wait();
                })
                .ok();
        }
        Err(e) => {
            warn!("open-new-window: spawn failed: {e}");
        }
    }
}

fn resolve_command_palette_working_directory(
    current_dir: Option<PathBuf>,
    argument: &str,
) -> Option<PathBuf> {
    let argument = argument.trim();
    if argument.is_empty() {
        return None;
    }
    let path = expand_home_path(argument).unwrap_or_else(|| PathBuf::from(argument));
    if path.is_absolute() {
        Some(path)
    } else {
        Some(current_dir.map_or(path.clone(), |dir| dir.join(path)))
    }
}

fn expand_home_path(argument: &str) -> Option<PathBuf> {
    if argument == "~" {
        return dirs::home_dir();
    }
    let rest = argument
        .strip_prefix("~/")
        .or_else(|| argument.strip_prefix("~\\"))?;
    Some(dirs::home_dir()?.join(rest))
}

fn parse_command_args() -> Option<Vec<String>> {
    let mut args = std::env::args();
    let _argv0 = args.next();
    let mut rest: Vec<String> = args.collect();
    if rest.first().map(String::as_str) == Some("--") {
        rest.remove(0);
    }
    if rest.is_empty() { None } else { Some(rest) }
}

#[cfg(test)]
mod command_palette_tests {
    use super::*;

    #[test]
    fn command_palette_items_are_sorted_by_label() {
        let items = command_palette_items("");
        let labels: Vec<_> = items.iter().map(|item| item.label.as_str()).collect();
        let mut sorted = labels.clone();
        sorted.sort_by_key(|label| label.to_ascii_lowercase());
        assert_eq!(labels, sorted);
    }

    #[test]
    fn command_palette_query_matches_labels_by_prefix() {
        let items = command_palette_items("close");
        assert_eq!(
            items
                .iter()
                .map(|item| item.action)
                .collect::<Vec<Action>>(),
            vec![Action::CloseActiveTab, Action::CloseWindow]
        );
    }

    #[test]
    fn command_palette_query_fuzzy_matches_labels() {
        let items = command_palette_items("cat");
        assert_eq!(
            items.first().map(|item| item.action),
            Some(Action::CloseActiveTab)
        );
    }

    #[test]
    fn command_palette_query_normalizes_unicode() {
        let items = fuzzy_command_palette_items(
            "resume",
            [
                CommandPaletteItem {
                    label: "Copy".to_owned(),
                    action: Action::Copy,
                    argument: None,
                },
                CommandPaletteItem {
                    label: "Résumé session".to_owned(),
                    action: Action::Paste,
                    argument: None,
                },
            ],
        );
        assert_eq!(items.first().map(|item| item.action), Some(Action::Paste));
    }

    #[test]
    fn command_palette_query_resets_selection() {
        let mut view = command_palette_view();
        move_command_palette_selection(&mut view, 1);
        set_command_palette_query(&mut view, "toggle".to_owned());
        assert_eq!(view.selected, 0);
        assert!(
            view.items
                .iter()
                .all(|item| item.label.to_ascii_lowercase().contains("toggle"))
        );
    }

    #[test]
    fn command_palette_includes_argument_command_with_colon() {
        let items = command_palette_items("open new window in dir");
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("Open new window in dir:")
        );
    }

    #[test]
    fn command_palette_includes_new_tab_argument_command_with_colon() {
        let items = command_palette_items("open new tab in dir");
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("Open new tab in dir:")
        );
    }

    #[test]
    fn command_palette_argument_text_does_not_affect_matching() {
        let items = command_palette_items("open new window in dir: Documents");
        assert_eq!(
            items.first().map(|item| item.label.as_str()),
            Some("Open new window in dir:")
        );
    }

    #[test]
    fn command_palette_tab_completes_selected_label() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir".to_owned());
        assert!(complete_command_palette_selection(&mut view));
        assert_eq!(view.query, "Open new window in dir: ");
    }

    #[test]
    fn command_palette_tab_preserves_argument_text() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir: Documents".to_owned());
        assert!(complete_command_palette_selection(&mut view));
        assert_eq!(view.query, "Open new window in dir: Documents");
    }

    #[test]
    fn command_palette_enter_requires_argument_for_argument_commands() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir".to_owned());
        assert_eq!(
            command_palette_selected_invocation(&view),
            Some(CommandPaletteAccept::NeedsArgument)
        );
    }

    #[test]
    fn command_palette_enter_returns_argument_invocation() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new window in dir: Documents".to_owned());
        assert_eq!(
            command_palette_selected_invocation(&view),
            Some(CommandPaletteAccept::Ready(CommandPaletteInvocation {
                action: Action::OpenNewWindow,
                argument: Some(CommandPaletteArgument::WorkingDirectory(
                    "Documents".to_owned()
                )),
            }))
        );
    }

    #[test]
    fn command_palette_enter_returns_new_tab_argument_invocation() {
        let mut view = command_palette_view();
        set_command_palette_query(&mut view, "open new tab in dir: Documents".to_owned());
        assert_eq!(
            command_palette_selected_invocation(&view),
            Some(CommandPaletteAccept::Ready(CommandPaletteInvocation {
                action: Action::NewTab,
                argument: Some(CommandPaletteArgument::WorkingDirectory(
                    "Documents".to_owned()
                )),
            }))
        );
    }
}

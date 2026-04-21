use clip41::ClipboardKind;

use super::*;

/// Effects produced by host-originated input routed back toward the PTY.
#[derive(Debug, Default)]
pub struct HostInputEffects {
    /// Bytes to write to the foreground PTY.
    pub host_bytes: Vec<u8>,
}

impl HostInputEffects {
    /// Return whether no host bytes were produced.
    pub fn is_empty(&self) -> bool {
        self.host_bytes.is_empty()
    }

    /// Append another effect batch.
    pub fn extend(
        &mut self,
        other: Self,
    ) {
        self.host_bytes.extend(other.host_bytes);
    }
}

/// Host-originated mouse event to be encoded for the foreground program.
#[derive(Debug, Clone, Copy)]
pub struct HostMouse {
    /// Mouse event kind.
    pub kind: MouseEventKind,
    /// Button associated with the event.
    pub button: MouseButton,
    /// Zero-based terminal column.
    pub col: u32,
    /// Zero-based terminal row.
    pub row: u32,
    /// Keyboard modifiers active during the event.
    pub mods: MouseModifiers,
}

/// Host-originated input routed through the terminal engine boundary.
#[derive(Debug, Clone, Copy)]
pub enum HostInput<'a> {
    /// Window focus changed.
    FocusChanged {
        /// Whether the terminal window gained focus.
        focused: bool,
    },
    /// Mouse event occurred inside the terminal grid.
    Mouse(HostMouse),
    /// Paste the provided text.
    PasteText(&'a str),
    /// Paste text from the selected clipboard.
    PasteFromClipboard {
        /// Clipboard selection to read from.
        kind: ClipboardKind,
    },
}

/// Apply one host-originated input event to terminal state and collect bytes
/// that should be written to the PTY.
pub fn apply_host_input(
    terminal: &mut Terminal,
    input: HostInput<'_>,
) -> HostInputEffects {
    let mut effects = HostInputEffects::default();

    match input {
        HostInput::FocusChanged { focused } => host::report_focus_change(
            &mut effects.host_bytes,
            terminal.modes.c1_mode,
            terminal.modes.focus_reporting,
            focused,
        ),
        HostInput::Mouse(mouse) => {
            host::mouse_report(
                &mut effects.host_bytes,
                terminal.modes.c1_mode,
                terminal.modes.mouse_tracking,
                terminal.modes.mouse_encoding,
                mouse.kind,
                mouse.button,
                mouse.col,
                mouse.row,
                mouse.mods,
            );
        }
        HostInput::PasteText(text) => io::clipboard::paste(
            &mut effects.host_bytes,
            terminal.modes.c1_mode,
            terminal.modes.bracketed_paste,
            text,
        ),
        HostInput::PasteFromClipboard { kind } => io::clipboard::paste_from_clipboard(
            &mut terminal.clipboard,
            &mut effects.host_bytes,
            terminal.modes.c1_mode,
            terminal.modes.bracketed_paste,
            kind,
        ),
    }

    effects
}

struct ParserFrame {
    parser: vtepp::Parser,
    hooks: Vec<dcs::HookState>,
}

impl Default for ParserFrame {
    fn default() -> Self {
        Self {
            parser: vtepp::Parser::new(),
            hooks: vec![],
        }
    }
}

enum FrameInput<'a> {
    Borrowed { bytes: &'a [u8], offset: usize },
    Owned { bytes: Vec<u8>, offset: usize },
}

impl<'a> FrameInput<'a> {
    fn remaining(&self) -> &[u8] {
        match self {
            Self::Borrowed { bytes, offset } => &bytes[*offset..],
            Self::Owned { bytes, offset } => &bytes[*offset..],
        }
    }

    fn advance(
        &mut self,
        amount: usize,
    ) {
        match self {
            Self::Borrowed { offset, .. } | Self::Owned { offset, .. } => *offset += amount,
        }
    }
}

/// Stateful parser/dispatcher for PTY output bytes.
pub struct TerminalProcessor {
    frames: Vec<ParserFrame>,
}

impl Default for TerminalProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalProcessor {
    /// Create a processor with one parser frame.
    pub fn new() -> Self {
        Self {
            frames: vec![ParserFrame::default()],
        }
    }

    /// Process a byte slice from the PTY and apply resulting actions.
    pub fn process_bytes(
        &mut self,
        terminal: &mut Terminal,
        data: &[u8],
    ) -> TerminalEffects {
        let popped_before = terminal.active.grid.total_popped;
        let mut effects = TerminalEffects::default();
        let mut inputs = vec![FrameInput::Borrowed {
            bytes: data,
            offset: 0,
        }];

        loop {
            let stack_depth = self.frames.len();
            let frame = self.frames.last_mut().expect("parser frame");
            let input = inputs.last_mut().expect("frame input");
            let mut parser = frame.parser.parse(input.remaining());
            let mut pushed_frame = false;

            for action in &mut parser {
                match action {
                    vtepp::Action::Hook {
                        params,
                        intermediates,
                        action,
                    } => dcs::push_hook_state(&mut frame.hooks, params, intermediates, action),
                    vtepp::Action::Put(bytes) => dcs::append_hook_bytes(&mut frame.hooks, bytes),
                    vtepp::Action::Unhook => {
                        let Some(hook) = frame.hooks.pop() else {
                            continue;
                        };
                        dcs::dispatch_hook(hook, terminal, &mut effects);
                    }
                    action => match terminal.apply(action, &mut effects) {
                        dispatch::PendingApplication::None => {}
                        dispatch::PendingApplication::Bytes(bytes) => {
                            input.advance(parser.tell());
                            terminal.protocol.macro_invocation_depth += 1;
                            self.frames.push(ParserFrame::default());
                            inputs.push(FrameInput::Owned { bytes, offset: 0 });
                            pushed_frame = true;
                            break;
                        }
                    },
                }
            }

            if pushed_frame {
                continue;
            }

            if stack_depth == 1 {
                break;
            }

            self.frames.pop();
            inputs.pop();
            terminal.protocol.macro_invocation_depth -= 1;
        }

        terminal.track_scroll(popped_before);
        effects
    }
}

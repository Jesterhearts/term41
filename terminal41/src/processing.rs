use super::*;

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

pub struct TerminalProcessor {
    frames: Vec<ParserFrame>,
}

impl Default for TerminalProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalProcessor {
    pub fn new() -> Self {
        Self {
            frames: vec![ParserFrame::default()],
        }
    }

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

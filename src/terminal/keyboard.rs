//! Kitty keyboard protocol mode stack.
//!
//! The protocol layers a richer key-event encoding on top of legacy xterm
//! sequences. Apps push a flag set with `CSI > flags u`, pop it back off with
//! `CSI < N u`, and the terminal reports the current flags on `CSI ? u`. We
//! keep the stack here; the actual key encoding lives in `main.rs` next to the
//! winit input handlers, which is where the modifier state is naturally in
//! scope.
//!
//! Spec: <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>

use std::io::Write;

use crate::vte::Params;

/// Cap on stack depth. Apps push/pop in pairs around things like inner shells
/// and TUI panes; a single misbehaving program could otherwise grow this
/// unbounded by pushing without popping. 16 is comfortably more than any
/// realistic nesting and matches kitty's own cap.
const MAX_STACK: usize = 16;

bitflags::bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct KittyKeys: u8 {
        const SHIFT = 0b0000_0001;
        const ALT   = 0b0000_0010;
        const CTRL  = 0b0000_0100;
        const SUPER = 0b0000_1000;
    }
}

bitflags::bitflags! {
    /// Flags advertised by the kitty keyboard protocol. Each bit toggles a
    /// distinct extension; apps OR together what they want.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
    pub struct KittyFlags: u8 {
        /// Disambiguate keys whose legacy xterm encoding clashes (Ctrl+I vs
        /// Tab, Ctrl+M vs Enter, Esc, Ctrl+letter outside a–z, …) by emitting
        /// `CSI codepoint ; modifiers u` for the modified form.
        const DISAMBIGUATE_ESCAPE_CODES = 0b0000_0001;
        /// Report key release and repeat as well as press.
        const REPORT_EVENT_TYPES        = 0b0000_0010;
        /// Include the shifted/base layout key codes alongside the primary one.
        const REPORT_ALTERNATE_KEYS     = 0b0000_0100;
        /// Report every key — including pure-text input — as a CSI sequence
        /// instead of the bare codepoint.
        const REPORT_ALL_KEYS_AS_ESCAPE_CODES = 0b0000_1000;
        /// Echo the text the key would produce as extra params.
        const REPORT_ASSOCIATED_TEXT    = 0b0001_0000;
    }
}

/// Push/pop stack of currently-active flag sets. The top of stack is the
/// effective mode; pushing/popping is how apps temporarily change behaviour
/// without trampling another layer's preference.
#[derive(Debug, Default)]
pub struct KittyKeyboardState {
    stack: Vec<KittyFlags>,
}

impl KittyKeyboardState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Effective flags = top of stack, or none when the stack is empty.
    pub fn current(&self) -> KittyFlags {
        self.stack.last().copied().unwrap_or(KittyFlags::empty())
    }

    /// `CSI > flags u`. Pushes a new entry. Trims the bottom of the stack
    /// once we exceed [`MAX_STACK`] so a runaway pusher can't grow memory.
    pub fn push(
        &mut self,
        flags: KittyFlags,
    ) {
        self.stack.push(flags);
        if self.stack.len() > MAX_STACK {
            self.stack.remove(0);
        }
    }

    /// `CSI < N u`. Pops up to `n` entries; popping an empty stack is a
    /// no-op (mirrors kitty's behaviour for over-popping apps).
    pub fn pop(
        &mut self,
        n: u32,
    ) {
        for _ in 0..n {
            if self.stack.pop().is_none() {
                break;
            }
        }
    }

    /// `CSI = flags ; mode u`. Mutate the top of stack; if the stack is
    /// empty, behave like a `push(flags)` so the very first `=` from an app
    /// installs a baseline. `mode`: 1=set, 2=or with current, 3=clear bits.
    pub fn set(
        &mut self,
        flags: KittyFlags,
        mode: u32,
    ) {
        if self.stack.is_empty() {
            self.push(flags);
            return;
        }
        let cur = self.stack.last_mut().expect("non-empty by guard");
        *cur = match mode {
            1 => flags,
            2 => *cur | flags,
            3 => *cur & !flags,
            _ => *cur,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_stack_yields_no_flags() {
        let s = KittyKeyboardState::new();
        assert!(s.current().is_empty());
    }

    #[test]
    fn push_pop_round_trip() {
        let mut s = KittyKeyboardState::new();
        s.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        assert_eq!(s.current(), KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        s.pop(1);
        assert!(s.current().is_empty());
    }

    #[test]
    fn pop_more_than_size_is_clamped() {
        let mut s = KittyKeyboardState::new();
        s.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        s.pop(10);
        assert!(s.current().is_empty());
    }

    #[test]
    fn set_replaces_top_in_place() {
        let mut s = KittyKeyboardState::new();
        s.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        s.set(KittyFlags::REPORT_EVENT_TYPES, 1);
        assert_eq!(s.current(), KittyFlags::REPORT_EVENT_TYPES);
        s.pop(1);
        // No second entry was pushed by `set`, so we're back to empty.
        assert!(s.current().is_empty());
    }

    #[test]
    fn set_or_mode_combines_bits() {
        let mut s = KittyKeyboardState::new();
        s.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        s.set(KittyFlags::REPORT_EVENT_TYPES, 2);
        assert_eq!(
            s.current(),
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_EVENT_TYPES
        );
    }

    #[test]
    fn set_clear_mode_strips_bits() {
        let mut s = KittyKeyboardState::new();
        s.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_EVENT_TYPES);
        s.set(KittyFlags::REPORT_EVENT_TYPES, 3);
        assert_eq!(s.current(), KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
    }

    #[test]
    fn set_on_empty_stack_pushes() {
        let mut s = KittyKeyboardState::new();
        s.set(KittyFlags::DISAMBIGUATE_ESCAPE_CODES, 1);
        assert_eq!(s.current(), KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
    }

    #[test]
    fn stack_is_capped() {
        let mut s = KittyKeyboardState::new();
        for _ in 0..(MAX_STACK + 5) {
            s.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        }
        // Bottom entries trimmed away — depth never exceeds the cap.
        s.pop(MAX_STACK as u32);
        assert!(s.current().is_empty());
    }
}

/// Dispatcher for `CSI <intermediate> <params> u`. The intermediate (one of
/// `>`, `<`, `=`, `?`) selects which kitty operation runs; query writes its
/// reply through `pending_output`.
pub(super) fn handle_kitty_keyboard(
    intermediate: u8,
    params: &Params,
    state: &mut KittyKeyboardState,
    pending_output: &mut Vec<u8>,
) {
    let first = params.iter().next().and_then(|g| g.first().copied());
    let second = params.iter().nth(1).and_then(|g| g.first().copied());

    match intermediate {
        b'>' => {
            let flags = KittyFlags::from_bits_truncate(first.unwrap_or(0) as u8);
            state.push(flags);
        }
        b'<' => {
            // Default pop count is 1 when no parameter is given.
            let n = first.map(u32::from).unwrap_or(1);
            state.pop(n);
        }
        b'=' => {
            let flags = KittyFlags::from_bits_truncate(first.unwrap_or(0) as u8);
            // Mode defaults to 1 (set) when omitted, per the spec.
            let mode = second.map(u32::from).unwrap_or(1);
            state.set(flags, mode);
        }
        b'?' => {
            // Query: respond with `CSI ? flags u`. Use write! into the Vec so
            // we only allocate once and skip the formatter detour.
            let flags = state.current().bits();
            let _ = write!(pending_output, "\x1b[?{flags}u");
        }
        _ => {}
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::vte;

    fn parse_csi(input: &[u8]) -> (u8, Params) {
        let mut parser = vte::Parser::new();
        for action in parser.parse(input) {
            if let vte::Action::CsiDispatch {
                params,
                intermediates,
                action,
            } = action
            {
                assert_eq!(action, 'u');
                return (intermediates.as_slice()[0], params);
            }
        }
        panic!("no CSI dispatch from input {input:?}");
    }

    #[test]
    fn push_records_flags() {
        let mut state = KittyKeyboardState::new();
        let mut out = Vec::new();
        let (intr, params) = parse_csi(b"\x1b[>1u");
        handle_kitty_keyboard(intr, &params, &mut state, &mut out);
        assert_eq!(state.current(), KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
    }

    #[test]
    fn pop_default_is_one() {
        let mut state = KittyKeyboardState::new();
        let mut out = Vec::new();
        state.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        let (intr, params) = parse_csi(b"\x1b[<u");
        handle_kitty_keyboard(intr, &params, &mut state, &mut out);
        assert!(state.current().is_empty());
    }

    #[test]
    fn query_emits_current_flags() {
        let mut state = KittyKeyboardState::new();
        state.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_EVENT_TYPES);
        let mut out = Vec::new();
        let (intr, params) = parse_csi(b"\x1b[?u");
        handle_kitty_keyboard(intr, &params, &mut state, &mut out);
        assert_eq!(out, b"\x1b[?3u");
    }

    #[test]
    fn set_with_mode_2_unions_bits() {
        let mut state = KittyKeyboardState::new();
        state.push(KittyFlags::DISAMBIGUATE_ESCAPE_CODES);
        let mut out = Vec::new();
        let (intr, params) = parse_csi(b"\x1b[=2;2u");
        handle_kitty_keyboard(intr, &params, &mut state, &mut out);
        assert_eq!(
            state.current(),
            KittyFlags::DISAMBIGUATE_ESCAPE_CODES | KittyFlags::REPORT_EVENT_TYPES
        );
    }
}

//! Pull-based VTE parser for ANSI/DEC terminal escape sequences.
//!
//! Implements the standard VTE state machine as a pull parser. Feed bytes via
//! [`Parser::parse`] and iterate over the resulting [`Action`] values.

use smol_str::SmolStr;

const MAX_PARAMS: usize = 16;
const MAX_INTERMEDIATES: usize = 4;

/// Maximum OSC string payload retained before further bytes are dropped.
///
/// Protects against unterminated OSC streams exhausting memory while leaving
/// ample room for legitimate uses such as base64-encoded clipboard payloads in
/// OSC 52 — generous enough to hold a raw image pasted to the system clipboard.
const MAX_OSC_LEN: usize = 32 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Params
// ---------------------------------------------------------------------------

/// Parsed parameters from a CSI or DCS sequence.
///
/// Parameters separated by `;` form distinct groups. Sub-parameters within a
/// group are separated by `:`. The iterator yields one `&[u16]` slice per
/// group.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    len: u8,
    group_starts: [u8; MAX_PARAMS],
    num_groups: u8,
}

pub struct ParamsIter<'a> {
    params: &'a Params,
    idx: u8,
}

impl Params {
    pub fn iter(&self) -> ParamsIter<'_> {
        ParamsIter {
            params: self,
            idx: 0,
        }
    }
}

impl<'a> Iterator for ParamsIter<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx >= self.params.num_groups {
            return None;
        }
        let start = self.params.group_starts[self.idx as usize] as usize;
        let end = if self.idx + 1 < self.params.num_groups {
            self.params.group_starts[(self.idx + 1) as usize] as usize
        } else {
            self.params.len as usize
        };
        self.idx += 1;
        Some(&self.params.values[start..end])
    }
}

// ---------------------------------------------------------------------------
// Intermediates
// ---------------------------------------------------------------------------

/// Small inline buffer for intermediate bytes in escape sequences.
#[derive(Debug, Clone, Copy)]
pub struct Intermediates {
    bytes: [u8; MAX_INTERMEDIATES],
    len: u8,
}

impl Intermediates {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

// ---------------------------------------------------------------------------
// Action
// ---------------------------------------------------------------------------

/// A single action produced by the parser.
#[derive(Debug)]
pub enum Action {
    /// A printable character (ASCII or decoded UTF-8). The payload is the raw
    /// UTF-8 for a single codepoint; grapheme-cluster accumulation happens
    /// downstream where the previous cell's contents are known.
    Print(SmolStr),
    /// A C0 or C1 control character.
    Execute(u8),
    /// A complete CSI (Control Sequence Introducer) sequence.
    CsiDispatch {
        params: Params,
        intermediates: Intermediates,
        action: char,
    },
    /// A complete ESC sequence.
    EscDispatch {
        intermediates: Intermediates,
        byte: u8,
    },
    /// A complete OSC (Operating System Command) string.
    ///
    /// The payload contains the raw bytes between the OSC introducer and its
    /// terminator (BEL, ST, or a cancelling control), with the terminator
    /// itself excluded.
    OscDispatch(Vec<u8>),
    /// Start of a DCS (Device Control String) — parameters are available.
    Hook { params: Params, action: char },
    /// A data byte within a DCS string.
    Put(u8),
    /// End of a DCS string.
    Unhook,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Ground,
    Utf8,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    SosPmApcString,
}

// ---------------------------------------------------------------------------
// Parser
// ---------------------------------------------------------------------------

/// Pull-based VTE parser.
///
/// Maintains state across calls to [`parse`](Self::parse) so sequences split
/// across buffer boundaries are handled correctly.
#[derive(Debug)]
pub struct Parser {
    state: State,

    // Parameter builder.
    param_values: [u16; MAX_PARAMS],
    param_len: u8,
    param_group_starts: [u8; MAX_PARAMS],
    param_num_groups: u8,
    param_current: u16,
    param_started: bool,

    // Intermediate bytes.
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediate_count: u8,

    // Buffered action when a single byte produces two actions.
    pending: Option<Action>,

    // UTF-8 decoder.
    utf8_buf: [u8; 4],
    utf8_len: u8,
    utf8_needed: u8,

    // OSC string accumulator — the payload is taken out on dispatch, leaving
    // an empty Vec ready to accept the next sequence without reallocating.
    osc_buf: Vec<u8>,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: State::Ground,
            param_values: [0; MAX_PARAMS],
            param_len: 0,
            param_group_starts: [0; MAX_PARAMS],
            param_num_groups: 0,
            param_current: 0,
            param_started: false,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_count: 0,
            pending: None,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_needed: 0,
            osc_buf: Vec::new(),
        }
    }

    /// Parse a chunk of bytes, returning an iterator of actions.
    ///
    /// The parser retains state between calls so multi-byte sequences that
    /// span buffer boundaries are handled correctly.
    pub fn parse<'p, 'd>(
        &'p mut self,
        data: &'d [u8],
    ) -> ParseIter<'p, 'd> {
        ParseIter {
            parser: self,
            data,
            pos: 0,
        }
    }

    // -- snapshots ----------------------------------------------------------

    fn snapshot_params(&self) -> Params {
        let mut p = Params {
            values: self.param_values,
            len: self.param_len,
            group_starts: self.param_group_starts,
            num_groups: self.param_num_groups,
        };
        if self.param_started && (p.len as usize) < MAX_PARAMS {
            p.values[p.len as usize] = self.param_current;
            p.len += 1;
        }
        p
    }

    fn snapshot_intermediates(&self) -> Intermediates {
        Intermediates {
            bytes: self.intermediates,
            len: self.intermediate_count,
        }
    }

    // -- builder helpers ----------------------------------------------------

    fn clear_params(&mut self) {
        self.param_len = 0;
        self.param_num_groups = 0;
        self.param_current = 0;
        self.param_started = false;
        self.intermediate_count = 0;
    }

    fn add_param_digit(
        &mut self,
        digit: u8,
    ) {
        if (self.param_len as usize) >= MAX_PARAMS {
            return;
        }
        if !self.param_started {
            self.param_started = true;
            self.param_group_starts[0] = 0;
            self.param_num_groups = 1;
        }
        self.param_current = self
            .param_current
            .saturating_mul(10)
            .saturating_add(digit as u16);
    }

    fn finish_param_group(&mut self) {
        if (self.param_len as usize) >= MAX_PARAMS {
            return;
        }
        if !self.param_started {
            self.param_started = true;
            self.param_group_starts[0] = 0;
            self.param_num_groups = 1;
        }
        self.param_values[self.param_len as usize] = self.param_current;
        self.param_len += 1;
        self.param_current = 0;
        if (self.param_num_groups as usize) < MAX_PARAMS {
            self.param_group_starts[self.param_num_groups as usize] = self.param_len;
            self.param_num_groups += 1;
        }
    }

    fn finish_subparam(&mut self) {
        if (self.param_len as usize) >= MAX_PARAMS {
            return;
        }
        if !self.param_started {
            self.param_started = true;
            self.param_group_starts[0] = 0;
            self.param_num_groups = 1;
        }
        self.param_values[self.param_len as usize] = self.param_current;
        self.param_len += 1;
        self.param_current = 0;
    }

    fn collect_intermediate(
        &mut self,
        byte: u8,
    ) {
        if (self.intermediate_count as usize) < MAX_INTERMEDIATES {
            self.intermediates[self.intermediate_count as usize] = byte;
            self.intermediate_count += 1;
        }
    }

    // -- exit actions -------------------------------------------------------

    fn exit_action(&mut self) -> Option<Action> {
        match self.state {
            State::DcsPassthrough => Some(Action::Unhook),
            State::OscString => Some(Action::OscDispatch(std::mem::take(&mut self.osc_buf))),
            _ => None,
        }
    }

    // -- main dispatch ------------------------------------------------------

    fn process_byte(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        // Handle UTF-8 continuation bytes before anywhere transitions.
        if self.state == State::Utf8 {
            if byte & 0xC0 == 0x80 {
                return self.utf8(byte);
            }
            // Not a continuation byte — abort the sequence and reprocess.
            self.state = State::Ground;
        }

        // Anywhere transitions (fire regardless of current state).
        match byte {
            0x18 | 0x1A => {
                let exit = self.exit_action();
                self.state = State::Ground;
                if let Some(exit) = exit {
                    self.pending = Some(Action::Execute(byte));
                    return Some(exit);
                }
                return Some(Action::Execute(byte));
            }
            0x1B => {
                let exit = self.exit_action();
                self.clear_params();
                self.state = State::Escape;
                return exit;
            }
            0x90 => {
                let exit = self.exit_action();
                self.clear_params();
                self.state = State::DcsEntry;
                return exit;
            }
            0x9B => {
                let exit = self.exit_action();
                self.clear_params();
                self.state = State::CsiEntry;
                return exit;
            }
            0x9C => {
                let exit = self.exit_action();
                self.state = State::Ground;
                return exit;
            }
            0x9D => {
                let exit = self.exit_action();
                self.state = State::OscString;
                return exit;
            }
            0x98 | 0x9E | 0x9F => {
                let exit = self.exit_action();
                self.state = State::SosPmApcString;
                return exit;
            }
            0x80..=0x8F | 0x91..=0x97 | 0x99 | 0x9A => {
                let exit = self.exit_action();
                self.state = State::Ground;
                if let Some(exit) = exit {
                    self.pending = Some(Action::Execute(byte));
                    return Some(exit);
                }
                return Some(Action::Execute(byte));
            }
            _ => {}
        }

        // State-specific handling.
        match self.state {
            State::Ground => self.ground(byte),
            State::Utf8 => unreachable!(),
            State::Escape => self.escape(byte),
            State::EscapeIntermediate => self.escape_intermediate(byte),
            State::CsiEntry => self.csi_entry(byte),
            State::CsiParam => self.csi_param(byte),
            State::CsiIntermediate => self.csi_intermediate(byte),
            State::CsiIgnore => self.csi_ignore(byte),
            State::DcsEntry => self.dcs_entry(byte),
            State::DcsParam => self.dcs_param(byte),
            State::DcsIntermediate => self.dcs_intermediate(byte),
            State::DcsPassthrough => self.dcs_passthrough(byte),
            State::DcsIgnore => self.dcs_ignore(byte),
            State::OscString => self.osc_string(byte),
            State::SosPmApcString => self.sos_pm_apc(byte),
        }
    }

    // -- state handlers -----------------------------------------------------

    fn ground(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(Action::Execute(byte)),
            0x20..=0x7E => {
                // Inline SmolStr for single ASCII byte — no allocation.
                let buf = [byte];
                let s = std::str::from_utf8(&buf).unwrap();
                Some(Action::Print(SmolStr::new_inline(s)))
            }
            0x7F => None,
            0xC2..=0xDF => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_needed = 2;
                self.state = State::Utf8;
                None
            }
            0xE0..=0xEF => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_needed = 3;
                self.state = State::Utf8;
                None
            }
            0xF0..=0xF4 => {
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_needed = 4;
                self.state = State::Utf8;
                None
            }
            _ => None,
        }
    }

    fn utf8(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        self.utf8_buf[self.utf8_len as usize] = byte;
        self.utf8_len += 1;
        if self.utf8_len == self.utf8_needed {
            self.state = State::Ground;
            let s = std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize]);
            // Up to 4 UTF-8 bytes → always fits inline in SmolStr (23-byte cap).
            match s.ok() {
                Some(s) => Some(Action::Print(SmolStr::new_inline(s))),
                None => Some(Action::Print(SmolStr::new_inline("\u{FFFD}"))),
            }
        } else {
            None
        }
    }

    fn escape(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(Action::Execute(byte)),
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                self.state = State::EscapeIntermediate;
                None
            }
            0x30..=0x4F | 0x51..=0x57 | 0x59 | 0x5A | 0x5C | 0x60..=0x7E => {
                self.state = State::Ground;
                Some(Action::EscDispatch {
                    intermediates: self.snapshot_intermediates(),
                    byte,
                })
            }
            0x50 => {
                self.clear_params();
                self.state = State::DcsEntry;
                None
            }
            0x58 | 0x5E | 0x5F => {
                self.state = State::SosPmApcString;
                None
            }
            0x5B => {
                self.clear_params();
                self.state = State::CsiEntry;
                None
            }
            0x5D => {
                self.state = State::OscString;
                None
            }
            0x7F => None,
            _ => None,
        }
    }

    fn escape_intermediate(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(Action::Execute(byte)),
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                None
            }
            0x30..=0x7E => {
                self.state = State::Ground;
                Some(Action::EscDispatch {
                    intermediates: self.snapshot_intermediates(),
                    byte,
                })
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_entry(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(Action::Execute(byte)),
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                self.state = State::CsiIntermediate;
                None
            }
            0x30..=0x39 => {
                self.add_param_digit(byte - b'0');
                self.state = State::CsiParam;
                None
            }
            0x3A => {
                self.finish_subparam();
                self.state = State::CsiParam;
                None
            }
            0x3B => {
                self.finish_param_group();
                self.state = State::CsiParam;
                None
            }
            // Private markers (?, >, <, =) are stored as intermediates.
            0x3C..=0x3F => {
                self.collect_intermediate(byte);
                self.state = State::CsiParam;
                None
            }
            0x40..=0x7E => {
                self.state = State::Ground;
                Some(Action::CsiDispatch {
                    params: self.snapshot_params(),
                    intermediates: self.snapshot_intermediates(),
                    action: byte as char,
                })
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_param(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(Action::Execute(byte)),
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                self.state = State::CsiIntermediate;
                None
            }
            0x30..=0x39 => {
                self.add_param_digit(byte - b'0');
                None
            }
            0x3A => {
                self.finish_subparam();
                None
            }
            0x3B => {
                self.finish_param_group();
                None
            }
            // Second private marker — sequence is invalid.
            0x3C..=0x3F => {
                self.state = State::CsiIgnore;
                None
            }
            0x40..=0x7E => {
                self.state = State::Ground;
                Some(Action::CsiDispatch {
                    params: self.snapshot_params(),
                    intermediates: self.snapshot_intermediates(),
                    action: byte as char,
                })
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_intermediate(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(Action::Execute(byte)),
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                None
            }
            0x30..=0x3F => {
                self.state = State::CsiIgnore;
                None
            }
            0x40..=0x7E => {
                self.state = State::Ground;
                Some(Action::CsiDispatch {
                    params: self.snapshot_params(),
                    intermediates: self.snapshot_intermediates(),
                    action: byte as char,
                })
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_ignore(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(Action::Execute(byte)),
            0x20..=0x3F => None,
            0x40..=0x7E => {
                self.state = State::Ground;
                None
            }
            _ => None,
        }
    }

    fn dcs_entry(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            // C0 controls are ignored in DCS states.
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => None,
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                self.state = State::DcsIntermediate;
                None
            }
            0x30..=0x39 => {
                self.add_param_digit(byte - b'0');
                self.state = State::DcsParam;
                None
            }
            0x3A => {
                self.finish_subparam();
                self.state = State::DcsParam;
                None
            }
            0x3B => {
                self.finish_param_group();
                self.state = State::DcsParam;
                None
            }
            0x3C..=0x3F => {
                self.collect_intermediate(byte);
                self.state = State::DcsParam;
                None
            }
            0x40..=0x7E => {
                self.state = State::DcsPassthrough;
                Some(Action::Hook {
                    params: self.snapshot_params(),
                    action: byte as char,
                })
            }
            0x7F => None,
            _ => None,
        }
    }

    fn dcs_param(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => None,
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                self.state = State::DcsIntermediate;
                None
            }
            0x30..=0x39 => {
                self.add_param_digit(byte - b'0');
                None
            }
            0x3A => {
                self.finish_subparam();
                None
            }
            0x3B => {
                self.finish_param_group();
                None
            }
            0x3C..=0x3F => {
                self.state = State::DcsIgnore;
                None
            }
            0x40..=0x7E => {
                self.state = State::DcsPassthrough;
                Some(Action::Hook {
                    params: self.snapshot_params(),
                    action: byte as char,
                })
            }
            0x7F => None,
            _ => None,
        }
    }

    fn dcs_intermediate(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => None,
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                None
            }
            0x30..=0x3F => {
                self.state = State::DcsIgnore;
                None
            }
            0x40..=0x7E => {
                self.state = State::DcsPassthrough;
                Some(Action::Hook {
                    params: self.snapshot_params(),
                    action: byte as char,
                })
            }
            0x7F => None,
            _ => None,
        }
    }

    fn dcs_passthrough(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x20..=0x7E => Some(Action::Put(byte)),
            _ => None,
        }
    }

    fn dcs_ignore(
        &mut self,
        _byte: u8,
    ) -> Option<Action> {
        None
    }

    fn osc_string(
        &mut self,
        byte: u8,
    ) -> Option<Action> {
        match byte {
            // BEL terminates the OSC string (xterm extension, widely supported).
            0x07 => {
                self.state = State::Ground;
                Some(Action::OscDispatch(std::mem::take(&mut self.osc_buf)))
            }
            _ => {
                if self.osc_buf.len() < MAX_OSC_LEN {
                    self.osc_buf.push(byte);
                }
                None
            }
        }
    }

    fn sos_pm_apc(
        &mut self,
        _byte: u8,
    ) -> Option<Action> {
        None
    }
}

// ---------------------------------------------------------------------------
// ParseIter
// ---------------------------------------------------------------------------

/// Iterator over actions produced by parsing a byte slice.
pub struct ParseIter<'p, 'd> {
    parser: &'p mut Parser,
    data: &'d [u8],
    pos: usize,
}

impl Iterator for ParseIter<'_, '_> {
    type Item = Action;

    fn next(&mut self) -> Option<Action> {
        if let Some(action) = self.parser.pending.take() {
            return Some(action);
        }
        while self.pos < self.data.len() {
            let byte = self.data[self.pos];
            self.pos += 1;
            if let Some(action) = self.parser.process_byte(byte) {
                return Some(action);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc_payloads(input: &[u8]) -> Vec<Vec<u8>> {
        let mut parser = Parser::new();
        parser
            .parse(input)
            .filter_map(|a| match a {
                Action::OscDispatch(data) => Some(data),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn osc_dispatch_carries_bel_terminated_payload() {
        assert_eq!(
            osc_payloads(b"\x1b]52;c;aGVsbG8=\x07"),
            vec![b"52;c;aGVsbG8=".to_vec()]
        );
    }

    #[test]
    fn osc_dispatch_carries_st_terminated_payload() {
        assert_eq!(
            osc_payloads(b"\x1b]0;title\x1b\\"),
            vec![b"0;title".to_vec()]
        );
    }

    #[test]
    fn osc_payload_reused_across_sequences() {
        let out = osc_payloads(b"\x1b]1;one\x07\x1b]2;two\x07");
        assert_eq!(out, vec![b"1;one".to_vec(), b"2;two".to_vec()]);
    }

    #[test]
    fn osc_payload_truncates_at_max_len() {
        // Sequence is well over MAX_OSC_LEN when we include the terminator.
        let mut input = Vec::with_capacity(MAX_OSC_LEN + 16);
        input.extend_from_slice(b"\x1b]");
        input.resize(input.len() + MAX_OSC_LEN + 8, b'a');
        input.push(0x07);
        let out = osc_payloads(&input);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), MAX_OSC_LEN);
    }
}

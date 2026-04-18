//! Pull-based VTE parser for ANSI/DEC terminal escape sequences.
//!
//! Implements the standard VTE state machine as a pull parser. Feed bytes via
//! [`Parser::parse`] and iterate over the resulting [`Action`] values.
//!
//! The hot path — scanning a contiguous run of printable ASCII bytes in the
//! [`State::Ground`] state — is dispatched through [`pulp`] so runtime CPU
//! detection picks AVX2 / SSE2 / scalar as available. See [`ScanPrintable`]
//! for the range-test predicate.

use pulp::Simd;
use smol_str::SmolStr;
use smol_str::SmolStrBuilder;
use vte_mode41::TextMode;

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
///
/// The lifetime `'d` ties any borrowed slice to the input buffer that was
/// passed to [`Parser::parse`]; callers typically consume actions immediately
/// inside the iteration loop so the borrow is trivially respected.
#[derive(Debug)]
pub enum Action<'d> {
    /// A contiguous run of printable ASCII bytes (0x20..=0x7E), borrowed from
    /// the input buffer. Emitted by the SIMD scanner in [`State::Ground`] for
    /// the common case of a text run; callers can fast-path this without
    /// grapheme or width reasoning since every byte is width-1.
    PrintAscii(&'d [u8]),
    /// A contiguous run of validated UTF-8 text that includes at least one
    /// multi-byte codepoint. The payload may contain printable ASCII
    /// (0x20..=0x7E) intermixed with UTF-8 multi-byte sequences; it never
    /// contains C0 controls, DEL, or standalone C1 controls (0x80..=0x9F).
    ///
    /// Emitted by the SIMD text scanner when the current byte in
    /// [`State::Ground`] is a valid UTF-8 lead (0xC2..=0xFF). The scanner
    /// finds the extent of the text region using SIMD (stopping at C0/DEL),
    /// then validates UTF-8 structure via `std::str::from_utf8` (which is
    /// itself SIMD-optimized). The validated `&str` is zero-copy from the
    /// input buffer.
    PrintText(&'d str),
    /// A single non-ASCII UTF-8 codepoint. The payload is the raw UTF-8 for
    /// the codepoint reassembled from `utf8_buf`; grapheme-cluster
    /// accumulation happens downstream where the previous cell's contents are
    /// known.
    Print(SmolStr),
    /// A single raw 8-bit graphic byte (0xA0..=0xFF) that could not be
    /// validated as UTF-8 text. This preserves the original byte for
    /// charset-engine consumers instead of dropping it or replacing it.
    Print8Bit(u8),
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
    /// itself excluded. Stays owned because OSCs typically span multiple
    /// `parse()` calls (clipboard/image payloads are large) and are reassembled
    /// inside the parser.
    OscDispatch(Vec<u8>),
    /// A complete APC (Application Program Command) string.
    ///
    /// Used by the kitty graphics protocol (`ESC _ G ...`). Payload contains
    /// the raw bytes between the APC introducer and its terminator (ST).
    ApcDispatch(Vec<u8>),
    /// Start of a DCS (Device Control String) — parameters and intermediates
    /// are available, mirroring CSI dispatch.
    Hook {
        params: Params,
        intermediates: Intermediates,
        action: char,
    },
    /// A contiguous run of DCS passthrough data, borrowed from the input
    /// buffer. Produced both by the SIMD scanner for printable runs and by
    /// the scalar path for individual kept-control bytes.
    Put(&'d [u8]),
    /// End of a DCS string.
    Unhook,
}

// ---------------------------------------------------------------------------
// RawAction (internal)
// ---------------------------------------------------------------------------

/// Scalar-path dispatch result. Kept separate from [`Action`] because the
/// state handlers produce these before the surrounding iterator has the input
/// slice lifetime in scope — the iterator attaches `'d` when it converts the
/// raw result to a public action.
enum RawAction {
    /// A multi-byte UTF-8 codepoint reassembled in `utf8_buf`.
    Print(SmolStr),
    Print8Bit(u8),
    Execute(u8),
    CsiDispatch {
        params: Params,
        intermediates: Intermediates,
        action: char,
    },
    EscDispatch {
        intermediates: Intermediates,
        byte: u8,
    },
    OscDispatch(Vec<u8>),
    ApcDispatch(Vec<u8>),
    Hook {
        params: Params,
        intermediates: Intermediates,
        action: char,
    },
    /// Scalar dispatch of a single DCS passthrough byte (rare — C0 bytes kept
    /// inside DCS). The iterator wraps this into a one-byte slice from the
    /// input buffer.
    PutByte,
    Unhook,
}

// ---------------------------------------------------------------------------
// SIMD scanner
// ---------------------------------------------------------------------------

/// Returns the length of the leading run of printable ASCII bytes
/// (0x20..=0x7E) in `slice`.
///
/// Predicate: a byte `b` is printable ASCII iff `b.wrapping_sub(0x20) < 0x5F`
/// (since 0x7E - 0x20 = 0x5E, which is `< 0x5F`). The wrapping semantics
/// fold 0x00..=0x1F into a large value that still fails the `< 0x5F` check.
///
/// Reused in both [`State::Ground`] (where printable bytes emit
/// [`Action::PrintAscii`]) and [`State::DcsPassthrough`] (where the same
/// range forms the bulk of a sixel stream — individual kept-C0 bytes fall
/// back to the scalar path).
struct ScanPrintable<'a>(&'a [u8]);

impl pulp::WithSimd for ScanPrintable<'_> {
    type Output = usize;

    #[inline(always)]
    fn with_simd<S: Simd>(
        self,
        simd: S,
    ) -> usize {
        let data = self.0;
        let lanes = S::U8_LANES;
        let base = simd.splat_u8s(0x20);
        let limit = simd.splat_u8s(0x5F);

        let mut i = 0;
        while i + lanes <= data.len() {
            // SAFETY: bounds checked above; `S::u8s: Pod` accepts any byte
            // pattern so the unaligned read is sound.
            let chunk: S::u8s =
                unsafe { core::ptr::read_unaligned(data.as_ptr().add(i) as *const S::u8s) };
            let diff = simd.sub_u8s(chunk, base);
            let non_printable = simd.greater_than_or_equal_u8s(diff, limit);
            let first = simd.first_true_m8s(non_printable);
            if first < lanes {
                return i + first;
            }
            i += lanes;
        }

        while i < data.len() {
            let b = data[i];
            if !(0x20..=0x7E).contains(&b) {
                return i;
            }
            i += 1;
        }
        data.len()
    }
}

/// Returns the length of the leading run of bytes that are NOT C0 controls
/// (0x00..=0x1F) and NOT DEL (0x7F).
///
/// Everything else — printable ASCII, UTF-8 lead and continuation bytes
/// (including C1-range continuations 0x80..=0x9F), and high bytes — is
/// treated as "candidate text." The caller validates UTF-8 structure on
/// the returned region via [`std::str::from_utf8`], which disambiguates
/// standalone C1 bytes (invalid UTF-8) from valid continuations.
struct ScanText<'a>(&'a [u8]);

impl pulp::WithSimd for ScanText<'_> {
    type Output = usize;

    #[inline(always)]
    fn with_simd<S: Simd>(
        self,
        simd: S,
    ) -> usize {
        let data = self.0;
        let lanes = S::U8_LANES;
        let splat_0x20 = simd.splat_u8s(0x20);
        let splat_del = simd.splat_u8s(0x7F);

        let mut i = 0;
        while i + lanes <= data.len() {
            // SAFETY: bounds checked above; `S::u8s: Pod` accepts any byte
            // pattern so the unaligned read is sound.
            let chunk: S::u8s =
                unsafe { core::ptr::read_unaligned(data.as_ptr().add(i) as *const S::u8s) };
            let is_c0 = simd.less_than_u8s(chunk, splat_0x20);
            let is_del = simd.equal_u8s(chunk, splat_del);
            let stop = simd.or_m8s(is_c0, is_del);
            let first = simd.first_true_m8s(stop);
            if first < lanes {
                return i + first;
            }
            i += lanes;
        }

        while i < data.len() {
            let b = data[i];
            if b < 0x20 || b == 0x7F {
                return i;
            }
            i += 1;
        }
        data.len()
    }
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
    ApcString,
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
    arch: pulp::Arch,
    state: State,
    text_mode: TextMode,

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

    // Buffered Execute byte deferred until after an exit action is emitted.
    pending_execute: Option<u8>,

    // Buffered byte to be reprocessed after aborting an in-flight UTF-8
    // sequence. Keeps control flow in the main iterator loop rather than
    // recursing through `process_byte`.
    pending_byte: Option<u8>,

    // UTF-8 decoder.
    utf8_buf: [u8; 4],
    utf8_len: u8,
    utf8_needed: u8,

    // OSC string accumulator — the payload is taken out on dispatch, leaving
    // an empty Vec ready to accept the next sequence without reallocating.
    osc_buf: Vec<u8>,

    // APC string accumulator — same pattern as osc_buf. Used by the kitty
    // graphics protocol (ESC _ G ...).
    apc_buf: Vec<u8>,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            arch: pulp::Arch::new(),
            state: State::Ground,
            text_mode: TextMode::Utf8,
            param_values: [0; MAX_PARAMS],
            param_len: 0,
            param_group_starts: [0; MAX_PARAMS],
            param_num_groups: 0,
            param_current: 0,
            param_started: false,
            intermediates: [0; MAX_INTERMEDIATES],
            intermediate_count: 0,
            pending_execute: None,
            pending_byte: None,
            utf8_buf: [0; 4],
            utf8_len: 0,
            utf8_needed: 0,
            osc_buf: Vec::new(),
            apc_buf: Vec::new(),
        }
    }

    /// Parse a chunk of bytes, returning an iterator of actions.
    ///
    /// The parser retains state between calls so multi-byte sequences that
    /// span buffer boundaries are handled correctly.
    pub fn parse<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> ParseIter<'a> {
        ParseIter {
            parser: self,
            data,
            pos: 0,
        }
    }

    pub fn text_mode(&self) -> TextMode {
        self.text_mode
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

    fn esc_dispatch(
        &mut self,
        byte: u8,
    ) -> RawAction {
        let intermediates = self.snapshot_intermediates();
        if intermediates.as_slice() == b"%"
            && let Some(mode) = TextMode::from_docs_final(byte)
        {
            self.text_mode = mode;
        } else if intermediates.as_slice().is_empty() && byte == b'c' {
            self.text_mode = TextMode::Utf8;
        }
        RawAction::EscDispatch {
            intermediates,
            byte,
        }
    }

    fn csi_dispatch(
        &mut self,
        action: char,
    ) -> RawAction {
        let params = self.snapshot_params();
        let intermediates = self.snapshot_intermediates();
        if intermediates.as_slice() == b"!" && action == 'p' {
            self.text_mode = TextMode::Utf8;
        }
        RawAction::CsiDispatch {
            params,
            intermediates,
            action,
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

    fn exit_action(&mut self) -> Option<RawAction> {
        match self.state {
            State::DcsPassthrough => Some(RawAction::Unhook),
            State::OscString => Some(RawAction::OscDispatch(std::mem::take(&mut self.osc_buf))),
            State::ApcString => Some(RawAction::ApcDispatch(std::mem::take(&mut self.apc_buf))),
            _ => None,
        }
    }

    // -- main dispatch ------------------------------------------------------

    fn process_byte(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        // Handle UTF-8 continuation bytes before anywhere transitions.
        if self.state == State::Utf8 {
            if byte & 0xC0 == 0x80 {
                return self.utf8(byte);
            }
            // Not a continuation byte — abort the sequence and reprocess.
            let lead = self.utf8_buf[0];
            self.pending_byte = Some(byte);
            self.utf8_len = 0;
            self.utf8_needed = 0;
            self.state = State::Ground;
            return Some(RawAction::Print8Bit(lead));
        }

        // 7-bit anywhere transitions. CAN/SUB/ESC are 7-bit controls that
        // fire regardless of state — including string states, because
        // that's how 7-bit hosts terminate an OSC/DCS (ESC \ sequence).
        match byte {
            0x18 | 0x1A => {
                let exit = self.exit_action();
                self.state = State::Ground;
                if let Some(exit) = exit {
                    self.pending_execute = Some(byte);
                    return Some(exit);
                }
                return Some(RawAction::Execute(byte));
            }
            0x1B => {
                let exit = self.exit_action();
                self.clear_params();
                self.state = State::Escape;
                return exit;
            }
            _ => {}
        }

        // 8-bit C1 anywhere transitions. These bytes (0x80..=0x9F) double
        // as UTF-8 continuation bytes, so firing them inside a string
        // payload truncates the payload mid-codepoint — e.g. an OSC 0 with
        // title "✳" (U+2733, UTF-8 `\xe2\x9c\xb3`) would terminate at the
        // `\x9c` byte because that's the 8-bit encoding of ST. String
        // states carry opaque payload bytes; the only valid terminators
        // there are BEL (handled by `osc_string`) and 7-bit ESC \ (handled
        // above). Skip C1 anywhere transitions while parsing a payload.
        let in_string_state = matches!(
            self.state,
            State::OscString
                | State::ApcString
                | State::DcsPassthrough
                | State::DcsIgnore
                | State::SosPmApcString,
        );
        if !in_string_state {
            match byte {
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
                0x98 | 0x9E => {
                    let exit = self.exit_action();
                    self.state = State::SosPmApcString;
                    return exit;
                }
                0x9F => {
                    let exit = self.exit_action();
                    self.state = State::ApcString;
                    return exit;
                }
                0x80..=0x8F | 0x91..=0x97 | 0x99 | 0x9A => {
                    let exit = self.exit_action();
                    self.state = State::Ground;
                    if let Some(exit) = exit {
                        self.pending_execute = Some(byte);
                        return Some(exit);
                    }
                    return Some(RawAction::Execute(byte));
                }
                _ => {}
            }
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
            State::ApcString => self.apc_string(byte),
            State::SosPmApcString => self.sos_pm_apc(byte),
        }
    }

    // -- state handlers -----------------------------------------------------

    fn ground(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(RawAction::Execute(byte)),
            // Printable ASCII is handled by the SIMD scanner in ParseIter; if
            // we somehow reach it on the scalar path (e.g. after a state
            // transition leaves us on an already-scanned boundary) emit a
            // one-byte run via the UTF-8 inline path so the branch remains
            // correct.
            0x20..=0x7E => {
                let buf = [byte];
                // SAFETY: single ascii byte guaranteed to be valid UTF-8.
                let s = unsafe { std::str::from_utf8_unchecked(&buf) };
                Some(RawAction::Print(SmolStr::new_inline(s)))
            }
            0xA0..=0xBF | 0xC0..=0xC1 | 0xF5..=0xFF => Some(RawAction::Print8Bit(byte)),
            0x7F => None,
            0xC2..=0xDF => {
                if self.text_mode == TextMode::EightBit {
                    return Some(RawAction::Print8Bit(byte));
                }
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_needed = 2;
                self.state = State::Utf8;
                None
            }
            0xE0..=0xEF => {
                if self.text_mode == TextMode::EightBit {
                    return Some(RawAction::Print8Bit(byte));
                }
                self.utf8_buf[0] = byte;
                self.utf8_len = 1;
                self.utf8_needed = 3;
                self.state = State::Utf8;
                None
            }
            0xF0..=0xF4 => {
                if self.text_mode == TextMode::EightBit {
                    return Some(RawAction::Print8Bit(byte));
                }
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
    ) -> Option<RawAction> {
        self.utf8_buf[self.utf8_len as usize] = byte;
        self.utf8_len += 1;
        if self.utf8_len == self.utf8_needed {
            self.state = State::Ground;
            let s = std::str::from_utf8(&self.utf8_buf[..self.utf8_len as usize]);
            // Up to 4 UTF-8 bytes → always fits inline in SmolStr (23-byte cap).
            match s.ok() {
                Some(s) => {
                    let mut builder = SmolStrBuilder::new();
                    builder.push_str(s);
                    Some(RawAction::Print(builder.finish()))
                }
                None => Some(RawAction::Print8Bit(self.utf8_buf[0])),
            }
        } else {
            None
        }
    }

    fn escape(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(RawAction::Execute(byte)),
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                self.state = State::EscapeIntermediate;
                None
            }
            0x30..=0x4F | 0x51..=0x57 | 0x59 | 0x5A | 0x5C | 0x60..=0x7E => {
                self.state = State::Ground;
                Some(self.esc_dispatch(byte))
            }
            0x50 => {
                self.clear_params();
                self.state = State::DcsEntry;
                None
            }
            0x58 | 0x5E => {
                self.state = State::SosPmApcString;
                None
            }
            0x5F => {
                self.state = State::ApcString;
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
    ) -> Option<RawAction> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(RawAction::Execute(byte)),
            0x20..=0x2F => {
                self.collect_intermediate(byte);
                None
            }
            0x30..=0x7E => {
                self.state = State::Ground;
                Some(self.esc_dispatch(byte))
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_entry(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(RawAction::Execute(byte)),
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
                Some(self.csi_dispatch(byte as char))
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_param(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(RawAction::Execute(byte)),
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
                Some(self.csi_dispatch(byte as char))
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_intermediate(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(RawAction::Execute(byte)),
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
                Some(self.csi_dispatch(byte as char))
            }
            0x7F => None,
            _ => None,
        }
    }

    fn csi_ignore(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            0x00..=0x17 | 0x19 | 0x1C..=0x1F => Some(RawAction::Execute(byte)),
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
    ) -> Option<RawAction> {
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
                Some(RawAction::Hook {
                    params: self.snapshot_params(),
                    intermediates: self.snapshot_intermediates(),
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
    ) -> Option<RawAction> {
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
                Some(RawAction::Hook {
                    params: self.snapshot_params(),
                    intermediates: self.snapshot_intermediates(),
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
    ) -> Option<RawAction> {
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
                Some(RawAction::Hook {
                    params: self.snapshot_params(),
                    intermediates: self.snapshot_intermediates(),
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
    ) -> Option<RawAction> {
        match byte {
            // Printable ASCII is batched by the SIMD scanner; this arm covers
            // the scalar boundary case where ParseIter calls into here with a
            // non-printable-but-kept byte. Emit PutByte so the iterator can
            // wrap it in a one-byte slice of the input buffer.
            0x00..=0x17 | 0x19 | 0x1C..=0x1F | 0x20..=0x7E => Some(RawAction::PutByte),
            _ => None,
        }
    }

    fn dcs_ignore(
        &mut self,
        _byte: u8,
    ) -> Option<RawAction> {
        None
    }

    fn osc_string(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            // BEL terminates the OSC string (xterm extension, widely supported).
            0x07 => {
                self.state = State::Ground;
                Some(RawAction::OscDispatch(std::mem::take(&mut self.osc_buf)))
            }
            _ => {
                if self.osc_buf.len() < MAX_OSC_LEN {
                    self.osc_buf.push(byte);
                }
                None
            }
        }
    }

    fn apc_string(
        &mut self,
        byte: u8,
    ) -> Option<RawAction> {
        match byte {
            // BEL terminates the APC string (same xterm extension as OSC).
            0x07 => {
                self.state = State::Ground;
                Some(RawAction::ApcDispatch(std::mem::take(&mut self.apc_buf)))
            }
            _ => {
                if self.apc_buf.len() < MAX_OSC_LEN {
                    self.apc_buf.push(byte);
                }
                None
            }
        }
    }

    fn sos_pm_apc(
        &mut self,
        _byte: u8,
    ) -> Option<RawAction> {
        None
    }
}

// ---------------------------------------------------------------------------
// ParseIter
// ---------------------------------------------------------------------------

/// Iterator over actions produced by parsing a byte slice.
pub struct ParseIter<'a> {
    parser: &'a mut Parser,
    data: &'a [u8],
    pos: usize,
}

impl<'a> Iterator for ParseIter<'a> {
    type Item = Action<'a>;

    fn next(&mut self) -> Option<Action<'a>> {
        if let Some(byte) = self.parser.pending_execute.take() {
            return Some(Action::Execute(byte));
        }

        loop {
            // Ground fast path: batch a printable-ASCII run to end of buffer
            // (or to the first non-printable byte, whichever comes first).
            if self.parser.state == State::Ground && self.pos < self.data.len() {
                let start = self.pos;
                let n = self
                    .parser
                    .arch
                    .dispatch(ScanPrintable(&self.data[start..]));
                if n > 0 {
                    self.pos += n;
                    return Some(Action::PrintAscii(&self.data[start..start + n]));
                }
            }

            // SIMD text fast path: when the current byte in Ground state is a
            // valid UTF-8 lead (0xC2..=0xFF), use ScanText to find the
            // extent of the candidate region (everything up to the first
            // C0 control or DEL), then validate UTF-8 structure with
            // std::str::from_utf8 (itself SIMD-optimized).  This
            // disambiguates standalone C1 bytes (0x80..=0x9F, rejected as
            // invalid UTF-8) from valid continuation bytes for free.
            if self.parser.state == State::Ground
                && self.pos < self.data.len()
                && self.parser.text_mode == TextMode::Utf8
                && matches!(self.data[self.pos], 0xC2..=0xFF)
            {
                let candidate_len = self.parser.arch.dispatch(ScanText(&self.data[self.pos..]));
                if candidate_len > 0 {
                    let candidate = &self.data[self.pos..self.pos + candidate_len];
                    let valid_len = match std::str::from_utf8(candidate) {
                        Ok(s) => s.len(),
                        Err(e) => e.valid_up_to(),
                    };
                    if valid_len > 0 {
                        // SAFETY: from_utf8 validated [pos..pos+valid_len].
                        let s = unsafe {
                            std::str::from_utf8_unchecked(
                                &self.data[self.pos..self.pos + valid_len],
                            )
                        };
                        self.pos += valid_len;
                        return Some(Action::PrintText(s));
                    }
                }
            }

            // APC fast path: kitty graphics payloads are base64 (all
            // printable ASCII). Batch the run and buffer it directly,
            // avoiding per-byte dispatch through the state machine.
            if self.parser.state == State::ApcString && self.pos < self.data.len() {
                let start = self.pos;
                let n = self
                    .parser
                    .arch
                    .dispatch(ScanPrintable(&self.data[start..]));
                if n > 0 {
                    let buf = &self.data[start..start + n];
                    let remaining = MAX_OSC_LEN.saturating_sub(self.parser.apc_buf.len());
                    self.parser
                        .apc_buf
                        .extend_from_slice(&buf[..buf.len().min(remaining)]);
                    self.pos += n;
                    continue;
                }
            }

            // DCS passthrough fast path: sixel streams are dominantly
            // 0x3F..=0x7E so the same printable range covers the bulk. The
            // scalar path handles the kept-C0 bytes that fall outside it.
            if self.parser.state == State::DcsPassthrough && self.pos < self.data.len() {
                let start = self.pos;
                let n = self
                    .parser
                    .arch
                    .dispatch(ScanPrintable(&self.data[start..]));
                if n > 0 {
                    self.pos += n;
                    return Some(Action::Put(&self.data[start..start + n]));
                }
            }

            let byte = if let Some(byte) = self.parser.pending_byte.take() {
                byte
            } else {
                if self.pos >= self.data.len() {
                    return None;
                }
                let byte = self.data[self.pos];
                self.pos += 1;
                byte
            };
            if let Some(raw) = self.parser.process_byte(byte) {
                return Some(self.convert_raw(raw));
            }
        }
    }
}

impl<'a> ParseIter<'a> {
    fn convert_raw(
        &self,
        raw: RawAction,
    ) -> Action<'a> {
        match raw {
            RawAction::Print(s) => Action::Print(s),
            RawAction::Print8Bit(b) => Action::Print8Bit(b),
            RawAction::Execute(b) => Action::Execute(b),
            RawAction::CsiDispatch {
                params,
                intermediates,
                action,
            } => Action::CsiDispatch {
                params,
                intermediates,
                action,
            },
            RawAction::EscDispatch {
                intermediates,
                byte,
            } => Action::EscDispatch {
                intermediates,
                byte,
            },
            RawAction::OscDispatch(data) => Action::OscDispatch(data),
            RawAction::ApcDispatch(data) => Action::ApcDispatch(data),
            RawAction::Hook {
                params,
                intermediates,
                action,
            } => Action::Hook {
                params,
                intermediates,
                action,
            },
            // The byte that produced this was at self.pos - 1.
            RawAction::PutByte => Action::Put(&self.data[self.pos - 1..self.pos]),
            RawAction::Unhook => Action::Unhook,
        }
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

    /// Owned mirror of [`Action`] so tests can collect into a `Vec` without
    /// the original actions' input-buffer lifetime escaping.
    #[derive(Debug, PartialEq, Eq)]
    enum Owned {
        PrintAscii(Vec<u8>),
        PrintText(String),
        Print(String),
        Print8Bit(u8),
        Execute(u8),
        Csi(Vec<u8>, char),
        Esc(Vec<u8>, u8),
        Osc(Vec<u8>),
        Apc(Vec<u8>),
        Hook(char),
        Put(Vec<u8>),
        Unhook,
    }

    fn collect(input: &[u8]) -> Vec<Owned> {
        let mut parser = Parser::new();
        parser
            .parse(input)
            .map(|a| match a {
                Action::PrintAscii(b) => Owned::PrintAscii(b.to_vec()),
                Action::PrintText(s) => Owned::PrintText(s.to_owned()),
                Action::Print(s) => Owned::Print(s.to_string()),
                Action::Print8Bit(b) => Owned::Print8Bit(b),
                Action::Execute(b) => Owned::Execute(b),
                Action::CsiDispatch {
                    intermediates,
                    action,
                    ..
                } => Owned::Csi(intermediates.as_slice().to_vec(), action),
                Action::EscDispatch {
                    intermediates,
                    byte,
                } => Owned::Esc(intermediates.as_slice().to_vec(), byte),
                Action::OscDispatch(d) => Owned::Osc(d),
                Action::ApcDispatch(d) => Owned::Apc(d),
                Action::Hook { action, .. } => Owned::Hook(action),
                Action::Put(b) => Owned::Put(b.to_vec()),
                Action::Unhook => Owned::Unhook,
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

    #[test]
    fn osc_preserves_utf8_with_c1_continuation_bytes() {
        // U+2733 ✳ encodes to \xe2\x9c\xb3. The \x9c byte is also the 8-bit
        // encoding of ST (String Terminator). A VT500-accurate parser with
        // anywhere-transitions on 0x9C would truncate the payload at the
        // \x9c and dispatch a half-codepoint — which is exactly what Claude
        // Code, tmux status lines, and any shell with emoji in PS1 hit.
        // The payload must survive intact.
        let out = osc_payloads(b"\x1b]0;\xe2\x9c\xb3 Claude Code\x07");
        assert_eq!(out, vec![b"0;\xe2\x9c\xb3 Claude Code".to_vec()]);
    }

    #[test]
    fn osc_preserves_cyrillic_payload() {
        // U+0410 А encodes to \xd0\x90. \x90 is 8-bit DCS. A legacy anywhere
        // transition would switch to DcsEntry mid-OSC and lose the rest of
        // the title to a phantom DCS.
        let out = osc_payloads(b"\x1b]2;\xd0\x90\xd0\xbb\xd0\xb0\x07");
        assert_eq!(out, vec![b"2;\xd0\x90\xd0\xbb\xd0\xb0".to_vec()]);
    }

    #[test]
    fn osc_still_terminates_on_bel_and_st_after_fix() {
        // Regression guard: the UTF-8 fix must not break legitimate
        // terminators. BEL (0x07) and 7-bit ESC \ (0x1B 0x5C) still end
        // the payload and dispatch it.
        assert_eq!(osc_payloads(b"\x1b]0;ascii\x07"), vec![b"0;ascii".to_vec()]);
        assert_eq!(
            osc_payloads(b"\x1b]0;ascii\x1b\\"),
            vec![b"0;ascii".to_vec()]
        );
    }

    #[test]
    fn print_ascii_run_batches_full_buffer() {
        let out = collect(b"hello world");
        assert_eq!(out, vec![Owned::PrintAscii(b"hello world".to_vec())]);
    }

    #[test]
    fn print_ascii_run_ends_at_control() {
        let out = collect(b"hello\nworld");
        assert_eq!(
            out,
            vec![
                Owned::PrintAscii(b"hello".to_vec()),
                Owned::Execute(0x0A),
                Owned::PrintAscii(b"world".to_vec()),
            ]
        );
    }

    #[test]
    fn print_ascii_run_ends_at_esc() {
        let out = collect(b"hi\x1b[31mred");
        assert_eq!(out[0], Owned::PrintAscii(b"hi".to_vec()));
        assert!(matches!(out[1], Owned::Csi(_, 'm')));
        assert_eq!(out[2], Owned::PrintAscii(b"red".to_vec()));
    }

    #[test]
    fn print_ascii_run_ends_at_utf8_lead() {
        // "hi" then "é" (0xc3 0xa9): the ASCII scanner stops at the 0xc3
        // lead byte, then the text scanner batches the two UTF-8 bytes into
        // a single PrintText action.
        let out = collect(b"hi\xc3\xa9");
        assert_eq!(
            out,
            vec![
                Owned::PrintAscii(b"hi".to_vec()),
                Owned::PrintText("é".to_owned()),
            ]
        );
    }

    #[test]
    fn print_text_batches_cjk_run() {
        // 3-byte CJK characters: the ScanText fast path should batch all of
        // them into a single PrintText action instead of one Print per char.
        let input = "你好世界";
        let out = collect(input.as_bytes());
        assert_eq!(out.len(), 1, "expected one PrintText, got: {out:?}");
        assert_eq!(out[0], Owned::PrintText(input.to_owned()));
    }

    #[test]
    fn print_text_mixed_ascii_and_utf8() {
        // A run that starts with a UTF-8 sequence but also contains ASCII:
        // the whole thing arrives as one PrintText because ScanText continues
        // through printable ASCII bytes too.
        let input = "é world"; // \xc3\xa9 + b" world"
        let out = collect(input.as_bytes());
        assert_eq!(out.len(), 1, "expected one PrintText, got: {out:?}");
        assert_eq!(out[0], Owned::PrintText(input.to_owned()));
    }

    #[test]
    fn print_text_terminates_at_c0_control() {
        // A C0 control in the middle of a UTF-8 run stops the text scan.
        let input = b"\xe4\xbd\xa0\x0a\xe5\xa5\xbd"; // 你 LF 好
        let out = collect(input);
        // PrintText("你"), Execute(LF), PrintText("好")
        assert_eq!(out.len(), 3, "got: {out:?}");
        assert_eq!(out[0], Owned::PrintText("你".to_owned()));
        assert_eq!(out[1], Owned::Execute(0x0A));
        assert_eq!(out[2], Owned::PrintText("好".to_owned()));
    }

    #[test]
    fn print_text_terminates_at_c1_control() {
        // A C1 control (0x9B = 8-bit CSI) interrupts the text scan so
        // the C1 anywhere-transition fires correctly.
        let out = collect(b"\xe4\xbd\xa0\x9b0J");
        // PrintText("你"), then the 8-bit CSI is processed as a state
        // transition → CsiDispatch 'J' with param 0.
        assert!(
            out.iter().any(|a| matches!(a, Owned::PrintText(_))),
            "expected PrintText: {out:?}"
        );
        assert!(
            out.iter().any(|a| matches!(a, Owned::Csi(_, 'J'))),
            "expected CSI J: {out:?}"
        );
    }

    #[test]
    fn print_text_large_cjk_buffer() {
        // Exercises the SIMD main loop across many AVX2 chunks.
        let reps = 64 * 1024 / 3 + 1;
        let text: String = "你".repeat(reps);
        let out = collect(text.as_bytes());
        assert_eq!(out.len(), 1, "expected single PrintText, got {}", out.len());
        match &out[0] {
            Owned::PrintText(s) => assert_eq!(s.len(), text.len()),
            other => panic!("expected PrintText, got {other:?}"),
        }
    }

    #[test]
    fn print_ascii_run_spans_large_buffer() {
        // Exercises the SIMD main loop across multiple chunks plus tail.
        let buf = vec![b'a'; 64 * 1024 + 7];
        let out = collect(&buf);
        assert_eq!(out.len(), 1);
        match &out[0] {
            Owned::PrintAscii(b) => assert_eq!(b.len(), buf.len()),
            other => panic!("expected PrintAscii, got {:?}", other),
        }
    }

    #[test]
    fn print_ascii_run_split_across_two_parse_calls() {
        let mut parser = Parser::new();
        let first: Vec<Vec<u8>> = parser
            .parse(b"hello")
            .filter_map(|a| match a {
                Action::PrintAscii(b) => Some(b.to_vec()),
                _ => None,
            })
            .collect();
        assert_eq!(first, vec![b"hello".to_vec()]);
        let second: Vec<Vec<u8>> = parser
            .parse(b"world")
            .filter_map(|a| match a {
                Action::PrintAscii(b) => Some(b.to_vec()),
                _ => None,
            })
            .collect();
        assert_eq!(second, vec![b"world".to_vec()]);
    }

    #[test]
    fn invalid_utf8_lead_surfaces_raw_8bit_byte() {
        let out = collect(b"\xe1A");
        assert_eq!(
            out,
            vec![Owned::Print8Bit(0xE1), Owned::Print("A".to_owned())]
        );
    }

    #[test]
    fn raw_8bit_graphic_bytes_in_ground_are_not_dropped() {
        let out = collect(b"\xa1\xa2");
        assert_eq!(out, vec![Owned::Print8Bit(0xA1), Owned::Print8Bit(0xA2)]);
    }

    #[test]
    fn valid_utf8_still_beats_raw_8bit_fallback() {
        let out = collect("áA".as_bytes());
        assert_eq!(out, vec![Owned::PrintText("áA".to_owned())]);
    }

    #[test]
    fn docs_switch_to_8bit_mode_surfaces_raw_high_bytes() {
        let out = collect(b"\x1b%@\xe1A");
        assert_eq!(
            out,
            vec![
                Owned::Esc(b"%".to_vec(), b'@'),
                Owned::Print8Bit(0xE1),
                Owned::PrintAscii(b"A".to_vec()),
            ]
        );
    }

    #[test]
    fn docs_switch_back_to_utf8_mode_restores_text_batching() {
        let out = collect(b"\x1b%@\xe1\x1b%G\xc3\xa1");
        assert_eq!(
            out,
            vec![
                Owned::Esc(b"%".to_vec(), b'@'),
                Owned::Print8Bit(0xE1),
                Owned::Esc(b"%".to_vec(), b'G'),
                Owned::PrintText("á".to_owned()),
            ]
        );
    }

    #[test]
    fn dcs_passthrough_batches_put_slices() {
        // DCS hook + payload + ST
        let out = collect(b"\x1bPq#0;2;0;0;0#1!14~-\x1b\\");
        let hooks: Vec<_> = out.iter().filter(|a| matches!(a, Owned::Hook(_))).collect();
        assert_eq!(hooks.len(), 1);
        let puts: Vec<&[u8]> = out
            .iter()
            .filter_map(|a| match a {
                Owned::Put(b) => Some(b.as_slice()),
                _ => None,
            })
            .collect();
        // All payload bytes are printable ASCII so the SIMD path batches them
        // into a single Put slice.
        assert_eq!(puts.len(), 1);
        assert_eq!(puts[0], b"#0;2;0;0;0#1!14~-");
        assert!(out.iter().any(|a| matches!(a, Owned::Unhook)));
    }

    #[test]
    fn execute_byte_after_osc_dispatched_in_order() {
        // SUB (0x1A) inside an OSC string should first emit the dispatched
        // OSC payload, then the Execute for SUB.
        let out = collect(b"\x1b]0;title\x1a");
        assert_eq!(out[0], Owned::Osc(b"0;title".to_vec()));
        assert_eq!(out[1], Owned::Execute(0x1A));
    }

    #[test]
    fn c0_control_inside_csi_is_executed_and_sequence_continues() {
        // BS (0x08) embedded in a CSI H sequence should be executed mid-
        // parse, then the CSI H dispatch should still fire with the
        // correct parameters.
        let out = collect(b"\x1b[1\x08;1H");
        // Expect: Execute(BS), then CsiDispatch for 'H' with params [1,1].
        assert!(out.contains(&Owned::Execute(0x08)));
        assert!(out.contains(&Owned::Csi(vec![], 'H')));
    }

    #[test]
    fn c0_controls_inside_escape_sequence_are_executed() {
        // A LF (0x0A) between ESC and [ should be executed, then the CSI
        // sequence should still parse.
        let out = collect(b"\x1b\x0a[H");
        assert!(out.contains(&Owned::Execute(0x0A)));
        assert!(out.contains(&Owned::Csi(vec![], 'H')));
    }

    #[test]
    fn ht_inside_csi_params_executes_and_dispatches() {
        // HT (0x09) embedded mid-parameter: ESC [ 3 HT ; 1 H
        // Should execute the HT and dispatch CHA (H) with params.
        let out = collect(b"\x1b[3\x09;1H");
        assert!(
            out.contains(&Owned::Execute(0x09)),
            "HT not executed: {out:?}"
        );
        assert!(
            out.contains(&Owned::Csi(vec![], 'H')),
            "CSI H not dispatched: {out:?}"
        );
    }

    #[test]
    fn cr_lf_inside_csi_params_execute_and_dispatches() {
        // CR + LF embedded in CSI params: ESC [ 2 CR LF ; 1 G
        let out = collect(b"\x1b[2\x0d\x0a;1G");
        assert!(
            out.contains(&Owned::Execute(0x0D)),
            "CR not executed: {out:?}"
        );
        assert!(
            out.contains(&Owned::Execute(0x0A)),
            "LF not executed: {out:?}"
        );
        assert!(
            out.contains(&Owned::Csi(vec![], 'G')),
            "CSI G not dispatched: {out:?}"
        );
    }
}

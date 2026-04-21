//! Shared terminal mode enums used by the parser and emulator.
//!
//! This crate keeps DEC conformance level, C1 encoding mode, and high-byte
//! text interpretation in one place so `vtepp` can parse byte streams without
//! depending on the full terminal state machine.

/// DEC operating level selected by DECSCL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceLevel {
    /// Level 1 — VT100 family.
    Level1,
    /// Level 2 — VT200 family.
    Level2,
    /// Level 3 — VT300 family.
    Level3,
    /// Level 4 — VT400 family.
    Level4,
}

impl ConformanceLevel {
    /// Parse the first DECSCL parameter into a DEC conformance level.
    pub fn from_decscl(ps1: u16) -> Option<Self> {
        match ps1 {
            61 => Some(Self::Level1),
            62 => Some(Self::Level2),
            63 => Some(Self::Level3),
            64 => Some(Self::Level4),
            _ => None,
        }
    }

    /// Return the DA1/DECSCL numeric code associated with this level.
    pub fn da1_code(self) -> u16 {
        match self {
            Self::Level1 => 61,
            Self::Level2 => 62,
            Self::Level3 => 63,
            Self::Level4 => 64,
        }
    }

    /// Whether this operating level can negotiate 7-bit vs 8-bit C1 output.
    pub fn supports_c1_negotiation(self) -> bool {
        !matches!(self, Self::Level1)
    }
}

/// Whether terminal-generated C1 controls are emitted in 7-bit or 8-bit form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C1Mode {
    /// Emit C1 controls as ESC-prefixed 7-bit sequences.
    SevenBit,
    /// Emit C1 controls as raw bytes in the 0x80..=0x9F range.
    EightBit,
}

impl C1Mode {
    /// Parse the second DECSCL parameter. Missing or unknown values follow
    /// DEC's default of 8-bit controls.
    pub fn from_decscl(ps2: Option<u16>) -> Self {
        match ps2.unwrap_or(0) {
            1 => Self::SevenBit,
            _ => Self::EightBit,
        }
    }

    /// Return this mode's DECSCL parameter value.
    pub fn decscl_param(self) -> u16 {
        match self {
            Self::SevenBit => 1,
            Self::EightBit => 2,
        }
    }
}

/// How ground-state bytes above ASCII should be interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TextMode {
    /// Treat high bytes as UTF-8 leads/continuations whenever they validate.
    #[default]
    Utf8,
    /// Treat 0xA0..=0xFF as raw 8-bit graphics routed through GR.
    EightBit,
}

impl TextMode {
    /// DOCS selector used by xterm/ECMA-35 style text-mode switching.
    pub fn from_docs_final(byte: u8) -> Option<Self> {
        match byte {
            b'@' => Some(Self::EightBit),
            b'G' | b'8' => Some(Self::Utf8),
            _ => None,
        }
    }
}

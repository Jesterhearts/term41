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
    pub fn from_decscl(ps1: u16) -> Option<Self> {
        match ps1 {
            61 => Some(Self::Level1),
            62 => Some(Self::Level2),
            63 => Some(Self::Level3),
            64 => Some(Self::Level4),
            _ => None,
        }
    }

    pub fn da1_code(self) -> u16 {
        match self {
            Self::Level1 => 61,
            Self::Level2 => 62,
            Self::Level3 => 63,
            Self::Level4 => 64,
        }
    }

    pub fn supports_c1_negotiation(self) -> bool {
        !matches!(self, Self::Level1)
    }
}

/// Whether terminal-generated C1 controls are emitted in 7-bit or 8-bit form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum C1Mode {
    SevenBit,
    EightBit,
}

impl C1Mode {
    pub fn from_decscl(ps2: Option<u16>) -> Self {
        match ps2.unwrap_or(0) {
            1 => Self::SevenBit,
            _ => Self::EightBit,
        }
    }

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

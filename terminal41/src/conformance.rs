use std::io::Write;

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

pub fn push_csi_prefix(
    out: &mut Vec<u8>,
    mode: C1Mode,
) {
    match mode {
        C1Mode::SevenBit => out.extend_from_slice(b"\x1b["),
        C1Mode::EightBit => out.push(0x9B),
    }
}

pub fn push_dcs_prefix(
    out: &mut Vec<u8>,
    mode: C1Mode,
) {
    match mode {
        C1Mode::SevenBit => out.extend_from_slice(b"\x1bP"),
        C1Mode::EightBit => out.push(0x90),
    }
}

pub fn push_osc_prefix(
    out: &mut Vec<u8>,
    mode: C1Mode,
) {
    match mode {
        C1Mode::SevenBit => out.extend_from_slice(b"\x1b]"),
        C1Mode::EightBit => out.push(0x9D),
    }
}

pub fn push_apc_prefix(
    out: &mut Vec<u8>,
    mode: C1Mode,
) {
    match mode {
        C1Mode::SevenBit => out.extend_from_slice(b"\x1b_"),
        C1Mode::EightBit => out.push(0x9F),
    }
}

pub fn push_st(
    out: &mut Vec<u8>,
    mode: C1Mode,
) {
    match mode {
        C1Mode::SevenBit => out.extend_from_slice(b"\x1b\\"),
        C1Mode::EightBit => out.push(0x9C),
    }
}

pub fn write_csi(
    out: &mut Vec<u8>,
    mode: C1Mode,
    args: std::fmt::Arguments<'_>,
) {
    push_csi_prefix(out, mode);
    out.write_fmt(args).expect("write to Vec is infallible");
}

pub fn write_dcs(
    out: &mut Vec<u8>,
    mode: C1Mode,
    args: std::fmt::Arguments<'_>,
) {
    push_dcs_prefix(out, mode);
    out.write_fmt(args).expect("write to Vec is infallible");
    push_st(out, mode);
}

pub fn write_osc(
    out: &mut Vec<u8>,
    mode: C1Mode,
    args: std::fmt::Arguments<'_>,
) {
    push_osc_prefix(out, mode);
    out.write_fmt(args).expect("write to Vec is infallible");
    push_st(out, mode);
}

pub fn write_apc(
    out: &mut Vec<u8>,
    mode: C1Mode,
    args: std::fmt::Arguments<'_>,
) {
    push_apc_prefix(out, mode);
    out.write_fmt(args).expect("write to Vec is infallible");
    push_st(out, mode);
}

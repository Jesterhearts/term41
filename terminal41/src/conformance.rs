use std::io::Write;

pub use vte_mode41::C1Mode;
pub use vte_mode41::ConformanceLevel;

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

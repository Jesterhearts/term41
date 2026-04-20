use std::io;
use std::io::Write;
use std::time::Duration;

use crate::capabilities::CapabilityReport;

pub struct Demo {
    pub title: &'static str,
    pub summary: &'static str,
    pub detail: &'static str,
    pub id: DemoId,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoId {
    Identity,
    ResetReports,
    Sgr,
    Charset,
    Drcs,
    LineAttrs,
    Rectangles,
    Vt525Color,
    StatusLine,
    Macros,
    PageMemory,
    Vt52,
}

const DRCS_SAMPLE_SCRIPT: &str = include_str!("../resources/icon.drcs");

type ReadReplyFn<'a> = dyn FnMut(Duration) -> io::Result<Vec<u8>> + 'a;

pub fn catalog() -> Vec<Demo> {
    vec![
        Demo {
            title: "Identity & Queries",
            summary: "DA1, DSR, DECRQM, DECRQSS, DECRQPSR, DECRQTSR, and DECCTR queries.",
            detail: "Captures raw terminal replies for the main report/query families and prints \
                     them back to the screen.",
            id: DemoId::Identity,
        },
        Demo {
            title: "Reset & State Restore",
            summary: "DECSC/DECRC, DECRSPS, DECRSTS, DECTST, and DECSR/DECSRC.",
            detail: "Exercises save/restore and reset families, including the secure-reset \
                     confirmation reply.",
            id: DemoId::ResetReports,
        },
        Demo {
            title: "SGR Styles",
            summary: "Bold, italic, underline variants, reverse, conceal, blink, and truecolor.",
            detail: "Exercises the styled text surface with 16-color, 256-color, truecolor, \
                     underline styles, and blink.",
            id: DemoId::Sgr,
        },
        Demo {
            title: "Charset Engine",
            summary: "DEC graphics, technical/supplemental sets, shifts, NRCM, and 8-bit text \
                      mode.",
            detail: "Exercises designation, locking shifts, single shifts, and UTF-8 vs 8-bit \
                     text mode routing.",
            id: DemoId::Charset,
        },
        Demo {
            title: "DRCS Soft Chars",
            summary: "Downloads a bundled soft character icon and prints it.",
            detail: "Exercises DECDLD plus SCS designation for downloadable soft characters.",
            id: DemoId::Drcs,
        },
        Demo {
            title: "DEC Line Attributes",
            summary: "Single-width, double-width, and double-height rows.",
            detail: "Exercises DECSWL, DECDWL, and the double-height top/bottom pair.",
            id: DemoId::LineAttrs,
        },
        Demo {
            title: "Rectangular Ops",
            summary: "DECERA, DECFRA, DECCRA, DECSERA, DECCARA, DECRARA, and DECSACE.",
            detail: "Exercises the full VT420 rectangular-area family with visible mutations.",
            id: DemoId::Rectangles,
        },
        Demo {
            title: "VT525 Colors",
            summary: "DECAC, DECATC, DECCTR, and lookup-table selection.",
            detail: "Exercises the VT525 color-control surface, including alternate text colors \
                     and table remaps.",
            id: DemoId::Vt525Color,
        },
        Demo {
            title: "Status Line",
            summary: "Indicator and host-writable status lines plus visible separation.",
            detail: "Exercises DECSSDT and DECSASD in both indicator and host-writable modes.",
            id: DemoId::StatusLine,
        },
        Demo {
            title: "Macros",
            summary: "Define and invoke a small VT420 macro if the terminal advertises them.",
            detail: "Exercises DECDMAC and DECINVM. If macros are not advertised, the demo \
                     explains that they are gated.",
            id: DemoId::Macros,
        },
        Demo {
            title: "Page Memory & Geometry",
            summary: "NP, PP, PPA/PPR/PPB, DECCRA, DECXCPR, and page-memory setup.",
            detail: "Exercises VT420 page-memory navigation and query behavior without relying on \
                     permanent window changes.",
            id: DemoId::PageMemory,
        },
        Demo {
            title: "VT52 / Conformance",
            summary: "Switch into VT52, use VT52 cursor motion, then return to ANSI/C1 \
                      negotiation.",
            detail: "Exercises VT52 mode entry, VT52 cursor sequences, DECSCL, and S7C1T/S8C1T.",
            id: DemoId::Vt52,
        },
    ]
}

pub fn run_demo(
    out: &mut impl Write,
    demo: DemoId,
    capabilities: &CapabilityReport,
    read_reply: &mut ReadReplyFn<'_>,
) -> io::Result<()> {
    clear_screen(out)?;
    match demo {
        DemoId::Identity => run_identity_demo(out, capabilities, read_reply),
        DemoId::ResetReports => run_reset_reports_demo(out, read_reply),
        DemoId::Sgr => run_sgr_demo(out),
        DemoId::Charset => run_charset_demo(out),
        DemoId::Drcs => run_drcs_demo(out),
        DemoId::LineAttrs => run_line_attrs_demo(out),
        DemoId::Rectangles => run_rectangles_demo(out),
        DemoId::Vt525Color => run_vt525_color_demo(out),
        DemoId::StatusLine => run_status_line_demo(out),
        DemoId::Macros => run_macro_demo(out, capabilities),
        DemoId::PageMemory => run_page_demo(out, read_reply),
        DemoId::Vt52 => run_vt52_demo(out),
    }?;
    write!(
        out,
        "\r\n\r\n\x1b[0mPress any key to return to selftest41.\x1b[0m"
    )?;
    out.flush()
}

fn clear_screen(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\x1b[0m\x1b[2J\x1b[H\x1b[?25l")
}

fn heading(
    out: &mut impl Write,
    title: &str,
) -> io::Result<()> {
    write!(out, "\x1b[1m{}\x1b[0m\r\n\r\n", title)
}

fn line(
    out: &mut impl Write,
    text: &str,
) -> io::Result<()> {
    write!(out, "{text}\r\n")
}

fn blank(out: &mut impl Write) -> io::Result<()> {
    line(out, "")
}

fn query_and_print(
    out: &mut impl Write,
    read_reply: &mut ReadReplyFn<'_>,
    label: &str,
    request: &[u8],
    timeout: Duration,
) -> io::Result<()> {
    out.write_all(request)?;
    out.flush()?;
    let reply = read_reply(timeout)?;
    line(out, &format!("{label}: {}", format_reply(&reply)))
}

fn format_reply(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::from("<no reply>");
    }
    let mut out = String::new();
    for &byte in bytes {
        out.push_str(&ascii_escape(byte));
    }
    out
}

fn ascii_escape(byte: u8) -> String {
    std::ascii::escape_default(byte)
        .map(char::from)
        .collect::<String>()
}

fn run_identity_demo(
    out: &mut impl Write,
    capabilities: &CapabilityReport,
    read_reply: &mut ReadReplyFn<'_>,
) -> io::Result<()> {
    heading(out, "Identity & Queries")?;
    if let Some(raw) = &capabilities.raw_reply {
        line(
            out,
            &format!("Captured DA1 reply: {}", raw.escape_default()),
        )?;
    } else {
        line(out, "No startup DA1 reply captured.")?;
    }
    blank(out)?;
    line(out, "Live query round-trip:")?;
    query_and_print(
        out,
        read_reply,
        "  DA1",
        b"\x1b[c",
        Duration::from_millis(200),
    )?;
    query_and_print(
        out,
        read_reply,
        "  DSR 5n",
        b"\x1b[5n",
        Duration::from_millis(200),
    )?;
    write!(out, "\x1b[6;11H")?;
    out.flush()?;
    query_and_print(
        out,
        read_reply,
        "  CPR 6n",
        b"\x1b[6n",
        Duration::from_millis(200),
    )?;
    query_and_print(
        out,
        read_reply,
        "  DECRQM ?6",
        b"\x1b[?6$p",
        Duration::from_millis(200),
    )?;
    query_and_print(
        out,
        read_reply,
        "  DECRQSS DECSCL",
        b"\x1bP$q\"p\x1b\\",
        Duration::from_millis(200),
    )?;
    query_and_print(
        out,
        read_reply,
        "  DECRQPSR DECCIR",
        b"\x1b[1$w",
        Duration::from_millis(200),
    )?;
    query_and_print(
        out,
        read_reply,
        "  DECRQTSR",
        b"\x1b[1$u",
        Duration::from_millis(200),
    )?;
    query_and_print(
        out,
        read_reply,
        "  DECCTR RGB",
        b"\x1b[2;2$u",
        Duration::from_millis(200),
    )?;
    Ok(())
}

fn run_reset_reports_demo(
    out: &mut impl Write,
    read_reply: &mut ReadReplyFn<'_>,
) -> io::Result<()> {
    heading(out, "Reset & State Restore")?;
    line(
        out,
        "Saving cursor, moving away, then restoring with DECSC/DECRC.",
    )?;
    write!(out, "\x1b[4;10H\x1b7\x1b[8;30Hmoved\x1b8")?;
    line(out, "Restored cursor should land back near row 4, col 10.")?;
    blank(out)?;
    line(out, "Now issuing DECSR with confirmation parameter 41.")?;
    out.write_all(b"\x1b[41+p")?;
    out.flush()?;
    let reply = read_reply(Duration::from_millis(200))?;
    clear_screen(out)?;
    heading(out, "Reset & State Restore")?;
    line(out, "DECSR reset the terminal state and returned:")?;
    line(out, &format!("  {}", format_reply(&reply)))?;
    blank(out)?;
    line(out, "Issuing DECTST power-up self-test form next.")?;
    out.write_all(b"\x1b[4;1y")?;
    out.flush()?;
    clear_screen(out)?;
    heading(out, "Reset & State Restore")?;
    line(
        out,
        "DECTST 4;1y completed. The screen should now be in default reset state.",
    )?;
    Ok(())
}

fn run_sgr_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "SGR Styles")?;
    line(
        out,
        "\x1b[1mBold\x1b[0m  \x1b[2mDim\x1b[0m  \x1b[3mItalic\x1b[0m  \x1b[7mReverse\x1b[0m  \
         \x1b[8mConceal\x1b[0m",
    )?;
    line(out, "\x1b[4mSingle underline\x1b[0m")?;
    line(out, "\x1b[21mDouble underline via SGR 21\x1b[0m")?;
    line(out, "\x1b[4:2mDouble underline via 4:2\x1b[0m")?;
    line(out, "\x1b[4:3mCurly underline\x1b[0m")?;
    line(out, "\x1b[4:4mDotted underline\x1b[0m")?;
    line(out, "\x1b[4:5mDashed underline\x1b[0m")?;
    line(out, "\x1b[9mStrikethrough\x1b[0m  \x1b[53mOverline\x1b[0m")?;
    line(out, "\x1b[38;5;202m256-color foreground\x1b[0m")?;
    line(
        out,
        "\x1b[48;2;20;40;80m\x1b[38;2;240;240;255mTruecolor background\x1b[0m",
    )?;
    line(
        out,
        "\x1b[58;2;255;128;0m\x1b[4mColored underline\x1b[59;24m\x1b[0m",
    )?;
    line(out, "\x1b[5mBlink\x1b[0m  \x1b[6mRapid blink\x1b[0m")?;
    Ok(())
}

fn run_charset_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Charset Engine")?;
    line(out, "ASCII:                +-- charset demo --+")?;
    line(out, "DEC Special Graphics:\x1b(0 lqqqqqqqqqqqqqqqqqqk")?;
    line(out, "\x1b(0                      x  locking shifts  x")?;
    line(out, "\x1b(0                      mqqqqqqqqqqqqqqqqqqj")?;
    line(
        out,
        "\x1b(BDEC Technical via G1/SO: \x1b)> \x0Eabc\x0F\x1b(B",
    )?;
    line(out, "\x1b.AISO Latin-1 via G2/SS2: \x1bN!\x1b(B")?;
    line(out, "\x1b/BDEC Supplemental via G3/SS3: \x1bO0\x1b(B")?;
    line(
        out,
        "NRCM British pound sign: \x1b[?42h\x1b(A#\x1b(B\x1b[?42l",
    )?;
    out.write_all(b"\r\n8-bit text mode GR sample: \x1b%@\xa1\xb0\x1b%G")?;
    out.flush()?;
    line(out, "")?;
    Ok(())
}

fn run_drcs_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "DRCS Soft Characters")?;
    write!(out, "{}", normalized_drcs_script(DRCS_SAMPLE_SCRIPT))?;
    Ok(())
}

fn normalized_drcs_script(script: &str) -> String {
    script
        .replace('\u{0090}', "\x1bP")
        .replace('\u{009c}', "\x1b\\")
        .replace('\n', "\r\n")
}

fn run_line_attrs_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "DEC Line Attributes")?;
    line(out, "Single-width line")?;
    write!(out, "\x1b#6Double-width line centered by the terminal")?;
    write!(out, "\r\n\x1b#3Double-width and double-height top")?;
    write!(out, "\r\n\x1b#4Double-width and double-height bottom")?;
    write!(out, "\r\n\x1b#5Back to single width")?;
    out.flush()?;
    Ok(())
}

fn run_rectangles_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "VT420 Rectangular Operations")?;
    line(
        out,
        "Building a box with DECERA / DECFRA / DECCRA / DECSERA / DECCARA / DECRARA.",
    )?;
    write!(out, "\x1b[2*x")?;
    write!(out, "\x1b#8")?;
    write!(out, "\x1b[5;5;14;30$z")?;
    write!(out, "\x1b[5;5;14;30;35$x")?;
    out.write_all(b"\x1b[6;6;13;29${")?;
    write!(out, "\x1b[5;5;14;30;7$r")?;
    write!(out, "\x1b[6;6;13;29;7$t")?;
    write!(out, "\x1b[5;32;14;57;1;1;10;5;1$v")?;
    write!(out, "\x1b[1*x\x1b[5;32;14;57;1$r")?;
    out.flush()?;
    Ok(())
}

fn run_vt525_color_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "VT525 Color Controls")?;
    line(
        out,
        "Assign normal text to white-on-black and load a dim blue cloud color.",
    )?;
    write!(out, "\x1b[1;7;0,|")?;
    write!(out, "\x1bP2$p0;2;12;12;12/4;2;33;33;44/7;2;90;90;90\x1b\\")?;
    line(out, "Normal text under DECAC.")?;
    out.write_all(b"\x1b[1;4;0,}")?;
    out.write_all(b"\x1b[3){")?;
    line(out, "\x1b[1mAlternate-color bold text\x1b[0m")?;
    out.write_all(b"\x1b[4;2;1,}")?;
    line(out, "\x1b[4mAlternate-color underline text\x1b[0m")?;
    write!(out, "\x1b[?114l\x1b[?115l\x1b[?116h\x1b[?117h")?;
    line(
        out,
        "DECATCUM/DECATCBM/DECBBSM/DECECM toggled for this row.",
    )?;
    Ok(())
}

fn run_status_line_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Status Line")?;
    line(out, "Switching to indicator line first.")?;
    write!(out, "\x1b[1$~")?;
    line(
        out,
        "The bottom row should now be emulator-owned indicator content.",
    )?;
    blank(out)?;
    line(
        out,
        "Switching to host-writable status line and routing output there.",
    )?;
    write!(out, "\x1b[2$~")?;
    out.write_all(b"\x1b[1$}STATUS > selftest41 > host-writable demo")?;
    out.write_all(b"\x1b[0$}")?;
    line(
        out,
        "Main display remains separate from the bottom status row.",
    )?;
    Ok(())
}

fn run_macro_demo(
    out: &mut impl Write,
    capabilities: &CapabilityReport,
) -> io::Result<()> {
    heading(out, "VT420 Macros")?;
    if !capabilities.features.contains(&32) {
        line(out, "Macros are not currently advertised in DA1.")?;
        line(
            out,
            "This usually means the terminal denied DECDMAC/DECINVM for the current program.",
        )?;
        return Ok(());
    }
    line(
        out,
        "Defining macro 1 to print a short status line, then invoking it.",
    )?;
    write!(out, "\x1bP1;0;0!zMacro path: DECDMAC works here.\x1b\\")?;
    write!(out, "\x1b[1*z")?;
    Ok(())
}

fn run_page_demo(
    out: &mut impl Write,
    read_reply: &mut ReadReplyFn<'_>,
) -> io::Result<()> {
    heading(out, "VT420 Page Memory & Geometry")?;
    line(
        out,
        "Creating page memory, writing to page 2, then copying back to page 1.",
    )?;
    write!(out, "\x1b[72t")?;
    write!(out, "\x1b[2U\x1b[1;1HThis text is on page 2.")?;
    write!(out, "\x1b[1V\x1b[1;1HBack on page 1.")?;
    write!(out, "\x1b[1;1;1;18;2;3;1;1$v")?;
    blank(out)?;
    query_and_print(
        out,
        read_reply,
        "  DECXCPR",
        b"\x1b[?6n",
        Duration::from_millis(200),
    )?;
    Ok(())
}

fn run_vt52_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "VT52 / Conformance")?;
    line(
        out,
        "Entering VT52 mode, drawing one line, then returning to ANSI and 8-bit C1.",
    )?;
    write!(out, "\x1b[?2l")?;
    write!(out, "\x1bHVT52 home\x1bY#$cursor addr")?;
    write!(out, "\x1b<\r\n\x1b[64;2\"p\x1b G")?;
    line(
        out,
        "Returned to ANSI level 4 with 8-bit C1 negotiation selected.",
    )?;
    Ok(())
}

use std::io;
use std::io::Write;
use std::thread::sleep;
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
    SixelLifecycle,
    HyperlinkLifecycle,
    LineAttrs,
    Tabs,
    CursorMarginsEdit,
    EraseProtection,
    PasteFocus,
    ScrollWrap,
    Rectangles,
    AltScreen,
    OscShell,
    Vt525Color,
    StatusLine,
    Macros,
    PageMemory,
    Vt52,
}

const DRCS_SAMPLE_SCRIPT: &str = include_str!("../resources/icon.drcs");
const SIXEL_SAMPLE: &str = include_str!("../resources/icon.six");
const DEMO_STEP_PAUSE: Duration = Duration::from_millis(1200);

type ReadReplyFn<'a> = dyn FnMut(Duration) -> io::Result<Vec<u8>> + 'a;

pub fn catalog() -> Vec<Demo> {
    let mut demos = vec![
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
            title: "Sixel Lifecycle",
            summary: "Draws a bundled sixel image, then exercises clear, alt-screen, and reset \
                      cleanup.",
            detail: "Exercises whether sixel images survive the transitions they should and get \
                     dropped by the ones that are supposed to clear them.",
            id: DemoId::SixelLifecycle,
        },
        Demo {
            title: "Hyperlink Lifecycle",
            summary: "Opens OSC 8 links, then exercises clear, alt-screen, and reset cleanup.",
            detail: "Exercises whether hyperlink spans survive the transitions they should and \
                     get dropped when their line or screen is cleared.",
            id: DemoId::HyperlinkLifecycle,
        },
        Demo {
            title: "DEC Line Attributes",
            summary: "Single-width, double-width, and double-height rows.",
            detail: "Exercises DECSWL, DECDWL, and the double-height top/bottom pair.",
            id: DemoId::LineAttrs,
        },
        Demo {
            title: "Tabs",
            summary: "HT, HTS, CHT, CBT, TBC, and visible tab-stop placement/removal.",
            detail: "Exercises hardware tab-stop creation, clearing, and movement so it is easy \
                     to see where the terminal believes stops exist.",
            id: DemoId::Tabs,
        },
        Demo {
            title: "Cursor, Margins & Edit",
            summary: "DECOM, DECLRMM, tab stops, IRM, ICH/DCH, and IL/DL.",
            detail: "Exercises cursor-addressing modes, left/right margins, hardware tab stops, \
                     insert mode, and editing controls on a live grid.",
            id: DemoId::CursorMarginsEdit,
        },
        Demo {
            title: "Erase & Protection",
            summary: "ED/EL, DECSED/DECSEL, and DECSCA protected text semantics.",
            detail: "Shows the difference between normal erase and selective erase by mixing \
                     protected and unprotected text on the same rows.",
            id: DemoId::EraseProtection,
        },
        Demo {
            title: "Paste & Focus",
            summary: "Bracketed paste and focus in/out reporting, with raw byte capture.",
            detail: "Enables focus reporting and bracketed paste, then shows the exact bytes \
                     received from the terminal as you paste or switch focus.",
            id: DemoId::PasteFocus,
        },
        Demo {
            title: "Scroll & Wrap",
            summary: "IND, NEL, RI, DECAWM on/off, REP, and scroll-region movement.",
            detail: "Exercises the core scrolling and wrapping behaviors that many fullscreen \
                     apps depend on, with visible intermediate states.",
            id: DemoId::ScrollWrap,
        },
        Demo {
            title: "Rectangular Ops",
            summary: "DECERA, DECFRA, DECCRA, DECSERA, DECCARA, DECRARA, and DECSACE.",
            detail: "Exercises the full VT420 rectangular-area family with visible mutations.",
            id: DemoId::Rectangles,
        },
        Demo {
            title: "Primary vs Alt Screen",
            summary: "Uses ?1049 to switch screens and shows state surviving the round-trip.",
            detail: "Exercises primary/alternate screen separation, saved cursor behavior, and \
                     visible content restoration.",
            id: DemoId::AltScreen,
        },
        Demo {
            title: "OSC & Shell Integration",
            summary: "OSC 0/2 titles, OSC 7 cwd tracking, OSC 8 hyperlinks, and OSC 52 query.",
            detail: "Exercises the OSC surfaces term41 supports without depending on the GUI \
                     codepath.",
            id: DemoId::OscShell,
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
    ];
    demos.sort_by_key(|demo| demo.title);
    demos
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
        DemoId::SixelLifecycle => run_sixel_lifecycle_demo(out),
        DemoId::HyperlinkLifecycle => run_hyperlink_lifecycle_demo(out),
        DemoId::LineAttrs => run_line_attrs_demo(out),
        DemoId::Tabs => run_tabs_demo(out),
        DemoId::CursorMarginsEdit => run_cursor_margins_edit_demo(out),
        DemoId::EraseProtection => run_erase_protection_demo(out),
        DemoId::PasteFocus => run_paste_focus_placeholder(out),
        DemoId::ScrollWrap => run_scroll_wrap_demo(out),
        DemoId::Rectangles => run_rectangles_demo(out),
        DemoId::AltScreen => run_alt_screen_demo(out),
        DemoId::OscShell => run_osc_shell_demo(out, read_reply),
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

pub(crate) fn clear_visible_screen(out: &mut impl Write) -> io::Result<()> {
    write!(out, "\x1b[0m\x1b[2J\x1b[H")
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

fn present_step(out: &mut impl Write) -> io::Result<()> {
    out.flush()?;
    sleep(DEMO_STEP_PAUSE);
    Ok(())
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

pub(crate) fn format_bytes(bytes: &[u8]) -> String {
    format_reply(bytes)
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
    present_step(out)?;
    blank(out)?;
    line(out, "Now issuing DECSR with confirmation parameter 41.")?;
    out.write_all(b"\x1b[41+p")?;
    present_step(out)?;
    let reply = read_reply(Duration::from_millis(200))?;
    clear_screen(out)?;
    heading(out, "Reset & State Restore")?;
    line(out, "DECSR reset the terminal state and returned:")?;
    line(out, &format!("  {}", format_reply(&reply)))?;
    present_step(out)?;
    blank(out)?;
    line(out, "Issuing DECTST power-up self-test form next.")?;
    out.write_all(b"\x1b[4;1y")?;
    present_step(out)?;
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

fn run_sixel_lifecycle_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Sixel Lifecycle")?;
    line(
        out,
        "Drawing the bundled sixel on the primary screen first.",
    )?;
    write!(out, "\x1b[4;4H{sixel}", sixel = SIXEL_SAMPLE)?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Switching to the alternate screen. Returning should restore the primary-screen image.",
    )?;
    present_step(out)?;
    write!(out, "\x1b[?1049h")?;
    write!(out, "\x1b[2J\x1b[H")?;
    line(
        out,
        "The primary-screen sixel should be hidden while alt-screen content is active.",
    )?;
    present_step(out)?;
    write!(out, "\x1b[?1049l")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "ED 2 should clear both the visible text and the sixel image.",
    )?;
    present_step(out)?;
    clear_visible_screen(out)?;
    heading(out, "Sixel Lifecycle")?;
    line(
        out,
        "If ED 2 worked correctly, the earlier sixel should now be gone.",
    )?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Redrawing the image now so RIS can prove it also drops sixels.",
    )?;
    write!(out, "\x1b[7;4H{sixel}", sixel = SIXEL_SAMPLE)?;
    present_step(out)?;
    write!(out, "\x1bc")?;
    heading(out, "Sixel Lifecycle")?;
    line(
        out,
        "RIS completed. The sixel should be gone and the terminal should be back in default state.",
    )?;
    out.flush()?;
    Ok(())
}

fn run_hyperlink_lifecycle_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Hyperlink Lifecycle")?;
    line(
        out,
        "Opening an OSC 8 hyperlink span on the primary screen first.",
    )?;
    write!(out, "\x1b[4;4H\x1b]8;;https://example.com/primary\x07")?;
    write!(out, "primary-screen link")?;
    write!(out, "\x1b]8;;\x07")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Switching to the alternate screen. Returning should restore the primary-screen link.",
    )?;
    present_step(out)?;
    write!(out, "\x1b[?1049h")?;
    write!(out, "\x1b[2J\x1b[H")?;
    write!(out, "\x1b]8;;https://example.com/alt\x07")?;
    line(out, "alternate-screen link")?;
    write!(out, "\x1b]8;;\x07")?;
    line(
        out,
        "The primary-screen link should be hidden while the alt-screen link is visible.",
    )?;
    present_step(out)?;
    write!(out, "\x1b[?1049l")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "EL 2 should clear the line and drop the hyperlink span on it.",
    )?;
    present_step(out)?;
    write!(out, "\x1b[4;1H\x1b[2K")?;
    line(
        out,
        "If EL 2 worked correctly, the earlier primary-screen hyperlink should now be gone.",
    )?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Redrawing a link now so RIS can prove it also drops hyperlink state.",
    )?;
    write!(out, "\x1b[9;4H\x1b]8;;https://example.com/reset\x07")?;
    write!(out, "reset-test link")?;
    write!(out, "\x1b]8;;\x07")?;
    present_step(out)?;
    write!(out, "\x1bc")?;
    heading(out, "Hyperlink Lifecycle")?;
    line(
        out,
        "RIS completed. The hyperlink should be gone and the terminal should be back in default \
         state.",
    )?;
    out.flush()?;
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
    write!(out, "Double-width line centered by the terminal\x1b#6")?;
    write!(out, "\r\nThis line should appear double-height\x1b#3")?;
    write!(out, "\r\nThis line should appear double-height\x1b#4")?;
    write!(out, "\r\nBack to single width\x1b#5")?;
    out.flush()?;
    Ok(())
}

fn run_tabs_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Tabs")?;
    line(
        out,
        "Starting from the default 8-column stops, then placing and clearing custom ones.",
    )?;
    line(
        out,
        "Ruler:    1.......2.......3.......4.......5.......6.......7.......8",
    )?;
    write!(out, "\x1b[3;1Hdefault:\tA\tB\tC")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Clearing all tab stops and setting custom stops at columns 12, 20, and 28.",
    )?;
    write!(out, "\x1b[3g")?;
    write!(out, "\x1b[6;12H\x1bH\x1b[6;20H\x1bH\x1b[6;28H\x1bH")?;
    write!(out, "\x1b[7;1Hcustom:\tA\tB\tC")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "CHT should advance to the next stop; CBT should move back to the previous one.",
    )?;
    write!(out, "\x1b[9;1Horigin")?;
    write!(out, "\x1b[9;1H\x1b[1Iafter CHT")?;
    write!(out, "\x1b[9;20H\x1b[1Zafter CBT")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "TBC should remove just the current stop first, then all stops.",
    )?;
    write!(out, "\x1b[6;20H\x1b[0g")?;
    write!(out, "\x1b[11;1Hafter TBC(0):\tA\tB\tC")?;
    present_step(out)?;

    write!(out, "\x1b[3g")?;
    write!(out, "\x1b[12;1Hafter TBC(3):\tA\tB\tC")?;
    out.flush()?;
    Ok(())
}

fn run_cursor_margins_edit_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Cursor, Margins & Edit Controls")?;
    line(out, "Setting tab stop at column 12, then using CHT/CBT.")?;
    write!(out, "\x1b[1;12H\x1bH")?;
    write!(out, "\x1b[2;1Htab:\t<- HTS target")?;
    write!(out, "\x1b[2;18H\x1b[1Z<- CBT")?;
    present_step(out)?;
    blank(out)?;
    line(out, "Testing insert mode and character insert/delete.")?;
    write!(out, "\x1b[4;1Habcdef")?;
    write!(out, "\x1b[4;3H\x1b[4hX\x1b[4l")?;
    write!(out, "\x1b[5;1H123456")?;
    write!(out, "\x1b[5;3H\x1b[2@<<\x1b[2P")?;
    present_step(out)?;
    blank(out)?;
    line(
        out,
        "Testing scroll region, origin mode, and left/right margins.",
    )?;
    write!(out, "\x1b[7;16r\x1b[?6h")?;
    write!(out, "\x1b[1;1Horigin-relative top of region")?;
    write!(out, "\x1b[?69h\x1b[10;30s")?;
    write!(out, "\x1b[9;1Hleft/right margin mode active")?;
    write!(out, "\x1b[10;1Hinside margins only")?;
    write!(out, "\x1b[11;1Hline a\r\nline b\r\nline c")?;
    write!(out, "\x1b[11;1H\x1b[1Linserted line")?;
    write!(out, "\x1b[13;1H\x1b[1M")?;
    present_step(out)?;
    write!(out, "\x1b[r\x1b[?6l\x1b[?69l")?;
    out.flush()?;
    Ok(())
}

fn run_erase_protection_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Erase & Protection")?;
    line(
        out,
        "Building rows with mixed protected/unprotected text, then applying erase variants.",
    )?;
    write!(out, "\x1b[4;1Hleft ")?;
    write!(out, "\x1b[1\"qPROTECTED\x1b[0\"q right")?;
    write!(out, "\x1b[5;1Hkeep ")?;
    write!(out, "\x1b[1\"qSAFE\x1b[0\"q erase-me")?;
    write!(out, "\x1b[6;1Hfull-row ")?;
    write!(out, "\x1b[1\"qGUARD\x1b[0\"q tail")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Normal EL/ED should erase both protected and unprotected text.",
    )?;
    write!(out, "\x1b[5;1H\x1b[0K")?;
    write!(out, "\x1b[6;1H\x1b[2K")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Rebuilding rows, then selective erase should leave only the DECSCA-protected cells \
         behind.",
    )?;
    write!(out, "\x1b[4;1Hleft ")?;
    write!(out, "\x1b[1\"qPROTECTED\x1b[0\"q right")?;
    write!(out, "\x1b[5;1Hkeep ")?;
    write!(out, "\x1b[1\"qSAFE\x1b[0\"q erase-me")?;
    write!(out, "\x1b[6;1Hfull-row ")?;
    write!(out, "\x1b[1\"qGUARD\x1b[0\"q tail")?;
    present_step(out)?;

    write!(out, "\x1b[4;1H\x1b[?2K")?;
    write!(out, "\x1b[5;1H\x1b[?0K")?;
    write!(out, "\x1b[6;1H\x1b[?2K")?;
    write!(out, "\x1b[5;12H\x1b[?1K")?;
    write!(out, "\x1b[5;1H")?;
    out.write_all(b"\x1b[?1J")?;
    out.flush()?;
    Ok(())
}

fn run_paste_focus_placeholder(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Paste & Focus")?;
    line(
        out,
        "This demo is handled by terminal_io because it needs a live raw-byte capture loop.",
    )?;
    Ok(())
}

fn run_scroll_wrap_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Scroll & Wrap")?;
    line(
        out,
        "Filling the screen tail, then using IND / NEL / RI and autowrap transitions.",
    )?;
    write!(out, "\x1b[6;1Hrow 6 seed")?;
    write!(out, "\x1b[7;1Hrow 7 seed")?;
    write!(out, "\x1b[8;1Hrow 8 seed")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "IND should move down in the current column and scroll at the bottom margin.",
    )?;
    write!(out, "\x1b[8;5H\x1bDIND")?;
    present_step(out)?;

    line(out, "")?;
    line(out, "NEL should move to the next line at column 1.")?;
    write!(out, "\x1b[7;12H\x1bENEL")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "RI should move upward and scroll backward at the top margin.",
    )?;
    write!(out, "\x1b[6;20H\x1bMRI")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Autowrap on: a run at the right edge should continue on the next line.",
    )?;
    write!(out, "\x1b[?7h")?;
    write!(out, "\x1b[12;36HWRAP-ON-ABCDE")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Autowrap off: the rightmost column should be overwritten instead of wrapping.",
    )?;
    write!(out, "\x1b[?7l")?;
    write!(out, "\x1b[14;36HWRAP-OFF-ABCDE")?;
    write!(out, "\x1b[?7h")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "REP should repeat the last graphic character across the row.",
    )?;
    write!(out, "\x1b[16;1H*\x1b[10b")?;
    present_step(out)?;

    line(out, "")?;
    line(
        out,
        "Now restricting the scroll region and exercising IND/RI inside it.",
    )?;
    write!(out, "\x1b[18;22r")?;
    write!(out, "\x1b[18;1Hregion top")?;
    write!(out, "\x1b[19;1Hregion mid")?;
    write!(out, "\x1b[20;1Hregion low")?;
    write!(out, "\x1b[21;1Hregion bot")?;
    present_step(out)?;

    write!(out, "\x1b[22;12H\x1bDinside region via IND")?;
    present_step(out)?;
    write!(out, "\x1b[18;22H\x1bMinside region via RI")?;
    write!(out, "\x1b[r")?;
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
    present_step(out)?;
    write!(out, "\x1b[5;5;14;30$z")?;
    write!(out, "\x1b[5;5;14;30;35$x")?;
    present_step(out)?;
    out.write_all(b"\x1b[6;6;13;29${")?;
    write!(out, "\x1b[5;5;14;30;7$r")?;
    write!(out, "\x1b[6;6;13;29;7$t")?;
    present_step(out)?;
    write!(out, "\x1b[5;32;14;57;1;1;10;5;1$v")?;
    write!(out, "\x1b[1*x\x1b[5;32;14;57;1$r")?;
    out.flush()?;
    Ok(())
}

fn run_alt_screen_demo(out: &mut impl Write) -> io::Result<()> {
    heading(out, "Primary vs Alternate Screen")?;
    line(
        out,
        "This text is on the primary screen before entering ?1049.",
    )?;
    write!(out, "\x1b7")?;
    write!(out, "\r\nSwitching to alternate screen now...\r\n")?;
    present_step(out)?;
    write!(out, "\x1b[?1049h")?;
    write!(out, "\x1b[2J\x1b[H")?;
    line(
        out,
        "Alternate screen content should replace the primary surface.",
    )?;
    line(out, "Returning to primary should restore the earlier text.")?;
    present_step(out)?;
    write!(out, "\r\n\x1b[?1049l")?;
    write!(out, "\r\nBack on the primary screen after 1049l.")?;
    out.flush()?;
    Ok(())
}

fn run_osc_shell_demo(
    out: &mut impl Write,
    read_reply: &mut ReadReplyFn<'_>,
) -> io::Result<()> {
    heading(out, "OSC & Shell Integration")?;
    line(
        out,
        "Setting title via OSC 0 / OSC 2, then opening a hyperlink span.",
    )?;
    write!(out, "\x1b]0;selftest41 osc title\x07")?;
    write!(out, "\x1b]2;selftest41 osc icon+title\x07")?;
    write!(out, "\x1b]7;file://localhost/tmp/selftest41/demo\x07")?;
    write!(out, "\x1b]8;;https://vt100.net/\x07")?;
    line(out, "Clickable OSC 8 hyperlink to vt100.net")?;
    write!(out, "\x1b]8;;\x07")?;
    blank(out)?;
    line(out, "Querying OSC 52 clipboard readback support.")?;
    query_and_print(
        out,
        read_reply,
        "  OSC 52 ?",
        b"\x1b]52;c;?\x07",
        Duration::from_millis(200),
    )?;
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
    out.write_all(b"\x1b[1){")?;
    line(out, "\x1b[1mAlternate-color bold text\x1b[0m")?;
    out.write_all(b"\x1b[?114h")?;
    out.write_all(b"\x1b[4;2;1,}")?;
    line(out, "\x1b[4mAlternate-color underline text\x1b[0m")?;
    write!(out, "\x1b[?115l\x1b[?116h\x1b[?117h")?;
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
    present_step(out)?;
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
    write!(out, "\x1b[2 P\x1b[6;1HThis text is on page 2.")?;
    present_step(out)?;
    write!(out, "\x1b[1 P\x1b[8;1HBack on page 1.")?;
    present_step(out)?;
    write!(out, "\x1b[6;1;6;18;2;10;1;1$v")?;
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
    present_step(out)?;
    write!(out, "\x1b<\r\n\x1b[64;2\"p\x1b G")?;
    line(
        out,
        "Returned to ANSI level 4 with 8-bit C1 negotiation selected.",
    )?;
    Ok(())
}

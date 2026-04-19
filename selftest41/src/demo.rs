use std::io::Write;

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
    Sgr,
    Charset,
    Drcs,
    Rectangles,
    Vt525Color,
    StatusLine,
    Macros,
    PageMemory,
    Vt52,
}

pub fn catalog() -> Vec<Demo> {
    vec![
        Demo {
            title: "Identity & Queries",
            summary: "Probe DA1 and report the currently exposed terminal feature bits.",
            detail: "Exercises DA1 and shows the parsed identity information that drives the \
                     status bar.",
            id: DemoId::Identity,
        },
        Demo {
            title: "SGR Styles",
            summary: "Bold, italic, underline variants, reverse, conceal, and blink.",
            detail: "Exercises the styled text surface with 16-color, 256-color, truecolor, \
                     underline styles, and blink.",
            id: DemoId::Sgr,
        },
        Demo {
            title: "Charset Engine",
            summary: "DEC Special Graphics, supplemental sets, and locking shifts.",
            detail: "Exercises G0/G1 designation and shows line-drawing through DEC Special \
                     Graphics.",
            id: DemoId::Charset,
        },
        Demo {
            title: "DRCS Soft Chars",
            summary: "Downloads a tiny soft character set and prints it.",
            detail: "Exercises DECDLD plus SCS designation for downloadable soft characters.",
            id: DemoId::Drcs,
        },
        Demo {
            title: "Rectangular Ops",
            summary: "DECCARA, DECRARA, and DECSERA on a live character grid.",
            detail: "Exercises VT420 rectangular-area completion features with visible mutations.",
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
            summary: "Host-writable status line plus visible separation from main display.",
            detail: "Exercises DECSSDT and DECSASD by writing to the status line and then \
                     restoring normal output routing.",
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
            title: "Page Memory",
            summary: "Populate multiple pages and copy between them.",
            detail: "Exercises VT420 page-memory commands without changing the outer window \
                     geometry.",
            id: DemoId::PageMemory,
        },
        Demo {
            title: "VT52 / Conformance",
            summary: "Switch temporarily into VT52 and back to ANSI mode.",
            detail: "Exercises VT52 mode entry, a few VT52 cursor sequences, and return to ANSI \
                     mode.",
            id: DemoId::Vt52,
        },
    ]
}

pub fn run_demo(
    out: &mut impl Write,
    demo: DemoId,
    capabilities: &CapabilityReport,
) -> std::io::Result<()> {
    clear_screen(out)?;
    match demo {
        DemoId::Identity => run_identity_demo(out, capabilities),
        DemoId::Sgr => run_sgr_demo(out),
        DemoId::Charset => run_charset_demo(out),
        DemoId::Drcs => run_drcs_demo(out),
        DemoId::Rectangles => run_rectangles_demo(out),
        DemoId::Vt525Color => run_vt525_color_demo(out),
        DemoId::StatusLine => run_status_line_demo(out),
        DemoId::Macros => run_macro_demo(out, capabilities),
        DemoId::PageMemory => run_page_demo(out),
        DemoId::Vt52 => run_vt52_demo(out),
    }?;
    write!(
        out,
        "\r\n\r\n\x1b[0mPress any key to return to selftest41.\x1b[0m"
    )?;
    out.flush()
}

fn clear_screen(out: &mut impl Write) -> std::io::Result<()> {
    write!(out, "\x1b[0m\x1b[2J\x1b[H\x1b[?25l")
}

fn heading(
    out: &mut impl Write,
    title: &str,
) -> std::io::Result<()> {
    write!(out, "\x1b[1m{}\x1b[0m\r\n\r\n", title)
}

fn line(
    out: &mut impl Write,
    text: &str,
) -> std::io::Result<()> {
    write!(out, "{text}\r\n")
}

fn run_identity_demo(
    out: &mut impl Write,
    capabilities: &CapabilityReport,
) -> std::io::Result<()> {
    heading(out, "Identity & Queries")?;
    if let Some(raw) = &capabilities.raw_reply {
        line(out, &format!("Captured DA1 reply: {}", raw.escape_default()))?;
    } else {
        line(out, "No DA1 reply captured.")?;
    }
    line(out, "Visible status bar tracks this same parsed capability set.")?;
    line(out, "")?;
    line(out, "Query sequences used:")?;
    line(out, "  CSI c               DA1")?;
    line(out, "  DCS $ q 1,| ST      DECRQSS / DECAC")?;
    line(out, "  CSI 2 ; 2 $ u       DECCTR report")?;
    Ok(())
}

fn run_sgr_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "SGR Styles")?;
    line(
        out,
        "\x1b[1mBold\x1b[0m  \x1b[3mItalic\x1b[0m  \x1b[7mReverse\x1b[0m  \x1b[8mConceal\x1b[0m",
    )?;
    line(out, "\x1b[4mSingle underline\x1b[0m")?;
    line(out, "\x1b[4:2mDouble underline\x1b[0m")?;
    line(out, "\x1b[4:3mCurly underline\x1b[0m")?;
    line(out, "\x1b[4:4mDotted underline\x1b[0m")?;
    line(out, "\x1b[4:5mDashed underline\x1b[0m")?;
    line(out, "\x1b[38;5;202m256-color foreground\x1b[0m")?;
    line(
        out,
        "\x1b[48;2;20;40;80m\x1b[38;2;240;240;255mTruecolor background\x1b[0m",
    )?;
    line(out, "\x1b[5mBlink\x1b[0m  \x1b[6mRapid blink\x1b[0m")?;
    Ok(())
}

fn run_charset_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "Charset Engine")?;
    line(out, "ASCII:  +-- charset demo --+")?;
    line(out, "\x1b(0DEC Special Graphics: lqqqqqqqqqqqqqqqqk")?;
    line(out, "\x1b(0                     x locking shifts x")?;
    line(out, "\x1b(0                     mqqqqqqqqqqqqqqqqj")?;
    line(out, "\x1b(BBack to ASCII.")?;
    Ok(())
}

fn run_drcs_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "DRCS Soft Characters")?;
    line(out, "Downloading one simple 94-character DRCS glyph into G0.")?;
    write!(out, "\x1bP1;1;1;6;0;2;16;0{{ @~~~~~~\x1b\\")?;
    write!(out, "\x1b( @")?;
    line(out, "DRCS sample: ! ! !")?;
    write!(out, "\x1b(B")?;
    Ok(())
}

fn run_rectangles_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "VT420 Rectangular Operations")?;
    line(out, "Initial grid:")?;
    line(out, "abcdefghijklmnop")?;
    line(out, "qrstuvwxyzABCDEF")?;
    line(out, "GHIJKLMNOPQRSTUV")?;
    line(out, "\x1b[2;3H\x1b[1;3;2;8;1$r")?;
    line(out, "")?;
    line(out, "The terminal should have applied DECCARA to a rectangle in the middle.")?;
    line(out, "A follow-up reverse-attributes pass runs next.")?;
    write!(out, "\x1b[1;3;2;8;1$t")?;
    Ok(())
}

fn run_vt525_color_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "VT525 Color Controls")?;
    line(out, "Assign normal text to white-on-black and load a dim blue cloud color.")?;
    write!(out, "\x1b[1;7;0,|")?;
    write!(out, "\x1bP2$p0;2;12;12;12/4;2;33;33;44/7;2;90;90;90\x1b\\")?;
    line(out, "Normal text under DECAC.")?;
    write!(out, "\x1b[1;4;0,}}")?;
    write!(out, "\x1b[1){{")?;
    line(out, "\x1b[1mAlternate-color bold text\x1b[0m")?;
    write!(out, "\x1b[3){{\x1b[0m")?;
    Ok(())
}

fn run_status_line_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "Status Line")?;
    line(out, "Enabling the host-writable status line for this transient demo.")?;
    write!(out, "\x1b[2$~")?;
    write!(out, "\x1b[1$}}STATUS > selftest41 > host-writable demo")?;
    write!(out, "\x1b[0$}}")?;
    line(out, "")?;
    line(out, "Main display stays visually separate from the bottom status line.")?;
    Ok(())
}

fn run_macro_demo(
    out: &mut impl Write,
    capabilities: &CapabilityReport,
) -> std::io::Result<()> {
    heading(out, "VT420 Macros")?;
    if !capabilities.features.contains(&32) {
        line(out, "Macros are not currently advertised in DA1.")?;
        line(
            out,
            "This usually means the terminal denied DECDMAC/DECINVM for the current program.",
        )?;
        return Ok(());
    }
    line(out, "Defining macro 1 to print a short status line, then invoking it.")?;
    write!(out, "\x1bP1;0;0!zMacro path: DECDMAC works here.\x1b\\")?;
    write!(out, "\x1b[1*z")?;
    Ok(())
}

fn run_page_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "VT420 Page Memory")?;
    line(out, "Building page memory and copying between pages without changing outer geometry.")?;
    write!(out, "\x1b[?64l")?;
    write!(out, "\x1b[2 P")?;
    line(out, "This text is on page 2.")?;
    write!(out, "\x1b[1 P")?;
    line(out, "Back on page 1.")?;
    Ok(())
}

fn run_vt52_demo(out: &mut impl Write) -> std::io::Result<()> {
    heading(out, "VT52 / Conformance")?;
    line(out, "Entering VT52 mode, drawing one line, then returning to ANSI.")?;
    write!(out, "\x1b[?2l")?;
    write!(out, "\x1bHVT52 home\x1bY#$cursor addr")?;
    write!(out, "\x1b<")?;
    line(out, "")?;
    line(out, "Returned to ANSI mode.")?;
    Ok(())
}

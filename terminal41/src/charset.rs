use smol_str::SmolStr;

const SUBSTITUTE: char = '\u{2426}';

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicSetSlot {
    G0,
    G1,
    G2,
    G3,
}

impl GraphicSetSlot {
    fn index(self) -> usize {
        match self {
            Self::G0 => 0,
            Self::G1 => 1,
            Self::G2 => 2,
            Self::G3 => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NationalReplacementSet {
    British,
    Dutch,
    Finnish,
    French,
    FrenchCanadian,
    German,
    Italian,
    NorwegianDanish,
    Portuguese,
    Spanish,
    Swedish,
    Swiss,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPreferredSupplementalSet {
    DecSupplemental,
    IsoLatin1Supplemental,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterSet {
    Ascii,
    DecSpecialGraphics,
    DecTechnical,
    DecSupplemental,
    IsoLatin1Supplemental,
    UserPreferredSupplemental,
    NationalReplacement(NationalReplacementSet),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharsetState {
    designated: [CharacterSet; 4],
    gl: GraphicSetSlot,
    gr: GraphicSetSlot,
    pub single_shift: Option<GraphicSetSlot>,
}

impl CharsetState {
    pub fn new() -> Self {
        Self {
            designated: [
                CharacterSet::Ascii,
                CharacterSet::Ascii,
                CharacterSet::DecSupplemental,
                CharacterSet::DecSupplemental,
            ],
            gl: GraphicSetSlot::G0,
            gr: GraphicSetSlot::G2,
            single_shift: None,
        }
    }

    pub fn designate(
        &mut self,
        slot: GraphicSetSlot,
        charset: CharacterSet,
    ) {
        self.designated[slot.index()] = charset;
    }

    pub fn designated(
        &self,
        slot: GraphicSetSlot,
    ) -> CharacterSet {
        self.designated[slot.index()]
    }

    pub fn set_gl(
        &mut self,
        slot: GraphicSetSlot,
    ) {
        self.gl = slot;
    }

    pub fn set_gr(
        &mut self,
        slot: GraphicSetSlot,
    ) {
        self.gr = slot;
    }

    pub fn gl_slot(&self) -> GraphicSetSlot {
        self.gl
    }

    pub fn gr_slot(&self) -> GraphicSetSlot {
        self.gr
    }

    pub fn gl_charset(&self) -> CharacterSet {
        self.designated(self.gl)
    }

    pub fn gr_charset(&self) -> CharacterSet {
        self.designated(self.gr)
    }

    pub fn take_single_shift_charset(&mut self) -> Option<CharacterSet> {
        self.single_shift.take().map(|slot| self.designated(slot))
    }
}

impl Default for CharsetState {
    fn default() -> Self {
        Self::new()
    }
}

pub fn parse_designation(
    intermediates: &[u8],
    final_byte: u8,
) -> Option<(GraphicSetSlot, CharacterSet)> {
    let (&slot_byte, rest) = intermediates.split_first()?;
    let slot = match slot_byte {
        b'(' => GraphicSetSlot::G0,
        b')' | b'-' => GraphicSetSlot::G1,
        b'*' | b'.' => GraphicSetSlot::G2,
        b'+' | b'/' => GraphicSetSlot::G3,
        _ => return None,
    };
    let is_96 = matches!(slot_byte, b'-' | b'.' | b'/');
    let prefix = rest.first().copied();
    let charset = match (is_96, prefix, final_byte) {
        (false, None, b'B') => CharacterSet::Ascii,
        (false, None, b'0') => CharacterSet::DecSpecialGraphics,
        (false, None, b'>') => CharacterSet::DecTechnical,
        (false, None, b'<') => CharacterSet::UserPreferredSupplemental,
        (false, None, b'A') => CharacterSet::NationalReplacement(NationalReplacementSet::British),
        (false, None, b'4') => CharacterSet::NationalReplacement(NationalReplacementSet::Dutch),
        (false, None, b'5' | b'C') => {
            CharacterSet::NationalReplacement(NationalReplacementSet::Finnish)
        }
        (false, None, b'R') => CharacterSet::NationalReplacement(NationalReplacementSet::French),
        (false, None, b'Q' | b'9') => {
            CharacterSet::NationalReplacement(NationalReplacementSet::FrenchCanadian)
        }
        (false, None, b'K') => CharacterSet::NationalReplacement(NationalReplacementSet::German),
        (false, None, b'Y') => CharacterSet::NationalReplacement(NationalReplacementSet::Italian),
        (false, None, b'`' | b'E' | b'6') => {
            CharacterSet::NationalReplacement(NationalReplacementSet::NorwegianDanish)
        }
        (false, None, b'Z') => CharacterSet::NationalReplacement(NationalReplacementSet::Spanish),
        (false, None, b'7' | b'H') => {
            CharacterSet::NationalReplacement(NationalReplacementSet::Swedish)
        }
        (false, None, b'=') => CharacterSet::NationalReplacement(NationalReplacementSet::Swiss),
        (false, Some(b'%'), b'5') => CharacterSet::DecSupplemental,
        (false, Some(b'%'), b'6') => {
            CharacterSet::NationalReplacement(NationalReplacementSet::Portuguese)
        }
        (true, None, b'A') => CharacterSet::IsoLatin1Supplemental,
        _ => return None,
    };
    Some((slot, charset))
}

pub fn gl_charset_requires_translation(
    state: &CharsetState,
    nrc_mode: bool,
) -> bool {
    if state.single_shift.is_some() {
        return true;
    }
    charset_requires_translation(state.gl_charset(), nrc_mode)
}

pub fn charset_requires_translation(
    charset: CharacterSet,
    nrc_mode: bool,
) -> bool {
    match charset {
        CharacterSet::Ascii => false,
        CharacterSet::NationalReplacement(_) => nrc_mode,
        CharacterSet::DecSpecialGraphics
        | CharacterSet::DecTechnical
        | CharacterSet::DecSupplemental
        | CharacterSet::IsoLatin1Supplemental
        | CharacterSet::UserPreferredSupplemental => true,
    }
}

pub fn translate_ascii_byte(
    byte: u8,
    charset: CharacterSet,
    nrc_mode: bool,
    upss: UserPreferredSupplementalSet,
) -> Option<SmolStr> {
    match charset {
        CharacterSet::Ascii => None,
        CharacterSet::DecSpecialGraphics => translate_dec_special_graphics(byte),
        CharacterSet::DecTechnical => translate_dec_technical(byte),
        CharacterSet::DecSupplemental => translate_dec_supplemental(byte),
        CharacterSet::IsoLatin1Supplemental => translate_iso_latin1_supplemental(byte),
        CharacterSet::UserPreferredSupplemental => match upss {
            UserPreferredSupplementalSet::DecSupplemental => translate_dec_supplemental(byte),
            UserPreferredSupplementalSet::IsoLatin1Supplemental => {
                translate_iso_latin1_supplemental(byte)
            }
        },
        CharacterSet::NationalReplacement(set) => {
            if !nrc_mode {
                return None;
            }
            translate_nrc_byte(byte, set)
        }
    }
}

pub fn parse_upss_assignment(
    ps: u16,
    payload: &[u8],
) -> Option<UserPreferredSupplementalSet> {
    match (ps, payload) {
        (0, b"%5") => Some(UserPreferredSupplementalSet::DecSupplemental),
        (1, b"A") => Some(UserPreferredSupplementalSet::IsoLatin1Supplemental),
        _ => None,
    }
}

pub fn decaupss_report(upss: UserPreferredSupplementalSet) -> &'static str {
    match upss {
        UserPreferredSupplementalSet::DecSupplemental => "0!u%5",
        UserPreferredSupplementalSet::IsoLatin1Supplemental => "1!uA",
    }
}

fn translate_dec_special_graphics(byte: u8) -> Option<SmolStr> {
    let s = match byte {
        0x60 => "\u{25C6}",
        0x61 => "\u{2592}",
        0x62 => "\u{2409}",
        0x63 => "\u{240C}",
        0x64 => "\u{240D}",
        0x65 => "\u{240A}",
        0x66 => "\u{00B0}",
        0x67 => "\u{00B1}",
        0x68 => "\u{2424}",
        0x69 => "\u{240B}",
        0x6A => "\u{2518}",
        0x6B => "\u{2510}",
        0x6C => "\u{250C}",
        0x6D => "\u{2514}",
        0x6E => "\u{253C}",
        0x6F => "\u{23BA}",
        0x70 => "\u{23BB}",
        0x71 => "\u{2500}",
        0x72 => "\u{23BC}",
        0x73 => "\u{23BD}",
        0x74 => "\u{251C}",
        0x75 => "\u{2524}",
        0x76 => "\u{2534}",
        0x77 => "\u{252C}",
        0x78 => "\u{2502}",
        0x79 => "\u{2264}",
        0x7A => "\u{2265}",
        0x7B => "\u{03C0}",
        0x7C => "\u{2260}",
        0x7D => "\u{00A3}",
        0x7E => "\u{00B7}",
        _ => return None,
    };
    Some(SmolStr::new_inline(s))
}

fn translate_dec_technical(byte: u8) -> Option<SmolStr> {
    if byte == b' ' {
        return None;
    }
    let ch = match byte {
        0x21 => '\u{23B7}',
        0x22 => '\u{250C}',
        0x23 => '\u{2500}',
        0x24 => '\u{2320}',
        0x25 => '\u{2321}',
        0x26 => '\u{2502}',
        0x27 => '\u{23A1}',
        0x28 => '\u{23A3}',
        0x29 => '\u{23A4}',
        0x2A => '\u{23A6}',
        0x2B => '\u{239B}',
        0x2C => '\u{239D}',
        0x2D => '\u{239E}',
        0x2E => '\u{23A0}',
        0x2F => '\u{23A8}',
        0x30 => '\u{23AC}',
        0x31..=0x3B => SUBSTITUTE,
        0x3C => '\u{2264}',
        0x3D => '\u{2260}',
        0x3E => '\u{2265}',
        0x3F => '\u{222B}',
        0x40 => '\u{2234}',
        0x41 => '\u{221D}',
        0x42 => '\u{221E}',
        0x43 => '\u{00F7}',
        0x44 => '\u{0394}',
        0x45 => '\u{2207}',
        0x46 => '\u{03A6}',
        0x47 => '\u{0393}',
        0x48 => '\u{223C}',
        0x49 => '\u{2243}',
        0x4A => '\u{0398}',
        0x4B => '\u{00D7}',
        0x4C => '\u{039B}',
        0x4D => '\u{21D4}',
        0x4E => '\u{21D2}',
        0x4F => '\u{2261}',
        0x50 => '\u{03A0}',
        0x51 => '\u{03A8}',
        0x52 => SUBSTITUTE,
        0x53 => '\u{03A3}',
        0x54..=0x55 => SUBSTITUTE,
        0x56 => '\u{221A}',
        0x57 => '\u{03A9}',
        0x58 => '\u{039E}',
        0x59 => '\u{03A5}',
        0x5A => '\u{2282}',
        0x5B => '\u{2283}',
        0x5C => '\u{2229}',
        0x5D => '\u{222A}',
        0x5E => '\u{2227}',
        0x5F => '\u{2228}',
        0x60 => '\u{00AC}',
        0x61 => '\u{03B1}',
        0x62 => '\u{03B2}',
        0x63 => '\u{03C7}',
        0x64 => '\u{03B4}',
        0x65 => '\u{03B5}',
        0x66 => '\u{03C6}',
        0x67 => '\u{03B3}',
        0x68 => '\u{03B7}',
        0x69 => '\u{03B9}',
        0x6A => '\u{03B8}',
        0x6B => '\u{03BA}',
        0x6C => '\u{03BB}',
        0x6D => SUBSTITUTE,
        0x6E => '\u{03BD}',
        0x6F => '\u{2202}',
        0x70 => '\u{03C0}',
        0x71 => '\u{03C8}',
        0x72 => '\u{03C1}',
        0x73 => '\u{03C3}',
        0x74 => '\u{03C4}',
        0x75 => SUBSTITUTE,
        0x76 => '\u{0192}',
        0x77 => '\u{03C9}',
        0x78 => '\u{03BE}',
        0x79 => '\u{03C5}',
        0x7A => '\u{03B6}',
        0x7B => '\u{2190}',
        0x7C => '\u{2191}',
        0x7D => '\u{2192}',
        0x7E => '\u{2193}',
        _ => return None,
    };
    Some(single_char(ch))
}

fn translate_dec_supplemental(byte: u8) -> Option<SmolStr> {
    translate_supplemental(byte, true)
}

fn translate_iso_latin1_supplemental(byte: u8) -> Option<SmolStr> {
    translate_supplemental(byte, false)
}

fn translate_supplemental(
    byte: u8,
    dec_mcs: bool,
) -> Option<SmolStr> {
    if byte == b' ' {
        return None;
    }
    let code = byte as u32 + 0x80;
    let ch = if dec_mcs {
        match code {
            0xA4 | 0xA6 | 0xAC | 0xAD | 0xAE | 0xAF | 0xB4 | 0xB8 | 0xBE | 0xD0 | 0xDE | 0xF0
            | 0xFE => SUBSTITUTE,
            0xA8 => '\u{00A4}',
            0xD7 => '\u{0152}',
            0xDD => '\u{0178}',
            0xF7 => '\u{0153}',
            0xFD => '\u{00FF}',
            _ => char::from_u32(code)?,
        }
    } else {
        char::from_u32(code)?
    };
    Some(single_char(ch))
}

fn translate_nrc_byte(
    byte: u8,
    set: NationalReplacementSet,
) -> Option<SmolStr> {
    let replacement = match set {
        NationalReplacementSet::British => match byte {
            b'#' => Some('\u{00A3}'),
            _ => None,
        },
        NationalReplacementSet::Dutch => match byte {
            b'#' => Some('\u{00A3}'),
            b'@' => Some('\u{00BE}'),
            b'[' => Some('\u{00FF}'),
            b'\\' => Some('\u{00BD}'),
            b']' => Some('\u{007C}'),
            b'{' => Some('\u{00A8}'),
            b'|' => Some('f'),
            b'}' => Some('\u{00BC}'),
            b'~' => Some('\''),
            _ => None,
        },
        NationalReplacementSet::Finnish => match byte {
            b'[' => Some('\u{00C4}'),
            b'\\' => Some('\u{00D6}'),
            b']' => Some('\u{00C5}'),
            b'^' => Some('\u{00DC}'),
            b'`' => Some('\u{00E9}'),
            b'{' => Some('\u{00E4}'),
            b'|' => Some('\u{00F6}'),
            b'}' => Some('\u{00E5}'),
            b'~' => Some('\u{00FC}'),
            _ => None,
        },
        NationalReplacementSet::French => match byte {
            b'#' => Some('\u{00A3}'),
            b'@' => Some('\u{00E0}'),
            b'[' => Some('\u{00B0}'),
            b'\\' => Some('\u{00E7}'),
            b']' => Some('\u{00A7}'),
            b'{' => Some('\u{00E9}'),
            b'|' => Some('\u{00F9}'),
            b'}' => Some('\u{00E8}'),
            b'~' => Some('\u{00A8}'),
            _ => None,
        },
        NationalReplacementSet::FrenchCanadian => match byte {
            b'@' => Some('\u{00E0}'),
            b'[' => Some('\u{00E2}'),
            b'\\' => Some('\u{00E7}'),
            b']' => Some('\u{00EA}'),
            b'^' => Some('\u{00EE}'),
            b'`' => Some('\u{00F4}'),
            b'{' => Some('\u{00E9}'),
            b'|' => Some('\u{00F9}'),
            b'}' => Some('\u{00E8}'),
            b'~' => Some('\u{00FB}'),
            _ => None,
        },
        NationalReplacementSet::German => match byte {
            b'@' => Some('\u{00A7}'),
            b'[' => Some('\u{00C4}'),
            b'\\' => Some('\u{00D6}'),
            b']' => Some('\u{00DC}'),
            b'{' => Some('\u{00E4}'),
            b'|' => Some('\u{00F6}'),
            b'}' => Some('\u{00A8}'),
            b'~' => Some('\u{00DF}'),
            _ => None,
        },
        NationalReplacementSet::Italian => match byte {
            b'#' => Some('\u{00A3}'),
            b'@' => Some('\u{00A7}'),
            b'[' => Some('\u{00B0}'),
            b'\\' => Some('\u{00E7}'),
            b']' => Some('\u{00E9}'),
            b'`' => Some('\u{00F9}'),
            b'{' => Some('\u{00E0}'),
            b'|' => Some('\u{00F2}'),
            b'}' => Some('\u{00E8}'),
            b'~' => Some('\u{00EC}'),
            _ => None,
        },
        NationalReplacementSet::NorwegianDanish => match byte {
            b'[' => Some('\u{00C6}'),
            b'\\' => Some('\u{00D8}'),
            b']' => Some('\u{00C5}'),
            b'{' => Some('\u{00E6}'),
            b'|' => Some('\u{00F8}'),
            b'}' => Some('\u{00E5}'),
            _ => None,
        },
        NationalReplacementSet::Portuguese => match byte {
            b'[' => Some('\u{00C3}'),
            b'\\' => Some('\u{00C7}'),
            b']' => Some('\u{00D5}'),
            b'{' => Some('\u{00E3}'),
            b'|' => Some('\u{00E7}'),
            b'}' => Some('\u{00F5}'),
            _ => None,
        },
        NationalReplacementSet::Spanish => match byte {
            b'#' => Some('\u{00A3}'),
            b'@' => Some('\u{00A7}'),
            b'[' => Some('\u{00A1}'),
            b'\\' => Some('\u{00D1}'),
            b']' => Some('\u{00BF}'),
            b'{' => Some('`'),
            b'|' => Some('\u{00B0}'),
            b'}' => Some('\u{00F1}'),
            b'~' => Some('\u{00E7}'),
            _ => None,
        },
        NationalReplacementSet::Swedish => match byte {
            b'@' => Some('\u{00C9}'),
            b'[' => Some('\u{00C4}'),
            b'\\' => Some('\u{00D6}'),
            b']' => Some('\u{00C5}'),
            b'^' => Some('\u{00DC}'),
            b'`' => Some('\u{00E9}'),
            b'{' => Some('\u{00E4}'),
            b'|' => Some('\u{00F6}'),
            b'}' => Some('\u{00E5}'),
            b'~' => Some('\u{00FC}'),
            _ => None,
        },
        NationalReplacementSet::Swiss => match byte {
            b'#' => Some('\u{00F9}'),
            b'@' => Some('\u{00E0}'),
            b'[' => Some('\u{00E9}'),
            b'\\' => Some('\u{00E7}'),
            b']' => Some('\u{00EA}'),
            b'^' => Some('\u{00EE}'),
            b'_' => Some('\u{00E8}'),
            b'`' => Some('\u{00F4}'),
            b'{' => Some('\u{00E4}'),
            b'|' => Some('\u{00F6}'),
            b'}' => Some('\u{00FC}'),
            b'~' => Some('\u{00FB}'),
            _ => None,
        },
    }?;
    Some(single_char(replacement))
}

fn single_char(ch: char) -> SmolStr {
    let mut buf = [0u8; 4];
    SmolStr::new_inline(ch.encode_utf8(&mut buf))
}

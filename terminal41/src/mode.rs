//! Named constants for DEC private modes, ANSI modes, and xterm extensions.
//!
//! The canonical representation is typed enums with explicit `u16`
//! discriminants and `TryFrom<u16>` conversion.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum PrivateMode {
    Decckm = 1,
    Decanm = 2,
    Deccolm = 3,
    Decscnm = 5,
    Decom = 6,
    Decawm = 7,
    Decarm = 8,
    X10Mouse = 9,
    Att610Blink = 12,
    Dectcem = 25,
    AllowDeccolm = 40,
    Decnrcm = 42,
    AltScreen = 47,
    Decnkm = 66,
    Declrmm = 69,
    Decncsm = 95,
    NormalMouse = 1000,
    ButtonEventMouse = 1002,
    AnyEventMouse = 1003,
    FocusReporting = 1004,
    Utf8Mouse = 1005,
    SgrMouse = 1006,
    UrxvtMouse = 1015,
    SgrPixelsMouse = 1016,
    AltScreenClear = 1047,
    SaveCursor = 1048,
    AltScreenSave = 1049,
    Decatcum = 114,
    Decatcbm = 115,
    Decbbsm = 116,
    Dececm = 117,
    BracketedPaste = 2004,
    SynchronizedUpdate = 2026,
}

impl TryFrom<u16> for PrivateMode {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Decckm),
            2 => Ok(Self::Decanm),
            3 => Ok(Self::Deccolm),
            5 => Ok(Self::Decscnm),
            6 => Ok(Self::Decom),
            7 => Ok(Self::Decawm),
            8 => Ok(Self::Decarm),
            9 => Ok(Self::X10Mouse),
            12 => Ok(Self::Att610Blink),
            25 => Ok(Self::Dectcem),
            40 => Ok(Self::AllowDeccolm),
            42 => Ok(Self::Decnrcm),
            47 => Ok(Self::AltScreen),
            66 => Ok(Self::Decnkm),
            69 => Ok(Self::Declrmm),
            95 => Ok(Self::Decncsm),
            1000 => Ok(Self::NormalMouse),
            1002 => Ok(Self::ButtonEventMouse),
            1003 => Ok(Self::AnyEventMouse),
            1004 => Ok(Self::FocusReporting),
            1005 => Ok(Self::Utf8Mouse),
            1006 => Ok(Self::SgrMouse),
            1015 => Ok(Self::UrxvtMouse),
            1016 => Ok(Self::SgrPixelsMouse),
            1047 => Ok(Self::AltScreenClear),
            1048 => Ok(Self::SaveCursor),
            1049 => Ok(Self::AltScreenSave),
            114 => Ok(Self::Decatcum),
            115 => Ok(Self::Decatcbm),
            116 => Ok(Self::Decbbsm),
            117 => Ok(Self::Dececm),
            2004 => Ok(Self::BracketedPaste),
            2026 => Ok(Self::SynchronizedUpdate),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum AnsiMode {
    Irm = 4,
    Lnm = 20,
    Mode4 = 1,
}

impl TryFrom<u16> for AnsiMode {
    type Error = ();

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 | 5 | 7 | 10 | 11 | 13 | 14 | 15 | 16 | 17 | 18 | 19 => Ok(Self::Mode4),
            4 => Ok(Self::Irm),
            20 => Ok(Self::Lnm),
            _ => Err(()),
        }
    }
}

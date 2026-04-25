use std::collections::BTreeMap;

pub const MAX_MACRO_ID: u16 = 63;

#[derive(Debug, Default)]
pub struct MacroStore {
    definitions: BTreeMap<u16, Vec<u8>>,
    used_bytes: usize,
}

impl MacroStore {
    pub fn clear(&mut self) {
        self.definitions.clear();
        self.used_bytes = 0;
    }

    pub fn define(
        &mut self,
        id: u16,
        delete_existing: bool,
        encoding: MacroEncoding,
        payload: &[u8],
        max_storage_bytes: usize,
    ) {
        if id > MAX_MACRO_ID {
            return;
        }
        if delete_existing {
            self.clear();
        }
        let Some(bytes) = decode_macro_payload(encoding, payload) else {
            return;
        };
        let previous_len = self.definitions.get(&id).map_or(0, Vec::len);
        let projected = self
            .used_bytes
            .saturating_sub(previous_len)
            .saturating_add(bytes.len());
        if projected > max_storage_bytes {
            return;
        }
        self.used_bytes = projected;
        if bytes.is_empty() {
            self.definitions.remove(&id);
        } else {
            self.definitions.insert(id, bytes);
        }
    }

    pub fn get(
        &self,
        id: u16,
    ) -> Option<&[u8]> {
        self.definitions.get(&id).map(Vec::as_slice)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacroEncoding {
    Ascii,
    Hex,
}

impl MacroEncoding {
    pub fn from_param(pen: u16) -> Option<Self> {
        match pen {
            0 => Some(Self::Ascii),
            1 => Some(Self::Hex),
            _ => None,
        }
    }
}

fn decode_macro_payload(
    encoding: MacroEncoding,
    payload: &[u8],
) -> Option<Vec<u8>> {
    let payload = strip_dcs_formatting_controls(payload);
    match encoding {
        MacroEncoding::Ascii => Some(payload.to_vec()),
        MacroEncoding::Hex => decode_hex_macro(payload),
    }
}

fn strip_dcs_formatting_controls(payload: &[u8]) -> &[u8] {
    let start = payload
        .iter()
        .position(|b| !matches!(*b, 0x08..=0x0D))
        .unwrap_or(payload.len());
    let end = payload
        .iter()
        .rposition(|b| !matches!(*b, 0x08..=0x0D))
        .map(|idx| idx + 1)
        .unwrap_or(start);
    &payload[start..end]
}

fn decode_hex_macro(payload: &[u8]) -> Option<Vec<u8>> {
    let mut output = Vec::new();
    let mut i = 0;
    while i < payload.len() {
        if payload[i] == b'!' {
            let (repeated, consumed) = decode_repeat_sequence(&payload[i + 1..])?;
            output.extend_from_slice(&repeated);
            i += consumed + 1;
        } else {
            let byte = decode_hex_byte(payload.get(i..i + 2)?)?;
            output.push(byte);
            i += 2;
        }
    }
    Some(output)
}

fn decode_repeat_sequence(payload: &[u8]) -> Option<(Vec<u8>, usize)> {
    let count_end = payload.iter().position(|&b| b == b';')?;
    let count = if count_end == 0 {
        1
    } else {
        std::str::from_utf8(&payload[..count_end])
            .ok()?
            .parse::<usize>()
            .ok()?
    };
    let body_start = count_end + 1;
    let body_end = payload[body_start..]
        .iter()
        .position(|&b| b == b';')
        .map(|idx| body_start + idx)?;
    let body = &payload[body_start..body_end];
    if !body.len().is_multiple_of(2) {
        return None;
    }
    let mut decoded = Vec::with_capacity(body.len() / 2 * count);
    for chunk in body.chunks_exact(2) {
        decoded.push(decode_hex_byte(chunk)?);
    }
    let repeated = decoded.repeat(count);
    Some((repeated, body_end + 1))
}

fn decode_hex_byte(bytes: &[u8]) -> Option<u8> {
    Some((hex_nibble(bytes[0])? << 4) | hex_nibble(bytes[1])?)
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

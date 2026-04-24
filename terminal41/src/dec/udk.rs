use std::collections::BTreeMap;

use crate::C1Mode;
use crate::conformance;

pub const MAX_UDK_BYTES: usize = 256;
pub const MAX_DECUDK_PAYLOAD_BYTES: usize = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFunctionKeyControl {
    Local,
    SendSequence,
    Disabled,
}

impl LocalFunctionKeyControl {
    fn from_param(param: u16) -> Option<Self> {
        match param {
            0 => Some(Self::SendSequence),
            1 => Some(Self::Local),
            2 => Some(Self::SendSequence),
            3 => Some(Self::Disabled),
            _ => None,
        }
    }

    fn report_param(self) -> u16 {
        match self {
            Self::Local => 1,
            Self::SendSequence => 2,
            Self::Disabled => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalFunctionControl {
    Enabled,
    Disabled,
}

impl LocalFunctionControl {
    fn from_param(param: u16) -> Option<Self> {
        match param {
            0 | 1 => Some(Self::Enabled),
            2 => Some(Self::Disabled),
            _ => None,
        }
    }

    fn report_param(self) -> u16 {
        match self {
            Self::Enabled => 1,
            Self::Disabled => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierKeyControl {
    Modifier,
    Report,
    Disabled,
}

impl ModifierKeyControl {
    fn from_param(param: u16) -> Option<Self> {
        match param {
            0 | 1 => Some(Self::Modifier),
            2 => Some(Self::Report),
            3 => Some(Self::Disabled),
            _ => None,
        }
    }

    fn report_param(self) -> u16 {
        match self {
            Self::Modifier => 1,
            Self::Report => 2,
            Self::Disabled => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecModifierKey {
    LeftShift,
    Ctrl,
    LeftAltFunction,
}

impl DecModifierKey {
    fn selector(self) -> u16 {
        match self {
            Self::LeftShift => 1,
            Self::Ctrl => 4,
            Self::LeftAltFunction => 5,
        }
    }
}

#[derive(Debug)]
pub struct UdkState {
    definitions: BTreeMap<u16, Vec<u8>>,
    used_bytes: usize,
    locked: bool,
    local_functions: [LocalFunctionControl; 3],
    local_function_keys: [LocalFunctionKeyControl; 4],
    modifier_keys: [ModifierKeyControl; 8],
}

impl Default for UdkState {
    fn default() -> Self {
        Self {
            definitions: BTreeMap::new(),
            used_bytes: 0,
            locked: false,
            local_functions: [LocalFunctionControl::Enabled; 3],
            local_function_keys: [LocalFunctionKeyControl::SendSequence; 4],
            modifier_keys: [ModifierKeyControl::Modifier; 8],
        }
    }
}

impl UdkState {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn define(
        &mut self,
        params: vtepp::Params,
        payload: &[u8],
        max_storage_bytes: usize,
    ) {
        if self.locked {
            return;
        }

        let clear = params
            .iter()
            .next()
            .and_then(|group| group.first().copied())
            .unwrap_or(0);
        let lock = params
            .iter()
            .nth(1)
            .and_then(|group| group.first().copied())
            .unwrap_or(0);

        if !matches!(clear, 0 | 1) || !matches!(lock, 0 | 1) {
            return;
        }
        if clear == 0 {
            self.clear_definitions();
        }

        for part in payload.split(|byte| *byte == b';') {
            if part.is_empty() {
                continue;
            }
            let Some((selector, definition)) = parse_definition(part) else {
                break;
            };
            if clear == 1 {
                self.remove_definition(selector);
            }
            if !self.set_definition(selector, definition, max_storage_bytes) {
                break;
            }
        }

        self.locked = lock == 0;
    }

    pub fn definition(
        &self,
        selector: u16,
    ) -> Option<&[u8]> {
        self.definitions.get(&selector).map(Vec::as_slice)
    }

    pub fn locked(&self) -> bool {
        self.locked
    }

    pub fn programmed_selectors(&self) -> Vec<u16> {
        self.definitions.keys().copied().collect()
    }

    pub fn set_local_functions(
        &mut self,
        params: &[&[u16]],
    ) {
        for (selector, control) in param_pairs(params) {
            let Some(control) = LocalFunctionControl::from_param(control) else {
                continue;
            };
            if selector == 0 {
                self.local_functions = [control; 3];
            } else if let Some(slot) = selector
                .checked_sub(1)
                .and_then(|idx| self.local_functions.get_mut(idx as usize))
            {
                *slot = control;
            }
        }
    }

    pub fn set_local_function_keys(
        &mut self,
        params: &[&[u16]],
    ) {
        for (selector, control) in param_pairs(params) {
            let Some(control) = LocalFunctionKeyControl::from_param(control) else {
                continue;
            };
            if selector == 0 {
                self.local_function_keys = [control; 4];
            } else if let Some(slot) = selector
                .checked_sub(1)
                .and_then(|idx| self.local_function_keys.get_mut(idx as usize))
            {
                *slot = control;
            }
        }
    }

    pub fn local_function_key(
        &self,
        selector: u16,
    ) -> Option<LocalFunctionKeyControl> {
        selector
            .checked_sub(1)
            .and_then(|idx| self.local_function_keys.get(idx as usize))
            .copied()
    }

    pub fn set_modifier_keys(
        &mut self,
        params: &[&[u16]],
    ) {
        for (selector, control) in param_pairs(params) {
            let Some(control) = ModifierKeyControl::from_param(control) else {
                continue;
            };
            if selector == 0 {
                self.modifier_keys = [control; 8];
            } else if let Some(slot) = selector
                .checked_sub(1)
                .and_then(|idx| self.modifier_keys.get_mut(idx as usize))
            {
                *slot = control;
            }
        }
    }

    pub fn modifier_key(
        &self,
        key: DecModifierKey,
    ) -> ModifierKeyControl {
        let idx = key.selector().saturating_sub(1) as usize;
        self.modifier_keys
            .get(idx)
            .copied()
            .unwrap_or(ModifierKeyControl::Modifier)
    }

    pub fn report_local_functions(&self) -> String {
        self.local_functions
            .iter()
            .enumerate()
            .map(|(idx, control)| format!("{};{}", idx + 1, control.report_param()))
            .collect::<Vec<_>>()
            .join(";")
    }

    pub fn report_local_function_keys(&self) -> String {
        self.local_function_keys
            .iter()
            .enumerate()
            .map(|(idx, control)| format!("{};{}", idx + 1, control.report_param()))
            .collect::<Vec<_>>()
            .join(";")
    }

    pub fn report_modifier_keys(&self) -> String {
        self.modifier_keys
            .iter()
            .enumerate()
            .map(|(idx, control)| format!("{};{}", idx + 1, control.report_param()))
            .collect::<Vec<_>>()
            .join(";")
    }

    fn clear_definitions(&mut self) {
        self.definitions.clear();
        self.used_bytes = 0;
    }

    fn remove_definition(
        &mut self,
        selector: u16,
    ) {
        if let Some(previous) = self.definitions.remove(&selector) {
            self.used_bytes = self.used_bytes.saturating_sub(previous.len());
        }
    }

    fn set_definition(
        &mut self,
        selector: u16,
        definition: Vec<u8>,
        max_storage_bytes: usize,
    ) -> bool {
        if !is_definable_key(selector) {
            return true;
        }
        let previous_len = self.definitions.get(&selector).map_or(0, Vec::len);
        let projected = self
            .used_bytes
            .saturating_sub(previous_len)
            .saturating_add(definition.len());
        if projected > max_storage_bytes {
            return false;
        }
        self.used_bytes = projected;
        if definition.is_empty() {
            self.definitions.remove(&selector);
        } else {
            self.definitions.insert(selector, definition);
        }
        true
    }
}

pub fn write_modifier_report(
    out: &mut Vec<u8>,
    c1_mode: C1Mode,
    key: DecModifierKey,
    pressed: bool,
) {
    let selector = key.selector();
    let state = u8::from(pressed);
    conformance::write_apc(out, c1_mode, format_args!(":{selector:03}{state}"));
}

fn param_pairs<'a>(params: &'a [&'a [u16]]) -> impl Iterator<Item = (u16, u16)> + 'a {
    params
        .chunks_exact(2)
        .filter_map(|pair| Some((pair[0].first().copied()?, pair[1].first().copied()?)))
}

fn parse_definition(part: &[u8]) -> Option<(u16, Vec<u8>)> {
    let split = part.iter().position(|byte| *byte == b'/')?;
    let selector = std::str::from_utf8(&part[..split])
        .ok()?
        .parse::<u16>()
        .ok()?;
    let value = decode_hex(&part[split + 1..])?;
    Some((selector, value))
}

fn decode_hex(hex: &[u8]) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    hex.chunks_exact(2)
        .map(|chunk| Some((hex_nibble(chunk[0])? << 4) | hex_nibble(chunk[1])?))
        .collect()
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_definable_key(selector: u16) -> bool {
    matches!(selector, 17..=21 | 23..=26 | 28 | 29 | 31..=34)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(groups: &[&[u16]]) -> vtepp::Params {
        let mut parser = vtepp::Parser::new();
        let mut bytes = b"\x1bP".to_vec();
        for (idx, group) in groups.iter().enumerate() {
            if idx > 0 {
                bytes.push(b';');
            }
            for (sub_idx, param) in group.iter().enumerate() {
                if sub_idx > 0 {
                    bytes.push(b':');
                }
                bytes.extend_from_slice(param.to_string().as_bytes());
            }
        }
        bytes.extend_from_slice(b"|17/41\x1b\\");
        match parser.parse(&bytes).next().expect("hook") {
            vtepp::Action::Hook { params, .. } => params,
            other => panic!("unexpected action {other:?}"),
        }
    }

    #[test]
    fn decudk_defines_hex_payload() {
        let mut state = UdkState::default();
        state.define(params(&[&[0], &[1]]), b"17/414243", MAX_UDK_BYTES);
        assert_eq!(state.definition(17), Some(&b"ABC"[..]));
    }

    #[test]
    fn decudk_clear_one_replaces_only_target_key() {
        let mut state = UdkState::default();
        state.define(params(&[&[0], &[1]]), b"17/41;18/42", MAX_UDK_BYTES);
        state.define(params(&[&[1], &[1]]), b"17/43", MAX_UDK_BYTES);
        assert_eq!(state.definition(17), Some(&b"C"[..]));
        assert_eq!(state.definition(18), Some(&b"B"[..]));
    }

    #[test]
    fn locked_decudk_rejects_future_definitions() {
        let mut state = UdkState::default();
        state.define(params(&[&[0], &[0]]), b"17/41", MAX_UDK_BYTES);
        state.define(params(&[&[1], &[1]]), b"17/42", MAX_UDK_BYTES);
        assert_eq!(state.definition(17), Some(&b"A"[..]));
    }

    #[test]
    fn modifier_report_uses_dec_apc_shape() {
        let mut out = Vec::new();
        write_modifier_report(&mut out, C1Mode::SevenBit, DecModifierKey::Ctrl, true);
        assert_eq!(out, b"\x1b_:0041\x1b\\");
    }
}

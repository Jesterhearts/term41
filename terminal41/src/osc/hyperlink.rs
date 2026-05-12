use super::split_key_value;
use super::split_osc;
use crate::screen::hyperlink::HyperlinkId;
use crate::screen::hyperlink::HyperlinkRegistry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum HyperlinkAction<'a> {
    Clear,
    Open { id: Option<&'a str>, uri: &'a str },
}

pub(super) fn parse(rest: &[u8]) -> HyperlinkAction<'_> {
    let (params, uri) = split_osc(rest);

    if uri.is_empty() {
        return HyperlinkAction::Clear;
    }

    let Ok(uri) = std::str::from_utf8(uri) else {
        return HyperlinkAction::Clear;
    };

    let id = params.split(|&b| b == b':').find_map(|kv| {
        let (key, value) = split_key_value(kv)?;
        (key == b"id").then(|| std::str::from_utf8(value).ok())?
    });

    HyperlinkAction::Open { id, uri }
}

pub(super) fn apply(
    action: HyperlinkAction<'_>,
    registry: &mut HyperlinkRegistry,
    current: &mut Option<HyperlinkId>,
) {
    match action {
        HyperlinkAction::Clear => *current = None,
        HyperlinkAction::Open { id, uri } => *current = Some(registry.intern(id, uri)),
    }
}

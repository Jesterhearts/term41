pub(super) fn parse(rest: &[u8]) -> Option<Option<&str>> {
    if rest.is_empty() {
        return Some(None);
    }
    std::str::from_utf8(rest).ok().map(Some)
}

pub(super) fn apply(
    title: Option<&str>,
    current_title: &mut Option<String>,
) {
    *current_title = title.map(ToOwned::to_owned);
}

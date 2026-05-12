use read_fonts::TableProvider;

pub(crate) fn charmap_lookup(
    font: &read_fonts::FontRef<'_>,
    ch: char,
) -> u32 {
    let cmap = match font.cmap() {
        Ok(c) => c,
        Err(_) => return 0,
    };
    for record in cmap.encoding_records() {
        if let Ok(subtable) = record.subtable(cmap.offset_data())
            && let Some(gid) = subtable.map_codepoint(ch)
        {
            return gid.to_u32();
        }
    }
    0
}

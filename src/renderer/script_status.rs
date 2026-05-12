use terminal41::TermSnapshot;

use crate::renderer::paint::local_status_line_row;

const SCRIPT_STATUS_GENERATION_BIT: u64 = 1 << 63;

pub(super) fn apply_script_status_line(
    snap: &mut TermSnapshot,
    status_text: Option<&str>,
    generation: u64,
) {
    let Some(text) = status_text else {
        return;
    };
    let Some(screen_row) = snap.status_line_row else {
        return;
    };
    let base_generation = snap
        .rows
        .iter()
        .find(|row| row.screen_row == screen_row)
        .map(|row| row.generation)
        .unwrap_or(snap.generation);
    let row = local_status_line_row(
        text,
        snap.viewport_cols,
        screen_row,
        script_status_row_generation(generation, base_generation),
        &snap.palette,
    );
    if let Some(existing) = snap
        .rows
        .iter_mut()
        .find(|row| row.screen_row == screen_row)
    {
        *existing = row;
    } else {
        snap.rows.push(row);
    }
}

fn script_status_row_generation(
    script_generation: u64,
    base_generation: u64,
) -> u64 {
    SCRIPT_STATUS_GENERATION_BIT
        | script_generation
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(base_generation)
            & !SCRIPT_STATUS_GENERATION_BIT
}

#[cfg(test)]
mod tests {
    use config41::ColorPalette;
    use config41::CursorStyle;
    use terminal41::TermSnapshot;

    use super::*;

    fn snapshot_with_status_row(generation: u64) -> TermSnapshot {
        let palette = ColorPalette::default();
        TermSnapshot {
            generation,
            rows: vec![local_status_line_row("", 8, 2, generation, &palette)],
            total_rows: 3,
            viewport_rows: 2,
            viewport_cols: 8,
            viewport_offset: 0,
            status_line_row: Some(2),
            drcs_glyphs: Default::default(),
            dec_color: terminal41::dec_color_state_from_palette(&palette),
            palette,
            search_active: false,
            search: None,
            cursor: None,
            cursor_style: CursorStyle::default(),
            screen_reverse: false,
            on_alt_screen: false,
            command_editor_hidden: false,
            synchronized_update_active: false,
            current_title: None,
            reset_cached_rows: false,
        }
    }

    #[test]
    fn script_status_generation_cannot_collide_with_terminal_status_row_generation() {
        let mut snap = snapshot_with_status_row(1);

        apply_script_status_line(&mut snap, Some("ok"), 1);

        let status_row = snap.rows.last().expect("status row");
        assert_eq!(&status_row.cells.concat()[..2], "ok");
        assert_ne!(status_row.generation, 1);
        assert_eq!(status_row.generation, script_status_row_generation(1, 1));
    }

    #[test]
    fn script_status_generation_tracks_base_status_row_generation() {
        let mut before = snapshot_with_status_row(1);
        let mut after = snapshot_with_status_row(2);

        apply_script_status_line(&mut before, Some("ok"), 1);
        apply_script_status_line(&mut after, Some("ok"), 1);

        assert_ne!(
            before.rows.last().expect("before status row").generation,
            after.rows.last().expect("after status row").generation
        );
    }
}

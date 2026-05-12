use super::super::*;

/// Resolve "is the cursor visible right now and what does it look like"
/// once per frame. Hidden cases — scrolled away from live or in the
/// blink-off phase — collapse to [`CursorRenderState::Hidden`] so the
/// per-cell loops don't have to know the rules.
/// Compute the cursor render state from the snapshot.
pub(in crate::renderer::r#impl) fn cursor_state_from_snapshot(
    snap: &TermSnapshot
) -> CursorRenderState {
    let Some((row, col)) = snap.cursor else {
        return CursorRenderState::Hidden;
    };
    let style = snap.cursor_style;
    if style.blink {
        let elapsed = APP_START_TIME.get().unwrap().elapsed().as_secs_f32();
        let half = CURSOR_BLINK_HALF_PERIOD.as_secs_f32();
        let phase = (elapsed / half) as u64;
        if phase & 1 == 1 {
            return CursorRenderState::Hidden;
        }
    }
    CursorRenderState::Visible {
        row,
        col,
        shape: style.shape,
    }
}

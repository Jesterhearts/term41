use config41::CursorShape;
use palette::Srgb;

use super::pack_color;

/// Tells the per-cell loops what (if anything) the cursor wants drawn at a
/// given coordinate. Computed once per frame so blink and viewport-offset
/// checks don't repeat for every cell.
#[derive(Debug, Clone, Copy)]
pub(super) enum CursorRenderState {
    Hidden,
    Visible {
        row: u32,
        col: u32,
        shape: CursorShape,
    },
}

/// Geometry of an underline / beam overlay. Not used for block — that path
/// inverts the cell instead and needs no separate quad.
pub(super) struct BarOverlay {
    pub(super) x: f32,
    pub(super) y: f32,
    pub(super) w: f32,
    pub(super) h: f32,
    pub(super) color: u32,
}

impl CursorRenderState {
    pub(super) fn block_cursor(self) -> Option<(u32, u32)> {
        match self {
            CursorRenderState::Visible {
                row,
                col,
                shape: CursorShape::Block,
            } => Some((row, col)),
            _ => None,
        }
    }

    /// Build a thin overlay quad for underline / beam shapes when this row
    /// holds the cursor cell. Returns `None` for block, hidden, or the
    /// wrong row. The configured cursor color takes precedence over the
    /// cell foreground fallback.
    pub(super) fn bar_overlay_at(
        self,
        row: u32,
        fg_row: &[Srgb<u8>],
        cursor_color: Option<Srgb<u8>>,
        cell_w: f32,
        cell_h: f32,
    ) -> Option<BarOverlay> {
        let CursorRenderState::Visible { row: r, col, shape } = self else {
            return None;
        };
        if r != row {
            return None;
        }
        let color = cursor_color.or_else(|| fg_row.get(col as usize).copied())?;
        let color = pack_color(&color, 255);
        let x0 = col as f32 * cell_w;
        let y0 = row as f32 * cell_h;
        match shape {
            CursorShape::Block => None,
            CursorShape::Underline => {
                // 2-px-ish strip along the bottom; matches xterm's default.
                let h = (cell_h * 0.12).max(2.0);
                Some(BarOverlay {
                    x: x0,
                    y: y0 + cell_h - h,
                    w: cell_w,
                    h,
                    color,
                })
            }
            CursorShape::Beam => {
                let w = (cell_w * 0.12).max(2.0);
                Some(BarOverlay {
                    x: x0,
                    y: y0,
                    w,
                    h: cell_h,
                    color,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_cursor_prefers_configured_cursor_color() {
        let cursor = CursorRenderState::Visible {
            row: 0,
            col: 0,
            shape: CursorShape::Beam,
        };
        let foreground = Srgb::new(1, 2, 3);
        let configured = Srgb::new(4, 5, 6);

        let overlay = cursor
            .bar_overlay_at(0, &[foreground], Some(configured), 8.0, 16.0)
            .expect("beam overlay");

        assert_eq!(overlay.color, pack_color(&configured, 255));
    }
}

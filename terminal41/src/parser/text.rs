use super::*;

pub(crate) fn execute(
    screen: &mut Screen,
    viewport: &Viewport,
    byte: u8,
    bell_pending: &mut bool,
    newline_mode: bool,
) {
    clamp_cursor_to_row_width(screen, viewport);

    match byte {
        b'\n' | VT | FF => {
            if newline_mode {
                screen.cursor.col = 0;
            }
            if screen.cursor.row == screen.scroll_bottom {
                if screen.scroll_top == 0 && screen.scroll_bottom == viewport.rows - 1 {
                    screen.grid.push_visible_row(viewport);
                } else {
                    screen.grid.scroll_up_in_region(
                        viewport,
                        &mut screen.images,
                        screen.scroll_top,
                        screen.scroll_bottom,
                        1,
                    );
                }
            } else if screen.cursor.row < viewport.rows - 1 {
                screen.cursor.row += 1;
                clamp_cursor_to_row_width(screen, viewport);
            }
        }
        b'\r' => {
            screen.cursor.col = 0;
        }
        BS => {
            screen.cursor.col = screen.cursor.col.saturating_sub(1);
        }
        b'\t' => {
            let cols = current_row_display_cols(screen, viewport);
            screen.cursor.col = next_tab_stop(&screen.tab_stops, screen.cursor.col, cols);
        }
        SO => {
            screen.charset.set_gl(GraphicSetSlot::G1);
        }
        SI => {
            screen.charset.set_gl(GraphicSetSlot::G0);
        }
        BEL => {
            *bell_pending = true;
        }
        NUL => {}
        _ => {}
    }
}

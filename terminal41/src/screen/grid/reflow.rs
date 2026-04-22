use crate::screen::grid::Grid;
use crate::screen::row::LineAttr;
use crate::screen::row::Row;

pub(crate) fn reflow(
    grid: &mut Grid,
    new_width: u32,
) {
    if grid.rows.is_empty() {
        return;
    }

    if grid.rows[0].len() == new_width {
        return;
    }

    if new_width > grid.rows[0].len() {
        grow(grid, new_width as usize);
    } else {
        shrink(grid, new_width);
    }
}

fn grow(
    grid: &mut Grid,
    new_width: usize,
) {
    let fg = grid.default_fg;
    let bg = grid.default_bg;
    let mut dst = 0;
    let mut dst_col = grid.rows[0].content_len() as usize;
    let mut src = 1;
    let mut src_col: usize = 0;

    while dst < grid.rows.len() && src < grid.rows.len() {
        grid.rows[dst].resize(new_width as u32, fg, bg);

        if !grid.rows[dst].wrapped {
            dst += 1;
            dst_col = if dst == src && grid.rows[dst].wrapped {
                grid.rows[dst].content_len() as usize
            } else {
                0
            };
            if dst == src {
                src += 1;
            }
            continue;
        }

        let (d, s) = split_current_next(grid, dst, src);
        let s_content = s.content_len() as usize;
        let n = d.copy_from(s, src_col..s_content, dst_col);
        dst_col += n;
        src_col += n;

        if src_col >= s_content {
            d.wrapped = s.wrapped;
            s.clear(fg, bg);
            s.wrapped = true;
            src += 1;
            src_col = 0;
        }

        if dst_col >= new_width {
            if src_col > 0 {
                grid.rows[dst].wrapped = true;
            }
            dst += 1;
            dst_col = 0;
            if dst == src {
                grid.rows[dst].copy_within(src_col.., 0);
                let len = grid.rows[dst].len() as usize;
                grid.rows[dst].clear_range(len - src_col..len, fg, bg);
                dst_col = len - src_col;
                src += 1;
                src_col = 0;
            }
        }
    }

    grid.rows[dst].resize(new_width as u32, fg, bg);
    grid.rows
        .truncate(dst + if grid.rows[dst].wrapped { 0 } else { 1 });
}

fn shrink(
    grid: &mut Grid,
    new_width: u32,
) {
    let mut row = 0;
    while row < grid.rows.len() {
        if grid.rows[row].len() > new_width {
            if grid.rows[row].content_len() > new_width {
                let overflow = Row {
                    cells: grid.rows[row].cells.split_off(new_width as usize),
                    fg: grid.rows[row].fg.split_off(new_width as usize),
                    bg: grid.rows[row].bg.split_off(new_width as usize),
                    attrs: grid.rows[row].attrs.split_off(new_width as usize),
                    underline: grid.rows[row].underline.split_off(new_width as usize),
                    underline_color: grid.rows[row].underline_color.split_off(new_width as usize),
                    links: grid.rows[row].links.split_off(new_width as usize),
                    wrapped: grid.rows[row].wrapped,
                    prompt_start: false,
                    output_start: false,
                    exit_status: None,
                    line_attr: LineAttr::Normal,
                };

                grid.rows[row].wrapped = true;
                grid.rows.insert(row + 1, overflow);
            } else {
                grid.rows[row].wrapped = false;
                grid.rows[row].truncate(new_width);
            }
        } else {
            let mut content = grid.rows[row].len() as usize;
            grid.rows[row].resize(new_width, grid.default_fg, grid.default_bg);

            while grid.rows[row].wrapped && row + 1 < grid.rows.len() {
                let room = new_width as usize - content;
                if room == 0 {
                    break;
                }

                let next = row + 1;
                let next_content = grid.rows[next].content_len() as usize;
                let to_copy = room.min(next_content);

                if to_copy > 0 {
                    let (dst, src) = split_current_next(grid, row, next);
                    for i in 0..to_copy {
                        dst.cells[content + i] = src.cells[i].clone();
                    }
                    dst.fg[content..content + to_copy].copy_from_slice(&src.fg[..to_copy]);
                    dst.bg[content..content + to_copy].copy_from_slice(&src.bg[..to_copy]);
                }

                if to_copy >= next_content {
                    let next_wrapped = grid.rows[next].wrapped;
                    grid.rows.remove(next);
                    grid.rows[row].wrapped = next_wrapped;
                    content += to_copy;
                } else {
                    grid.rows[next].copy_within(to_copy.., 0);
                    let remaining = grid.rows[next].len() as usize - to_copy;
                    grid.rows[next].truncate(remaining as u32);
                    break;
                }
            }
        }
        row += 1;
    }
}

fn split_current_next(
    grid: &mut Grid,
    row: usize,
    next: usize,
) -> (&mut Row, &mut Row) {
    let (front, back) = grid.rows.as_mut_slices();

    if row < front.len() && next >= front.len() {
        let next = next - front.len();
        (&mut front[row], &mut back[next])
    } else if next < front.len() && row >= front.len() {
        (&mut back[row - front.len()], &mut front[next])
    } else if next < front.len() {
        let (first, second) = front.split_at_mut(next);
        (&mut first[row], &mut second[0])
    } else {
        let (first, second) = back.split_at_mut(next - front.len());
        (&mut first[row - front.len()], &mut second[0])
    }
}

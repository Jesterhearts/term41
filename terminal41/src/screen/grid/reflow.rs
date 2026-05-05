use crate::screen::grid::Grid;
use crate::screen::row::LineAttr;
use crate::screen::row::Row;
use crate::screen::row::cells_contain_wide;

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
        move_markers_in_copied_range(
            d,
            s,
            src_col,
            src_col + n,
            dst_col,
            src_col + n >= s_content,
        );
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
                shift_markers_left(&mut grid.rows[dst], src_col);
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
                let cells = grid.rows[row].cells.split_off(new_width as usize);
                let has_wide_cells = cells_contain_wide(&cells);
                let mut overflow = Row {
                    cells,
                    fg: grid.rows[row].fg.split_off(new_width as usize),
                    bg: grid.rows[row].bg.split_off(new_width as usize),
                    attrs: grid.rows[row].attrs.split_off(new_width as usize),
                    underline_color: grid.rows[row].underline_color.split_off(new_width as usize),
                    links: grid.rows[row].links.split_off(new_width as usize),
                    wrapped: grid.rows[row].wrapped,
                    prompt_start: false,
                    command_start_col: None,
                    output_start: false,
                    output_start_col: None,
                    exit_status: None,
                    line_attr: LineAttr::Normal,
                    has_wide_cells,
                };
                move_markers_after_split(&mut grid.rows[row], &mut overflow, new_width);

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
                    move_markers_in_copied_range(
                        dst,
                        src,
                        0,
                        to_copy,
                        content,
                        to_copy >= next_content,
                    );
                }

                if to_copy >= next_content {
                    let next_wrapped = grid.rows[next].wrapped;
                    grid.rows.remove(next);
                    grid.rows[row].wrapped = next_wrapped;
                    content += to_copy;
                } else {
                    grid.rows[next].copy_within(to_copy.., 0);
                    shift_markers_left(&mut grid.rows[next], to_copy);
                    let remaining = grid.rows[next].len() as usize - to_copy;
                    grid.rows[next].truncate(remaining as u32);
                    break;
                }
            }
        }
        row += 1;
    }
}

fn move_markers_after_split(
    row: &mut Row,
    overflow: &mut Row,
    split_col: u32,
) {
    let split_col = split_col as usize;
    move_command_start_after_split(row, overflow, split_col);
    move_output_start_after_split(row, overflow, split_col);
}

fn move_command_start_after_split(
    row: &mut Row,
    overflow: &mut Row,
    split_col: usize,
) {
    let Some(col) = row.command_start_col else {
        return;
    };
    let col = col as usize;
    if col < split_col {
        return;
    }
    row.command_start_col = None;
    overflow.command_start_col = Some((col - split_col) as u32);
}

fn move_output_start_after_split(
    row: &mut Row,
    overflow: &mut Row,
    split_col: usize,
) {
    if !row.output_start {
        return;
    }
    let col = row.output_start_col.unwrap_or(0) as usize;
    if col < split_col {
        return;
    }
    row.output_start = false;
    row.output_start_col = None;
    overflow.output_start = true;
    overflow.output_start_col = Some((col - split_col) as u32);
}

fn move_markers_in_copied_range(
    dst: &mut Row,
    src: &mut Row,
    src_start: usize,
    src_end: usize,
    dst_start: usize,
    include_end: bool,
) {
    move_command_start_in_copied_range(dst, src, src_start, src_end, dst_start, include_end);
    move_output_start_in_copied_range(dst, src, src_start, src_end, dst_start, include_end);
}

fn point_in_copied_range(
    col: usize,
    start: usize,
    end: usize,
    include_end: bool,
) -> bool {
    col >= start && (col < end || (include_end && col == end))
}

fn move_command_start_in_copied_range(
    dst: &mut Row,
    src: &mut Row,
    src_start: usize,
    src_end: usize,
    dst_start: usize,
    include_end: bool,
) {
    let Some(col) = src.command_start_col else {
        return;
    };
    let col = col as usize;
    if !point_in_copied_range(col, src_start, src_end, include_end) {
        return;
    }
    src.command_start_col = None;
    dst.command_start_col = Some((dst_start + col.saturating_sub(src_start)) as u32);
}

fn move_output_start_in_copied_range(
    dst: &mut Row,
    src: &mut Row,
    src_start: usize,
    src_end: usize,
    dst_start: usize,
    include_end: bool,
) {
    if !src.output_start {
        return;
    }
    let col = src.output_start_col.unwrap_or(0) as usize;
    if !point_in_copied_range(col, src_start, src_end, include_end) {
        return;
    }
    src.output_start = false;
    src.output_start_col = None;
    dst.output_start = true;
    dst.output_start_col = Some((dst_start + col.saturating_sub(src_start)) as u32);
}

fn shift_markers_left(
    row: &mut Row,
    amount: usize,
) {
    if amount == 0 {
        return;
    }
    if let Some(col) = row.command_start_col {
        row.command_start_col = (col as usize >= amount).then_some(col - amount as u32);
    }
    if row.output_start {
        let col = row.output_start_col.unwrap_or(0) as usize;
        if col >= amount {
            row.output_start_col = Some((col - amount) as u32);
        } else {
            row.output_start = false;
            row.output_start_col = None;
        }
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

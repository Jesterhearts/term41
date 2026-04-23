use std::time::Instant;

use vte_mode41::C1Mode;

use crate::PlacedImage;
use crate::Screen;
use crate::Viewport;
use crate::conformance;
use crate::screen;

pub(crate) fn handle_kitty_graphics(
    data: &[u8],
    store: &mut image41::kitty::KittyImageStore,
    chunked: &mut image41::kitty::ChunkedTransmission,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    if data.first() != Some(&b'G') {
        return;
    }

    let parsed = image41::kitty::parse_command(&data[1..]);
    let cmd = if parsed.action == b'd' {
        chunked.clear();
        parsed
    } else {
        match chunked.feed(parsed) {
            Some(cmd) => cmd,
            None => return,
        }
    };

    match cmd.action {
        b'q' => handle_kitty_query(&cmd, c1_mode, pending_output),
        b'T' => handle_kitty_transmit_display(
            &cmd,
            store,
            screen,
            viewport,
            next_image_id,
            cell_height,
            cell_width,
            c1_mode,
            pending_output,
        ),
        b't' => handle_kitty_transmit(&cmd, store, c1_mode, pending_output),
        b'p' => handle_kitty_place(
            &cmd,
            store,
            screen,
            viewport,
            next_image_id,
            cell_height,
            cell_width,
            c1_mode,
            pending_output,
        ),
        b'd' => handle_kitty_delete(
            &cmd,
            screen,
            viewport,
            store,
            chunked,
            cell_height,
            cell_width,
        ),
        b'a' | b'c' | b'f' => send_kitty_response(
            &cmd,
            response_image_id(&cmd),
            false,
            "ENOTSUP",
            c1_mode,
            pending_output,
        ),
        _ => {}
    }
}

fn decode_kitty_image(cmd: &image41::kitty::KittyCommand) -> Option<image41::DecodedImage> {
    match cmd.transmission {
        b'f' => image41::kitty::decode_file_payload(cmd, &cmd.payload, false),
        b't' => image41::kitty::decode_file_payload(cmd, &cmd.payload, true),
        _ => image41::kitty::decode_payload(cmd, &cmd.payload),
    }
}

fn unsupported_transmission(cmd: &image41::kitty::KittyCommand) -> Option<&'static str> {
    if cmd.transmission == b's' {
        Some("ENOTSUP")
    } else {
        None
    }
}

fn command_has_conflicting_ids(cmd: &image41::kitty::KittyCommand) -> bool {
    cmd.image_id != 0 && cmd.image_number != 0
}

fn response_image_id(cmd: &image41::kitty::KittyCommand) -> u32 {
    cmd.image_id
}

fn place_kitty_image(
    image: image41::DecodedImage,
    cmd: &image41::kitty::KittyCommand,
    kitty_image_id: u32,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
) -> Result<(), &'static str> {
    if cmd.virtual_placement {
        return Err("ENOTSUP");
    }

    let image = image41::kitty::crop_source_rect(image, cmd);

    let id = *next_image_id;
    *next_image_id += 1;

    let (row, col, move_cursor) = placement_anchor(cmd, screen, viewport)?;
    let (display_width, display_height) = match (cmd.columns > 0, cmd.rows > 0) {
        (true, true) => (cmd.columns * cell_width, cmd.rows * cell_height),
        (true, false) => {
            let w = cmd.columns * cell_width;
            let h = if image.width > 0 {
                (image.height as u64 * w as u64 / image.width as u64) as u32
            } else {
                image.height
            };
            (w, h)
        }
        (false, true) => {
            let h = cmd.rows * cell_height;
            let w = if image.height > 0 {
                (image.width as u64 * h as u64 / image.height as u64) as u32
            } else {
                image.width
            };
            (w, h)
        }
        (false, false) => (image.width, image.height),
    };

    let image_rows = display_height.div_ceil(cell_height);

    screen.images.insert(
        id,
        PlacedImage {
            image,
            id,
            kitty_image_id: (kitty_image_id != 0).then_some(kitty_image_id),
            kitty_placement_id: (kitty_image_id != 0 && cmd.placement_id != 0)
                .then_some(cmd.placement_id),
            row,
            col,
            display_width,
            display_height,
            cell_x_offset: cmd.cell_x_offset,
            cell_y_offset: cmd.cell_y_offset,
            z_index: cmd.z_index,
            placed_at: Instant::now(),
        },
    );

    if move_cursor && !cmd.no_move_cursor {
        let advance_rows = image_rows;
        for _ in 0..advance_rows {
            screen.cursor.row += 1;
            if screen.cursor.row >= viewport.rows {
                screen.grid.push_visible_row(viewport);
                screen.cursor.row = viewport.rows - 1;
            }
        }
        screen.cursor.col = 0;
    }

    Ok(())
}

fn placement_anchor(
    cmd: &image41::kitty::KittyCommand,
    screen: &Screen,
    viewport: &Viewport,
) -> Result<(usize, u32, bool), &'static str> {
    if cmd.parent_image_id == 0 && cmd.parent_placement_id == 0 {
        return Ok((
            screen::active_row_index(screen, viewport),
            screen.cursor.col,
            true,
        ));
    }
    let Some(parent) = screen.images.values().find(|img| {
        img.kitty_image_id == Some(cmd.parent_image_id)
            && img.kitty_placement_id.unwrap_or(0) == cmd.parent_placement_id
    }) else {
        return Err("ENOPARENT");
    };

    let row = parent
        .row
        .saturating_add_signed(cmd.relative_row_offset as isize);
    let col = parent.col.saturating_add_signed(cmd.relative_col_offset);
    Ok((row, col, false))
}

fn send_kitty_response(
    cmd: &image41::kitty::KittyCommand,
    image_id: u32,
    ok: bool,
    message: &str,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    if cmd.quiet >= 2 {
        return;
    }
    if cmd.quiet >= 1 && ok {
        return;
    }
    let status = if ok { "OK" } else { message };
    let image_number = if cmd.image_number != 0 {
        format!(",I={}", cmd.image_number)
    } else {
        String::new()
    };
    let placement = if cmd.placement_id != 0 && image_id != 0 {
        format!(",p={}", cmd.placement_id)
    } else {
        String::new()
    };
    conformance::write_apc(
        pending_output,
        c1_mode,
        format_args!("Gi={image_id}{image_number}{placement};{status}"),
    );
}

fn handle_kitty_query(
    cmd: &image41::kitty::KittyCommand,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    if command_has_conflicting_ids(cmd) {
        send_kitty_response(
            cmd,
            response_image_id(cmd),
            false,
            "EINVAL",
            c1_mode,
            pending_output,
        );
        return;
    }
    let id = response_image_id(cmd);
    if let Some(message) = unsupported_transmission(cmd) {
        send_kitty_response(cmd, id, false, message, c1_mode, pending_output);
        return;
    }
    match decode_kitty_image(cmd) {
        Some(_) => send_kitty_response(cmd, id, true, "", c1_mode, pending_output),
        None => send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output),
    }
}

fn handle_kitty_transmit(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    if command_has_conflicting_ids(cmd) {
        send_kitty_response(
            cmd,
            response_image_id(cmd),
            false,
            "EINVAL",
            c1_mode,
            pending_output,
        );
        return;
    }
    if let Some(message) = unsupported_transmission(cmd) {
        send_kitty_response(
            cmd,
            response_image_id(cmd),
            false,
            message,
            c1_mode,
            pending_output,
        );
        return;
    }
    let id = store.resolve_transmission_id(cmd);
    match decode_kitty_image(cmd) {
        Some(image) => {
            if id != 0 {
                store.store(id, image);
            }
            send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
        }
        None => send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output),
    }
}

fn handle_kitty_transmit_display(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    if command_has_conflicting_ids(cmd) {
        send_kitty_response(
            cmd,
            response_image_id(cmd),
            false,
            "EINVAL",
            c1_mode,
            pending_output,
        );
        return;
    }
    if let Some(message) = unsupported_transmission(cmd) {
        send_kitty_response(
            cmd,
            response_image_id(cmd),
            false,
            message,
            c1_mode,
            pending_output,
        );
        return;
    }
    let id = store.resolve_transmission_id(cmd);
    match decode_kitty_image(cmd) {
        Some(image) => {
            if id != 0 {
                store.store(id, image.clone());
            }
            let placed = place_kitty_image(
                image,
                cmd,
                id,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
            match placed {
                Ok(()) => send_kitty_response(cmd, id, true, "", c1_mode, pending_output),
                Err(message) => {
                    send_kitty_response(cmd, id, false, message, c1_mode, pending_output)
                }
            }
        }
        None => send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output),
    }
}

fn handle_kitty_place(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    if command_has_conflicting_ids(cmd) {
        send_kitty_response(
            cmd,
            response_image_id(cmd),
            false,
            "EINVAL",
            c1_mode,
            pending_output,
        );
        return;
    }
    let Some(id) = store.resolve_existing_id(cmd) else {
        send_kitty_response(
            cmd,
            response_image_id(cmd),
            false,
            "ENOENT",
            c1_mode,
            pending_output,
        );
        return;
    };
    match store.get(id) {
        Some(image) => {
            let image = image.clone();
            let placed = place_kitty_image(
                image,
                cmd,
                id,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
            match placed {
                Ok(()) => send_kitty_response(cmd, id, true, "", c1_mode, pending_output),
                Err(message) => {
                    send_kitty_response(cmd, id, false, message, c1_mode, pending_output)
                }
            }
        }
        None => send_kitty_response(cmd, id, false, "ENOENT", c1_mode, pending_output),
    }
}

fn handle_kitty_delete(
    cmd: &image41::kitty::KittyCommand,
    screen: &mut Screen,
    viewport: &Viewport,
    store: &mut image41::kitty::KittyImageStore,
    chunked: &mut image41::kitty::ChunkedTransmission,
    cell_height: u32,
    cell_width: u32,
) {
    chunked.clear();
    let uppercase = cmd.delete.is_ascii_uppercase();
    match cmd.delete.to_ascii_lowercase() {
        b'a' | 0 => {
            screen.images.clear();
            if uppercase {
                store.clear();
            }
        }
        b'i' => {
            let id = cmd.image_id;
            if cmd.placement_id != 0 {
                screen.images.retain(|_, img| {
                    !(img.kitty_image_id == Some(id)
                        && img.kitty_placement_id == Some(cmd.placement_id))
                });
            } else {
                screen
                    .images
                    .retain(|_, img| img.kitty_image_id != Some(id));
            }
            if uppercase {
                store.remove(id);
            }
        }
        b'n' => {
            let Some(id) = store.resolve_existing_id(cmd) else {
                return;
            };
            if cmd.placement_id != 0 {
                screen.images.retain(|_, img| {
                    !(img.kitty_image_id == Some(id)
                        && img.kitty_placement_id == Some(cmd.placement_id))
                });
            } else {
                screen
                    .images
                    .retain(|_, img| img.kitty_image_id != Some(id));
            }
            if uppercase {
                store.remove(id);
            }
        }
        b'c' => {
            let cursor_row = screen::active_row_index(screen, viewport);
            let cursor_col = screen.cursor.col;
            screen.images.retain(|_, img| {
                !placement_intersects_cell(img, cursor_row, cursor_col, cell_height, cell_width)
            });
        }
        b'p' => {
            let row = viewport.top + cmd.src_y.saturating_sub(1) as usize;
            let col = cmd.src_x.saturating_sub(1);
            screen.images.retain(|_, img| {
                !placement_intersects_cell(img, row, col, cell_height, cell_width)
            });
        }
        b'q' => {
            let row = viewport.top + cmd.src_y.saturating_sub(1) as usize;
            let col = cmd.src_x.saturating_sub(1);
            screen.images.retain(|_, img| {
                img.z_index != cmd.z_index
                    || !placement_intersects_cell(img, row, col, cell_height, cell_width)
            });
        }
        b'r' => {
            let lo = cmd.src_x;
            let hi = cmd.src_y;
            screen.images.retain(|_, img| {
                img.kitty_image_id
                    .map(|id| id < lo || id > hi)
                    .unwrap_or(true)
            });
            if uppercase {
                store.remove_range(lo, hi);
            }
        }
        b'x' => {
            let col = cmd.src_x.saturating_sub(1);
            screen
                .images
                .retain(|_, img| !placement_intersects_col(img, col, cell_width));
        }
        b'y' => {
            let row = viewport.top + cmd.src_y.saturating_sub(1) as usize;
            screen
                .images
                .retain(|_, img| !placement_intersects_row(img, row, cell_height));
        }
        b'z' => {
            screen.images.retain(|_, img| img.z_index != cmd.z_index);
        }
        _ => {}
    }
}

fn placement_intersects_cell(
    img: &PlacedImage,
    row: usize,
    col: u32,
    cell_height: u32,
    cell_width: u32,
) -> bool {
    placement_intersects_row(img, row, cell_height)
        && placement_intersects_col(img, col, cell_width)
}

fn placement_intersects_row(
    img: &PlacedImage,
    row: usize,
    cell_height: u32,
) -> bool {
    let top = img.row as u64 * cell_height as u64 + img.cell_y_offset as u64;
    let bottom = top + img.display_height as u64;
    let cell_top = row as u64 * cell_height as u64;
    let cell_bottom = cell_top + cell_height as u64;
    top < cell_bottom && bottom > cell_top
}

fn placement_intersects_col(
    img: &PlacedImage,
    col: u32,
    cell_width: u32,
) -> bool {
    let left = img.col as u64 * cell_width as u64 + img.cell_x_offset as u64;
    let right = left + img.display_width as u64;
    let cell_left = col as u64 * cell_width as u64;
    let cell_right = cell_left + cell_width as u64;
    left < cell_right && right > cell_left
}

pub(crate) fn is_iterm_image_cmd(rest: &[u8]) -> bool {
    rest.starts_with(b"File=")
        || rest.starts_with(b"MultipartFile=")
        || rest.starts_with(b"FilePart=")
        || rest == b"FileEnd"
}

pub(crate) fn handle_iterm_graphics(
    rest: &[u8],
    chunked: &mut image41::iterm::ChunkedTransmission,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
) {
    if let Some(cmd) = image41::iterm::parse_file(rest) {
        if let Some(image) = image41::iterm::decode_payload(&cmd.payload) {
            place_iterm_image(
                cmd,
                image,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
        }
        return;
    }
    if let Some(header) = image41::iterm::parse_multipart_start(rest) {
        chunked.begin(header);
        return;
    }
    if let Some(chunk) = image41::iterm::parse_file_part(rest) {
        chunked.push(chunk);
        return;
    }
    if image41::iterm::is_file_end(rest)
        && let Some(cmd) = chunked.finish()
        && let Some(image) = image41::iterm::decode_payload(&cmd.payload)
    {
        place_iterm_image(
            cmd,
            image,
            screen,
            viewport,
            next_image_id,
            cell_height,
            cell_width,
        );
    }
}

fn place_iterm_image(
    cmd: image41::iterm::ItermCommand,
    image: image41::DecodedImage,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
) {
    if !cmd.inline {
        return;
    }

    let viewport_px_w = viewport.cols * cell_width;
    let viewport_px_h = viewport.rows * cell_height;

    let w_given = !matches!(cmd.width, image41::iterm::Dimension::Auto);
    let h_given = !matches!(cmd.height, image41::iterm::Dimension::Auto);

    let mut display_width = cmd.width.to_pixels(cell_width, viewport_px_w, image.width);
    let mut display_height = cmd
        .height
        .to_pixels(cell_height, viewport_px_h, image.height);

    if cmd.preserve_aspect_ratio && w_given != h_given && image.width > 0 && image.height > 0 {
        if w_given {
            display_height =
                (display_width as u64 * image.height as u64 / image.width as u64) as u32;
        } else {
            display_width =
                (display_height as u64 * image.width as u64 / image.height as u64) as u32;
        }
    }

    if display_width == 0 || display_height == 0 {
        return;
    }

    let id = *next_image_id;
    *next_image_id += 1;

    let row = screen::active_row_index(screen, viewport);
    let image_rows = display_height.div_ceil(cell_height);

    crate::image::remove_overlapping(
        &mut screen.images,
        row,
        image_rows.max(1) as usize,
        screen.cursor.col,
        cell_height,
    );

    screen.images.insert(
        id,
        PlacedImage {
            image,
            id,
            kitty_image_id: None,
            kitty_placement_id: None,
            row,
            col: screen.cursor.col,
            display_width,
            display_height,
            cell_x_offset: 0,
            cell_y_offset: 0,
            z_index: 0,
            placed_at: Instant::now(),
        },
    );

    if !cmd.do_not_move_cursor {
        for _ in 0..image_rows {
            screen.cursor.row += 1;
            if screen.cursor.row >= viewport.rows {
                screen.grid.push_visible_row(viewport);
                screen.cursor.row = viewport.rows - 1;
            }
        }
        screen.cursor.col = 0;
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::TestTerm;

    #[test]
    fn kitty_shared_memory_query_is_explicitly_unsupported() {
        let mut term = TestTerm::new_80x24();

        term.process(b"\x1b_Ga=q,t=s,i=7;AAAA\x1b\\");

        assert_eq!(term.take_pending_output(), b"\x1b_Gi=7;ENOTSUP\x1b\\");
    }

    #[test]
    fn kitty_shared_memory_transmit_does_not_allocate_image_number() {
        let mut term = TestTerm::new_80x24();

        term.process(b"\x1b_Ga=t,t=s,I=13;AAAA\x1b\\");
        term.process(b"\x1b_Ga=p,I=13\x1b\\");

        assert_eq!(
            term.take_pending_output(),
            b"\x1b_Gi=0,I=13;ENOTSUP\x1b\\\x1b_Gi=0,I=13;ENOENT\x1b\\"
        );
    }
}

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

    let cmd = image41::kitty::parse_command(&data[1..]);
    let cmd = match chunked.feed(cmd) {
        Some(cmd) => cmd,
        None => return,
    };

    match cmd.action {
        b'q' => handle_kitty_query(&cmd, store, c1_mode, pending_output),
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
        b'd' => handle_kitty_delete(&cmd, screen, store, cell_height),
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

fn place_kitty_image(
    image: image41::DecodedImage,
    cmd: &image41::kitty::KittyCommand,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
) {
    let image = image41::kitty::crop_source_rect(image, cmd);

    let id = *next_image_id;
    *next_image_id += 1;

    let row = screen::active_row_index(screen, viewport);
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
            row,
            col: screen.cursor.col,
            display_width,
            display_height,
            placed_at: Instant::now(),
        },
    );

    if !cmd.no_move_cursor {
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
    conformance::write_apc(
        pending_output,
        c1_mode,
        format_args!("Gi={image_id};{status}"),
    );
}

fn handle_kitty_query(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let id = store.resolve_id(cmd);
    send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
}

fn handle_kitty_transmit(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let id = store.resolve_id(cmd);
    match decode_kitty_image(cmd) {
        Some(image) => {
            store.store(id, image);
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
    let id = store.resolve_id(cmd);
    match decode_kitty_image(cmd) {
        Some(image) => {
            store.store(id, image.clone());
            place_kitty_image(
                image,
                cmd,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
            send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
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
    let id = store.resolve_id(cmd);
    match store.get(id) {
        Some(image) => {
            let image = image.clone();
            place_kitty_image(
                image,
                cmd,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
            );
            send_kitty_response(cmd, id, true, "", c1_mode, pending_output);
        }
        None => send_kitty_response(cmd, id, false, "ENOENT", c1_mode, pending_output),
    }
}

fn handle_kitty_delete(
    cmd: &image41::kitty::KittyCommand,
    screen: &mut Screen,
    store: &mut image41::kitty::KittyImageStore,
    cell_height: u32,
) {
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
                if let Some(stored) = store.get(id) {
                    let (sw, sh) = (stored.width, stored.height);
                    screen
                        .images
                        .retain(|_, img| img.image.width != sw || img.image.height != sh);
                }
            } else if let Some(stored) = store.get(id) {
                let (sw, sh) = (stored.width, stored.height);
                screen
                    .images
                    .retain(|_, img| img.image.width != sw || img.image.height != sh);
            }
            if uppercase {
                store.remove(id);
            }
        }
        b'c' => {
            let cursor_row = screen.grid.active_row_index(
                &screen.cursor,
                &Viewport {
                    rows: screen.grid.rows.len() as u32,
                    cols: 0,
                    top: 0,
                },
            );
            let cursor_col = screen.cursor.col;
            screen.images.retain(|_, img| {
                if img.col != cursor_col {
                    return true;
                }
                let img_rows = img.image.height.div_ceil(cell_height).max(1) as usize;
                let img_bottom = img.row + img_rows;
                !(img.row <= cursor_row && cursor_row < img_bottom)
            });
        }
        b'r' => {
            let lo = cmd.src_x;
            let hi = cmd.src_y;
            if let Some(lo_stored) = store.get(lo) {
                let _ = lo_stored;
            }
            if uppercase {
                store.remove_range(lo, hi);
            }
        }
        _ => {}
    }
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
            row,
            col: screen.cursor.col,
            display_width,
            display_height,
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

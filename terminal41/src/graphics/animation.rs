use super::*;

#[cfg(test)]
mod tests;

pub(super) fn existing_image_id(
    cmd: &image41::kitty::KittyCommand,
    store: &image41::kitty::KittyImageStore,
) -> Result<u32, &'static str> {
    if command_has_conflicting_ids(cmd) || (cmd.image_id == 0 && cmd.image_number == 0) {
        return Err("EINVAL");
    }
    store
        .resolve_existing_id(cmd)
        .filter(|id| store.get(*id).is_some())
        .ok_or("ENOENT")
}

pub(super) fn handle_frame(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    screen: &mut Screen,
    viewport: &Viewport,
    next_image_id: &mut u64,
    cell_height: u32,
    cell_width: u32,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
    limits: TerminalLimits,
    file_requests: &mut Vec<KittyFileRequest>,
    file_permission: PermissionPolicy,
    palette: &ColorPalette,
) {
    let id = match existing_image_id(cmd, store) {
        Ok(id) => id,
        Err(message) => {
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
    };
    if let Some(message) = unsupported_transmission(cmd) {
        send_kitty_response(cmd, id, false, message, c1_mode, pending_output);
        return;
    }
    if !matches!(cmd.format, 24 | 32 | 100)
        || !matches!(cmd.transmission, b'd' | b'f' | b't')
        || !matches!(cmd.compression, 0 | b'z')
    {
        send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output);
        return;
    }
    let target = store.frame_identity(id, cmd.rows);
    let base = if target.is_none() && cmd.columns != 0 {
        let Some(base) = store.frame_identity(id, cmd.columns) else {
            send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output);
            return;
        };
        Some(base)
    } else {
        None
    };
    let action = KittyFileAction::Frame {
        image_id: id,
        generation: store.image_generation(id),
        target,
        base,
    };
    match make_file_request(cmd, action, c1_mode, limits) {
        Ok(Some(mut request)) => {
            // An append must remain an append if other frames arrive while
            // this request waits for file permission.
            if target.is_none() {
                request.cmd.rows = 0;
            }
            handle_file_request(
                request,
                file_permission,
                store,
                screen,
                viewport,
                next_image_id,
                cell_height,
                cell_width,
                pending_output,
                file_requests,
                palette,
            );
        }
        Ok(None) => {
            let decoded = decode_kitty_image(cmd, limits);
            finish_frame(cmd, id, decoded, store, limits, c1_mode, pending_output);
        }
        Err(message) => send_kitty_response(cmd, id, false, message, c1_mode, pending_output),
    }
}

pub(super) fn finish_frame(
    cmd: &image41::kitty::KittyCommand,
    id: u32,
    decoded: Option<image41::DecodedImage>,
    store: &mut image41::kitty::KittyImageStore,
    limits: TerminalLimits,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let Some(image) = decoded else {
        send_kitty_response(cmd, id, false, "EINVAL", c1_mode, pending_output);
        return;
    };
    match store.transmit_frame(
        id,
        cmd,
        image,
        limits.kitty_graphics_storage_bytes,
        Instant::now(),
    ) {
        Ok(number) => {
            let mut response = cmd.clone();
            response.rows = number;
            send_kitty_response(&response, id, true, "", c1_mode, pending_output);
        }
        Err(error) => send_kitty_response(cmd, id, false, error.code(), c1_mode, pending_output),
    }
}

pub(super) fn handle_control(
    cmd: &image41::kitty::KittyCommand,
    store: &mut image41::kitty::KittyImageStore,
    c1_mode: C1Mode,
    pending_output: &mut Vec<u8>,
) {
    let id = match existing_image_id(cmd, store) {
        Ok(id) => id,
        Err(message) => {
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
    };
    let result = if cmd.action == b'a' {
        store.control_animation(id, cmd, Instant::now())
    } else {
        store.compose_frame(id, cmd)
    };
    match result {
        Ok(()) if cmd.action == b'c' => {
            send_kitty_response(cmd, id, true, "", c1_mode, pending_output)
        }
        Ok(()) => {}
        Err(error) => send_kitty_response(cmd, id, false, error.code(), c1_mode, pending_output),
    }
}

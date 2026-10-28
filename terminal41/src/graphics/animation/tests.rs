use std::time::Duration;
use std::time::Instant;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::test_support::TestTerm;

fn send(
    term: &mut TestTerm,
    keys: &str,
    pixels: &[u8],
) {
    term.process(format!("\x1b_G{keys};{}\x1b\\", BASE64.encode(pixels)).as_bytes());
}

fn visible(
    term: &TestTerm,
    now: Instant,
) -> Vec<crate::VisibleImage> {
    crate::view::visible_images(
        term.active(),
        term.viewport(),
        term.cell_height(),
        term.cell_width(),
        term.kitty_images(),
        term.palette(),
        now,
    )
    .collect()
}

fn seed(term: &mut TestTerm) {
    send(
        term,
        "a=T,i=7,s=2,v=1,C=1",
        &[255, 0, 0, 255, 255, 0, 0, 255],
    );
    term.take_pending_output();
}

#[test]
fn physical_and_virtual_crops_have_distinct_renderer_cache_keys() {
    let mut term = TestTerm::new_80x24();
    send(
        &mut term,
        "a=T,i=7,s=2,v=1,x=1,w=1,C=1",
        &[255, 0, 0, 255, 0, 0, 255, 255],
    );
    term.process(b"\x1b_Ga=p,i=7,U=1,c=2,r=1\x1b\\\x1b[2;1H");
    term.process("\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}".as_bytes());
    let images = visible(&term, Instant::now());
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].image.width, 1);
    assert_eq!(images[1].image.width, 2);
    assert_ne!(images[0].id, images[1].id);
}

#[test]
fn replacing_root_discards_old_virtual_animation_placements() {
    let mut term = TestTerm::new_80x24();
    send(&mut term, "a=T,i=7,s=1,v=1,U=1,c=1,r=1", &[255, 0, 0, 255]);
    term.process("\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}".as_bytes());
    assert_eq!(visible(&term, Instant::now()).len(), 1);
    send(&mut term, "a=t,i=7,s=1,v=1", &[0, 0, 255, 255]);
    assert!(visible(&term, Instant::now()).is_empty());
}

#[test]
fn animation_updates_every_placement_and_preserves_crop_and_cursor() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    term.process(b"\x1b[2;1H\x1b_Ga=p,i=7,x=1,w=1,C=1\x1b\\");
    let before = visible(&term, Instant::now());
    assert_eq!(before.len(), 2);
    let cursor = (term.active.cursor.row, term.active.cursor.col);
    send(&mut term, "a=f,i=7,r=1,s=1,v=1,x=1,X=1", &[0, 255, 0, 128]);
    let edited = visible(&term, Instant::now());
    assert_eq!(
        edited[0].image.frames[0].pixels,
        [255, 0, 0, 255, 0, 255, 0, 128]
    );
    assert_eq!(edited[1].image.frames[0].pixels, [0, 255, 0, 128]);
    assert_eq!(
        before[0].image.frames[0].pixels,
        [255, 0, 0, 255, 255, 0, 0, 255]
    );
    assert_ne!(before[0].id, edited[0].id);
    assert_ne!(before[1].id, edited[1].id);

    send(
        &mut term,
        "a=f,i=7,s=2,v=1,z=10",
        &[0, 0, 255, 255, 0, 0, 255, 255],
    );
    term.process(b"\x1b_Ga=a,i=7,c=2\x1b\\");
    let selected = visible(&term, Instant::now());
    assert_eq!(
        selected[0].image.frames[0].pixels,
        [0, 0, 255, 255, 0, 0, 255, 255]
    );
    assert_eq!(selected[1].image.frames[0].pixels, [0, 0, 255, 255]);
    assert!(selected.iter().all(|image| image.next_frame_in.is_none()));
    assert_eq!((term.active.cursor.row, term.active.cursor.col), cursor);
    assert_eq!(
        term.take_pending_output(),
        b"\x1b_Gi=7;OK\x1b\\\x1b_Gi=7,r=1;OK\x1b\\\x1b_Gi=7,r=2;OK\x1b\\"
    );
}

#[test]
fn chunked_compressed_rgb_frame_is_atomic_and_preserves_first_chunk_keys() {
    use std::io::Write;
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&[0, 255, 0, 0, 255, 0]).unwrap();
    let encoded = BASE64.encode(encoder.finish().unwrap());
    let first = format!(
        "\x1b_Ga=f,i=7,f=24,s=2,v=1,o=z,z=50,m=1;{}\x1b\\",
        &encoded[..4]
    );
    for byte in first.bytes() {
        term.process(&[byte]);
    }
    assert!(term.take_pending_output().is_empty());
    term.process(format!("\x1b_Ga=f,m=0;{}\x1b\\", &encoded[4..]).as_bytes());
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,r=2;OK\x1b\\");
    term.process(b"\x1b_Ga=a,i=7,c=2\x1b\\");
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [0, 255, 0, 255, 0, 255, 0, 255]
    );
}

#[test]
fn animation_control_reaches_unicode_placeholders_and_stops_scheduling_at_loop_end() {
    let mut term = TestTerm::new_80x24();
    send(&mut term, "a=T,i=7,s=1,v=1,U=1,c=1,r=1", &[255, 0, 0, 255]);
    term.process("\x1b[38;5;7m\u{10eeee}\u{0305}\u{0305}".as_bytes());
    send(&mut term, "a=f,i=7,s=1,v=1,z=100", &[0, 0, 255, 255]);
    term.process(b"\x1b_Ga=a,i=7,r=1,z=100,s=3,v=2\x1b\\");
    let now = Instant::now();
    let running = visible(&term, now);
    assert_eq!(running.len(), 1);
    assert!(running[0].next_frame_in.is_some());
    let finished = visible(&term, now + Duration::from_secs(1));
    assert_eq!(finished[0].image.frames[0].pixels, [0, 0, 255, 255]);
    assert!(finished[0].next_frame_in.is_none());
    assert_ne!(running[0].id, finished[0].id);
}

#[test]
fn png_frames_and_composition_work_through_the_protocol() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    let mut png = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0, 0, 255, 255])
            .unwrap();
    }
    send(&mut term, "a=f,i=7,f=100,c=1,x=1", &png);
    term.process(b"\x1b_Ga=c,i=7,r=2,c=1,w=1,h=1,X=1,x=0,C=1\x1b\\");
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [0, 0, 255, 255, 255, 0, 0, 255]
    );
    assert_eq!(
        term.take_pending_output(),
        b"\x1b_Gi=7,r=2;OK\x1b\\\x1b_Gi=7;OK\x1b\\"
    );
    term.process(b"\x1b_Ga=c,i=7,r=1,c=1\x1b\\");
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7;EINVAL\x1b\\");
}

#[test]
fn image_numbers_errors_quiet_modes_and_c1_replies() {
    let mut term = TestTerm::new_80x24();
    send(&mut term, "a=t,I=13,s=1,v=1", &[255, 0, 0, 255]);
    term.take_pending_output();
    send(&mut term, "a=f,I=13,s=1,v=1,q=1", &[0, 0, 255, 255]);
    assert!(term.take_pending_output().is_empty());
    term.process(b"\x1b_Ga=c,I=13,c=99,r=1,q=1\x1b\\");
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=1,I=13;ENOENT\x1b\\");
    term.process(b"\x1b_Ga=a,i=99,c=2,q=2\x1b\\");
    assert!(term.take_pending_output().is_empty());
    term.process(b"\x1b G");
    send(&mut term, "a=f,I=13,s=1,v=1", &[0, 255, 0, 255]);
    assert_eq!(term.take_pending_output(), b"\x9fGi=1,I=13,r=3;OK\x9c");
    term.process(b"\x1b_Ga=a,i=1,I=13\x1b\\\x1b_Ga=f,i=1,t=s\x1b\\");
    assert_eq!(
        term.take_pending_output(),
        b"\x9fGi=1,I=13;EINVAL\x9c\x9fGi=1;ENOTSUP\x9c"
    );
}

#[test]
fn invalid_animation_selectors_are_silent_and_valid_controls_still_apply() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(&mut term, "a=f,i=7,s=2,v=1", &[0, 0, 255, 255].repeat(2));
    term.take_pending_output();
    term.process(b"\x1b_Ga=a,i=7,r=99,c=2,s=1\x1b\\");
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [0, 0, 255, 255].repeat(2)
    );
    term.process(b"\x1b_Ga=a,i=7,c=99,r=1,z=100,s=3,v=2\x1b\\");
    let finished = visible(&term, Instant::now() + Duration::from_secs(1));
    assert_eq!(
        finished[0].image.frames[0].pixels,
        [0, 0, 255, 255].repeat(2)
    );
    assert!(finished[0].next_frame_in.is_none());
    assert!(term.take_pending_output().is_empty());
}

#[test]
fn invalid_upload_base_returns_einval_without_queuing_file_requests() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(&mut term, "a=f,i=7,c=99,s=1,v=1", &[0, 0, 255, 255]);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7;EINVAL\x1b\\");
    send(&mut term, "a=f,i=7,c=99,s=1,v=1,t=f", b"/unused.rgba");
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7;EINVAL\x1b\\");
    assert!(term.effects.kitty_file_requests.is_empty());
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [255, 0, 0, 255].repeat(2)
    );
}

#[test]
fn frame_edits_ignore_the_base_selector() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(
        &mut term,
        "a=f,i=7,r=1,c=99,s=1,v=1,x=1,X=1",
        &[0, 0, 255, 255],
    );
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,r=1;OK\x1b\\");
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [255, 0, 0, 255, 0, 0, 255, 255]
    );
}

#[test]
fn delete_frame_cancels_partial_upload_and_uppercase_removes_last_frame() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(
        &mut term,
        "a=f,i=7,s=2,v=1",
        &[0, 0, 255, 255, 0, 0, 255, 255],
    );
    term.take_pending_output();
    term.process(b"\x1b_Ga=f,i=7,s=2,v=1,m=1;AAAA\x1b\\\x1b_Ga=d,d=f,i=7,r=1\x1b\\");
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [0, 0, 255, 255, 0, 0, 255, 255]
    );
    send(
        &mut term,
        "a=f,i=7,s=2,v=1",
        &[0, 255, 0, 255, 0, 255, 0, 255],
    );
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,r=2;OK\x1b\\");
    term.process(b"\x1b_Ga=d,d=F,i=7,r=2\x1b\\\x1b_Ga=d,d=f,i=7\x1b\\");
    assert_eq!(visible(&term, Instant::now()).len(), 1);
    term.process(b"\x1b_Ga=d,d=F,i=7\x1b\\");
    assert!(visible(&term, Instant::now()).is_empty());
    assert!(term.kitty_images().get(7).is_none());
}

#[test]
fn frame_delete_with_conflicting_ids_replies_without_deleting_the_image() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    term.process(b"\x1b_Ga=d,d=F,i=7,I=8\x1b\\");
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,I=8;EINVAL\x1b\\");
    assert!(term.kitty_images().get(7).is_some());
}

#[test]
fn frame_quota_failure_preserves_the_image_and_deletion_returns_capacity() {
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    crate::settings::set_terminal_limits(
        &mut term.inner.protocol,
        config41::TerminalLimits {
            kitty_graphics_storage_bytes: 16,
            ..Default::default()
        },
    );
    send(&mut term, "a=f,i=7,s=2,v=1", &[0; 8]);
    term.take_pending_output();
    send(&mut term, "a=f,i=7,s=2,v=1", &[0; 8]);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7;ENOSPC\x1b\\");
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [255, 0, 0, 255, 255, 0, 0, 255]
    );
    term.process(b"\x1b_Ga=d,d=f,i=7,r=2\x1b\\");
    send(&mut term, "a=f,i=7,s=2,v=1", &[0; 8]);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,r=2;OK\x1b\\");
}

#[test]
fn frame_edits_reach_completed_blocks_and_stale_placements_do_not_bind_reused_ids() {
    let mut term = TestTerm::new_80x24();
    term.process(b"\x1b]133;A\x07prompt\x1b]133;B\x07cmd\x1b]133;C\x07\r\n");
    seed(&mut term);
    term.process(b"\r\n\x1b]133;D;0\x07\x1b]133;A\x07next");
    assert!(!term.active.scrollback_blocks.is_empty());
    send(
        &mut term,
        "a=f,i=7,r=1,s=2,v=1,X=1",
        &[0, 255, 0, 255, 0, 255, 0, 255],
    );
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [0, 255, 0, 255, 0, 255, 0, 255]
    );
    send(&mut term, "a=t,i=7,s=1,v=1", &[0, 0, 255, 255]);
    assert!(visible(&term, Instant::now()).is_empty());
}

#[test]
fn file_frame_uses_existing_permission_flow_and_rejects_replaced_targets() {
    let path = std::env::temp_dir().join(format!("term41-animation-{}.rgba", std::process::id()));
    std::fs::write(&path, [0, 255, 0, 255]).unwrap();
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(
        &mut term,
        "a=f,i=7,t=f,s=1,v=1,c=1",
        path.to_str().unwrap().as_bytes(),
    );
    assert!(term.take_pending_output().is_empty());
    let request = term.effects.kitty_file_requests.pop().unwrap();
    let effects = term.inner.apply_kitty_file_request(request);
    term.effects.extend(effects);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,r=2;OK\x1b\\");
    send(
        &mut term,
        "a=f,i=7,t=f,s=1,v=1",
        path.to_str().unwrap().as_bytes(),
    );
    let request = term.effects.kitty_file_requests.pop().unwrap();
    seed(&mut term);
    let effects = term.inner.apply_kitty_file_request(request);
    term.effects.extend(effects);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7;ENOENT\x1b\\");
    std::fs::remove_file(path).unwrap();
}

#[test]
fn deferred_file_frame_rejects_renumbered_targets() {
    let path = std::env::temp_dir().join(format!(
        "term41-animation-renumber-{}.rgba",
        std::process::id()
    ));
    std::fs::write(&path, [0, 255, 0, 255]).unwrap();
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(
        &mut term,
        "a=f,i=7,s=2,v=1",
        &[0, 0, 255, 255, 0, 0, 255, 255],
    );
    send(&mut term, "a=f,i=7,s=2,v=1", &[255; 8]);
    term.take_pending_output();
    send(
        &mut term,
        "a=f,i=7,r=2,t=f,s=1,v=1",
        path.to_str().unwrap().as_bytes(),
    );
    let request = term.effects.kitty_file_requests.pop().unwrap();
    term.process(b"\x1b_Ga=d,d=f,i=7,r=1\x1b\\");
    let effects = term.inner.apply_kitty_file_request(request);
    term.effects.extend(effects);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,r=2;ENOENT\x1b\\");
    term.process(b"\x1b_Ga=a,i=7,c=2\x1b\\");
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [255; 8]
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn deferred_file_frame_rejects_a_deleted_base() {
    let path =
        std::env::temp_dir().join(format!("term41-animation-base-{}.rgba", std::process::id()));
    std::fs::write(&path, [0, 255, 0, 255]).unwrap();
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(&mut term, "a=f,i=7,s=2,v=1", &[0, 0, 255, 255].repeat(2));
    term.take_pending_output();
    send(
        &mut term,
        "a=f,i=7,c=1,t=f,s=1,v=1",
        path.to_str().unwrap().as_bytes(),
    );
    let request = term.effects.kitty_file_requests.pop().unwrap();
    term.process(b"\x1b_Ga=d,d=f,i=7,r=1\x1b\\");
    let effects = term.inner.apply_kitty_file_request(request);
    term.effects.extend(effects);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7;ENOENT\x1b\\");
    assert!(term.kitty_images().frame_identity(7, 2).is_none());
    assert_eq!(
        visible(&term, Instant::now())[0].image.frames[0].pixels,
        [0, 0, 255, 255].repeat(2)
    );
    std::fs::remove_file(path).unwrap();
}

#[test]
fn deferred_file_frames_preserve_append_intent_and_allow_successive_edits() {
    let path = std::env::temp_dir().join(format!(
        "term41-animation-queue-{}.rgba",
        std::process::id()
    ));
    std::fs::write(&path, [0, 255, 0, 255]).unwrap();
    let mut term = TestTerm::new_80x24();
    seed(&mut term);
    send(
        &mut term,
        "a=f,i=7,r=2,t=f,s=1,v=1,c=1",
        path.to_str().unwrap().as_bytes(),
    );
    let request = term.effects.kitty_file_requests.pop().unwrap();
    send(&mut term, "a=f,i=7,s=1,v=1", &[0, 0, 255, 255]);
    term.take_pending_output();
    let effects = term.inner.apply_kitty_file_request(request);
    term.effects.extend(effects);
    assert_eq!(term.take_pending_output(), b"\x1b_Gi=7,r=3;OK\x1b\\");
    for _ in 0..2 {
        send(
            &mut term,
            "a=f,i=7,r=2,c=99,t=f,s=1,v=1",
            path.to_str().unwrap().as_bytes(),
        );
    }
    let requests = std::mem::take(&mut term.effects.kitty_file_requests);
    for request in requests {
        let effects = term.inner.apply_kitty_file_request(request);
        term.effects.extend(effects);
    }
    assert_eq!(
        term.take_pending_output(),
        b"\x1b_Gi=7,r=2;OK\x1b\\\x1b_Gi=7,r=2;OK\x1b\\"
    );
    std::fs::remove_file(path).unwrap();
}

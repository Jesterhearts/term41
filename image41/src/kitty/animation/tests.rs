use super::*;
use crate::kitty::parse_command;

fn pixel(rgba: [u8; 4]) -> DecodedImage {
    DecodedImage::single_frame(1, 1, rgba.to_vec())
}

fn store() -> KittyImageStore {
    let mut store = KittyImageStore::new();
    store.store(7, pixel([255, 0, 0, 255]), 1024).unwrap();
    store
}

fn control(
    store: &mut KittyImageStore,
    keys: &str,
    now: Instant,
) {
    store
        .control_animation(7, &parse_command(keys.as_bytes()), now)
        .unwrap();
}

fn append(
    store: &mut KittyImageStore,
    rgba: [u8; 4],
    keys: &str,
    now: Instant,
) -> u32 {
    store
        .transmit_frame(7, &parse_command(keys.as_bytes()), pixel(rgba), 1024, now)
        .unwrap()
}

fn pixels(
    store: &KittyImageStore,
    now: Instant,
) -> &[u8] {
    &store.presentation(7, now).unwrap().image.frames[0].pixels
}

#[test]
fn frames_blend_over_background_and_snapshot_their_base() {
    let now = Instant::now();
    let mut store = KittyImageStore::new();
    store
        .store(
            7,
            DecodedImage::single_frame(2, 1, [255, 0, 0, 255].repeat(2)),
            1024,
        )
        .unwrap();
    append(&mut store, [0, 0, 255, 128], "c=1,x=1", now);
    control(&mut store, "c=2", now);
    assert_eq!(pixels(&store, now), [255, 0, 0, 255, 127, 0, 128, 255]);

    append(&mut store, [0, 255, 0, 255], "r=1", now);
    assert_eq!(pixels(&store, now), [255, 0, 0, 255, 127, 0, 128, 255]);
    append(&mut store, [10, 20, 30, 0], "r=2,x=1,X=1", now);
    assert_eq!(pixels(&store, now), [255, 0, 0, 255, 10, 20, 30, 0]);

    append(&mut store, [0, 0, 255, 128], "Y=4278190335", now);
    control(&mut store, "c=3", now);
    assert_eq!(pixels(&store, now), [127, 0, 128, 255, 255, 0, 0, 255]);
}

#[test]
fn composition_checks_bounds_and_overlap_before_mutation() {
    let now = Instant::now();
    let mut store = KittyImageStore::new();
    store
        .store(
            7,
            DecodedImage::single_frame(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]),
            1024,
        )
        .unwrap();
    let original = store.presentation(7, now).unwrap().revision;
    for keys in [
        "r=1,c=1",
        "r=1,c=1,w=1,x=4294967295",
        "r=1,c=1,w=3",
        "r=1,c=3",
    ] {
        assert!(
            store
                .compose_frame(7, &parse_command(keys.as_bytes()))
                .is_err()
        );
        assert_eq!(store.presentation(7, now).unwrap().revision, original);
    }
    store
        .compose_frame(7, &parse_command(b"r=1,c=1,w=1,X=1,x=0"))
        .unwrap();
    assert_eq!(pixels(&store, now), [127, 0, 128, 255, 0, 0, 255, 128]);
    store
        .compose_frame(7, &parse_command(b"r=1,c=1,w=1,X=1,x=0,C=1"))
        .unwrap();
    assert_eq!(pixels(&store, now), [0, 0, 255, 128, 0, 0, 255, 128]);
    assert_ne!(store.presentation(7, now).unwrap().revision, original);
}

#[test]
fn terminal_playback_skips_gapless_frames_and_honors_loop_limit() {
    let now = Instant::now();
    let mut store = store();
    append(&mut store, [0, 255, 0, 255], "z=10", now);
    append(&mut store, [0, 0, 255, 255], "z=20", now);
    control(&mut store, "s=3,v=3", now);
    for (ms, expected) in [
        (0, [0, 255, 0, 255]),
        (10, [0, 0, 255, 255]),
        (30, [0, 255, 0, 255]),
        (40, [0, 0, 255, 255]),
        (60, [0, 0, 255, 255]),
        (1000, [0, 0, 255, 255]),
    ] {
        assert_eq!(pixels(&store, now + Duration::from_millis(ms)), expected);
    }
    assert_eq!(
        store
            .presentation(7, now + Duration::from_millis(9))
            .unwrap()
            .next_frame_in,
        Some(Duration::from_millis(1))
    );
    assert!(
        store
            .presentation(7, now + Duration::from_millis(60))
            .unwrap()
            .next_frame_in
            .is_none()
    );
    control(&mut store, "s=1,c=1", now + Duration::from_secs(1));
    assert_eq!(
        pixels(&store, now + Duration::from_secs(2)),
        [255, 0, 0, 255]
    );
    control(&mut store, "s=3,v=1", now + Duration::from_secs(2));
    assert_eq!(
        pixels(&store, now + Duration::from_secs(86402)),
        [0, 255, 0, 255]
    );
}

#[test]
fn root_gap_and_stop_resume_use_one_shared_clock() {
    let now = Instant::now();
    let mut store = store();
    append(&mut store, [0, 255, 0, 255], "z=20", now);
    control(&mut store, "r=1,z=10,s=3", now);
    assert_eq!(
        pixels(&store, now + Duration::from_millis(9)),
        [255, 0, 0, 255]
    );
    let stop = now + Duration::from_millis(15);
    control(&mut store, "s=1", stop);
    assert_eq!(
        pixels(&store, now + Duration::from_secs(1)),
        [0, 255, 0, 255]
    );
    assert!(store.presentation(7, stop).unwrap().next_frame_in.is_none());
    control(&mut store, "s=3", now + Duration::from_secs(2));
    assert_eq!(
        pixels(&store, now + Duration::from_millis(2020)),
        [255, 0, 0, 255]
    );
}

#[test]
fn reselecting_current_frame_preserves_its_remaining_gap() {
    let now = Instant::now();
    let mut store = store();
    append(&mut store, [0, 255, 0, 255], "z=10", now);
    control(&mut store, "r=1,z=20,s=3", now);
    for (ms, selector, expected, remaining) in [
        (5, "c=1", [255, 0, 0, 255], 15),
        (25, "c=2", [0, 255, 0, 255], 5),
    ] {
        let selected_at = now + Duration::from_millis(ms);
        control(&mut store, selector, selected_at);
        assert_eq!(pixels(&store, selected_at), expected);
        assert_eq!(
            store.presentation(7, selected_at).unwrap().next_frame_in,
            Some(Duration::from_millis(remaining))
        );
    }
    control(&mut store, "s=1,c=2", now + Duration::from_millis(26));
    let later = now + Duration::from_secs(1);
    assert_eq!(pixels(&store, later), [0, 255, 0, 255]);
    assert!(
        store
            .presentation(7, later)
            .unwrap()
            .next_frame_in
            .is_none()
    );
    control(&mut store, "s=3,c=2", later);
    assert_eq!(
        store.presentation(7, later).unwrap().next_frame_in,
        Some(Duration::from_millis(10))
    );
    assert_eq!(
        pixels(&store, later + Duration::from_millis(10)),
        [255, 0, 0, 255]
    );
}

#[test]
fn loading_waits_without_redraws_and_resumes_when_a_frame_arrives() {
    let now = Instant::now();
    let mut store = store();
    control(&mut store, "s=2", now);
    append(&mut store, [0, 255, 0, 255], "z=10", now);
    let later = now + Duration::from_secs(100);
    assert_eq!(pixels(&store, later), [0, 255, 0, 255]);
    assert!(
        store
            .presentation(7, later)
            .unwrap()
            .next_frame_in
            .is_none()
    );
    append(&mut store, [0, 0, 255, 255], "z=20", later);
    assert_eq!(pixels(&store, later), [0, 0, 255, 255]);
    control(&mut store, "s=3", later);
    assert_eq!(
        pixels(&store, later + Duration::from_millis(20)),
        [0, 255, 0, 255]
    );
}

#[test]
fn zero_gap_animation_does_not_spin() {
    let now = Instant::now();
    let mut store = store();
    append(&mut store, [0, 255, 0, 255], "z=-1", now);
    control(&mut store, "s=3", now);
    assert!(
        store
            .presentation(7, now + Duration::from_secs(100))
            .unwrap()
            .next_frame_in
            .is_none()
    );
    assert_eq!(pixels(&store, now), [255, 0, 0, 255]);
    control(&mut store, "c=2,s=1", now);
    assert_eq!(pixels(&store, now), [0, 255, 0, 255]);
}

#[test]
fn invalid_control_frame_numbers_do_not_block_valid_controls() {
    let now = Instant::now();
    let mut store = store();
    append(&mut store, [0, 255, 0, 255], "z=10", now);
    control(&mut store, "c=99,r=1,z=20,s=3,v=2", now);
    assert_eq!(pixels(&store, now), [255, 0, 0, 255]);
    assert_eq!(
        store.presentation(7, now).unwrap().next_frame_in,
        Some(Duration::from_millis(20))
    );
    let finished = now + Duration::from_millis(30);
    assert_eq!(pixels(&store, finished), [0, 255, 0, 255]);
    assert!(
        store
            .presentation(7, finished)
            .unwrap()
            .next_frame_in
            .is_none()
    );

    control(&mut store, "r=99,z=-1,c=1,s=3,v=1", finished);
    let later = finished + Duration::from_millis(35);
    assert_eq!(pixels(&store, later), [255, 0, 0, 255]);
    assert_eq!(
        store.presentation(7, later).unwrap().next_frame_in,
        Some(Duration::from_millis(15))
    );
    control(&mut store, "r=99,c=99,s=1", later);
    assert_eq!(
        pixels(&store, later + Duration::from_secs(1)),
        [255, 0, 0, 255]
    );
    assert!(
        store
            .presentation(7, later)
            .unwrap()
            .next_frame_in
            .is_none()
    );
}

#[test]
fn stopping_a_loading_animation_freezes_the_visible_frame_not_a_gapless_tail() {
    let now = Instant::now();
    let mut store = store();
    append(&mut store, [0, 255, 0, 255], "z=-1", now);
    control(&mut store, "s=2,r=1,z=10", now);
    let later = now + Duration::from_secs(1);
    assert_eq!(pixels(&store, later), [255, 0, 0, 255]);
    control(&mut store, "s=1", later);
    assert_eq!(pixels(&store, later), [255, 0, 0, 255]);
    assert!(
        store
            .presentation(7, later)
            .unwrap()
            .next_frame_in
            .is_none()
    );
}

#[test]
fn deletion_promotes_root_renumbers_frames_and_releases_quota() {
    let now = Instant::now();
    let mut store = store();
    append(&mut store, [0, 255, 0, 255], "", now);
    append(&mut store, [0, 0, 255, 255], "", now);
    control(&mut store, "c=3", now);
    let revision = store.presentation(7, now).unwrap().revision;
    store.delete_frame(7, 1, false, now).unwrap();
    assert_eq!(store.get(7).unwrap().frames[0].pixels, [0, 255, 0, 255]);
    assert_eq!(store.presentation(7, now).unwrap().revision, revision);
    store.delete_frame(7, 99, false, now).unwrap();
    assert_eq!(pixels(&store, now), [0, 255, 0, 255]);
    store.delete_frame(7, 0, false, now).unwrap();
    assert!(store.get(7).is_some());
    store.store(8, pixel([1; 4]), 8).unwrap();
    store.delete_frame(7, 1, true, now).unwrap();
    assert!(store.get(7).is_none());
    store.store(9, pixel([2; 4]), 8).unwrap();
}

#[test]
fn quota_and_invalid_patch_fail_without_changing_existing_frames() {
    let now = Instant::now();
    let mut store = store();
    let cmd = parse_command(b"a=f");
    let old = store.presentation(7, now).unwrap().revision;
    assert_eq!(
        store.transmit_frame(7, &cmd, pixel([0; 4]), 7, now),
        Err(KittyAnimationError::StorageLimitExceeded)
    );
    assert_eq!(store.presentation(7, now).unwrap().revision, old);
    assert_eq!(
        store.transmit_frame(7, &cmd, DecodedImage::single_frame(1, 1, vec![]), 1024, now),
        Err(KittyAnimationError::InvalidArgument)
    );
    assert_eq!(
        store.transmit_frame(7, &parse_command(b"x=4294967295"), pixel([0; 4]), 1024, now),
        Err(KittyAnimationError::InvalidArgument)
    );
    assert_eq!(
        store.transmit_frame(7, &parse_command(b"c=5"), pixel([0; 4]), 1024, now),
        Err(KittyAnimationError::InvalidArgument)
    );
    assert_eq!(store.presentation(7, now).unwrap().revision, old);
    store
        .transmit_frame(7, &parse_command(b"r=1,X=1"), pixel([0; 4]), 4, now)
        .unwrap();
    assert_eq!(pixels(&store, now), [0; 4]);
}

#[test]
fn frame_count_is_bounded_even_when_pixel_quota_has_room() {
    let now = Instant::now();
    let mut store = store();
    for _ in 1..MAX_FRAMES {
        store
            .transmit_frame(7, &KittyCommand::default(), pixel([0; 4]), usize::MAX, now)
            .unwrap();
    }
    assert_eq!(
        store.transmit_frame(7, &KittyCommand::default(), pixel([0; 4]), usize::MAX, now),
        Err(KittyAnimationError::StorageLimitExceeded)
    );
}

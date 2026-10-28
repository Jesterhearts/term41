use std::time::Duration;
use std::time::Instant;

use super::KittyCommand;
use super::KittyImageStore;
use crate::DecodedImage;

#[cfg(test)]
mod tests;

/// Bounds frame metadata even for animations made of single-pixel frames.
const MAX_FRAMES: usize = 4096;
const DEFAULT_GAP: Duration = Duration::from_millis(40);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KittyAnimationError {
    MissingImage,
    MissingFrame,
    InvalidArgument,
    StorageLimitExceeded,
}

impl KittyAnimationError {
    pub fn code(self) -> &'static str {
        match self {
            Self::MissingImage | Self::MissingFrame => "ENOENT",
            Self::InvalidArgument => "EINVAL",
            Self::StorageLimitExceeded => "ENOSPC",
        }
    }
}

/// One immutable frame selected by the image-wide animation clock.
pub struct KittyPresentation<'a> {
    pub image: &'a DecodedImage,
    /// Changes whenever these pixels change, including frame deletion and
    /// reuse.
    pub revision: u64,
    /// No timer is needed for stopped, exhausted, or waiting animations.
    pub next_frame_in: Option<Duration>,
}

/// Stable frame identity within an image transmission. Deleting a frame
/// invalidates it; editing pixels or changing the frame number does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KittyFrameIdentity(u64);

#[derive(Debug)]
struct AnimationFrame {
    image: DecodedImage,
    gap: Duration,
    revision: u64,
    identity: KittyFrameIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackMode {
    Stopped,
    Loading,
    Running,
}

#[derive(Debug)]
pub(super) struct StoredImage {
    frames: Vec<AnimationFrame>,
    mode: PlaybackMode,
    current: usize,
    started_at: Instant,
    completed_loops: u64,
    loop_limit: Option<u64>,
}

struct PlaybackPosition {
    index: usize,
    elapsed: Duration,
    completed_loops: u64,
    finished: bool,
    next_frame_in: Option<Duration>,
}

impl StoredImage {
    pub(super) fn new(
        image: DecodedImage,
        revision: u64,
    ) -> Self {
        Self {
            frames: vec![AnimationFrame {
                image,
                gap: Duration::ZERO,
                revision,
                identity: KittyFrameIdentity(revision),
            }],
            mode: PlaybackMode::Stopped,
            current: 0,
            started_at: Instant::now(),
            completed_loops: 0,
            loop_limit: None,
        }
    }

    pub(super) fn root(&self) -> &DecodedImage {
        &self.frames[0].image
    }

    pub(super) fn storage_bytes(&self) -> usize {
        self.frames
            .iter()
            .map(|frame| super::image_storage_bytes(&frame.image))
            .sum()
    }
}

fn frame_index(
    number: u32,
    count: usize,
) -> Result<usize, KittyAnimationError> {
    let index = number
        .checked_sub(1)
        .ok_or(KittyAnimationError::MissingFrame)? as usize;
    if index >= count {
        return Err(KittyAnimationError::MissingFrame);
    }
    Ok(index)
}

fn frame_gap(
    value: i32,
    previous: Duration,
) -> Duration {
    match value.cmp(&0) {
        std::cmp::Ordering::Less => Duration::ZERO,
        std::cmp::Ordering::Equal => previous,
        std::cmp::Ordering::Greater => Duration::from_millis(value as u64),
    }
}

pub(super) fn valid_image(image: &DecodedImage) -> bool {
    image.width != 0
        && image.height != 0
        && image.frames.len() == 1
        && super::rgba_bytes(image.width, image.height) == Some(image.frames[0].pixels.len())
}

fn playback_position(
    image: &StoredImage,
    now: Instant,
) -> PlaybackPosition {
    let mut position = PlaybackPosition {
        index: image.current,
        elapsed: Duration::ZERO,
        completed_loops: image.completed_loops,
        finished: false,
        next_frame_in: None,
    };
    if image.mode == PlaybackMode::Stopped {
        return position;
    }
    if image.frames.len() == 1 {
        position.elapsed = now
            .saturating_duration_since(image.started_at)
            .min(image.frames[0].gap);
        return position;
    }
    let cycle: Duration = image.frames.iter().map(|frame| frame.gap).sum();
    if cycle.is_zero() {
        return position;
    }

    let mut elapsed = now.saturating_duration_since(image.started_at);
    loop {
        // Skip whole cycles so a long-hidden tab takes O(frame count), not
        // O(elapsed time).
        if position.index == 0 && image.mode == PlaybackMode::Running && elapsed >= cycle {
            let cycles = (elapsed.as_nanos() / cycle.as_nanos()).min(u64::MAX as u128) as u64;
            if image
                .loop_limit
                .is_some_and(|limit| cycles >= limit.saturating_sub(position.completed_loops))
            {
                position.index = image
                    .frames
                    .iter()
                    .rposition(|frame| !frame.gap.is_zero())
                    .unwrap_or(0);
                position.completed_loops = image.loop_limit.unwrap();
                position.finished = true;
                return position;
            }
            position.completed_loops = position.completed_loops.saturating_add(cycles);
            elapsed = duration_from_nanos(elapsed.as_nanos() % cycle.as_nanos());
        }
        let gap = image.frames[position.index].gap;
        let last = position.index + 1 == image.frames.len();
        if elapsed < gap {
            position.elapsed = elapsed;
            if !(last && image.mode == PlaybackMode::Loading) {
                position.next_frame_in = Some(gap - elapsed);
            }
            return position;
        }
        if last {
            if image.mode == PlaybackMode::Loading {
                position.elapsed = gap;
                return position;
            }
            position.completed_loops = position.completed_loops.saturating_add(1);
            if image
                .loop_limit
                .is_some_and(|limit| position.completed_loops >= limit)
            {
                position.index = image
                    .frames
                    .iter()
                    .rposition(|frame| !frame.gap.is_zero())
                    .unwrap_or(0);
                position.finished = true;
                return position;
            }
        }
        elapsed -= gap;
        position.index = (position.index + 1) % image.frames.len();
    }
}

fn duration_from_nanos(nanos: u128) -> Duration {
    Duration::new(
        (nanos / 1_000_000_000) as u64,
        (nanos % 1_000_000_000) as u32,
    )
}

fn settle_playback(
    image: &mut StoredImage,
    now: Instant,
) {
    let position = playback_position(image, now);
    image.current = position.index;
    image.started_at = now - position.elapsed;
    image.completed_loops = position.completed_loops;
    if position.finished {
        image.mode = PlaybackMode::Stopped;
    }
}

fn displayed_frame(
    image: &StoredImage,
    index: usize,
) -> usize {
    if image.mode != PlaybackMode::Stopped && image.frames[index].gap.is_zero() {
        // A gapless tail in loading mode is retained as data, not displayed.
        return image.frames[..=index]
            .iter()
            .rposition(|frame| !frame.gap.is_zero())
            .unwrap_or(image.current);
    }
    index
}

#[derive(Clone, Copy)]
struct Rectangle {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

fn validate_rectangle(
    rect: Rectangle,
    width: u32,
    height: u32,
) -> Result<(), KittyAnimationError> {
    if rect.width == 0
        || rect.height == 0
        || rect.x > width
        || rect.y > height
        || rect.width > width - rect.x
        || rect.height > height - rect.y
    {
        return Err(KittyAnimationError::InvalidArgument);
    }
    Ok(())
}

fn rectangles_overlap(
    a: Rectangle,
    b: Rectangle,
) -> bool {
    a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

fn blend_pixel(
    destination: &mut [u8],
    source: &[u8],
) {
    let source_alpha = u32::from(source[3]);
    let destination_alpha = u32::from(destination[3]);
    let alpha = source_alpha * 255 + destination_alpha * (255 - source_alpha);
    if alpha == 0 {
        destination.fill(0);
        return;
    }
    for channel in 0..3 {
        let color = u32::from(source[channel]) * source_alpha * 255
            + u32::from(destination[channel]) * destination_alpha * (255 - source_alpha);
        destination[channel] = ((color + alpha / 2) / alpha) as u8;
    }
    destination[3] = ((alpha + 127) / 255) as u8;
}

fn compose_pixels(
    destination: &mut [u8],
    width: u32,
    source: &DecodedImage,
    src: Rectangle,
    dst: Rectangle,
    overwrite: bool,
) {
    let source_pixels = &source.frames[0].pixels;
    for row in 0..src.height {
        let src_start = ((src.y + row) as usize * source.width as usize + src.x as usize) * 4;
        let dst_start = ((dst.y + row) as usize * width as usize + dst.x as usize) * 4;
        let length = src.width as usize * 4;
        let source_row = &source_pixels[src_start..src_start + length];
        let destination_row = &mut destination[dst_start..dst_start + length];
        if overwrite {
            destination_row.copy_from_slice(source_row);
        } else {
            for (destination, source) in destination_row
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(source_row.as_chunks::<4>().0)
            {
                blend_pixel(destination, source);
            }
        }
    }
}

impl KittyImageStore {
    /// Identify a 1-based frame for deferred commands without retaining pixels.
    pub fn frame_identity(
        &self,
        id: u32,
        number: u32,
    ) -> Option<KittyFrameIdentity> {
        let image = self.images.get(&id)?;
        let index = number.checked_sub(1)? as usize;
        image.frames.get(index).map(|frame| frame.identity)
    }

    /// Sample the shared clock without changing image data or allocating
    /// pixels.
    pub fn presentation(
        &self,
        id: u32,
        now: Instant,
    ) -> Option<KittyPresentation<'_>> {
        let image = self.images.get(&id)?;
        let position = playback_position(image, now);
        let index = displayed_frame(image, position.index);
        let frame = &image.frames[index];
        Some(KittyPresentation {
            image: &frame.image,
            revision: frame.revision,
            next_frame_in: position.next_frame_in,
        })
    }

    /// Add or edit a fully composed frame. The quota includes all decoded
    /// frames. Failure leaves both pixels and playback unchanged. At most
    /// 4096 frames are retained.
    ///
    /// `r=0` or a number past the last frame appends, matching Kitty. Other
    /// `r` values edit that 1-based frame. A new frame copies its `c` base at
    /// creation; later edits to the base do not change that copy. The patch
    /// must fit inside the image. The returned number identifies the frame.
    pub fn transmit_frame(
        &mut self,
        id: u32,
        cmd: &KittyCommand,
        patch: DecodedImage,
        max_bytes: usize,
        now: Instant,
    ) -> Result<u32, KittyAnimationError> {
        let image = self
            .images
            .get(&id)
            .ok_or(KittyAnimationError::MissingImage)?;
        let width = image.root().width;
        let height = image.root().height;
        let dst = Rectangle {
            x: cmd.src_x,
            y: cmd.src_y,
            width: patch.width,
            height: patch.height,
        };
        validate_rectangle(dst, width, height)?;
        if cmd.cell_x_offset > 1 || !valid_image(&patch) {
            return Err(KittyAnimationError::InvalidArgument);
        }
        let target = if cmd.rows == 0 || cmd.rows as usize > image.frames.len() {
            None
        } else {
            Some(frame_index(cmd.rows, image.frames.len())?)
        };
        let base = if let Some(index) = target {
            Some(index)
        } else if cmd.columns != 0 {
            Some(
                frame_index(cmd.columns, image.frames.len())
                    .map_err(|_| KittyAnimationError::InvalidArgument)?,
            )
        } else {
            None
        };
        let bytes = super::image_storage_bytes(image.root());
        if target.is_none()
            && (image.frames.len() >= MAX_FRAMES
                || self.used_bytes.saturating_add(bytes) > max_bytes)
        {
            return Err(KittyAnimationError::StorageLimitExceeded);
        }
        let mut pixels = match base {
            Some(index) => image.frames[index].image.frames[0].pixels.clone(),
            None => cmd.cell_y_offset.to_be_bytes().repeat(bytes / 4),
        };
        let src = Rectangle {
            x: 0,
            y: 0,
            width: patch.width,
            height: patch.height,
        };
        compose_pixels(&mut pixels, width, &patch, src, dst, cmd.cell_x_offset == 1);
        let gap = frame_gap(
            cmd.z_index,
            target.map_or(DEFAULT_GAP, |index| image.frames[index].gap),
        );
        let frame = AnimationFrame {
            image: DecodedImage::single_frame(width, height, pixels),
            gap,
            revision: self.next_generation,
            identity: target.map_or(KittyFrameIdentity(self.next_generation), |index| {
                image.frames[index].identity
            }),
        };
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        let image = self.images.get_mut(&id).unwrap();
        settle_playback(image, now);
        let index = if let Some(index) = target {
            image.frames[index] = frame;
            index
        } else {
            image.frames.push(frame);
            self.used_bytes += bytes;
            image.frames.len() - 1
        };
        Ok(index as u32 + 1)
    }

    /// Apply frame selection, timing, playback mode, and loop count.
    /// Frame numbers are 1-based. Zero and missing frame numbers leave the
    /// corresponding setting alone without blocking other controls.
    /// Selecting the current frame preserves its remaining gap.
    /// Stopping freezes the current frame and resets the completed loop count.
    pub fn control_animation(
        &mut self,
        id: u32,
        cmd: &KittyCommand,
        now: Instant,
    ) -> Result<(), KittyAnimationError> {
        let image = self
            .images
            .get_mut(&id)
            .ok_or(KittyAnimationError::MissingImage)?;
        if cmd.width > 3 {
            return Err(KittyAnimationError::InvalidArgument);
        }
        let selected = frame_index(cmd.columns, image.frames.len()).ok();
        let affected = frame_index(cmd.rows, image.frames.len()).ok();
        settle_playback(image, now);
        let displayed = displayed_frame(image, image.current);
        if let Some(index) = affected {
            image.frames[index].gap = frame_gap(cmd.z_index, image.frames[index].gap);
        }
        if let Some(index) = selected
            && index != image.current
        {
            image.current = index;
            image.started_at = now;
        }
        if cmd.width != 0 {
            let mode = match cmd.width {
                1 => PlaybackMode::Stopped,
                2 => PlaybackMode::Loading,
                _ => PlaybackMode::Running,
            };
            if image.mode == PlaybackMode::Stopped || mode == PlaybackMode::Stopped {
                image.started_at = now;
            }
            image.mode = mode;
            image.completed_loops = 0;
            if mode == PlaybackMode::Stopped && selected.is_none() {
                image.current = displayed;
            }
        }
        if cmd.height != 0 {
            image.loop_limit = (cmd.height > 1).then_some(u64::from(cmd.height - 1));
        }
        Ok(())
    }

    /// Compose two in-bounds, non-overlapping (when on the same frame)
    /// rectangles.
    pub fn compose_frame(
        &mut self,
        id: u32,
        cmd: &KittyCommand,
    ) -> Result<(), KittyAnimationError> {
        let image = self
            .images
            .get_mut(&id)
            .ok_or(KittyAnimationError::MissingImage)?;
        let source = frame_index(cmd.rows, image.frames.len())?;
        let destination = frame_index(cmd.columns, image.frames.len())?;
        let width = image.root().width;
        let height = image.root().height;
        let rect_width = if cmd.src_w == 0 { width } else { cmd.src_w };
        let rect_height = if cmd.src_h == 0 { height } else { cmd.src_h };
        let src = Rectangle {
            x: cmd.cell_x_offset,
            y: cmd.cell_y_offset,
            width: rect_width,
            height: rect_height,
        };
        let dst = Rectangle {
            x: cmd.src_x,
            y: cmd.src_y,
            ..src
        };
        validate_rectangle(src, width, height)?;
        validate_rectangle(dst, width, height)?;
        if source == destination && rectangles_overlap(src, dst) {
            return Err(KittyAnimationError::InvalidArgument);
        }
        let mut pixels = image.frames[destination].image.frames[0].pixels.clone();
        compose_pixels(
            &mut pixels,
            width,
            &image.frames[source].image,
            src,
            dst,
            cmd.no_move_cursor,
        );
        image.frames[destination].image = DecodedImage::single_frame(width, height, pixels);
        image.frames[destination].revision = self.next_generation;
        self.next_generation = self.next_generation.wrapping_add(1).max(1);
        Ok(())
    }

    /// Delete a 1-based frame, clamping zero to the root and large numbers to
    /// the last. Lowercase deletion keeps the last frame. Uppercase
    /// deletion removes its image.
    pub fn delete_frame(
        &mut self,
        id: u32,
        number: u32,
        delete_last: bool,
        now: Instant,
    ) -> Result<(), KittyAnimationError> {
        let image = self
            .images
            .get_mut(&id)
            .ok_or(KittyAnimationError::MissingImage)?;
        if image.frames.len() == 1 {
            if delete_last {
                self.remove(id);
            }
            return Ok(());
        }
        settle_playback(image, now);
        let index = (number.saturating_sub(1) as usize).min(image.frames.len() - 1);
        let removed = image.frames.remove(index);
        self.used_bytes -= super::image_storage_bytes(&removed.image);
        if index < image.current {
            image.current -= 1;
        } else if index == image.current {
            image.current = index.min(image.frames.len() - 1);
            image.started_at = now;
        }
        Ok(())
    }
}

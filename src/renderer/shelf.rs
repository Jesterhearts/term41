//! Shelf-based rectangle packer shared by the glyph and image atlases.
//!
//! A "shelf" is a horizontal row of fixed height within one layer of the
//! atlas. Rectangles are packed left-to-right into shelves whose height
//! matches the first allocation on them; taller rectangles open new shelves.
//! Freed sub-rectangles within a shelf go on a per-shelf free list so that
//! space vacated by LRU eviction can be recycled.
//!
//! The packer supports multiple 2D layers (e.g. a texture array); a
//! single-layer atlas just constructs it with `layers = 1`.

/// A shelf-based rectangle packer.
pub struct ShelfPacker {
    size: u32,
    shelves: Vec<Shelf>,
    layers: Vec<Layer>,
}

/// The region granted for one allocation. Carries the shelf index so
/// [`ShelfPacker::free`] can return the region to the shelf's free list.
#[derive(Clone, Copy)]
pub struct Allocation {
    pub layer: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub shelf_idx: usize,
}

struct Shelf {
    layer: u32,
    y: u32,
    height: u32,
    cursor_x: u32,
    /// Freed sub-rectangles on this shelf: (x, width). Each entry spans the
    /// full shelf height.
    free: Vec<(u32, u32)>,
}

struct Layer {
    next_shelf_y: u32,
}

impl ShelfPacker {
    pub fn new(
        size: u32,
        layers: u32,
    ) -> Self {
        Self {
            size,
            shelves: Vec::new(),
            layers: (0..layers).map(|_| Layer { next_shelf_y: 0 }).collect(),
        }
    }

    /// Try to allocate a `width × height` rectangle. Returns `None` if the
    /// rectangle is larger than a single layer, or if no layer has room for
    /// a new shelf and no existing shelf can fit it.
    pub fn allocate(
        &mut self,
        width: u32,
        height: u32,
    ) -> Option<Allocation> {
        if width > self.size || height > self.size {
            return None;
        }

        if let Some(alloc) = pack_in_existing_shelf(&mut self.shelves, self.size, width, height) {
            return Some(alloc);
        }

        open_new_shelf(
            &mut self.shelves,
            &mut self.layers,
            self.size,
            width,
            height,
        )
    }

    /// Return an allocation's region to its shelf's free list. Safe to call
    /// on any `Allocation` produced by this packer; double-freeing is not
    /// checked — callers must ensure each allocation is freed at most once.
    pub fn free(
        &mut self,
        alloc: &Allocation,
    ) {
        self.shelves[alloc.shelf_idx]
            .free
            .push((alloc.x, alloc.width));
    }
}

fn pack_in_existing_shelf(
    shelves: &mut [Shelf],
    size: u32,
    width: u32,
    height: u32,
) -> Option<Allocation> {
    for (idx, shelf) in shelves.iter_mut().enumerate() {
        if height > shelf.height {
            continue;
        }
        if let Some(pos) = shelf.free.iter().position(|&(_, w)| w >= width) {
            let (fx, fw) = shelf.free.swap_remove(pos);
            if fw > width {
                shelf.free.push((fx + width, fw - width));
            }
            return Some(Allocation {
                layer: shelf.layer,
                x: fx,
                y: shelf.y,
                width,
                height,
                shelf_idx: idx,
            });
        }
        if shelf.cursor_x + width <= size {
            let x = shelf.cursor_x;
            shelf.cursor_x += width;
            return Some(Allocation {
                layer: shelf.layer,
                x,
                y: shelf.y,
                width,
                height,
                shelf_idx: idx,
            });
        }
    }
    None
}

fn open_new_shelf(
    shelves: &mut Vec<Shelf>,
    layers: &mut [Layer],
    size: u32,
    width: u32,
    height: u32,
) -> Option<Allocation> {
    for (layer_idx, layer) in layers.iter_mut().enumerate() {
        if layer.next_shelf_y + height > size {
            continue;
        }
        let idx = shelves.len();
        let y = layer.next_shelf_y;
        shelves.push(Shelf {
            layer: layer_idx as u32,
            y,
            height,
            cursor_x: width,
            free: Vec::new(),
        });
        layer.next_shelf_y += height;
        return Some(Allocation {
            layer: layer_idx as u32,
            x: 0,
            y,
            width,
            height,
            shelf_idx: idx,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_shelf_opens_at_y_zero() {
        let mut packer = ShelfPacker::new(1024, 1);
        let alloc = packer.allocate(10, 20).unwrap();
        assert_eq!((alloc.layer, alloc.x, alloc.y), (0, 0, 0));
    }

    #[test]
    fn same_height_rects_share_shelf() {
        let mut packer = ShelfPacker::new(1024, 1);
        let a = packer.allocate(10, 20).unwrap();
        let b = packer.allocate(15, 20).unwrap();
        assert_eq!(a.shelf_idx, b.shelf_idx);
        assert_eq!((b.x, b.y), (10, 0));
    }

    #[test]
    fn taller_rect_opens_new_shelf() {
        let mut packer = ShelfPacker::new(1024, 1);
        let _ = packer.allocate(10, 20).unwrap();
        let tall = packer.allocate(10, 30).unwrap();
        assert_eq!(tall.y, 20);
    }

    #[test]
    fn oversized_rect_returns_none() {
        let mut packer = ShelfPacker::new(256, 1);
        assert!(packer.allocate(300, 10).is_none());
        assert!(packer.allocate(10, 300).is_none());
    }

    #[test]
    fn exhausted_single_layer_returns_none() {
        let mut packer = ShelfPacker::new(256, 1);
        // Fill the whole atlas with one shelf that covers every pixel.
        let _ = packer.allocate(256, 256).unwrap();
        assert!(packer.allocate(1, 1).is_none());
    }

    #[test]
    fn spills_to_next_layer() {
        let mut packer = ShelfPacker::new(256, 2);
        let _ = packer.allocate(256, 256).unwrap();
        let next = packer.allocate(10, 10).unwrap();
        assert_eq!(next.layer, 1);
    }

    #[test]
    fn free_region_is_reused() {
        let mut packer = ShelfPacker::new(1024, 1);
        let a = packer.allocate(10, 20).unwrap();
        let _b = packer.allocate(10, 20).unwrap();
        packer.free(&a);
        let c = packer.allocate(10, 20).unwrap();
        assert_eq!((c.layer, c.x, c.y), (a.layer, a.x, a.y));
    }

    #[test]
    fn free_slot_wider_than_request_leaves_remainder() {
        let mut packer = ShelfPacker::new(1024, 1);
        let a = packer.allocate(30, 20).unwrap();
        let _b = packer.allocate(10, 20).unwrap();
        packer.free(&a);
        // Take a portion — the rest should stay on the free list.
        let c = packer.allocate(10, 20).unwrap();
        assert_eq!((c.x, c.width), (a.x, 10));
        let d = packer.allocate(20, 20).unwrap();
        assert_eq!((d.x, d.width), (a.x + 10, 20));
    }
}

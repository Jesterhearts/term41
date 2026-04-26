# Changelog

All notable changes to `term41` are documented here.

## [0.1.1] - 2026-04-26

Generated from the changes between tags `0.1.0` and `0.1.1`.

### Added

- Added Kitty graphics Unicode placeholder support (`U=1`), rendering virtual
  placements from `U+10EEEE` placeholder cells and the row/column/image-id
  combining marks used by the protocol.
- Added JPEG decoding for ordinary image payloads and Kitty `f=100` payloads as
  a compatibility extension alongside PNG.
- Added iTerm2 Terminal Feature Reporting through `OSC 1337;Capabilities` and
  `TERM_FEATURES`, with clipboard write reporting filtered by runtime policy.
- Added `OSC 1` icon-title handling by mapping it into term41's shared title
  field.
- Added a dedicated `config41` crate for configuration, keybindings, palettes,
  feature permissions, limits, cursor/status-line settings, and scripting
  permissions.
- Added terminal-processing Criterion coverage in `benches/vte_parse.rs` so
  parser benchmarks also cover terminal grid mutation.

### Changed

- Moved terminal snapshots to a triple-buffered pipeline so the renderer can
  consume published terminal state without locking the terminal during normal
  rendering.
- Reworked renderer caching around a terminal texture layer, row generation
  keys, dirty layer rects, and per-row geometry reuse.
- Made startup rendering use the same snapshot and visible-image pipeline as
  normal rendering, which keeps the terminal area responsive during startup.
- Recomputed terminal cell metrics from the largest loaded non-color font
  metrics instead of relying only on the embedded fallback font.
- Added an installed Nerd Font symbol fallback path and centered private-use
  symbol glyphs within the terminal cell.
- Kept scripting output and input updates change-driven so unchanged script
  state does not repeatedly wake the renderer.
- Increased the glyph atlas page size from `512` to `1024` while retaining a
  bounded page count.

### Fixed

- Fixed inline image anchor cleanup when cells are erased, overwritten, shifted,
  or scrolled by line, column, and rectangular operations.
- Clipped terminal images to the terminal content area so images do not draw
  into chrome or outside the visible terminal region.
- Fixed image replacement caching by tracking Kitty image generations.
- Fixed undefined DRCS private-use scalars being claimed before font fallback,
  which could prevent Nerd Font icons from rendering.
- Fixed Nerd Font private-use symbols taking the emoji/color-font path instead
  of the text fallback path.
- Fixed PowerShell-style `OSC 7` current-directory URIs of the form
  `file://host//absolute/path` on Unix.
- Fixed terminal chrome freezing while synchronized output is suspended.
- Improved synchronized-output timeout handling.
- Improved narrow UTF-8, emoji modifier, and wide-cell overwrite handling in the
  terminal write path.

### Performance

- Cached terminal rendering in a texture layer and reused row geometry when row
  generations have not changed.
- Batched terminal snapshot invalidation across PTY byte batches instead of
  invalidating renderer rows per parser action.
- Added fast paths for ASCII writes and simple cell overwrites, including a
  conservative `Row::has_wide_cells` flag.
- Avoided expensive grapheme-extension checks for ordinary single-scalar text
  while keeping combining marks, emoji modifiers, regional indicators, and
  multi-scalar clusters on the correctness path.
- Reduced renderer wakeups during heavy streaming output, including animated
  background redraw gating while the active terminal stream is saturated.
- Borrowed parser parameters through CSI dispatch instead of eagerly copying
  parameter groups.

### Documentation

- Updated README protocol tables for Kitty Unicode placeholders, iTerm2
  capabilities, JPEG support, and the `0.1.1` install tag.
- Refined README wording around LLM-assisted development and performance goals.

### Internal

- Promoted config-related data types from `term41`/`terminal41` internals into
  the new `config41` crate.
- Threaded explicit feature-permission and terminal-limit data through terminal
  parsing, reporting, image, and renderer paths.
- Added a `layer.wgsl` shader for compositing the cached terminal layer.
- Added and expanded tests for terminal feature reporting, Kitty/JPEG decoding,
  image anchor lifetimes, Nerd Font fallback, DRCS fallback behavior, OSC 7 URI
  normalization, text writes, snapshot generations, and scripting change
  delivery.

[0.1.1]: https://github.com/Jesterhearts/term41/compare/0.1.0...0.1.1

# Changelog

All notable changes to `term41` are documented here.

## [0.2.1] - 2026-05-13

Generated from the changes between tags `0.2.0` and `0.2.1`.

### Added

- Added persistent-history management actions for clearing all history, clearing
  history for the current directory, and deleting fuzzy-filtered history entries.
  These actions are available through the command palette and use confirmation
  or deletion UI before mutating the history database.
- Added a user-local desktop asset installer script which installs or removes
  the `.desktop` launcher and hicolor icon assets, with options for custom
  `Exec=` commands and custom XDG data directories.
- Added command-block document and query helpers that expose command text,
  output text, exit status, command state, and prompt references for navigation,
  selection, and gutter/status UI.

### Changed

- Made prompt, command, failure, and success navigation operate from the
  command-block document view instead of walking raw prompt marks directly.
- Made command-editor multiline submission and path completion use the current
  shell's escape character, including PowerShell backtick escaping, while
  preserving line continuations that are already present.
- Replaced hand-rolled emoji property ranges with `icu_properties` data for
  emoji modifiers, emoji presentation, regional indicators, extended
  pictographs, and font-shaping emoji components.
- Kept the terminal contents visible behind confirmation-style modals.

### Fixed

- Fixed several DEC/status-line rendering and invalidation bugs, including blank
  status bars, status bars that failed to update, dirty status-row clearing, and
  script status-row cache invalidation.
- Fixed active command rendering and metadata bugs around wrapped commands,
  resized/reflowed command rows, and active commands whose final line could fail
  to render.
- Fixed mouse selection after active scrollback recycling and while reading
  scrollback.
- Fixed command-editor multiline submission so line continuations are not
  blindly appended when a line already has the shell's continuation escape.

### Documentation

- Updated install instructions to use GitLab and the `0.2.1` release tag.
- Documented the desktop asset installer workflow.
- Corrected changelog compare links from GitHub to GitLab.
- Added a short README note clarifying that term41 is used as the author's
  primary shell.

### Internal

- Bumped workspace crate versions to `0.2.1`.
- Split large modules into focused submodules across `commands41`, `config41`,
  `font41`, `terminal41`, `renderer`, renderer chrome, selection, OSC handling,
  and the window host.
- Split terminal metadata, protocol state, snapshot dirtiness, effects, and
  terminal application helpers into dedicated modules.
- Split renderer frame construction into explicit geometry, layout, layer,
  upload, row-cache, image, cursor, gutter, and vertex helpers.
- Added and expanded tests for history deletion, command-block queries,
  shell-specific command-editor escaping, status/script row generation, prompt
  navigation, and renderer geometry behavior.

## [0.2.0] - 2026-05-05

Generated from the changes between tag `0.1.1` and the pending `0.2.0`
release.

### Added

- Added a command editor layer with command/path/history completion, multiline
  editing, selection/copy support, undo/redo, and an optional Vim-style editing
  mode.
- Added a command palette with fuzzy matching, argument commands, configurable
  unbound jump actions, and command completion metadata.
- Added persistent per-directory command history backed by SQLite, plus deeper
  shell history integration for bash, zsh, and PowerShell history sources.
- Added opt-in shell integration hooks which emit prompt, command, exit-status,
  and current-directory lifecycle markers.
- Added scrollback-aware mouse selection across rendered command blocks,
  including selection extension after viewport scroll.
- Added SGR pixel mouse reporting (`?1016`).
- Added image z-index ordering and page-position aware visible image ordering.
- Added default Vulkan rendering through the `vulkan` Cargo feature.

### Changed

- Reworked scrollback around completed command blocks, sticky prompt rows,
  command-block gutter markers, and command-block image anchoring.
- Reflowed completed command blocks during resize instead of treating completed
  output as fixed-width snapshots.
- Routed command-editor interactions through the same input, mouse, paste, and
  resize paths used by the rest of the terminal UI.
- Reduced atlas memory usage and cached gutter markers with terminal rows.

### Fixed

- Fixed mixed-DPI flicker and a rare resize panic across mixed-DPI setups.
- Fixed high CPU usage from shift-click selection.
- Fixed soft-wrap row clearing and stale wrapped-row snapshot continuations.
- Fixed Kitty placeholder image storage recovery.
- Fixed prompt/gutter placement issues around sticky prompts, block cursors,
  status layout, and gutter popup prompts.

### Documentation

- Updated README feature-default documentation to reflect Vulkan being enabled
  by default.
- Documented primary-screen clear padding behavior.

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

[0.2.1]: https://gitlab.com/Jesterhearts/term41/compare/0.2.0...0.2.1
[0.2.0]: https://gitlab.com/Jesterhearts/term41/compare/0.1.1...0.2.0
[0.1.1]: https://gitlab.com/Jesterhearts/term41/compare/0.1.0...0.1.1

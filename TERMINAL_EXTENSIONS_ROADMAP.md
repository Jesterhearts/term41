# Terminal Extensions Roadmap

This document tracks modern terminal extension families outside the DEC
VT420/VT520/VT525 compatibility roadmap. It focuses on protocols implemented by
widely used terminals such as Ghostty, Alacritty, kitty, iTerm2, WezTerm,
Windows Terminal, VS Code's integrated terminal, VTE-based terminals, foot,
mintty, Konsole, and xterm.

The goal is not to clone every terminal-specific feature. The goal is to choose
extension families that improve real terminal applications while preserving
`term41`'s trust boundaries.

## Rules

- Keep terminal chrome, local files, local clipboard reads, focus stealing,
  notifications, uploads, and host-triggered UI actions behind explicit local
  policy.
- Treat host-provided metadata as untrusted. It may annotate terminal content,
  but it must not impersonate trusted `term41` UI.
- Bound all in-band binary payloads by per-message and total-session quotas.
- Advertise capabilities only after policy filtering. A disabled risky feature
  should not be reported as supported.

## Status Legend

- `Implemented`: present and worth maintaining.
- `Partial`: useful support exists, but advertised protocol behavior is not yet
  complete.
- `Planned`: useful enough to design and implement.
- `Watch`: promising, but either new, unstable, or not yet clearly adopted.
- `Explicitly not planned`: the compatibility value does not justify the
  security or complexity cost.

## Implemented And Maintained

### Common xterm / VTE / iTerm Surface

Status:

- `Implemented`

Implemented:

- OSC 0 / OSC 2 titles
- OSC 4 / OSC 10 / OSC 11 / OSC 12 color queries and setters
- OSC 104 / OSC 110 / OSC 111 / OSC 112 color resets
- OSC 7 current-working-directory tracking
- OSC 8 hyperlinks
- OSC 52 clipboard read/write
- bracketed paste mode
- focus reporting
- xterm mouse tracking modes and encodings
- window and cell size reports
- XTVERSION-style version reporting
- XTGETTCAP policy-filtered capability reporting
- synchronized output mode (`DECSET 2026`)

Maintenance direction:

- Keep these compatible with xterm, VTE, iTerm2, Ghostty, WezTerm, Alacritty,
  and common TUI libraries.
- Add regression coverage from real application probes where possible: `vttest`,
  shell-integration scripts, editor probes, `crossterm`, `vaxis`, `ratatui`, and
  terminal image tools.
- Revisit OSC 52 policy. Clipboard writes are common and useful; clipboard reads
  are more sensitive and should move toward local configuration with clear
  defaults.
- Keep mutable xterm colors integrated with all 256 indexed colors, configured
  theme resets, DEC defaults, and live theme reloads.
- Keep XTGETTCAP reporting coarse and policy-filtered. It should report
  implemented special-key sequences and useful terminal facts, not detailed host
  configuration.

Security:

- `LOW` to `HIGH`
- Clipboard readback is the major high-risk subcase because it can exfiltrate
  local data to a remote process.
- Capability reporting is a fingerprinting surface, so reported facts stay
  intentionally coarse.

### Images And Media

Status:

- `Implemented`

Implemented:

- sixel graphics
- kitty graphics protocol core parsing, direct/file/temp-file transmission,
  PNG/RGB/RGBA decode, JPEG compatibility for `f=100` payloads, placement
  IDs, relative placement, cell offsets, cropping, expanded deletion
  selectors, z-index ordering, chunking, image numbers, query/ack responses,
  and bounded image storage
- iTerm2 OSC 1337 inline images, including multipart image payloads
- iTerm2 OSC 1337 `ReportCellSize`
- iTerm2 Terminal Feature Reporting via `OSC 1337;Capabilities` and
  `TERM_FEATURES`

Maintenance direction:

- Keep image lifecycle behavior aligned with screen clears, alternate-screen
  switches, scrollback, and resize behavior.
- Add compatibility probes for common senders: `kitty +kitten icat`,
  `wezterm imgcat`, `chafa`, `yazi`, `onefetch`, `notcurses`, and
  `libsixel`-based tools.
- Keep kitty graphics parity focused on real tools. The remaining deliberately
  scoped follow-up is kitty animation frame support.

Security:

- `MEDIUM`
- Image protocols are parser and resource-exhaustion surfaces. They should
  remain quota-bound and should never access arbitrary local files outside the
  protocol's safe temp-file rules. Kitty shared-memory transport (`t=s`) is
  intentionally not supported.

### Keyboard And Input

Status:

- `Implemented`

Implemented:

- kitty keyboard protocol main/alternate mode stacks, key encoding,
  repeat/release events, alternate keys, associated text, and IME commits
- legacy xterm keyboard encodings
- xterm mouse protocols
- bracketed paste
- focus reports

Maintenance direction:

- Track kitty keyboard behavior as implemented by kitty, Ghostty, Alacritty,
  foot, iTerm2, WezTerm, and Rio.
- Prefer compatibility tests that compare emitted byte sequences for ambiguous
  keys: Escape vs `Ctrl+[`, Tab vs `Ctrl+I`, Enter vs `Ctrl+M`, modified
  function keys, IME commits, and press/release/repeat modes.

Security:

- `LOW` to `MEDIUM`
- Rich keyboard reporting is opt-in by the foreground app. It should not let the
  host disable local terminal keybindings or trusted UI actions.

### Shell Integration And Prompt Marks

Status:

- `Implemented`

Implemented:

- OSC 7 current directory
- OSC 133 prompt, command, output, and exit-status marks
- OSC 633 prompt, command, output, exit-status marks, current-directory
  property, and untrusted command-line metadata storage
- prompt navigation and gutter status markers

Maintenance direction:

- Keep OSC 133 aligned with FinalTerm, iTerm2, WezTerm, Ghostty, and Windows
  Terminal behavior.
- Keep OSC 633 aligned with VS Code's shell-integration subset, mapping safe
  overlap into the same prompt model as OSC 133.
- Preserve the distinction between host-provided semantic marks and
  terminal-owned UI. Prompt marks may annotate rows; they must not become
  trusted banners.

Security:

- `MEDIUM`
- Prompt metadata is spoofable by any process that can write to the PTY.

## Planned

### Kitty Graphics Animation

Status:

- `Planned`

Why:

- `term41` now covers the practical kitty graphics transmit/place/delete
  surface, including placement IDs, z-index, image numbers, relative placement,
  expanded delete selectors, and Unicode placeholder virtual placements.
- The remaining upstream graphics area is a larger semantic feature rather than
  a collection of missing parser keys.

Scope:

- Implement kitty animation frame actions only as a separate project. These
  mutate stored frame data and affect renderer lifecycle, quotas, and scrollback
  semantics.
- Keep kitty shared-memory transport (`t=s`) unsupported unless a future design
  introduces a local policy gate and a safe copy-only implementation model.

Security:

- `MEDIUM`
- Animation support needs explicit quotas for frame storage and mutation.
- Shared-memory transport is a local cross-process attack surface and remains
  out of scope.

## Watch

### Glyph Protocol

Status:

- `Watch`

Why:

- Glyph Protocol is a new APC protocol for registering session-local glyphs at
  Unicode Private Use Area codepoints, querying glyph coverage, and avoiding
  patched-font distribution problems.
- It is a good conceptual fit for `term41`: the renderer already has font
  shaping, glyph rasterization, fallback, and session-local DRCS-style custom
  glyph experience.

Potential scope:

- Implement support/query first: `s` support and `q` coverage replies.
- Then implement `r` registration for the simple `glyf` payload subset.
- Enforce PUA-only codepoints.
- Keep the cell buffer authoritative: copy, search, selection, hyperlinks, and
  shell history must expose the emitted codepoint, not the rendered outline.
- Keep registrations session-local and clear them on hard reset/session end.
- Bound registrations with a small quota and FIFO eviction.
- Defer `colrv0` / `colrv1` until the protocol and application ecosystem
  stabilize.

Security:

- `LOW` to `MEDIUM` if PUA-only and session-local
- `HIGH` if generalized beyond PUA, because arbitrary glyph replacement can
  spoof ordinary text, URLs, commands, and filenames.

Decision:

- Do not implement immediately while the protocol is still settling.
- Revisit after Rio ships it in a stable release and at least one more major
  terminal or TUI library adopts it.

### Light/Dark Mode Notifications

Status:

- `Watch`

Why:

- The Contour palette-preference protocol, also implemented by Kitty and
  Ghostty, exposes a coarse light/dark preference so TUIs can adapt colors.
- This is useful, but it is also environment fingerprinting.

Potential scope:

- Provide a coarse light/dark report only.
- Make the value user-configurable and optionally fixed.
- Do not expose detailed desktop theme names or platform settings.

Security:

- `LOW` to `MEDIUM`

### Kitty Text Sizing Protocol

Status:

- `Watch`

Why:

- kitty documents a text sizing protocol for richer TUI presentation.
- `term41` already supports DEC double-width/double-height rows, but arbitrary
  text sizing would touch shaping, cursor addressing, hit testing, selection,
  scrollback, and reflow.

Decision:

- Watch for adoption. Implement only if real applications depend on it and the
  semantics can fit the grid model without making text extraction dishonest.

Security:

- `MEDIUM`
- Visual-size changes can become spoofing-adjacent if copied text, hit testing,
  and cell ownership do not remain clear.

### Mouse Pointer Shapes

Status:

- `Watch`

Why:

- kitty includes a mouse-pointer-shape extension, and GUI terminals can use it
  to improve app affordances.

Decision:

- Reasonable to implement if applications use it.
- Keep it scoped to the pointer over terminal content; it must not affect
  trusted chrome or window controls.

Security:

- `LOW`

### Kitty / Ghostty Color Protocols

Status:

- `Watch`

Why:

- kitty documents additional color-control extensions, and Ghostty documents
  support for OSC 21 as the kitty color protocol.
- `term41` already has mutable OSC 4 / OSC 10 / OSC 11 / OSC 12 colors,
  xterm resets, and a substantial VT525 color-control implementation. Kitty
  OSC 21 and the Kitty color stack would add another color namespace.

Decision:

- Watch for real application use before implementing more color namespaces.
- Any implementation should integrate with the existing color table and theme
  reload model rather than adding a parallel palette path.

Security:

- `LOW` to `MEDIUM`

### Kitty Drag And Drop

Status:

- `Watch`

Why:

- Kitty 0.47 introduced OSC 72 for negotiating inbound and outbound drag and
  drop operations, including MIME data and remote files.
- The complete protocol needs native MIME negotiation, drag-source support,
  cancellation, and remote-data transfer. Basic path-oriented window drop
  events are not enough for conformance.

Decision:

- Do not advertise partial OSC 72 support as the full protocol.
- Revisit only if the windowing layer gains suitable cross-platform MIME and
  drag-source APIs and real terminal applications adopt the protocol.

Security:

- `HIGH`
- Drag and drop can disclose local files or broker remote content, so it needs
  explicit local policy, strict quotas, and clear directionality.

## Explicitly Not Planned

### Host-Triggered File Transfer And Uploads

Status:

- `Explicitly not planned`

Includes:

- kitty file transfer over the TTY
- iTerm2 `RequestUpload`
- iTerm2 download/silent file-placement behavior
- arbitrary local file reads from image or graphics protocols

Why:

- These cross from terminal rendering into local filesystem brokerage.
- Local file access should be initiated by the user through local UI or shell
  tools, not by an untrusted PTY stream.

Security:

- `HIGH`

### Shared-Memory Graphics Transport

Status:

- `Explicitly not planned`

Includes:

- kitty graphics shared-memory transmission (`t=s`)

Why:

- It creates a local cross-process data channel for untrusted PTY output.
- The existing direct, file, and safe temp-file kitty graphics transports are
  enough for current consumers.
- Keeping it unsupported avoids designing local policy, lifetime, and
  mutation-race semantics for a transport that is not needed.

Security:

- `HIGH`

### Host-Controlled Terminal Chrome And Attention

Status:

- `Explicitly not planned`

Includes:

- iTerm2 `StealFocus`
- iTerm2 profile switching
- iTerm2 background-image setting
- iTerm2 cursor guide toggles
- iTerm2 fireworks/attention effects
- host-triggered desktop notifications unless a future local policy gate makes
  them clearly useful
- terminal-owned tab/window/pane manipulation through escape sequences

Why:

- These let host output manipulate trusted UI or user attention.
- `term41` should keep tabs, windows, panes, focus, and visible trusted chrome
  under local control.

Security:

- `MEDIUM` to `HIGH`

### Interactive Host-Defined Buttons

Status:

- `Explicitly not planned`

Includes:

- iTerm2 custom buttons that send escape sequences back to the app
- host-defined interactive chrome outside normal terminal content

Why:

- A host-defined button that later emits input is the same basic risk class as
  macros and user-defined keys, but visually harder for the user to reason
  about.

Security:

- `HIGH`

### Terminal-Side Desktop Session Management

Status:

- `Explicitly not planned`

Includes:

- host-driven tab/window/session manipulation through terminal-control streams
- DEC/VT500-like host-routable session/window controls
- host-driven pane splitting or session switching

Why:

- This overlaps with local tabs/windows and with multiplexers such as `tmux` and
  `zellij`.
- Host output should not decide where local input goes.

Security:

- `HIGH`

### Arbitrary Region Styling Extensions

Status:

- `Explicitly not planned`

Includes:

- kitty arbitrary region style/color mutation beyond normal terminal content
  drawing
- extensions that mutate old screen regions without passing through the normal
  cell-write model

Why:

- `term41` already implements standard rectangular operations where they belong
  in the terminal model.
- New region styling extensions create surprising retroactive visual changes and
  make spoofing analysis harder.

Security:

- `MEDIUM`

### Kitty Multiple Terminal Cursors

Status:

- `Explicitly not planned`

Why:

- Kitty 0.43 introduced a protocol for screen-fixed extra cursor locations,
  cursor shapes, colors, and queries.
- Multiple cursors are an editor/application concept, not a terminal transport
  concept.
- The terminal should keep one protocol cursor and let applications render any
  additional cursors as ordinary content.

Security:

- `LOW` to `MEDIUM`

### Rich Clipboard Data Types

Status:

- `Explicitly not planned`

Includes:

- kitty "copying all data types to the clipboard"
- host-provided clipboard payloads beyond text

Why:

- Non-text clipboard formats increase parser and local-integration surface.
- Local copy/paste of images or files should stay user-initiated.

Security:

- `HIGH`

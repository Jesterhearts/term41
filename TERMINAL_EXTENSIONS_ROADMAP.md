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
- OSC 4 / OSC 10 / OSC 11 style color queries and palette updates
- OSC 7 current-working-directory tracking
- OSC 8 hyperlinks
- OSC 52 clipboard read/write
- bracketed paste mode
- focus reporting
- xterm mouse tracking modes and encodings
- window and cell size reports
- XTVERSION-style version reporting
- synchronized output mode (`DECSET 2026`)

Maintenance direction:

- Keep these compatible with xterm, VTE, iTerm2, Ghostty, WezTerm, Alacritty,
  and common TUI libraries.
- Add regression coverage from real application probes where possible:
  `vttest`, shell-integration scripts, editor probes, `crossterm`, `vaxis`,
  `ratatui`, and terminal image tools.
- Revisit OSC 52 policy. Clipboard writes are common and useful; clipboard
  reads are more sensitive and should move toward local configuration with
  clear defaults.

Security:

- `LOW` to `HIGH`
- Clipboard readback is the major high-risk subcase because it can exfiltrate
  local data to a remote process.

### Images And Media

Status:

- `Implemented`

Implemented:

- sixel graphics
- kitty graphics protocol core parsing, direct/file/temp-file transmission,
  PNG/RGB/RGBA decode, placement, cropping, deletion, z-index, chunking, and
  bounded image storage
- iTerm2 OSC 1337 inline images, including multipart image payloads
- iTerm2 OSC 1337 `ReportCellSize`

Maintenance direction:

- Keep image lifecycle behavior aligned with screen clears, alternate-screen
  switches, scrollback, and resize behavior.
- Add compatibility probes for common senders: `kitty +kitten icat`,
  `wezterm imgcat`, `chafa`, `yazi`, `onefetch`, `notcurses`, and
  `libsixel`-based tools.
- Audit kitty graphics parity against the current spec before filling gaps.
  Candidate follow-ups are query/ack behavior, Unicode placeholders,
  placement-id edge cases, and animation.

Security:

- `MEDIUM`
- Image protocols are parser and resource-exhaustion surfaces. They should
  remain quota-bound and should never access arbitrary local files outside the
  protocol's safe temp-file rules.

### Keyboard And Input

Status:

- `Implemented`

Implemented:

- kitty keyboard protocol mode stack and key encoding
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
- Rich keyboard reporting is opt-in by the foreground app. It should not let
  the host disable local terminal keybindings or trusted UI actions.

### Shell Integration And Prompt Marks

Status:

- `Implemented`

Implemented:

- OSC 7 current directory
- OSC 133 prompt, command, output, and exit-status marks
- prompt navigation and gutter status markers

Maintenance direction:

- Keep OSC 133 aligned with FinalTerm, iTerm2, WezTerm, Ghostty, and Windows
  Terminal behavior.
- Preserve the distinction between host-provided semantic marks and
  terminal-owned UI. Prompt marks may annotate rows; they must not become
  trusted banners.

Security:

- `MEDIUM`
- Prompt metadata is spoofable by any process that can write to the PTY.

## Planned

### OSC 633 Shell Integration

Status:

- `Planned`

Why:

- VS Code's integrated terminal uses OSC 633 for richer shell integration,
  including prompt boundaries, command boundaries, current directory, and an
  optional nonce for command-line metadata.
- OSC 633 overlaps with OSC 133 enough that `term41` can map the safe subset
  into the existing shell-integration model.

Scope:

- Support `OSC 633 ; A/B/C/D` as aliases for existing prompt lifecycle marks.
- Support `OSC 633 ; P ; Cwd=...` as a current-directory update, with the same
  local-path filtering as OSC 7.
- Treat `OSC 633 ; E ; <commandline> [; <nonce>]` as untrusted metadata unless
  `term41` eventually owns a shell-integration script and nonce handshake.

Security:

- `MEDIUM`
- Command-line metadata is useful, but spoofable. Display it only as untrusted
  terminal-content annotation.

### Clipboard Policy Controls

Status:

- `Planned`

Why:

- Alacritty and Ghostty expose explicit OSC 52 policy controls because
  clipboard reads and writes have different risk profiles.
- `term41` currently implements OSC 52, but the policy should be visible and
  user-configurable.

Scope:

- Add configuration for OSC 52 read and write behavior.
- Strong candidate defaults:
  - writes allowed or ask, because editor-copy use cases are common
  - reads denied or ask, because they exfiltrate local clipboard contents
- Keep per-tab clipboard scoping where it already exists.
- Add payload size limits and clear failure behavior.

Security:

- `HIGH` for reads
- `MEDIUM` for writes

### Kitty Graphics Parity Audit

Status:

- `Planned`

Why:

- `term41` has a substantial kitty graphics implementation, but the upstream
  protocol has grown to include features such as Unicode placeholders,
  animation, richer query behavior, and more deletion/placement variants.

Scope:

- Produce a compatibility matrix against the current kitty graphics spec.
- Implement missing behavior only when it affects real tools.
- Keep animation and placeholder support separately scoped because both affect
  renderer lifecycle and scrollback semantics.

Security:

- `MEDIUM`
- The audit should verify quotas, temp-file restrictions, and response
  suppression behavior.

### Capability Reporting

Status:

- `Planned`

Why:

- Modern apps increasingly probe terminal capabilities instead of relying only
  on `$TERM`.
- iTerm2 has feature reporting, kitty has protocol-specific queries, and many
  terminals expose some form of version/capability response.

Scope:

- Continue XTVERSION support.
- Add policy-filtered responses for implemented protocols where there is a
  documented query.
- Avoid inventing a large `term41`-specific feature namespace until there is a
  concrete application need.

Security:

- `LOW` to `MEDIUM`
- Capability reporting is fingerprinting surface. Report coarse, useful facts,
  not detailed host configuration.

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

- Ghostty and kitty-family protocols expose system-theme information to
  applications so TUIs can adapt colors.
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
- `term41` already has OSC 4 / OSC 10 / OSC 11 color support plus a substantial
  VT525 color-control implementation, so the remaining question is application
  demand rather than basic capability.

Decision:

- Watch for real application use before implementing more color namespaces.
- Any implementation should integrate with the existing color table and theme
  reload model rather than adding a parallel palette path.

Security:

- `LOW` to `MEDIUM`

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

- This overlaps with local tabs/windows and with multiplexers such as `tmux`
  and `zellij`.
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
- New region styling extensions create surprising retroactive visual changes
  and make spoofing analysis harder.

Security:

- `MEDIUM`

### Multiple Terminal Cursors

Status:

- `Explicitly not planned`

Why:

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

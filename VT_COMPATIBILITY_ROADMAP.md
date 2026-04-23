# VT420/VT520/VT525 Coverage Roadmap

This document tracks major DEC VT-family feature coverage in `term41`, based on
the currently published VT420 and VT520/VT525 manuals. It records both
implemented compatibility work and explicit non-goals.

It is intentionally DEC-spec-focused. `term41` already implements a useful
subset of VT220/VT420-style screen control plus modern xterm/kitty features.
Full VT420/VT520/VT525 coverage is no longer a project goal when the remaining
features are better handled by local tools, carry disproportionate complexity,
or cross security-sensitive trust boundaries.

## Security Legend

- `HIGH`: can plausibly inject input, exfiltrate data, or create a serious
  spoofing/trust-boundary problem
- `MEDIUM`: can mislead users, fingerprint the terminal, or expand parser /
  resource-exhaustion attack surface
- `LOW`: mostly correctness / compatibility impact

## Important Note On Command Injection

The main DEC-era features with real "host programs terminal, terminal later
emits input" risk are:

- `DECUDK` user-defined keys
- `DECDMAC` / `DECINVM` downloaded macros
- `ENQ` / answerback and `DECAAM` auto-answerback

I did not find a DEC control function in the manuals I checked that is literally
named "unknown command echo". If that memory is accurate, it is likely referring
to one of the features above, or to host-side shell behavior rather than a
distinct DEC terminal primitive.

## Priority 1: VT420 Core Gaps

### [x] 1. Full conformance-level and 7-bit / 8-bit control negotiation

Missing or incomplete:

- [x] `DECSCL`
- [x] `S7C1T` / `S8C1T`
- [x] `DECSCLM` smooth/jump scroll
- [x] complete VT-level switching behavior across VT100 / VT200 / VT400 modes

Why it matters:

- This is the foundation for "real VT420 mode" rather than "VT420-like reply
  strings on a mostly VT220/xterm core."
- Other controls such as status-line behavior and character-set behavior are
  defined in terms of the active conformance level.

Security:

- `LOW`

### [x] 2. Status-line model

Completed:

- [x] `DECSASD`
- [x] `DECSSDT`
- [x] dedicated status-line storage and routing rules
- [x] host-writable status-line semantics
- [x] visually distinct renderer treatment with border, gutter marker, and
  dedicated configurable colors

Why it matters:

- VT420/VT520 status-line behavior is not just decoration; it is a separate
  display surface with distinct routing rules.
- The current `Screen` model has no status-line buffer or active-display
  selector.

Security:

- `MEDIUM`
- A host-writable status line can spoof prompts, trusted banners, or terminal
  status. If implemented, it should be visually distinct from normal shell
  output.

### [x] 3. Full DEC character-set engine

Completed:

- [x] `DECNRCM`
- [x] `DECAUPSS` / `DECRQUPSS`
- [x] National Replacement Character Sets
- [x] DEC Technical Character Set
- [x] DEC Supplemental / ISO Latin-1 style supplemental handling
- [x] 94- and 96-character set designation
- [x] locking-shift behavior across the full DEC model
- [x] parser-aware UTF-8 vs 8-bit text-mode handling (`ESC % G` / `ESC % 8` /
  `ESC % @`)
- [x] raw 8-bit graphic-byte routing through GR

Why it matters:

- Full VT420/VT520 behavior requires a real ISO-2022-style designation and
  invocation model.

Explicitly unplanned follow-up work:

- Batch raw 8-bit graphic-byte runs in `vtepp` for performance parity with
  `PrintAscii` / `PrintText`.

Decision:

- Correctness is complete, and the remaining scalar raw-byte path is not worth
  the parser complexity unless profiling shows it matters in real workloads.

Security:

- `LOW`

### [x] 4. Downloadable soft character sets (DRCS / `DECDLD`)

Implemented:

- [x] `DECDLD`
- [x] DRCS storage, designation, invocation, and lifetime rules
- [x] bounded single-payload and total-storage limits
- [x] render-path support in both the GPU and startup software backends

Why it matters:

- VT420-class terminals support downloadable character glyphs; this is part of
  the character-set subsystem, not the graphics subsystem.
- `term41` now routes `DCS ... { ... ST` through a dedicated DRCS path while
  still treating bare `DCS ... q` as sixel.

Security:

- `MEDIUM`
- This expands the binary parser surface and introduces memory-management and
  rendering-safety issues.
- The implementation therefore hard-limits both a single DRCS payload and the
  total in-memory soft-font store to bound denial-of-service risk.

### [x] 5. Full tab/page/geometry control family

Implemented:

- [x] `DECSNLS`
- [x] `DECSLPP`
- [x] `DECSCPP`
- [x] `DECTABSR` / `DECRQPSR`
- [x] page navigation / reporting (`NP`, `PP`, `PPA`, `PPR`, `PPB`, DECXCPR
  page)
- [x] per-tab host-driven geometry requests wired through the window thread
- [x] cross-page rectangular-area semantics for `DECCRA` source/destination
  pages

Why it matters:

- VT420 and later terminals define more than just viewport rows/cols. They have
  page length, screen line count, and page-memory semantics that many control
  functions reference.

Security:

- `LOW`

### [x] 6. Rectangular-area completion

Implemented:

- [x] `DECSERA`
- [x] `DECSACE`
- [x] correct VT420 opcode mapping for `DECCARA` / `DECRARA`
- [x] VT420 attribute-change semantics for `DECCARA` / `DECRARA`

Why it matters:

- `term41` now supports the full VT420 rectangular-area control family:
  `DECERA`, `DECFRA`, `DECCRA`, `DECSERA`, `DECSACE`, `DECCARA`, and `DECRARA`.

Security:

- `LOW`

### [x] 7. Reset, test, and state-report families

Missing or incomplete:

- [x] `DECTST`
- [x] `DECSR` / `DECSRC`
- [x] `DECRPM`
- [x] broader `DECRQSS`
- [x] `DECRQPSR` / `DECCIR`
- [x] `DECRQTSR` / `DECRSTS`
- [x] `DECRSPS`

Why it matters:

- A complete VT420/VT520 implementation needs more than DA/DSR. DEC terminals
  expose richer report, restore, and self-test behavior.
- `term41` now covers the VT420 reset/test/state-report family: `DECTST`,
  `DECSR` / `DECSRC`, `DECRPM`, broader `DECRQSS`, `DECRQPSR` / `DECCIR`,
  `DECRQTSR` / `DECRSTS`, and `DECRSPS`.

Security:

- `LOW` to `MEDIUM`
- State save/restore can make host-controlled changes persist longer than users
  expect. Keep any persistent-state hooks explicit and local-only.

## Priority 2: Keyboard, Local Functions, and Input Programming

### [x] 8. User-defined keys (`DECUDK`) and related controls

Implemented:

- [x] `DECUDK`
- [x] `DECLFKC`
- [x] `DECELF`
- [x] `DECSMKR`
- [x] UDK lock DSR (`CSI ? 25 n`)
- [x] DA1 advertisement gated on UDK authorization
- [x] bounded UDK storage
- [x] basic legacy DEC modifier reports for aggregate Shift/Ctrl/Alt state
- [x] emulator-owned indicator-line UDK status badges

Why it matters:

- VT420/VT520 keyboards are host-programmable.
- `term41` now implements the host-visible VT420 UDK surface while keeping it
  behind the same style of explicit security gate as downloaded macros.

Security:

- `HIGH`
- A hostile host can redefine function keys so that a later local keypress emits
  shell commands or control sequences into the session.
- UDK loading and related keyboard-control mutations are disabled by default.
  Users can opt in with `[security.features] udks = "all"`.
- Users who also enable `status_line = "indicator"` get a visible terminal-owned
  status surface showing UDK enablement and programmed keys such as `[F6]`.
- winit exposes aggregate modifier state rather than reliable left/right DEC
  modifier identity, so `term41` reports aggregate Shift/Ctrl/Alt transitions
  through the closest DEC modifier selectors.

### [x] 9. Downloaded macros (`DECDMAC`, `DECINVM`)

Implemented:

- [x] `DECDMAC`
- [x] `DECINVM`
- [x] bounded macro storage and invocation depth
- [x] DA1 advertisement gated on macro authorization
- [x] foreground-process-set allowlist enforcement for define/invoke paths

Why it matters:

- VT420 supports downloadable macros as a first-class feature.
- This is now implemented with a stricter security model than a stock DEC
  terminal: macro support is only exposed if explicitly enabled.

Security:

- `HIGH`
- Downloaded macros are effectively remote-programmed local actions. If the user
  later invokes one, it can inject arbitrary text or escape sequences into the
  active session.
- This is one of the clearest shell-injection vectors in the DEC feature set.
- `term41` therefore keeps the feature default-deny.

### [x] 10. Answerback and auto-answerback

Status:

- Explicitly not planned.

Not planned:

- `ENQ` answerback handling
- `DECAAM`
- related answerback setup/report plumbing

Decision:

- Answerback is a legacy terminal-identity mechanism with little practical
  value for modern shells and terminal applications.
- It creates a host-triggered terminal-originated string surface. Even if
  user-configured rather than host-programmable, that is another input-like
  path whose behavior is easy to misunderstand.
- The compatibility benefit does not justify the security model, configuration
  surface, and reporting complexity.

Security:

- `MEDIUM`
- Answerback leaks a terminal-originated string to the host.
- If users ever configure a command-like answerback string, a remote host that
  provokes answerback at the wrong time can cause confusing or unsafe input to
  appear in the session.
- If implemented, keep answerback user-configured, not host-programmable.

### [x] 11. Remaining DEC keyboard modes and reports

Status:

- Explicitly not planned.

Not planned:

- `DECBKM`
- `DECKBUM`
- DEC keyboard identify / report families such as `DECEKBD`
- full DEC keypad / editing-key compatibility behavior

Decision:

- These are narrow compatibility controls that let the host reshape local
  keyboard semantics.
- The modern value is low compared with the complexity of faithfully modeling
  old DEC keyboard variants, reporting behavior, and local overrides.
- `term41` should keep local keybindings and trust-sensitive keyboard behavior
  under local configuration, with only broadly useful compatibility modes
  exposed to host applications.

Security:

- `MEDIUM`
- Host-controlled keyboard behavior can confuse users and break local trust
  assumptions even when it is not directly injecting commands.

## Priority 3: Printer, Media Copy, and External I/O

### [x] 12. Printer port control and media-copy family

Status:

- Explicitly not planned.

Not planned:

- `DECPEX`
- `DECPFF`
- `MC` media-copy family
- autoprint mode
- printer controller mode
- print-page / print-screen / print-line variants
- printer-to-host session and printer assignment controls

Decision:

- Printer support is a large, explicit part of VT420/VT520 behavior, but it is
  an external-I/O subsystem rather than normal terminal rendering.
- Modern users are better served by shell tools, print commands, and local
  desktop print flows than by host-controlled terminal printer modes.
- Implementing this would add parser, buffering, routing, configuration, and
  device/security-policy complexity for a feature that should almost never be
  exposed to an untrusted host.

Security:

- `HIGH`
- This is a direct data-exfiltration surface if the terminal has access to a
  local printer, print spooler, or serial side channel.
- Printer controller mode also creates another raw byte-stream parser and I/O
  path that should not be exposed by default.

### [x] 13. Session management and multi-port behavior

Status:

- Explicitly not planned.

Not planned:

- session-management control families
- dual-session routing behavior
- host-selectable printer/session coupling
- page-memory and split-session behavior

Decision:

- VT420 and later hardware supported multi-session use cases, but that design
  belongs to terminal hardware with multiple ports.
- In a modern emulator, this overlaps with local tabs, windows, `tmux`, and
  `zellij`, which provide clearer user-controlled session routing.
- Host-selectable session and port routing is a poor fit for `term41`'s trust
  boundary.

Security:

- `MEDIUM` to `HIGH`
- Any feature that can reroute local I/O between sessions or external ports
  should be treated as privileged.

## Priority 4: VT52 Completeness

### [x] 14. Remaining VT52 printer / media-copy controls

Status:

- Explicitly not planned.

Not planned from the VT52 subset:

- `ESC ^` enter autoprint mode
- `ESC _` exit autoprint mode
- `ESC W` enter printer controller mode
- `ESC X` exit printer controller mode
- `ESC ]` print screen
- `ESC V` print cursor line

Decision:

- These inherit the same external-I/O and spoofing concerns as the ANSI/DEC
  printer and media-copy controls.
- `term41` will keep VT52 support focused on common cursor, erase, and identify
  behavior rather than adding legacy printer routing.

Security:

- `HIGH`
- These inherit the same printer/media-copy concerns as the ANSI/DEC printer
  controls.

## Priority 5: VT500 / VT520 / VT525-Only Features

### [x] 15. Bidirectional text, Hebrew, and VT500 internationalization features

Status:

- Explicitly not planned.

Not planned:

- `DECRLM`
- `DECRLCM`
- VT500-era keyboard and charset variants tied to bidi / Hebrew support

Decision:

- These are part of real VT520/VT525 coverage, but faithful bidi/Hebrew support
  would require substantial text-layout, keyboard, charset, selection, and
  rendering work.
- That complexity would serve very little modern terminal software and would
  increase the risk of inconsistent visual ordering, confusing copy/paste
  behavior, and spoofing-adjacent text presentation bugs.

Security:

- `LOW`

### [x] 16. VT500 page/window/session features

Status:

- Explicitly not planned.

Decision:

- VT500-class terminals add richer desktop/session concepts, but those use
  cases are covered better by local multiplexers such as `tmux` and `zellij`.
- `term41` keeps session, tab, window, pane, and input-routing UI under local
  terminal control rather than exposing host-driven escape-sequence controls
  that can create or reshape those surfaces.

Security:

- `MEDIUM`
- These features can obscure what session the user is interacting with and may
  become spoofing hazards if they are rendered too similarly to normal shell
  content.

### [x] 17. Additional VT500 report / setup families

Status:

- Explicitly not planned.

Not planned:

- VT500-specific status and setup report controls
- broader host-manageable desktop and setup surfaces

Decision:

- These close the gap between "VT420-ish emulator" and "VT525 emulator," but
  they mostly expose setup and desktop surfaces that do not map cleanly onto a
  modern, locally configured emulator.
- Host-manageable setup surfaces increase fingerprinting and spoofing risk
  without enough practical compatibility value.

Security:

- `LOW` to `MEDIUM`

## High-Risk Features And Non-Goals

Host-programmable input and external-I/O features are either default-deny when
implemented or explicitly not planned when their complexity and security risk
outweigh their compatibility value:

- [x] `DECUDK`
- [x] `DECDMAC` / `DECINVM`
- [x] printer controller mode: explicitly not planned
- [x] autoprint / print-page / print-screen features: explicitly not planned
- [x] printer-to-host session features: explicitly not planned
- [x] any feature that reroutes data between sessions or external ports:
  explicitly not planned

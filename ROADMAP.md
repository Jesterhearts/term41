# VT420/VT520/VT525 Coverage Roadmap

This document tracks the major DEC VT-family features that `term41` does not
yet implement, based on the currently published VT420 and VT520/VT525 manuals.

It is intentionally DEC-spec-focused. `term41` already implements a useful
subset of VT220/VT420-style screen control plus modern xterm/kitty features,
but it is still far from full VT420/VT520/VT525 coverage.

## Scope

This roadmap is derived from:

- `README.md`
- `terminal41/src/mode.rs`
- `terminal41/src/screen.rs`
- `terminal41/src/parser.rs`
- `terminal41/src/lib.rs`
- DEC VT420 programming summary and reference material
- DEC VT520/VT525 programmer information

The current implementation already covers:

- Core cursor/screen movement and erase functions
- Primary/alternate screens
- Scroll regions and left/right margins
- DECALN and DEC line attributes
- A subset of VT52
- DA1/DA2/DA3, DSR, DECRQM, part of DECRQSS
- Sixel images
- xterm mouse/focus/window-title features
- kitty keyboard, kitty graphics, OSC 8/52/133/1337

The missing items below are the major blockers for "supports the published VT
specs" rather than "supports the subset most full-screen Unix apps care about."

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

I did not find a DEC control function in the manuals I checked that is
literally named "unknown command echo". If that memory is accurate, it is
likely referring to one of the features above, or to host-side shell behavior
rather than a distinct DEC terminal primitive.

## Priority 0: Security Policy Before Feature Work

These guardrails should exist before implementing any of the `HIGH` items:

- [ ] Add a config gate for host-programmable local-input features:
  `allow_udk`, `allow_downloaded_macros`, `allow_answerback`, and
  `allow_printer_control`, all defaulting to `false`.
- [ ] Make dangerous features visibly discoverable at runtime:
  mode indicator, log line, or one-shot warning when a host first attempts to
  enable them.
- [ ] Never let a remote escape sequence synthesize PTY input immediately unless
  the spec explicitly requires it and the user opted in.
- [ ] Put strict size, time, and allocation limits on any new DCS payloads
  (especially DRCS / soft character downloads).
- [ ] Keep parser state and "host may control local hardware / local keys" state
  separate from ordinary screen emulation state.

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
- [x] parser-aware UTF-8 vs 8-bit text-mode handling (`ESC % G` / `ESC % 8` / `ESC % @`)
- [x] raw 8-bit graphic-byte routing through GR

Why it matters:

- Full VT420/VT520 behavior requires a real ISO-2022-style designation and
  invocation model.

Remaining follow-up work:

- [ ] Batch raw 8-bit graphic-byte runs in `vtepp` for performance parity with
  `PrintAscii` / `PrintText` (correctness is complete; current raw-byte path is
  scalar)

Security:

- `LOW`

### [ ] 4. Downloadable soft character sets (DRCS / `DECDLD`)

Missing:

- [ ] `DECDLD`
- [ ] DRCS storage, designation, invocation, and lifetime rules

Why it matters:

- VT420-class terminals support downloadable character glyphs; this is part of
  the character-set subsystem, not the graphics subsystem.
- `term41` currently treats bare `DCS ... q` as sixel payload, which overlaps
  with a control-family DEC also used for soft-character loading.

Security:

- `MEDIUM`
- This expands the binary parser surface and introduces memory-management and
  rendering-safety issues. It should be bounded and opt-in.

### [x] 5. Full tab/page/geometry control family

Implemented:

- [x] `DECSNLS`
- [x] `DECSLPP`
- [x] `DECSCPP`
- [x] `DECTABSR` / `DECRQPSR`
- [x] page navigation / reporting (`NP`, `PP`, `PPA`, `PPR`, `PPB`, DECXCPR page)
- [x] per-tab host-driven geometry requests wired through the window thread
- [x] cross-page rectangular-area semantics for `DECCRA` source/destination pages

Why it matters:

- VT420 and later terminals define more than just viewport rows/cols. They
  have page length, screen line count, and page-memory semantics that many
  control functions reference.

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
  `DECERA`, `DECFRA`, `DECCRA`, `DECSERA`, `DECSACE`, `DECCARA`, and
  `DECRARA`.

Security:

- `LOW`

### [ ] 7. Reset, test, and state-report families

Missing or incomplete:

- [ ] `DECTST`
- [ ] `DECSR` / `DECSRC`
- [ ] `DECRPM`
- [ ] broader `DECRQSS`
- [ ] `DECRQPSR` / `DECCIR`
- [ ] `DECRQTSR` / `DECRSTS`
- [ ] `DECRSPS`

Why it matters:

- A complete VT420/VT520 implementation needs more than DA/DSR. DEC terminals
  expose richer report, restore, and self-test behavior.

Security:

- `LOW` to `MEDIUM`
- State save/restore can make host-controlled changes persist longer than users
  expect. Keep any persistent-state hooks explicit and local-only.

## Priority 2: Keyboard, Local Functions, and Input Programming

### [ ] 8. User-defined keys (`DECUDK`) and related controls

Missing:

- [ ] `DECUDK`
- [ ] `DECLFKC`
- [ ] `DECELF`
- [ ] `DECSMKR`
- [ ] legacy DEC keyboard reports and compatibility behaviors

Why it matters:

- VT420/VT520 keyboards are host-programmable in ways `term41` does not model.
- The current code supports kitty keyboard mode negotiation, not DEC UDK /
  local-function programming.

Security:

- `HIGH`
- A hostile host can redefine function keys so that a later local keypress
  emits shell commands or control sequences into the session.
- If implemented at all, UDK loading should be disabled by default and
  surfaced prominently to the user.

### [ ] 9. Downloaded macros (`DECDMAC`, `DECINVM`)

Missing:

- [ ] `DECDMAC`
- [ ] `DECINVM`

Why it matters:

- VT420 supports downloadable macros as a first-class feature.
- This is a meaningful functional gap if the goal is full VT420 compatibility.

Security:

- `HIGH`
- Downloaded macros are effectively remote-programmed local actions. If the
  user later invokes one, it can inject arbitrary text or escape sequences into
  the active session.
- This is one of the clearest shell-injection vectors in the DEC feature set.

### [ ] 10. Answerback and auto-answerback

Missing:

- [ ] `ENQ` answerback handling
- [ ] `DECAAM`
- [ ] related answerback setup/report plumbing

Why it matters:

- The VT420 summary explicitly lists `ENQ` answerback behavior, and later DEC
  terminals define auto-answerback mode as a private mode.

Security:

- `MEDIUM`
- Answerback leaks a terminal-originated string to the host.
- If users ever configure a command-like answerback string, a remote host that
  provokes answerback at the wrong time can cause confusing or unsafe input to
  appear in the session.
- If implemented, keep answerback user-configured, not host-programmable.

### [ ] 11. Remaining DEC keyboard modes and reports

Missing or incomplete:

- [ ] `DECBKM`
- [ ] `DECKBUM`
- [ ] DEC keyboard identify / report families such as `DECEKBD`
- [ ] full DEC keypad / editing-key compatibility behavior

Why it matters:

- VT420/VT520 compatibility includes more than cursor-key and keypad-app mode.

Security:

- `MEDIUM`
- Host-controlled keyboard behavior can confuse users and break local trust
  assumptions even when it is not directly injecting commands.

## Priority 3: Printer, Media Copy, and External I/O

### [ ] 12. Printer port control and media-copy family

Missing:

- [ ] `DECPEX`
- [ ] `DECPFF`
- [ ] `MC` media-copy family
- [ ] autoprint mode
- [ ] printer controller mode
- [ ] print-page / print-screen / print-line variants
- [ ] printer-to-host session and printer assignment controls

Why it matters:

- Printer support is a large, explicit section of VT420/VT520 behavior.
- The current code advertises printer capability in DA1-style replies without
  implementing a printer subsystem.

Security:

- `HIGH`
- This is a direct data-exfiltration surface if the terminal has access to a
  local printer, print spooler, or serial side channel.
- Printer controller mode also creates another raw byte-stream parser and I/O
  path that should not be exposed by default.

### [ ] 13. Session management and multi-port behavior

Missing:

- [ ] session-management control families
- [ ] dual-session routing behavior
- [ ] host-selectable printer/session coupling
- [ ] page-memory and split-session behavior

Why it matters:

- VT420 and later hardware supported multi-session use cases that `term41`
  does not model at all.

Security:

- `MEDIUM` to `HIGH`
- Any feature that can reroute local I/O between sessions or external ports
  should be treated as privileged.

## Priority 4: VT52 Completeness

### [ ] 14. Remaining VT52 printer / media-copy controls

Missing from the VT52 subset:

- [ ] `ESC ^` enter autoprint mode
- [ ] `ESC _` exit autoprint mode
- [ ] `ESC W` enter printer controller mode
- [ ] `ESC X` exit printer controller mode
- [ ] `ESC ]` print screen
- [ ] `ESC V` print cursor line

Why it matters:

- The current VT52 implementation covers only the common cursor/erase/identify
  subset.

Security:

- `HIGH`
- These inherit the same printer/media-copy concerns as the ANSI/DEC printer
  controls.

## Priority 5: VT500 / VT520 / VT525-Only Features

### [ ] 15. Bidirectional text, Hebrew, and VT500 internationalization features

Missing:

- [ ] `DECRLM`
- [ ] `DECRLCM`
- [ ] VT500-era keyboard and charset variants tied to bidi / Hebrew support

Why it matters:

- These are part of real VT520/VT525 coverage, even though they are irrelevant
  to most Unix full-screen programs.

Security:

- `LOW`

### [ ] 16. VT500 page/window/session features

Missing:

- [ ] `DECVSSM`
- [ ] `DECPCCM`
- [ ] `CSI & x` session command family
- [ ] other VT500 multi-session / multi-window controls

Why it matters:

- VT500-class terminals are not just "VT420 plus a few more reports." They add
  richer desktop/session concepts.

Security:

- `MEDIUM`
- These features can obscure what session the user is interacting with and may
  become spoofing hazards if they are rendered too similarly to normal shell
  content.

### [ ] 17. Additional VT500 report / setup families

Missing:

- [ ] VT500-specific status and setup report controls
- [ ] broader host-manageable desktop and setup surfaces

Why it matters:

- These close the gap between "VT420-ish emulator" and "VT525 emulator."

Security:

- `LOW` to `MEDIUM`

## Features That Should Probably Stay Disabled By Default

Even if implemented for spec completeness, these should almost certainly be
off unless the user explicitly enables them:

- [ ] `DECUDK`
- [ ] `DECDMAC` / `DECINVM`
- [ ] printer controller mode
- [ ] autoprint / print-page / print-screen features
- [ ] printer-to-host session features
- [ ] any feature that reroutes data between sessions or external ports

## Suggested Implementation Order

1. [ ] Finish safe VT420 display-state work:
   `DECSCL`, `S7C1T` / `S8C1T`, `DECSASD`, `DECSSDT`, `DECSNLS`, `DECSLPP`,
   `DECSCPP`, `DECSERA`, `DECSACE`, `DECTST`, more `DECRQSS`.
2. [ ] Build a real DEC character-set subsystem:
   `DECNRCM`, `DECAUPSS` / `DECRQUPSS`, NRC sets, DEC Technical, supplemental
   sets.
3. [ ] Decide whether full page/session/printer behavior is actually in scope:
   if yes, implement it as a separate subsystem rather than extending the
   current single-screen xterm-like model ad hoc.
4. [ ] Add dangerous local-programming features only behind explicit opt-in:
   `DECUDK`, `DECDMAC`, answerback / auto-answerback, and printer/media-copy.
5. [ ] Only after the above, chase VT520/VT525-specific desktop/session features.

## Sources

- VT420 Programming Summary:
  https://vt100.net/docs/vt420-uu/chapter9.html
- VT420 Programmer Reference Manual:
  https://vt100.net/mirror/mds-199909/cd3/term/vt420rm2.pdf
- VT520/VT525 Programmer Information:
  https://vt100.net/dec/ek-vt520-rm.pdf
- DECSSDT reference:
  https://vt100.net/docs/vt510-rm/DECSSDT.html
- DEC Technical Character Set notes:
  https://vt100.net/charsets/technical.html

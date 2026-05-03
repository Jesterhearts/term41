<p align="center">
  <img src="resources/icon.svg" alt="term41 icon" width="160" />
</p>

# term41

<p align="center">
  <video src="https://github.com/user-attachments/assets/13ff206f-2e1a-4c82-b629-9257f3d9cf4d">
  </video>
</p>

A GPU-accelerated terminal emulator written in Rust. It features fast startup
times (target <100ms TTFP on my machine) and responsive handling (target <1
frame of delay even under heavy load).

> **Note:** This project uses a decent amount of LLM-assisted coding. VTEs have
> a huge feature surface, and implementing it in a reasonable time frame is only
> possible thanks to LLM assistance.

## Why?

I've been enjoying coding apps I never had the time for in the past with the
assistance of LLMs, and term41 is the product of one such experiment. I know
there are many other terminal emulators and this one is nothing special, but
I've always wanted to write my own, with the features I prefer.

## Possible Objections

1. You use AI.
   - Fair enough.
2. It's ugly.
   - I don't think it's that bad, but only using the same graphics primitives
     for menus/modals as the rest of the terminal does give it a certain
     character. Maybe someday it won't be.
3. It doesn't support `$feature` from the VT feature set.
   - I probably haven't implemented it yet, or it has security concerns and I'm
     leery of implementing it.
4. It lies about being iTerm, and breaks my app because you don't support
   `$extension`.
   - I'd like to add `$extension` so your app isn't broken. Please file a bug :)
   - I lie about being iTerm because testing for e.g. iTerm's image support
     feature in terminals is hardcoded by terminal host name sometimes, and it
     seemed like the most reasonable choice to lie about so they use the iTerm
     image API.

## Overview

What I wanted out of this terminal was pretty straightforward:

- Fast startup times
- High throughput
- Low latency
- Unicode shaping and fallback fonts
- modern image protocols (`sixel`, Kitty, OSC 1337)
- DEC/VT-style terminal emulation, including page geometry, rectangular ops,
  status lines, macros, and user-defined keys
- shell integration, tabs, scrollback search, hyperlinks, and background images

Release notes live in [CHANGELOG.md](CHANGELOG.md).

<details>
<summary><strong>Protocol and Spec Compatibility</strong></summary>

Legend:

- ✅ Supported
- ❌ Unplanned
- 🟨 Planned, not supported yet
- 🟦 Watching, not committed yet

### DEC / VT Compatibility

| Area                                       | Status       | Notes                                                                                                                   |
| ------------------------------------------ | ------------ | ----------------------------------------------------------------------------------------------------------------------- |
| VT100/VT220-style screen control           | ✅ Supported | Primary/alternate screen, cursor movement, erase/edit operations, scroll regions, tab stops, SGR, DA/DSR-style reports. |
| VT420 conformance and 7-bit/8-bit controls | ✅ Supported | `DECSCL`, `S7C1T`, `S8C1T`, and VT100/VT200/VT400 switching behavior.                                                   |
| DEC status lines                           | ✅ Supported | Host-writable status-line routing with visually distinct terminal rendering.                                            |
| DEC character-set engine                   | ✅ Supported | NRC sets, DEC Technical, DEC Supplemental, 94/96 designation, GL/GR invocation, UTF-8 and raw 8-bit modes.              |
| DRCS downloadable soft fonts               | ✅ Supported | `DECDLD` with bounded storage and render-path support.                                                                  |
| VT420 page, tab, and geometry controls     | ✅ Supported | `DECSNLS`, `DECSLPP`, `DECSCPP`, page navigation/reporting, tab-stop reports, and host resize requests.                 |
| VT420 rectangular-area controls            | ✅ Supported | `DECERA`, `DECFRA`, `DECCRA`, `DECSERA`, `DECSACE`, `DECCARA`, and `DECRARA`.                                           |
| VT420 reset, test, and state reports       | ✅ Supported | `DECTST`, `DECSR`/`DECSRC`, `DECRPM`, `DECRQSS`, `DECRQPSR`/`DECCIR`, `DECRQTSR`/`DECRSTS`, and `DECRSPS`.              |
| DEC user-defined keys                      | ✅ Supported | Implemented behind the `[security.features] udks` allowlist.                                                            |
| DEC downloaded macros                      | ✅ Supported | `DECDMAC`/`DECINVM`, bounded and disabled unless explicitly allowed.                                                    |
| Answerback / auto-answerback               | ❌ Unplanned | Legacy terminal-originated string surface with low modern value.                                                        |
| DEC keyboard reshape/report families       | ❌ Unplanned | `DECBKM`, `DECKBUM`, `DECEKBD`, and related host-controlled keyboard behavior.                                          |
| Printer and media-copy controls            | ❌ Unplanned | Printer controller, autoprint, print-screen/page/line, and printer-session controls.                                    |
| Host-routed session / multi-port behavior  | ❌ Unplanned | Covered better by local tabs/windows and multiplexers.                                                                  |
| VT52 cursor/erase/identify subset          | ✅ Supported | VT52 compatibility is focused on common cursor, erase, and identify behavior.                                           |
| VT52 printer controls                      | ❌ Unplanned | Same external-I/O risk as DEC printer controls.                                                                         |
| VT500 bidi/Hebrew features                 | ❌ Unplanned | Requires large text-layout and input work for little current app demand.                                                |
| VT500 desktop/session/setup surfaces       | ❌ Unplanned | Host-manageable local UI/setup surfaces do not fit the trust model.                                                     |

### Kitty Protocols

| Area                                   | Status                        | Notes                                                                                                               |
| -------------------------------------- | ----------------------------- | ------------------------------------------------------------------------------------------------------------------- |
| Kitty keyboard protocol                | ✅ Supported                  | Mode stack, key encoding, associated text, IME commits, and 7-bit/8-bit reply handling.                             |
| Kitty graphics direct payloads         | ✅ Supported                  | RGB, RGBA, PNG, zlib compression, chunking, transmit, transmit-and-display, and placement.                          |
| Kitty graphics file/temp-file payloads | ✅ Supported                  | File and temp-file media with byte range support and safe temp-file deletion rules.                                 |
| Kitty graphics placement model         | ✅ Supported                  | Image IDs, image numbers, placement IDs, relative placements, cell offsets, z-index, and expanded delete selectors. |
| Kitty graphics shared memory (`t=s`)   | ❌ Unplanned                  | Rejected as a local cross-process attack surface.                                                                   |
| Kitty graphics Unicode placeholders    | ✅ Supported                  | `U=1` virtual placements render from `U+10EEEE` placeholder cells with row/column/image-id combining marks.         |
| Kitty graphics animation actions       | 🟨 Planned, not supported yet | Needs separate frame mutation, lifecycle, and quota design.                                                         |
| Kitty text sizing protocol             | 🟦 Watching                   | Could be useful, but affects shaping, selection, hit testing, scrollback, and reflow.                               |
| Kitty mouse pointer shapes             | 🟦 Watching                   | Reasonable if real applications use it; should stay scoped to terminal content.                                     |
| Kitty color protocol additions         | 🟦 Watching                   | Watch for real app demand beyond existing OSC 4/10/11 and DEC color support.                                        |
| Kitty file transfer                    | ❌ Unplanned                  | Local file brokerage from untrusted PTY output is outside scope.                                                    |

### iTerm2 / OSC 1337

| Area                                                 | Status       | Notes                                                                                                     |
| ---------------------------------------------------- | ------------ | --------------------------------------------------------------------------------------------------------- |
| OSC 1337 inline images                               | ✅ Supported | Includes multipart image payloads.                                                                        |
| OSC 1337 `ReportCellSize`                            | ✅ Supported | Used by image-aware tools to size output.                                                                 |
| OSC 1337 `Capabilities` / `TERM_FEATURES`            | ✅ Supported | iTerm2 Terminal Feature Reporting with policy-filtered clipboard-write advertisement.                     |
| OSC 1337 current-directory / user-var style metadata | ✅ Supported | Safe metadata subset is accepted as untrusted annotation data.                                            |
| iTerm2 upload / download / silent file placement     | ❌ Unplanned | Host-triggered local file transfer is outside the trust model.                                            |
| iTerm2 terminal chrome controls                      | ❌ Unplanned | Profile switching, focus stealing, cursor guides, attention effects, and similar trusted-UI manipulation. |
| iTerm2 custom buttons                                | ❌ Unplanned | Host-defined UI that later emits input is too easy to confuse with trusted terminal controls.             |

### Other Modern Extensions

| Area                                           | Status       | Notes                                                                                             |
| ---------------------------------------------- | ------------ | ------------------------------------------------------------------------------------------------- |
| OSC 0 / OSC 2 titles                           | ✅ Supported | Common xterm-compatible title updates.                                                            |
| OSC 4 / OSC 10 / OSC 11 colors                 | ✅ Supported | Palette/default foreground/background queries and updates.                                        |
| OSC 7 current directory                        | ✅ Supported | Stored as untrusted metadata.                                                                     |
| OSC 8 hyperlinks                               | ✅ Supported | Hyperlinks attach to terminal cells.                                                              |
| OSC 52 clipboard                               | ✅ Supported | Read/write requests are policy-gated and default to asking.                                       |
| Bracketed paste                                | ✅ Supported | xterm-compatible paste wrapping.                                                                  |
| Focus reporting                                | ✅ Supported | Standard focus in/out reporting.                                                                  |
| xterm mouse protocols                          | ✅ Supported | Legacy, UTF-8, SGR, URXVT, and SGR-Pixels (`?1016`) encodings.                                    |
| Window and cell size reports                   | ✅ Supported | Includes common size-query responses.                                                             |
| XTVERSION-style reports                        | ✅ Supported | Coarse terminal version reporting.                                                                |
| Synchronized output (`DECSET 2026`)            | ✅ Supported | Buffered painting during synchronized update windows.                                             |
| OSC 133 shell integration                      | ✅ Supported | Prompt, command, output, and exit-status marks.                                                   |
| OSC 633 shell integration                      | ✅ Supported | VS Code-compatible safe subset mapped into the prompt model.                                      |
| Policy-filtered capability reporting           | ✅ Supported | XTGETTCAP reports implemented special-key, color, styling, and coarse terminal-name capabilities. |
| Glyph Protocol                                 | 🟦 Watching  | Interesting fit for session-local PUA glyphs once adoption stabilizes.                            |
| Light/dark mode notifications                  | 🟦 Watching  | Useful but fingerprinting-adjacent; would need coarse, configurable reporting.                    |
| Host-triggered desktop notifications           | ❌ Unplanned | Local attention and desktop integration should stay user-controlled.                              |
| Host-driven tabs/windows/panes/session routing | ❌ Unplanned | Local UI and input routing are not controlled by escape sequences.                                |
| Arbitrary region styling extensions            | ❌ Unplanned | Retroactive host styling makes spoofing and text ownership harder to reason about.                |
| Rich non-text clipboard data                   | ❌ Unplanned | Non-text clipboard formats expand local-integration and parser surface.                           |

</details>

## Building

If you just want to build it and run it:

```sh
cargo build --release
cargo run --release
```

### Installing

Releases are source-only. To build and install from the GitHub tag into your
cargo bin dir:

```sh
cargo install --git https://github.com/Jesterhearts/term41.git --tag 0.1.1 --locked term41
```

The default install enables FFmpeg-backed GIF/video decoding and Wayland
data-control clipboard support. If you want the smallest dependency surface, or
the fastest build times:

```sh
cargo install --git https://github.com/Jesterhearts/term41.git --tag 0.1.1 --locked --no-default-features term41
```

From a local checkout, use:

```sh
cargo install --path . --locked
```

### Cargo Features

| Feature                | Default | Description                                                                                                       |
| ---------------------- | ------- | ----------------------------------------------------------------------------------------------------------------- |
| `ffmpeg`               | on      | Enables GIF/video decode for inline images and animated backgrounds. Requires host `libav*` development packages. |
| `vulkan`               | off     | Uses Vulkan instead of OpenGL for rendering.                                                                      |
| `wayland-data-control` | on      | Enables `zwlr_data_control_manager_v1` clipboard access on Wayland compositors that support it.                   |

If you want a smaller build without ffmpeg or Wayland data-control:

```sh
cargo build --release --no-default-features
```

### Logging

If something is broken and you want to get more diagnostics:

```sh
RUST_LOG=info cargo run --release
```

## Security Model

While there is a broad feature set implemented, certain features carry security
considerations because they go beyond ordinary terminal text output and could
potentially be used for spoofing, injection, system fingerprinting, or data
exfiltration.

In practice, extensions that allow either of these should be default-deny:

1. Target-controlled content outside standard text output
2. Target-controlled emulator behavior

So the default is: do nothing unless the user explicitly opted in, or the
feature has a real authorization path.

VT420 macros and DEC user-defined keys stay denied unless you explicitly allow
them. This is currently a binary toggle between None/All processes. As far as I
know, there's no reliable way to say "these bytes in the pty came from this
process", so there's no safe way to authenticate that some set of bytes in the
input is from `good` vs `evil`. If such a way becomes available, I'm open to
adding a per-process allowlist.

OSC 52 clipboard reads and writes default to asking for each request. Allowing
from the confirmation modal applies only to the single clipboard request that
triggered the prompt.

<details>
<summary><strong>Feature Set</strong></summary>

### Rendering and UI

- GPU text/background rendering via `wgpu`
- configurable VSync and GPU power preference
- startup software paint path for low time-to-first-paint
- tabs, multiple windows, configurable opacity, and background images

### Text, Fonts, and Drawing

- Unicode shaping with fallback fonts (`harfrust` + `fontdb`)
- embedded Fairfax HD fallback font
- bold, italic, underline styles, strikethrough, overline, truecolor, 256-color
- color emoji and custom rasterisation for block/braille/box/legacy shapes
- wide characters, grapheme clusters, ZWJ sequences, variation selectors

### Terminal Emulation

- primary and alternate screens
- scroll regions, hardware tab stops, DECSCUSR cursor styles
- DA1/DA2, DSR, DECRQSS, window-size queries
- OSC 0/2 titles, OSC 7 cwd tracking, OSC 8 hyperlinks, OSC 52 clipboard
- DEC character-set engine including NRC sets, GL/GR invocation, UTF-8 and 8-bit
  text modes
- VT420 page/geometry controls, rectangular-area controls, and DEC status lines
- VT420 macros and DEC user-defined keys with allowlist-based gating

### Images and Media

- sixel graphics
- Kitty graphics protocol, including Unicode placeholder placements
- OSC 1337 inline images
- PNG and JPEG always available
- GIF/video formats available with the `ffmpeg` feature

### Input, Selection, and Shell Integration

- Kitty keyboard protocol
- xterm mouse tracking modes and encodings
- scrollback search
- OSC 133 / OSC 633 shell integration with prompt navigation and gutter status
  markers
- copy/paste, primary selection, hyperlink opening, and image paste as wallpaper

</details>

<details>
<summary><strong>Configuration</strong></summary>

Configuration is loaded from:

```text
$XDG_CONFIG_HOME/term41/config.toml
```

On Linux this is usually `~/.config/term41/config.toml`.

Everything is optional. If the file is missing or broken, term41 falls back to
built-in defaults. It tries hard to parse the config, so a failure in one
setting shouldn't break all the others. If something isn't working correctly,
try running with `warning` level logging, as parsing issues should be logged.

Most settings live-reload on save.

Example:

```toml
# ~/.config/term41/config.toml

opacity = 1.0
fonts = "JetBrains Mono, monospace"
font_size = 24.0
font_supersampling = 4
scrollback_lines = 10000
strict_altscreen_scrollback = false

cursor_shape = "block"
cursor_blink = true
bell = "off"

gutter = true
status_line = "indicator" # "off" or "indicator"

vsync = "auto"
# power_preference = "LowPower"

# background_image = "/path/to/wallpaper.png"
# background_opacity = 0.3

[security.features]
# Only turn this on if you need it.
# macros = "all"
# udks = "all"

[security.clipboard]
# read/write accept "ask", "all", "*", "allow", "deny", "no", or "none".
# read = "ask"
# write = "ask"

[security.kitty_graphics]
# files accepts "ask", "all", "*", "allow", "deny", "no", or "none".
# It controls kitty graphics t=f/t=t file payload reads.
# files = "ask"

[security.limits]
# Byte counts and nesting limits for terminal-owned protocol state.
# macro_storage_bytes = 6144
# macro_invocation_depth = 32
# udk_storage_bytes = 256
# decudk_payload_bytes = 2048
# drcs_payload_bytes = 65536
# xtgettcap_payload_bytes = 4096
# drcs_storage_bytes = 262144
# kitty_graphics_payload_bytes = 33554432
# kitty_graphics_storage_bytes = 134217728

[security.scripts.status]
# Scripts live in ~/.config/term41/scripts/<name>.lua.
# All permissions default to false.
# filesystem = true
# shell = false
# process_info = false
# resource_usage = false

[command_editor]
# Off by default. When enabled, it is active while OSC 133 / OSC 633 shell
# integration reports command-line editing on the primary screen. If the editor
# remains visible during command output, input keeps targeting the editor until
# foreground-app heuristics hide it.
# enabled = true
# vim_mode = false
# completions = ["cargo", "git", "rg"]
# Extra binary directories to scan for command-name completion. By default,
# these are merged with term41's platform/user tool directory list.
# binary_dirs = ["~/project/bin"]
# Set merge_extra_dirs = false to replace the default binary_dirs list instead.
# merge_extra_dirs = true
# Opt in to read-only shell history discovery through shellhist41.
# deep_history_integration = false
# Maximum entries loaded into the command editor history list.
# max_history = 200
# Maximum persisted command-editor entries retained per working directory.
# max_persistent_history_per_dir = 200

[colors.status_line]
# foreground = "#d8dee9"
# background = "#3b4252"

# Either form is accepted:
# [colors]
# cursor = "#88c0d0"
#
# [colors.cursor]
# cursor = "#88c0d0"
# text = "#2e3440"

keybindings = [
  { keys = "Shift+PageUp", action = "ScrollPageUp" },
  { keys = "Shift+PageDown", action = "ScrollPageDown" },
  { keys = "Ctrl+Shift+C", action = "Copy" },
  { keys = "Ctrl+Shift+V", action = "Paste" },
  { keys = "Ctrl+Shift+D", action = "ToggleCommandEditor" },
  { keys = "Ctrl+Shift+P", action = "OpenCommandPalette" },
  { keys = "Alt+Shift+L", action = "CycleEmojiCompatibility" },
]
```

Notes:

- `keybindings` replaces the default binding set rather than merging with it.
- `strict_altscreen_scrollback = true` restores a zero-scrollback alternate
  screen.
- `status_line = "indicator"` enables the emulator-owned DEC indicator line by
  default; when UDKs are enabled, it also shows UDK status and programmed key
  badges such as `[F6]`.
- `security.features.macros` and `security.features.udks` can be `"all"` or
  omitted/default-denied.
- `security.clipboard.read` and `security.clipboard.write` default to `"ask"`;
  `"allow"`/`"all"`/`"*"` skips the prompt, while `"deny"`/`"no"`/`"none"`
  blocks OSC 52 access.
- `security.kitty_graphics.files` defaults to `"ask"` for Kitty graphics
  `t=f`/`t=t` local-file payload reads. Ask mode shows the requested path in the
  trusted permission modal; deny mode rejects the image request.
- `[security.limits]` settings live-reload for new protocol actions. They
  control how much macro/UDK/DRCS/kitty graphics state term41 accepts or
  retains.
- Lua scripts are discovered from `$XDG_CONFIG_HOME/term41/scripts/*.lua`. Each
  script runs in its own Lua state on its own thread and can
  `require("terminal")` to read the active tab title/cwd and set the current tab
  title or indicator status text.
- `[security.scripts.<script_name>]` controls which optional libraries a script
  receives. The default sandbox has only basic string/table/math/utf8 support
  plus `require("terminal")`.
- `[command_editor]` enables the terminal-local command editor layer.
  `Ctrl+Shift+D` toggles the editor for the current runtime session without
  rewriting the config file.

  It keeps keyboard handling unchanged while disabled, uses Up/Down for its own
  command history while active, and completes prefixes from recent history,
  configured words, executable commands discovered from `PATH` plus
  `[command_editor]` `binary_dirs`, and paths relative to the shell's
  OSC-reported current directory. The default binary-dir list is platform-based
  and includes common user tool directories such as `~/.cargo/bin` and the
  `dirs` crate's per-user executable directory, usually `~/.local/bin` on
  Linux. User-supplied `binary_dirs` are merged into that list by default; set
  `merge_extra_dirs = false` to make `binary_dirs` replace the default list.
  Set `deep_history_integration = true` to let `shellhist41` attempt read-only
  discovery of the active shell history and merge those entries into editor
  history navigation and completion. It currently supports bash, zsh, fish,
  PowerShell/PowerShell Core, and Atuin-backed history when Atuin is active.
  Discovered commands are offered only where a shell command can start, so they
  do not pollute normal argument completion. For history completions, Tab
  accepts the next whitespace-delimited token or path element, while Right
  accepts the full visible history item. When a filesystem path has multiple
  matches, Tab cycles the ghost candidate and Right accepts the active one;
  ambiguous completions show up to five ranked matches near the editor area,
  and Up/Down rotates the active match while the list is visible. Command-name
  and whole-command history candidates also include fuzzy matches after prefix
  matches; fuzzy matches never create ghost text and require explicit Up/Down
  selection before Tab or Right accepts them. While
  enabled, the editor is rendered in a three-row area with an edge-to-edge top
  border under the current prompt on the primary screen, with terminal history
  shifted upward by those three rows. It stays visible through ordinary command
  output but hides when a foreground command advertises stronger interactive
  terminal modes such as mouse tracking, app cursor, or app keypad. Multi-line
  input scrolls inside that three-row area with a small scrollbar, and Up/Down
  move between input lines when possible.

  Mouse drag selects editor text, release copies it to the primary selection,
  right-click copies a selected editor range to the clipboard or pastes when no
  editor selection is active, middle-click pastes the primary selection, and
  the configured Copy/Paste actions operate on the editor while it is active.
  While the editor is open, right- and middle-click paste gestures target the
  editor even when the pointer is over the terminal area. Terminal and editor
  selections clear each other, and Copy/right-click copy prefer an active
  terminal selection before an active editor selection.
  Path completion understands single- and double-quoted arguments and escapes
  spaces for unquoted paths.

  The command palette supports argument-bearing commands whose labels end in
  `:`. Text after the colon is treated as the argument; for example,
  `Open new window in dir: Documents` launches a new window with `Documents`
  resolved relative to the active session's current directory, and
  `Open new tab in dir: Documents` does the same for a new tab in the current
  window. Tab fills the currently highlighted palette row into the palette
  input, and Enter on an argument-bearing row without an argument fills the
  `: ` prompt instead of running an empty argument.

  Alternate-screen applications always receive normal terminal input. While the
  editor is visible on the primary screen, keyboard input targets the editor;
  foreground-app heuristics hide the editor so interactive terminal programs
  keep receiving normal terminal input.

  It supports common readline-style editing keys: `Ctrl+A/E`, `Ctrl+D`,
  `Alt+B/F`, `Ctrl+W`, `Alt+Backspace`, `Alt+D`, `Ctrl+K/U`, and `Ctrl+Y`;
  `Ctrl+Left/Right` and `Ctrl+Backspace/Delete` are also accepted.
  `Shift+Enter` inserts a newline for multi-line input; plain Enter submits the
  buffer. Set `vim_mode = true` to start the editor in normal mode with
  mostly-vim emulation. I've probably missed just enough commands you use to
  annoy you, but it's everything I use so I don't know it. File a bug if you
  want more emulation!
- Example scripts are available under `examples/`, including
  `examples/sys_info.lua` for Linux CPU and memory status text.

</details>

## License

term41 is released into the public domain under [The Unlicense](LICENSE).

The embedded Fairfax HD font is distributed under the SIL Open Font License; see
[`resources/fonts/FairfaxHD-OFL.txt`](font41/resources/fonts/FairfaxHD-OFL.txt).

<p align="center">
  <img src="resources/icon.svg" alt="term41 icon" width="160" />
</p>

# term41

A GPU-accelerated terminal emulator written in Rust. It uses [`wgpu`] for
rendering, [`harfrust`] for text shaping, [`winit`] for windowing, and talks to
a child shell over a local PTY.

> **Note:** This project was in some portions vibe-coded, in other portions
> hand-written where vibe-coding broke down/produced poor code.

## Why?

I've been enjoying vibe-coding apps I never had the time for in the past, and
term41 is the product of one such experiment. I know there are many other
terminal emulators and this one is nothing special, but I've always wanted to
write my own, with the features I prefer.

## Features

### Rendering

- GPU-accelerated glyph atlas and foreground/background pipelines via `wgpu`
  (Vulkan backend, WGSL shaders)
- Configurable VSync modes: `auto`, `fast`, `on`, `off`
- GPU power preference selection (integrated vs. discrete)
- Configurable window opacity (transparent windows when < 1.0)

### Text and fonts

- Unicode text shaping with per-run font fallback (`harfrust` + `fontdb`)
- Comma-separated font family list with generic names (`monospace`, `serif`,
  `sans-serif`)
- Embedded [Fairfax HD](font41/resources/fonts/FairfaxHD-OFL.txt) as ultimate
  fallback — unknown glyphs always render
- Bold, italic, and underline attributes
- True color (24-bit RGB), 256-color palette, and 16 standard ANSI colors
- Color emoji via COLR, CBDT/sbix bitmap, and SVG font tables
- Custom rasteriser for block elements, braille patterns, box-drawing, and
  Symbols for Legacy Computing
- Wide character and grapheme cluster support (ZWJ sequences, variation
  selectors, regional indicators)
- Configurable font supersampling factor
- DPI scale override

### Inline images

- Sixel graphics
- [Kitty graphics protocol](https://sw.kovidgoyal.net/kitty/graphics-protocol/)
- [iTerm2 inline images](https://iterm2.com/documentation-images.html) (OSC
  1337)
- PNG always available; GIF (including animated) and video formats (MP4, WebM,
  Matroska) require the `ffmpeg` build feature

### Background images

- Static or animated background image behind terminal cells (`background_image`
  config key)
- Animated GIF and video backgrounds (with `ffmpeg` feature)
- Adjustable background brightness via `background_opacity`
- Paste an image from the clipboard as background (`Ctrl+Shift+B`) and clear it
  (`Ctrl+Shift+Backspace`)

### Terminal emulation

- Primary and alternate screen buffers (DECSET 47/1047/1049)
- Scroll regions (DECSTBM)
- DECSCUSR cursor styles (block / underline / beam, blinking or steady)
- Cursor save/restore (DECSC/DECRC)
- Full CSI repertoire: cursor movement, erase, insert/delete lines and
  characters, scroll up/down, device attributes (DA1/DA2, reports as VT220),
  device status reports, window size queries
- Bracketed paste (DECSET 2004)
- Synchronized output (DECSET 2026)
- Focus reporting (DECSET 1004)
- OSC 0 / OSC 2 window title
- OSC 7 current-working-directory tracking
- OSC 8 hyperlinks — underlined cells, `Ctrl`+click to open
- OSC 52 clipboard read/write
- Hardware tab stops

### Keyboard

- [Kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/)
  — disambiguate escape codes, report event types, report alternate keys, report
  all keys as escape codes, report associated text
- Fully configurable keybindings via `config.toml`

### Mouse

- xterm mouse tracking modes: X10, normal, button-event, any-event
- Mouse encodings: legacy, UTF-8, SGR, urxvt
- Modifier reporting (Shift, Alt, Ctrl)
- `Shift`+wheel bypasses app tracking and scrolls locally

### Selection and clipboard

- Single-click drag (char), double-click (word), triple-click (line) selection
- Auto-staged to primary selection on release
- Right-click paste (or copy, if a selection is active)
- `Ctrl+Shift+C` / `Ctrl+Shift+V` for clipboard copy/paste
- OSC 52 clipboard integration
- Wayland `zwlr_data_control_manager_v1` support (optional build feature)
- Image data support from clipboard

### Scrollback

- Configurable scrollback size (default 10 000 lines)
- `Shift+PageUp` / `Shift+PageDown` and mouse wheel
- Search in scrollback (`Ctrl+Shift+F`) with match highlighting and
  next/previous navigation

### Shell integration (OSC 133)

- Prompt, command, and output region marking
- Navigate between prompts (`Ctrl+Shift+Up` / `Ctrl+Shift+Down`)
- Gutter with per-command exit-status indicators and execution timing
- Right-click popup menu on gutter marks: rerun command, copy command, copy
  output

### Tabs and windows

- Multiple tabs per window (`Ctrl+Shift+T` / `Ctrl+Shift+W`)
- Tab switching (`Ctrl+PageUp` / `Ctrl+PageDown`)
- New window (`Ctrl+Shift+N`) inheriting the active session's working directory

### VTE parser (`vtepp` crate)

- Pull-based state machine with SIMD-optimised ASCII scanning (AVX2/SSE2)
- CSI, OSC, ESC, APC, and DCS sequence parsing
- Multi-byte UTF-8 reassembly

### Configuration

- TOML config at `$XDG_CONFIG_HOME/term41/config.toml`
- All fields optional; missing or unparseable files fall back to built-in
  defaults
- Live reload — all settings re-apply on save except opacity, which requires a
  restart

## Building

term41 is a standard Cargo project.

```sh
cargo build --release
cargo run   --release
```

### Installing

```sh
cargo install --path .
```

### Build features

| Feature                | Default | Description |
| ---------------------- | ------- | ----------- |
| `ffmpeg`               | on      | Pulls in `ffmpeg-next` so image protocols can decode formats beyond the built-in PNG path (animated GIF, MP4, WebM, Matroska). Requires the `libav*` dev packages on the host. |
| `vulkan`               | off     | Uses Vulkan instead of OpenGL for rendering. Off by default as it can cause slow startup times. |
| `wayland-data-control` | on      | Enables `arboard`'s `zwlr_data_control_manager_v1` backend on Wayland, giving clipboard access on compositors that implement the protocol (sway, wayfire, Hyprland, etc.) without a focus-hack. |

Disable default features for a lightweight build without ffmpeg or Wayland
data-control:

```sh
cargo build --release --no-default-features
```

### Logging

Log verbosity is controlled via `RUST_LOG`:

```sh
RUST_LOG=info cargo run --release
```

## Configuration

term41 loads its config from:

```
$XDG_CONFIG_HOME/term41/config.toml
```

(On Linux this typically resolves to `~/.config/term41/config.toml`. The path is
determined by the [`dirs`] crate's `config_dir()`.)

All fields are optional. If the file is missing or unparseable, built-in
defaults are used.

```toml
# ~/.config/term41/config.toml

# Window opacity, clamped to [0.0, 1.0]. Values < 1.0 enable a transparent
# window at creation time.
opacity = 1.0

# Comma-separated list of font families, searched in order. The generic
# names "monospace", "serif", and "sans-serif" are recognised. The embedded
# Fairfax HD font is always appended as the final fallback, so unknown
# glyphs still render.
fonts = "JetBrains Mono, monospace"

# Font size in points. Minimum 1.0.
font_size = 24.0

# Supersampling factor for font rasterisation (1–16). Higher values produce
# smoother results at the cost of CPU and memory. Default 4.
font_supersampling = 4

# Override the monitor's DPI scale factor. Omit to use the system value;
# set to 1.0 to disable DPI scaling entirely.
# dpi_scale = 1.0

# Number of scrollback lines retained above the visible viewport.
scrollback_lines = 10000

# Default cursor shape: "block", "underline", or "beam". Apps can still
# override at runtime via DECSCUSR (`CSI Ps SP q`).
cursor_shape = "block"

# Whether the cursor blinks. Apps can still override at runtime via
# DECSCUSR.
cursor_blink = true

# Bell handling: "off" (default), "visual" (brief screen flash), or
# "urgent" (request the compositor mark the window as needing
# attention — taskbar bobbing, urgency hint, etc).
bell = "off"

# Show the shell-integration gutter on the left edge. The gutter displays
# coloured dots for OSC 133 prompt marks with exit-status indicators.
# Defaults to true; disable for a clean edge or shells without OSC 133.
gutter = true

# GPU power preference: "LowPower" or "HighPerformance".
# power_preference = "LowPower"

# VSync mode: "auto" (default), "fast", "on", or "off".
vsync = "auto"

# Path to an image file drawn behind terminal cells. PNG always works;
# GIF and video formats (MP4, WebM, Matroska) require the ffmpeg feature.
# Cells with the default background become transparent over the image;
# cells with an explicit SGR background paint over it.
# background_image = "/path/to/wallpaper.png"

# Dim the background image (0.0 invisible … 1.0 full brightness).
# background_opacity = 0.3

# Keybindings. Setting this *replaces* the defaults — to disable a
# default binding, omit it. Modifiers: Ctrl/Shift/Alt/Super (case
# insensitive). Keys: any printable character, or named keys like
# PageUp, PageDown, Home, End, F1..F12, Enter, Tab, Escape, Space,
# Up/Down/Left/Right, Delete, Insert, Backspace.
keybindings = [
  { keys = "Shift+PageUp",   action = "ScrollPageUp"   },
  { keys = "Shift+PageDown", action = "ScrollPageDown" },
  { keys = "Ctrl+Shift+C",   action = "Copy"           },
  { keys = "Ctrl+Shift+V",   action = "Paste"          },
]
```

### Default key bindings

| Binding                              | Action                                                 |
| ------------------------------------ | ------------------------------------------------------ |
| `Shift+PageUp` / `Shift+PageDown`    | Scroll viewport by one page                            |
| `Ctrl+Shift+Up` / `Ctrl+Shift+Down`  | Jump to previous / next shell prompt (OSC 133)         |
| Mouse wheel                          | Scroll viewport (forwarded to app when tracking)       |
| `Shift` + wheel                      | Bypass app mouse tracking and scroll locally           |
| Left-click drag                      | Select text (char mode)                                |
| Double-click                         | Select word                                            |
| Triple-click                         | Select line                                            |
| Right-click (no selection)           | Paste from system clipboard                            |
| Right-click (with selection)         | Copy selection to system clipboard                     |
| `Ctrl+Shift+C`                       | Copy selection to clipboard                            |
| `Ctrl+Shift+V`                       | Paste from clipboard                                   |
| `Ctrl+Shift+F`                       | Open search bar                                        |
| `Ctrl+Shift+N`                       | Open new window                                        |
| `Ctrl+Shift+T`                       | Open new tab                                           |
| `Ctrl+Shift+W`                       | Close active tab                                       |
| `Ctrl+PageUp` / `Ctrl+PageDown`      | Previous / next tab                                    |
| `Ctrl+Shift+B`                       | Paste clipboard image as background                    |
| `Ctrl+Shift+Backspace`               | Clear pasted background                                |
| `Ctrl` + left-click on a link        | Open the OSC 8 hyperlink in the system handler         |

## License

term41 is released into the public domain under [The Unlicense](LICENSE).

The embedded Fairfax HD font is distributed under the SIL Open Font License; see
[`resources/fonts/FairfaxHD-OFL.txt`](font41/resources/fonts/FairfaxHD-OFL.txt).

[`wgpu`]: https://github.com/gfx-rs/wgpu
[`harfrust`]: https://crates.io/crates/harfrust
[`winit`]: https://github.com/rust-windowing/winit
[`dirs`]: https://crates.io/crates/dirs

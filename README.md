<p align="center">
  <img src="resources/icon.svg" alt="term41 icon" width="160" />
</p>

# term41

A GPU-accelerated terminal emulator written in Rust.

> **Note:** This project was in some portions vibe-coded, in other portions
> hand-written where vibe-coding broke down/produced poor code.

## Why?

I've been enjoying vibe-coding apps I never had the time for in the past, and
term41 is the product of one such experiment. I know there are many other
terminal emulators and this one is nothing special, but I've always wanted to
write my own, with the features I prefer.

## Overview

term41 is a desktop terminal emulator with:

- GPU rendering via `wgpu`
- Unicode shaping and fallback fonts
- modern image protocols (`sixel`, Kitty, OSC 1337)
- DEC/VT-style terminal emulation, including page geometry, rectangular ops,
  status lines, and macro support
- shell integration, tabs, scrollback search, hyperlinks, and background images

The project is split into a few focused crates:

- `term41`: windowing, rendering, config, input, and app orchestration
- `terminal41`: terminal state machine and escape-sequence behavior
- `vtepp`: pull-based VTE parser
- `font41`, `image41`, `pty-pipe41`: supporting subsystems

## Building

```sh
cargo build --release
cargo run --release
```

### Installing

```sh
cargo install --path .
```

### Cargo Features

| Feature                | Default | Description |
| ---------------------- | ------- | ----------- |
| `ffmpeg`               | on      | Enables GIF/video decode for inline images and animated backgrounds. Requires host `libav*` development packages. |
| `vulkan`               | off     | Uses Vulkan instead of OpenGL for rendering. |
| `wayland-data-control` | on      | Enables `zwlr_data_control_manager_v1` clipboard access on Wayland compositors that support it. |

For a smaller build without ffmpeg or Wayland data-control:

```sh
cargo build --release --no-default-features
```

### Logging

```sh
RUST_LOG=info cargo run --release
```

## Security Model

term41 treats ordinary terminal text output as the baseline capability. Features
that let the target do more than draw the current text screen are treated as
privileged.

In particular, extensions that allow either of these are intended to be
default-deny:

1. Target-controlled content outside standard text output
2. Target-controlled emulator behavior

That means the safe default is "do nothing unless the user explicitly opted in
or the feature has an authorization path." The current concrete example is
VT420 macros: they are denied unless the foreground process set is identified
and matches the configured allowlist. On Linux and macOS that process identity
comes from the PTY foreground process group; on Windows there is no equivalent
trusted probe yet, so users must opt into broad allow rules themselves if they
want them.

This is the project direction for new privileged extensions as well: they
should not silently become available just because a remote program emitted an
escape sequence.

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
- DEC character-set engine including NRC sets, GL/GR invocation, UTF-8 and
  8-bit text modes
- VT420 page/geometry controls, rectangular-area controls, and DEC status lines
- VT420 macros with allowlist-based gating

### Images and Media

- sixel graphics
- Kitty graphics protocol
- OSC 1337 inline images
- PNG always available
- GIF/video formats available with the `ffmpeg` feature

### Input, Selection, and Shell Integration

- Kitty keyboard protocol
- xterm mouse tracking modes and encodings
- scrollback search
- OSC 133 shell integration with prompt navigation and gutter status markers
- copy/paste, primary selection, hyperlink opening, and image paste as wallpaper

</details>

<details>
<summary><strong>Configuration</strong></summary>

Configuration is loaded from:

```text
$XDG_CONFIG_HOME/term41/config.toml
```

On Linux this is usually `~/.config/term41/config.toml`. All fields are
optional. If the file is missing or unparseable, built-in defaults are used.
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

[allow_features]
macros = ["vtrex"]

[colors.status_line]
# foreground = "#d8dee9"
# background = "#3b4252"

keybindings = [
  { keys = "Shift+PageUp", action = "ScrollPageUp" },
  { keys = "Shift+PageDown", action = "ScrollPageDown" },
  { keys = "Ctrl+Shift+C", action = "Copy" },
  { keys = "Ctrl+Shift+V", action = "Paste" },
]
```

Notes:

- `keybindings` replaces the default binding set rather than merging with it.
- `strict_altscreen_scrollback = true` restores a zero-scrollback alternate
  screen.
- `status_line = "indicator"` enables the DEC indicator line by default.
- `allow_features.macros` can be `"all"` or a list of executable names/paths.

</details>

## License

term41 is released into the public domain under [The Unlicense](LICENSE).

The embedded Fairfax HD font is distributed under the SIL Open Font License; see
[`resources/fonts/FairfaxHD-OFL.txt`](font41/resources/fonts/FairfaxHD-OFL.txt).

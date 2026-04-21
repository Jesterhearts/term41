<p align="center">
  <img src="resources/icon.svg" alt="term41 icon" width="160" />
</p>

# term41

<p align="center">
  <video src="https://github.com/user-attachments/assets/d5e13ecc-1adf-446e-8fee-8c09ca605df4">
  </video>
</p>

A GPU-accelerated terminal emulator written in Rust. It features fast startup
times (target <100ms TTFP on my machine) and responsive handling (target <1
frame of delay even under heavy load).

> **Note:** This project uses a decent amount of vibe coding. VTEs have a huge
> feature surface, and implementing it in a reasonable time frame is only
> possible thanks to LLM assistance.

## Why?

I've been enjoying vibe-coding apps I never had the time for in the past, and
term41 is the product of one such experiment. I know there are many other
terminal emulators and this one is nothing special, but I've always wanted to
write my own, with the features I prefer.

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

- GPU rendering via `wgpu`
- Unicode shaping and fallback fonts
- modern image protocols (`sixel`, Kitty, OSC 1337)
- DEC/VT-style terminal emulation, including page geometry, rectangular ops,
  status lines, and macro support
- shell integration, tabs, scrollback search, hyperlinks, and background images

The codebase is split into a few focused crates:

- `term41`: windowing, rendering, config, input, and app orchestration
- `terminal41`: terminal state machine and escape-sequence behavior
- `vtepp`: pull-based VTE parser
- `font41`, `image41`, `pty-pipe41`: supporting subsystems

## Building

If you just want to build it and run it:

```sh
cargo build --release
cargo run --release
```

### Installing

If you'd rather install it into your cargo bin dir:

```sh
cargo install --path .
```

### Cargo Features

| Feature                | Default | Description |
| ---------------------- | ------- | ----------- |
| `ffmpeg`               | on      | Enables GIF/video decode for inline images and animated backgrounds. Requires host `libav*` development packages. |
| `vulkan`               | off     | Uses Vulkan instead of OpenGL for rendering. |
| `wayland-data-control` | on      | Enables `zwlr_data_control_manager_v1` clipboard access on Wayland compositors that support it. |

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

The concrete example today is VT420 macros. They stay denied unless you
explicitly allow them. This is currently a binary toggle between None/All
processes. As far as I know, there's no reliable way to say "these bytes in the
pty came from this process", so there's no safe way to authenticate that some
set of bytes in the input is from `good` vs `evil`. If such a way becomes
available, I'm open to adding a per-process allowlist.

An example of a grey area is clipboard integration. I currently don't gate it
behind an allowlist because I think it would be surprising if it was broken due
to this. I'd really like to move it behind a gate if it turns out restricting it
wouldn't hurt too bad.


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

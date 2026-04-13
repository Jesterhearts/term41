<p align="center">
  <img src="resources/icon.svg" alt="term41 icon" width="160" />
</p>

# term41

A small GPU-accelerated terminal emulator written in Rust. It uses [`wgpu`] for
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

- GPU-accelerated glyph atlas + background/foreground pipelines (`wgpu`)
- Unicode shaping with per-run font fallback (`harfrust` + `fontdb`)
- Embedded Fairfax HD as an ultimate font fallback
- Scrollback buffer with `Shift+PageUp`/`Shift+PageDown` and mouse-wheel scroll
- Mouse tracking (xterm modes, including motion reporting)
- Selection with single/double/triple-click (char / word / line) and click-drag;
  auto-staged on the primary selection on release
- Right-click paste (or copy, if a selection is active)
- `Ctrl+Shift+C` / `Ctrl+Shift+V` for clipboard copy/paste
- OSC 52 clipboard integration
- Sixel image rendering
- Configurable window opacity, fonts, font size, and scrollback size

## Building

term41 is a standard Cargo project.

```sh
cargo build --release
cargo run   --release
```

### Build features

| Feature                | Default | Description                                                                                                                                                                                        |
| ---------------------- | ------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `wayland-data-control` | on      | Enables `arboard`'s `zwlr_data_control_manager_v1` backend on Wayland, giving clipboard access on compositors that implement the protocol (sway, wayfire, Hyprland, etc.) without a focus-hack.    |

Disable default features if the Wayland data-control dependency is unavailable
in your environment:

```sh
cargo build --release --no-default-features
```

### Logging

term41 uses `env_logger`, so log verbosity is controlled via `RUST_LOG`:

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

# Number of scrollback lines retained above the visible viewport.
scrollback_lines = 10000
```

### Key bindings

| Binding                          | Action                                                 |
| -------------------------------- | ------------------------------------------------------ |
| `Shift+PageUp` / `Shift+PageDown`| Scroll viewport by one page through scrollback         |
| Mouse wheel                      | Scroll viewport (or forwarded to app when tracking)    |
| `Shift` + wheel                  | Bypass app mouse tracking and scroll locally           |
| Left-click drag                  | Select text (char mode)                                |
| Double-click                     | Start selection in word mode                           |
| Triple-click                     | Start selection in line mode                           |
| Right-click (no selection)       | Paste from system clipboard                            |
| Right-click (with selection)     | Copy selection to system clipboard                     |
| `Ctrl+Shift+C`                   | Copy selection to system clipboard                     |
| `Ctrl+Shift+V`                   | Paste from system clipboard                            |

## License

term41 is released into the public domain under [The Unlicense](LICENSE).

The embedded Fairfax HD font is distributed under the SIL Open Font License; see
[`resources/fonts/FairfaxHD-OFL.txt`](resources/fonts/FairfaxHD-OFL.txt).

[`wgpu`]: https://github.com/gfx-rs/wgpu
[`harfrust`]: https://crates.io/crates/harfrust
[`winit`]: https://github.com/rust-windowing/winit
[`dirs`]: https://crates.io/crates/dirs

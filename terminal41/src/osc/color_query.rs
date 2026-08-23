use config41::ColorPalette;
use palette::Srgb;

use crate::C1Mode;
use crate::DecColorState;
use crate::conformance;
use crate::dynamic_color::ColorMutation;
use crate::dynamic_color::DynamicColorTarget;
use crate::dynamic_color::RuntimeColorOverrides;
use crate::dynamic_color::apply_mutations;
use crate::screen::Screen;

#[derive(Debug, Clone, PartialEq)]
pub(super) enum ColorControlAction {
    Palette(Vec<PaletteOperation>),
    Dynamic(Vec<DynamicOperation>),
    ResetPalette(Vec<u8>),
    ResetAllPalette,
    ResetDynamic(DynamicColorTarget),
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct PaletteOperation {
    index: u8,
    value: ColorValue,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct DynamicOperation {
    target: DynamicColorTarget,
    value: ColorValue,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ColorValue {
    Query,
    Set(Srgb<u8>),
}

const MAX_PALETTE_OPERATIONS: usize = 256;

pub(super) fn parse_palette(rest: &[u8]) -> Option<ColorControlAction> {
    let text = std::str::from_utf8(rest).ok()?;
    let mut operations = Vec::new();
    let mut fields = text.split(';');
    while operations.len() < MAX_PALETTE_OPERATIONS {
        let Some(index) = fields.next() else {
            break;
        };
        let Some(value) = fields.next() else {
            break;
        };
        let Some(index) = index.parse::<u8>().ok() else {
            break;
        };
        let Some(value) = parse_value(value) else {
            break;
        };
        operations.push(PaletteOperation { index, value });
    }
    (!operations.is_empty()).then_some(ColorControlAction::Palette(operations))
}

pub(super) fn parse_dynamic(
    rest: &[u8],
    first_target: DynamicColorTarget,
) -> Option<ColorControlAction> {
    let first_index = dynamic_target_index(first_target);
    let text = std::str::from_utf8(rest).ok()?;
    let mut operations = Vec::new();
    for (offset, field) in text.split(';').enumerate() {
        let Some(target) = dynamic_target(first_index + offset) else {
            break;
        };
        if let Some(value) = parse_value(field) {
            operations.push(DynamicOperation { target, value });
        }
    }
    (!operations.is_empty()).then_some(ColorControlAction::Dynamic(operations))
}

pub(super) fn parse_palette_reset(rest: &[u8]) -> Option<ColorControlAction> {
    if rest.is_empty() {
        return Some(ColorControlAction::ResetAllPalette);
    }
    let text = std::str::from_utf8(rest).ok()?;
    let mut indices = Vec::new();
    for field in text.split(';').take(MAX_PALETTE_OPERATIONS) {
        let Some(index) = field.parse::<u8>().ok() else {
            break;
        };
        indices.push(index);
    }
    (!indices.is_empty()).then_some(ColorControlAction::ResetPalette(indices))
}

pub(super) fn parse_dynamic_reset(
    rest: &[u8],
    target: DynamicColorTarget,
) -> Option<ColorControlAction> {
    rest.is_empty()
        .then_some(ColorControlAction::ResetDynamic(target))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn apply(
    action: ColorControlAction,
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    active: &mut Screen,
    stash: &mut Screen,
    palette: &mut ColorPalette,
    base_palette: &ColorPalette,
    dec_color: &mut DecColorState,
    runtime_colors: &mut RuntimeColorOverrides,
) {
    match action {
        ColorControlAction::Palette(operations) => {
            let mut query_palette = palette.clone();
            let mut mutations = Vec::new();
            for operation in operations {
                match operation.value {
                    ColorValue::Query => write_palette_query_reply(
                        pending_output,
                        c1_mode,
                        operation.index,
                        query_palette.indexed_color(operation.index),
                    ),
                    ColorValue::Set(color) => {
                        query_palette.set_indexed_color(operation.index, color);
                        mutations.push(ColorMutation::SetIndexed {
                            index: operation.index,
                            color,
                        });
                    }
                }
            }
            apply_mutations(
                active,
                stash,
                palette,
                base_palette,
                dec_color,
                runtime_colors,
                mutations,
            );
        }
        ColorControlAction::Dynamic(operations) => {
            let query_palette = palette.clone();
            for operation in &operations {
                if operation.value == ColorValue::Query {
                    write_dynamic_query_reply(
                        pending_output,
                        c1_mode,
                        operation.target,
                        &query_palette,
                    );
                }
            }
            apply_mutations(
                active,
                stash,
                palette,
                base_palette,
                dec_color,
                runtime_colors,
                operations.into_iter().filter_map(|operation| {
                    if let ColorValue::Set(color) = operation.value {
                        Some(ColorMutation::SetDynamic {
                            target: operation.target,
                            color,
                        })
                    } else {
                        None
                    }
                }),
            );
        }
        ColorControlAction::ResetPalette(indices) => {
            apply_mutations(
                active,
                stash,
                palette,
                base_palette,
                dec_color,
                runtime_colors,
                indices.into_iter().map(ColorMutation::ResetIndexed),
            );
        }
        ColorControlAction::ResetAllPalette => apply_mutations(
            active,
            stash,
            palette,
            base_palette,
            dec_color,
            runtime_colors,
            [ColorMutation::ResetAllIndexed],
        ),
        ColorControlAction::ResetDynamic(target) => apply_mutations(
            active,
            stash,
            palette,
            base_palette,
            dec_color,
            runtime_colors,
            [ColorMutation::ResetDynamic(target)],
        ),
    }
}

fn parse_value(field: &str) -> Option<ColorValue> {
    if field == "?" {
        Some(ColorValue::Query)
    } else {
        parse_color(field).map(ColorValue::Set)
    }
}

fn parse_color(text: &str) -> Option<Srgb<u8>> {
    if text
        .as_bytes()
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"rgb:"))
    {
        parse_rgb_components(&text[4..])
    } else if text
        .as_bytes()
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"rgbi:"))
    {
        parse_rgbi_components(&text[5..])
    } else if let Some(hex) = text.strip_prefix('#') {
        parse_hash_color(hex)
    } else {
        None
    }
}

fn parse_rgb_components(components: &str) -> Option<Srgb<u8>> {
    let mut components = components.split('/');
    let red = parse_scaled_hex_component(components.next()?)?;
    let green = parse_scaled_hex_component(components.next()?)?;
    let blue = parse_scaled_hex_component(components.next()?)?;
    components
        .next()
        .is_none()
        .then_some(Srgb::new(red, green, blue))
}

fn parse_rgbi_components(components: &str) -> Option<Srgb<u8>> {
    let mut components = components.split('/');
    let red = parse_intensity(components.next()?)?;
    let green = parse_intensity(components.next()?)?;
    let blue = parse_intensity(components.next()?)?;
    components
        .next()
        .is_none()
        .then_some(Srgb::new(red, green, blue))
}

fn parse_hash_color(hex: &str) -> Option<Srgb<u8>> {
    if !matches!(hex.len(), 3 | 6 | 9 | 12) {
        return None;
    }
    let width = hex.len() / 3;
    Some(Srgb::new(
        parse_high_bits_hex_component(&hex[..width])?,
        parse_high_bits_hex_component(&hex[width..width * 2])?,
        parse_high_bits_hex_component(&hex[width * 2..])?,
    ))
}

fn parse_scaled_hex_component(component: &str) -> Option<u8> {
    let value = parse_hex_component(component)?;
    let max = (1u32 << (component.len() * 4)) - 1;
    Some(((u32::from(value) * 65_535 / max) >> 8) as u8)
}

fn parse_high_bits_hex_component(component: &str) -> Option<u8> {
    let value = parse_hex_component(component)?;
    let sixteen_bit = value << (4 * (4 - component.len()));
    Some((sixteen_bit >> 8) as u8)
}

fn parse_hex_component(component: &str) -> Option<u16> {
    if component.is_empty() || component.len() > 4 {
        return None;
    }
    u16::from_str_radix(component, 16).ok()
}

fn parse_intensity(component: &str) -> Option<u8> {
    let value = component.parse::<f32>().ok()?;
    (value.is_finite() && (0.0..=1.0).contains(&value)).then_some((value * 255.0).round() as u8)
}

fn dynamic_target_index(target: DynamicColorTarget) -> usize {
    match target {
        DynamicColorTarget::Foreground => 0,
        DynamicColorTarget::Background => 1,
        DynamicColorTarget::Cursor => 2,
    }
}

fn dynamic_target(index: usize) -> Option<DynamicColorTarget> {
    match index {
        0 => Some(DynamicColorTarget::Foreground),
        1 => Some(DynamicColorTarget::Background),
        2 => Some(DynamicColorTarget::Cursor),
        _ => None,
    }
}

fn write_palette_query_reply(
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    index: u8,
    color: Srgb<u8>,
) {
    let reply = rgb_reply(color.red, color.green, color.blue);
    conformance::write_osc(pending_output, c1_mode, format_args!("4;{index};{reply}"));
}

fn write_dynamic_query_reply(
    pending_output: &mut Vec<u8>,
    c1_mode: C1Mode,
    target: DynamicColorTarget,
    palette: &ColorPalette,
) {
    let (command, color) = match target {
        DynamicColorTarget::Foreground => (10, palette.fg),
        DynamicColorTarget::Background => (11, palette.bg),
        DynamicColorTarget::Cursor => (12, palette.cursor.unwrap_or(palette.fg)),
    };
    let reply = rgb_reply(color.red, color.green, color.blue);
    conformance::write_osc(pending_output, c1_mode, format_args!("{command};{reply}"));
}

/// Format an 8-bit channel as the 16-bit representation used in X11 replies.
fn rgb_reply(
    red: u8,
    green: u8,
    blue: u8,
) -> String {
    let red = u16::from(red) << 8 | u16::from(red);
    let green = u16::from(green) << 8 | u16::from(green);
    let blue = u16::from(blue) << 8 | u16::from(blue);
    format!("rgb:{red:04x}/{green:04x}/{blue:04x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn x11_color_specs_scale_rgb_and_use_hash_most_significant_bits() {
        assert_eq!(
            parse_color("rgb:f/80/123"),
            Some(Srgb::new(0xff, 0x80, 0x12))
        );
        assert_eq!(parse_color("RGB:F/0/0"), Some(Srgb::new(255, 0, 0)));
        assert_eq!(parse_color("rgb:009/0/0"), Some(Srgb::new(0, 0, 0)));
        assert_eq!(parse_color("#3a7"), Some(Srgb::new(0x30, 0xa0, 0x70)));
        assert_eq!(parse_color("#123456789"), Some(Srgb::new(0x12, 0x45, 0x78)));
        assert_eq!(parse_color("#ffff80000000"), Some(Srgb::new(255, 128, 0)));
    }

    #[test]
    fn color_specs_accept_intensities() {
        assert_eq!(parse_color("rgbi:1/0.5/0"), Some(Srgb::new(255, 128, 0)));
    }

    #[test]
    fn malformed_color_specs_are_rejected() {
        for invalid in ["rgb:/0/0", "rgb:00000/0/0", "#12", "rgbi:2/0/0", "unknown"] {
            assert_eq!(parse_color(invalid), None, "accepted {invalid}");
        }
    }

    #[test]
    fn palette_controls_are_bounded_to_the_palette_size() {
        let setters = (0..300)
            .map(|index| format!("{};?", index % 256))
            .collect::<Vec<_>>()
            .join(";");
        let resets = (0..300)
            .map(|index| (index % 256).to_string())
            .collect::<Vec<_>>()
            .join(";");

        let Some(ColorControlAction::Palette(operations)) = parse_palette(setters.as_bytes())
        else {
            panic!("palette operations");
        };
        let Some(ColorControlAction::ResetPalette(indices)) =
            parse_palette_reset(resets.as_bytes())
        else {
            panic!("palette resets");
        };
        assert_eq!(operations.len(), MAX_PALETTE_OPERATIONS);
        assert_eq!(indices.len(), MAX_PALETTE_OPERATIONS);
    }
}

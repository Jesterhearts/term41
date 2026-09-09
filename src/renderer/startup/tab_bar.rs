use font41::FontSystem;
use palette::Srgb;
use smol_str::SmolStr;
use smol_str::ToSmolStr;
use terminal41::TermSnapshot;

use super::centered_label_x;
use super::fill_rect;
use super::label_row;
use super::pack_rgb;
use super::paint_shaped_label;
use crate::renderer::r#impl::TabInfo;
use crate::renderer::paint::TabTooltip;
use crate::renderer::paint::build_tab_bar_plan;

pub(super) fn paint_tab_bar(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    tabs: &[TabInfo<'_>],
    new_tab_text: SmolStr,
    cell_w: i32,
    width: usize,
    height: usize,
    tab_bar_h: i32,
    bg: Srgb<u8>,
    fg: Srgb<u8>,
    hovered_button: Option<crate::renderer::TabBarHover>,
    maximized: bool,
) -> Option<TabTooltip> {
    let tab_infos: Vec<TabInfo<'_>> = tabs
        .iter()
        .map(|tab| TabInfo {
            label: tab.label,
            active: tab.active,
        })
        .collect();
    let plan = build_tab_bar_plan(
        &tab_infos,
        &snap.palette,
        new_tab_text,
        hovered_button,
        maximized,
        width as f32,
        cell_w as f32,
    );

    fill_rect(
        buffer,
        width,
        height,
        0,
        0,
        width as i32,
        tab_bar_h,
        pack_rgb(plan.base_bg),
    );

    for tab in &plan.tabs {
        if let Some(tab_bg) = tab.bg {
            fill_rect(
                buffer,
                width,
                height,
                tab.x.round() as i32,
                0,
                tab.width.round() as i32,
                tab_bar_h,
                pack_rgb(tab_bg),
            );
        }
        let row = label_row(&tab.label, fg, bg, true);
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            tab.label_x,
            0.0,
        );
    }

    if let Some(button_bg) = plan.new_tab_button.bg {
        fill_rect(
            buffer,
            width,
            height,
            plan.new_tab_button.x.round() as i32,
            0,
            plan.new_tab_button.width.round() as i32,
            tab_bar_h,
            pack_rgb(button_bg),
        );
    }
    let row = label_row(
        &plan.new_tab_button.label.to_smolstr(),
        fg,
        plan.base_bg,
        false,
    );
    let x = centered_label_x(
        font_system,
        snap,
        &row,
        plan.new_tab_button.x,
        plan.new_tab_button.width,
    );
    paint_shaped_label(font_system, snap, buffer, width, height, &row, x, 0.0);

    for button in &plan.buttons {
        if let Some(button_bg) = button.bg {
            fill_rect(
                buffer,
                width,
                height,
                button.x.round() as i32,
                0,
                button.width.round() as i32,
                tab_bar_h,
                pack_rgb(button_bg),
            );
        }
        let row = label_row(button.label, fg, plan.base_bg, false);
        let x = centered_label_x(font_system, snap, &row, button.x, button.width);
        paint_shaped_label(font_system, snap, buffer, width, height, &row, x, 0.0);
    }

    for tab in &plan.tabs {
        if let Some(separator) = tab.separator {
            fill_rect(
                buffer,
                width,
                height,
                tab.x.round() as i32 + tab.width.round() as i32,
                0,
                3,
                tab_bar_h,
                pack_rgb(separator),
            );
        }
    }

    plan.tooltip
}

pub(super) fn paint_tab_tooltip(
    font_system: &mut FontSystem,
    snap: &TermSnapshot,
    buffer: &mut [u32],
    width: usize,
    height: usize,
    tooltip: &TabTooltip,
) {
    let cell_w = font_system.cell_width as f32;
    let cell_h = font_system.cell_height as f32;
    let panel_x = tooltip.x.round() as i32;
    let panel_y = cell_h.round() as i32;
    let panel_w = tooltip.width.round() as i32;
    let panel_h = ((tooltip.lines.len() as f32 + 0.5) * cell_h).round() as i32;
    let panel_bg = Srgb::new(30, 30, 38);
    let border = Srgb::new(80, 80, 100);
    let normal_fg = Srgb::new(220, 220, 220);

    fill_rect(
        buffer,
        width,
        height,
        panel_x,
        panel_y,
        panel_w,
        panel_h,
        pack_rgb(panel_bg),
    );
    fill_rect(
        buffer,
        width,
        height,
        panel_x,
        panel_y,
        panel_w,
        1,
        pack_rgb(border),
    );
    fill_rect(
        buffer,
        width,
        height,
        panel_x,
        panel_y + panel_h - 1,
        panel_w,
        1,
        pack_rgb(border),
    );

    for (idx, line) in tooltip.lines.iter().enumerate() {
        let row = label_row(line, normal_fg, panel_bg, false);
        paint_shaped_label(
            font_system,
            snap,
            buffer,
            width,
            height,
            &row,
            tooltip.x + cell_w * 0.5,
            cell_h * (1.25 + idx as f32),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::renderer::TabBarHover;
    use crate::renderer::startup::tests::test_snapshot;

    #[test]
    fn tab_bar_returns_tooltip_without_painting_it() {
        let mut font_system = FontSystem::new(None, 18.0, 4);
        let cell_w = font_system.cell_width as usize;
        let cell_h = font_system.cell_height as usize;
        let width = 40 * cell_w;
        let height = 5 * cell_h;
        let snap = test_snapshot(40, 4, &[0]);
        let title = "long tab title ".repeat(10);
        let tabs = [TabInfo {
            label: &title,
            active: true,
        }];

        for hovered in [None, Some(TabBarHover::Tab(0))] {
            let mut buffer = vec![0x123456; width * height];
            let tooltip = paint_tab_bar(
                &mut font_system,
                &snap,
                &mut buffer,
                &tabs,
                SmolStr::new_inline("+"),
                cell_w as i32,
                width,
                height,
                cell_h as i32,
                snap.palette.bg,
                snap.palette.fg,
                hovered,
                false,
            );

            assert_eq!(tooltip.is_some(), hovered.is_some());
            if let Some(tooltip) = tooltip {
                assert!(tooltip.lines.len() > 1);
            }
            assert!(
                buffer[2 * cell_h * width..]
                    .iter()
                    .all(|&pixel| pixel == 0x123456)
            );
        }
    }

    #[test]
    fn tooltip_paints_wrapped_text_panel_and_clips_to_surface() {
        let mut font_system = FontSystem::new(None, 18.0, 4);
        let cell_w = font_system.cell_width as usize;
        let cell_h = font_system.cell_height as usize;
        let width = 10 * cell_w;
        let height = 4 * cell_h;
        let snap = test_snapshot(10, 3, &[0]);
        let tooltip = TabTooltip {
            x: (2 * cell_w) as f32,
            width: (5 * cell_w) as f32,
            lines: vec!["AAAA".into(), "BBBB".into()],
        };
        let mut buffer = vec![0x123456; width * height];
        paint_tab_tooltip(
            &mut font_system,
            &snap,
            &mut buffer,
            width,
            height,
            &tooltip,
        );

        let left = 2 * cell_w;
        let right = 7 * cell_w;
        let bottom = cell_h + (2.5 * cell_h as f32).round() as usize;
        for y in 0..height {
            for x in 0..width {
                let pixel = buffer[y * width + x];
                if y < cell_h || y >= bottom || x < left || x >= right {
                    assert_eq!(pixel, 0x123456, "outside panel at ({x}, {y})");
                } else if y == cell_h || y == bottom - 1 {
                    assert_eq!(pixel, 0x505064, "border at ({x}, {y})");
                } else if x == left || x == right - 1 {
                    assert_eq!(pixel, 0x1e1e26, "horizontal padding at ({x}, {y})");
                }
            }
        }
        for idx in 0..tooltip.lines.len() {
            let top = ((1.25 + idx as f32) * cell_h as f32).round() as usize;
            assert!((top..top + cell_h).any(|y| {
                buffer[y * width + left..y * width + right]
                    .iter()
                    .any(|&pixel| (pixel >> 16) > 80 && pixel <= 0xdcdcdc)
            }));
        }

        let clipped_height = 2 * cell_h;
        let mut clipped = vec![0x123456; width * clipped_height];
        paint_tab_tooltip(
            &mut font_system,
            &snap,
            &mut clipped,
            width,
            clipped_height,
            &tooltip,
        );
        assert_eq!(clipped, buffer[..width * clipped_height]);
    }
}

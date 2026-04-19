use ratatui::Frame;
use ratatui::layout::Constraint;
use ratatui::layout::Direction;
use ratatui::layout::Layout;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Borders;
use ratatui::widgets::Clear;
use ratatui::widgets::List;
use ratatui::widgets::ListItem;
use ratatui::widgets::ListState;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Wrap;

use crate::app::App;
use crate::capabilities;

pub struct Regions {
    pub menu: Rect,
    pub detail: Rect,
    pub controls: Rect,
    pub status: Rect,
    pub run_button: Rect,
    pub reprobe_button: Rect,
}

pub fn draw(
    frame: &mut Frame<'_>,
    app: &App,
) {
    let regions = layout_regions(frame.area());
    frame.render_widget(Clear, frame.area());

    let list_items: Vec<_> = app
        .demos()
        .iter()
        .map(|demo| {
            ListItem::new(vec![
                Line::from(Span::styled(
                    demo.title,
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    demo.summary,
                    Style::default().fg(Color::DarkGray),
                )),
            ])
        })
        .collect();
    let list = List::new(list_items)
        .block(Block::default().title("Feature Menu").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");
    let mut list_state = ListState::default();
    list_state.select(Some(app.selected_index()));
    frame.render_stateful_widget(list, regions.menu, &mut list_state);

    let selected = app.selected_demo();
    let detail = Paragraph::new(vec![
        Line::from(Span::styled(
            selected.title,
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::default(),
        Line::from(selected.summary),
        Line::default(),
        Line::from(selected.detail),
        Line::default(),
        Line::from("Controls:"),
        Line::from("  Enter / Space  Run selected demo"),
        Line::from("  r              Reprobe DA1"),
        Line::from("  q / Esc        Quit"),
        Line::from("  Mouse          Select demos / click buttons / scroll menu"),
        Line::default(),
        Line::from("Demos suspend the TUI, emit escape sequences directly to the terminal,"),
        Line::from("then return here when you press a key."),
    ])
    .block(Block::default().title("Demo Detail").borders(Borders::ALL))
    .wrap(Wrap { trim: false });
    frame.render_widget(detail, regions.detail);

    frame.render_widget(button(" Run Demo ", true), regions.run_button);
    frame.render_widget(button(" Reprobe ", false), regions.reprobe_button);

    let caps = capabilities::describe(app.capabilities());
    let caps_lines: Vec<_> = caps.into_iter().map(Line::from).collect();
    let controls = Paragraph::new(caps_lines)
        .block(
            Block::default()
                .title("Current Capability Probe")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(controls, regions.controls);

    let status = format!(
        "{}    {}",
        app.status(),
        capabilities::format_status(app.capabilities(), regions.status.width as usize)
    );
    let status = Paragraph::new(status)
        .style(Style::default().bg(Color::DarkGray).fg(Color::White))
        .block(Block::default());
    frame.render_widget(status, regions.status);
}

pub fn layout_regions(area: Rect) -> Regions {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(area);
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(34), Constraint::Percentage(66)])
        .split(outer[0]);
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(12),
            Constraint::Length(3),
            Constraint::Length(6),
        ])
        .split(body[1]);
    let buttons = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Min(0),
        ])
        .split(right[1]);

    Regions {
        menu: body[0],
        detail: right[0],
        controls: right[2],
        status: outer[1],
        run_button: buttons[0],
        reprobe_button: buttons[1],
    }
}

pub fn hit_test_demo_list(
    menu: Rect,
    row: u16,
    demo_count: usize,
) -> Option<usize> {
    if row <= menu.y || row >= menu.y + menu.height.saturating_sub(1) {
        return None;
    }
    let body_row = row - menu.y - 1;
    let item = usize::from(body_row / 2);
    (item < demo_count).then_some(item)
}

pub fn point_in_rect(
    x: u16,
    y: u16,
    rect: Rect,
) -> bool {
    x >= rect.x && x < rect.x + rect.width && y >= rect.y && y < rect.y + rect.height
}

fn button(
    label: &str,
    primary: bool,
) -> Paragraph<'static> {
    let style = if primary {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    };
    Paragraph::new(label.to_string())
        .style(style)
        .block(Block::default().borders(Borders::ALL))
        .centered()
}

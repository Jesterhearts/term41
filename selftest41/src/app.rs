use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::MouseButton;
use crossterm::event::MouseEventKind;
use ratatui::layout::Rect;

use crate::capabilities::CapabilityReport;
use crate::demo::Demo;
use crate::ui;

pub struct App {
    demos: Vec<Demo>,
    selected: usize,
    capabilities: CapabilityReport,
    status: String,
    should_quit: bool,
}

pub enum AppCommand {
    None,
    Quit,
    RunSelected,
    Reprobe,
}

impl App {
    pub fn new(
        demos: Vec<Demo>,
        capabilities: CapabilityReport,
    ) -> Self {
        Self {
            demos,
            selected: 0,
            capabilities,
            status: String::from("Enter runs the selected feature demo. r reprobes DA1. q quits."),
            should_quit: false,
        }
    }

    pub fn demos(&self) -> &[Demo] {
        &self.demos
    }

    pub fn selected_demo(&self) -> &Demo {
        &self.demos[self.selected]
    }

    pub fn selected_index(&self) -> usize {
        self.selected
    }

    pub fn capabilities(&self) -> &CapabilityReport {
        &self.capabilities
    }

    pub fn set_capabilities(
        &mut self,
        capabilities: CapabilityReport,
    ) {
        self.capabilities = capabilities;
        self.status = String::from("Reprobed terminal capabilities.");
    }

    pub fn status(&self) -> &str {
        &self.status
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub fn handle_event(
        &mut self,
        event: Event,
        size: Rect,
    ) -> AppCommand {
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.should_quit = true;
                    AppCommand::Quit
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.select_next();
                    AppCommand::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.select_prev();
                    AppCommand::None
                }
                KeyCode::Home => {
                    self.selected = 0;
                    AppCommand::None
                }
                KeyCode::End => {
                    self.selected = self.demos.len().saturating_sub(1);
                    AppCommand::None
                }
                KeyCode::Enter | KeyCode::Char(' ') => AppCommand::RunSelected,
                KeyCode::Char('r') => AppCommand::Reprobe,
                _ => AppCommand::None,
            },
            Event::Mouse(mouse) => self.handle_mouse(mouse.kind, mouse.column, mouse.row, size),
            _ => AppCommand::None,
        }
    }

    fn handle_mouse(
        &mut self,
        kind: MouseEventKind,
        column: u16,
        row: u16,
        size: Rect,
    ) -> AppCommand {
        let regions = ui::layout_regions(size);
        match kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(index) = ui::hit_test_demo_list(regions.menu, row, self.demos.len()) {
                    self.selected = index;
                    return AppCommand::None;
                }
                if ui::point_in_rect(column, row, regions.run_button) {
                    return AppCommand::RunSelected;
                }
                if ui::point_in_rect(column, row, regions.reprobe_button) {
                    return AppCommand::Reprobe;
                }
                AppCommand::None
            }
            MouseEventKind::ScrollDown => {
                if ui::point_in_rect(column, row, regions.menu) {
                    self.select_next();
                }
                AppCommand::None
            }
            MouseEventKind::ScrollUp => {
                if ui::point_in_rect(column, row, regions.menu) {
                    self.select_prev();
                }
                AppCommand::None
            }
            _ => AppCommand::None,
        }
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1) % self.demos.len();
    }

    fn select_prev(&mut self) {
        self.selected = if self.selected == 0 {
            self.demos.len().saturating_sub(1)
        } else {
            self.selected - 1
        };
    }
}

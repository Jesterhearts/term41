use terminal41::settings;

use crate::window_host::Tab;

pub(super) fn resize_tab_to_grid(
    tab: &mut Tab,
    cols: u32,
    rows: u32,
) {
    let pty_rows = {
        let mut terminal = tab.terminal.lock();
        terminal.resize(cols, rows);
        crate::unpark_thread_if_started(&tab.terminal_thread.thread_handle);
        terminal.viewport.rows
    };
    tab.pty.resize(cols as u16, pty_rows as u16);
}

pub(super) fn update_terminal_cell_dimensions(
    tab: &Tab,
    cell_width: u32,
    cell_height: u32,
) {
    let mut terminal = tab.terminal.lock();
    let terminal41::Terminal {
        cell_width: terminal_cell_width,
        cell_height: terminal_cell_height,
        ..
    } = &mut *terminal;
    settings::set_cell_dimensions(
        terminal_cell_width,
        terminal_cell_height,
        cell_width,
        cell_height,
    );
}

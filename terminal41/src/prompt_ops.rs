use super::*;

pub(crate) fn format_indicator_status(
    current_directory: Option<&std::path::Path>,
    current_prompt_row: Option<u64>,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> String {
    let mut segments = path_segments(current_directory);
    if let Some(command) = running_command_text(current_prompt_row, command_metas, screen) {
        segments.push(command);
    }
    segments.join(" > ")
}

fn path_segments(path: Option<&std::path::Path>) -> Vec<String> {
    let Some(path) = path else {
        return Vec::new();
    };
    path.components()
        .map(|component| match component {
            std::path::Component::RootDir => "/".to_owned(),
            std::path::Component::Prefix(prefix) => prefix.as_os_str().to_string_lossy().into(),
            std::path::Component::CurDir => ".".to_owned(),
            std::path::Component::ParentDir => "..".to_owned(),
            std::path::Component::Normal(part) => part.to_string_lossy().into_owned(),
        })
        .collect()
}

fn running_command_text(
    current_prompt_row: Option<u64>,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let prompt_abs = current_prompt_row?;
    let meta = command_metas.get(&prompt_abs)?;
    let command_running = meta.started_at.is_some() && meta.finished_at.is_none();
    if !command_running {
        return None;
    }
    let command = command_text_at(prompt_abs, command_metas, screen)?;
    let flattened = command.split_whitespace().collect::<Vec<_>>().join(" ");
    (!flattened.is_empty()).then_some(flattened)
}

pub(crate) fn find_prompt_for_screen_row(
    screen: &Screen,
    viewport: &Viewport,
    screen_row: u32,
) -> Option<u64> {
    let base = selection_ops::active_viewport(screen, viewport).top_index(screen.grid.rows.len());
    let start = base + screen_row as usize;
    let popped = screen.grid.total_popped as u64;
    for i in (0..=start).rev() {
        if screen.grid.rows[i].prompt_start {
            return Some(popped + i as u64);
        }
    }
    None
}

fn find_next_prompt_after(
    screen: &Screen,
    after_abs: u64,
) -> Option<u64> {
    let popped = screen.grid.total_popped as u64;
    let start = after_abs.checked_sub(popped)? as usize + 1;
    for i in start..screen.grid.rows.len() {
        if screen.grid.rows[i].prompt_start {
            return Some(popped + i as u64);
        }
    }
    None
}

fn command_end_abs(
    prompt_abs: u64,
    screen: &Screen,
) -> u64 {
    if let Some(next) = find_next_prompt_after(screen, prompt_abs) {
        next.saturating_sub(1)
    } else {
        (screen.grid.total_popped + screen.grid.rows.len() - 1) as u64
    }
}

fn extract_rows_text(
    screen: &Screen,
    start_abs: u64,
    start_col: u32,
    end_abs: u64,
) -> String {
    let popped = screen.grid.total_popped as u64;
    let mut out = String::new();
    for abs in start_abs..=end_abs {
        let Some(local) = abs.checked_sub(popped).map(|l| l as usize) else {
            continue;
        };
        if local >= screen.grid.rows.len() {
            break;
        }
        let row = &screen.grid.rows[local];
        let cs = if abs == start_abs {
            start_col as usize
        } else {
            0
        };
        let ce = row.cells.len();
        if cs >= ce {
            if abs < end_abs && !row.wrapped {
                out.push('\n');
            }
            continue;
        }
        let mut seg = String::new();
        for cell in &row.cells[cs..ce] {
            seg.push_str(cell);
        }
        out.push_str(seg.trim_end_matches(' '));
        if abs < end_abs && !row.wrapped {
            out.push('\n');
        }
    }
    out
}

pub(crate) fn command_text_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let meta = command_metas.get(&prompt_abs);
    let start_col = meta.and_then(|m| m.command_col).unwrap_or(0);
    let start_row = meta.and_then(|m| m.command_row).unwrap_or(prompt_abs);
    let end_row = command_text_end(prompt_abs, meta, screen);
    if start_row > end_row {
        return None;
    }
    let text = extract_rows_text(screen, start_row, start_col, end_row);
    if text.is_empty() { None } else { Some(text) }
}

fn command_text_end(
    prompt_abs: u64,
    meta: Option<&CommandMeta>,
    screen: &Screen,
) -> u64 {
    if let Some(meta) = meta
        && let Some(output) = meta.output_row
    {
        return output.saturating_sub(1);
    }
    if let Some(next) = find_next_prompt_after(screen, prompt_abs) {
        return next.saturating_sub(1);
    }
    prompt_abs
}

pub(crate) fn output_text_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let output_row = command_metas.get(&prompt_abs)?.output_row?;
    let end_row = command_end_abs(prompt_abs, screen);
    if output_row > end_row {
        return None;
    }
    let text = extract_rows_text(screen, output_row, 0, end_row);
    if text.is_empty() { None } else { Some(text) }
}

pub(crate) fn command_and_output_text_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) -> Option<String> {
    let meta = command_metas.get(&prompt_abs);
    let start_col = meta.and_then(|m| m.command_col).unwrap_or(0);
    let start_row = meta.and_then(|m| m.command_row).unwrap_or(prompt_abs);
    let end_row = command_end_abs(prompt_abs, screen);
    if start_row > end_row {
        return None;
    }
    let text = extract_rows_text(screen, start_row, start_col, end_row);
    if text.is_empty() { None } else { Some(text) }
}

pub(crate) fn command_duration_at(
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
) -> Option<Duration> {
    let meta = command_metas.get(&prompt_abs)?;
    let start = meta.started_at?;
    let end = meta.finished_at?;
    Some(end.duration_since(start))
}

pub(crate) fn select_command_at(
    selection: &mut Option<Selection>,
    prompt_abs: u64,
    command_metas: &HashMap<u64, CommandMeta>,
    screen: &Screen,
) {
    let meta = command_metas.get(&prompt_abs);
    let start_col = meta.and_then(|m| m.command_col).unwrap_or(0);
    let start_row = meta.and_then(|m| m.command_row).unwrap_or(prompt_abs);
    let end_row = command_text_end(prompt_abs, meta, screen);
    if start_row > end_row {
        return;
    }
    let text = extract_rows_text(screen, start_row, start_col, end_row);
    if text.trim().is_empty() {
        return;
    }
    let end_col = selection_ops::absolute_row_to_local(screen, end_row)
        .map(|l| screen.grid.rows[l].content_len().saturating_sub(1))
        .unwrap_or(0);
    let anchor = SelectionPoint {
        row: start_row,
        col: start_col,
    };
    let head = SelectionPoint {
        row: end_row,
        col: end_col,
    };
    *selection = Some(Selection {
        anchor,
        head,
        mode: SelectionMode::Char,
        origin: anchor,
    });
}

impl Terminal {
    pub fn find_prompt_for_screen_row(
        &self,
        screen_row: u32,
    ) -> Option<u64> {
        find_prompt_for_screen_row(&self.active, &self.viewport, screen_row)
    }

    pub fn command_text_at(
        &self,
        prompt_abs: u64,
    ) -> Option<String> {
        command_text_at(prompt_abs, &self.command_metas, &self.active)
    }

    pub fn output_text_at(
        &self,
        prompt_abs: u64,
    ) -> Option<String> {
        output_text_at(prompt_abs, &self.command_metas, &self.active)
    }

    pub fn command_and_output_text_at(
        &self,
        prompt_abs: u64,
    ) -> Option<String> {
        command_and_output_text_at(prompt_abs, &self.command_metas, &self.active)
    }

    pub fn command_duration_at(
        &self,
        prompt_abs: u64,
    ) -> Option<Duration> {
        command_duration_at(prompt_abs, &self.command_metas)
    }

    pub fn select_command_at(
        &mut self,
        prompt_abs: u64,
    ) {
        select_command_at(
            &mut self.selection,
            prompt_abs,
            &self.command_metas,
            &self.active,
        );
    }
}

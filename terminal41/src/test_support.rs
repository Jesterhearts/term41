use std::ops::Deref;
use std::ops::DerefMut;

use crate::ColorPalette;
use crate::CursorStyle;
use crate::FeaturePermissions;
use crate::MouseButton;
use crate::MouseEventKind;
use crate::MouseModifiers;
use crate::ProgramAllowlist;
use crate::Row;
use crate::StatusDisplayKind;
use crate::Terminal;
use crate::TerminalEffects;
use crate::TerminalProcessor;
use crate::host;
use crate::selection;
use crate::settings;
use crate::view;

/// Shared test harness that drives the production byte-processing pipeline.
/// Exposed so integration tests can stop carrying their own terminal wrappers.
pub struct TestTerm {
    pub inner: Terminal,
    pub effects: TerminalEffects,
    processor: TerminalProcessor,
}

impl TestTerm {
    pub fn new(
        cols: u32,
        rows: u32,
        scrollback: u32,
        cell_h: u32,
        cell_w: u32,
    ) -> Self {
        Self::new_with_alt_scrollback_policy(cols, rows, scrollback, false, cell_h, cell_w)
    }

    pub fn new_with_alt_scrollback_policy(
        cols: u32,
        rows: u32,
        scrollback: u32,
        strict_altscreen_scrollback: bool,
        cell_h: u32,
        cell_w: u32,
    ) -> Self {
        Self {
            inner: Terminal::new(
                cols,
                rows,
                scrollback,
                StatusDisplayKind::None,
                strict_altscreen_scrollback,
                FeaturePermissions::default(),
                cell_h,
                cell_w,
                ColorPalette::default(),
            ),
            effects: TerminalEffects::default(),
            processor: TerminalProcessor::new(),
        }
    }

    pub fn new_80x24() -> Self {
        Self::new(80, 24, 1000, 16, 8)
    }

    pub fn process(
        &mut self,
        data: &[u8],
    ) {
        let effects = self.processor.process_bytes(&mut self.inner, data);
        self.effects.extend(effects);
    }

    pub fn set_macro_permissions(
        &mut self,
        macros: ProgramAllowlist,
    ) {
        settings::set_feature_permissions(&mut self.inner.protocol, FeaturePermissions { macros });
    }

    pub fn total_rows(&self) -> u32 {
        view::total_rows(&self.inner.active, &self.inner.viewport)
    }

    pub fn status_line_visible(&self) -> bool {
        view::status_line_visible(&self.inner.active)
    }

    pub fn indicator_status_text(&self) -> Option<String> {
        view::indicator_status_text(&self.inner.metadata, &self.inner.active)
    }

    pub fn visible_row(
        &self,
        row: u32,
    ) -> &Row {
        view::visible_row(&self.inner.active, &self.inner.viewport, row)
    }

    pub fn row_text(
        &self,
        row: u32,
    ) -> String {
        self.visible_row(row).cells.concat()
    }

    pub fn cell_char(
        &self,
        row: u32,
        col: u32,
    ) -> char {
        self.visible_row(row).cells[col as usize]
            .chars()
            .next()
            .unwrap_or(' ')
    }

    pub fn cursor(&self) -> (u32, u32) {
        (self.inner.active.cursor.row, self.inner.active.cursor.col)
    }

    pub fn hyperlink_at(
        &self,
        row: u32,
        col: u32,
    ) -> Option<&str> {
        view::hyperlink_at(
            &self.inner.active,
            &self.inner.viewport,
            &self.inner.hyperlinks,
            row,
            col,
        )
    }

    pub fn scroll_to_prev_prompt(&mut self) {
        let viewport = self.inner.viewport;
        view::scroll_to_prev_prompt(&mut self.inner.active, &viewport)
    }

    pub fn scroll_to_next_prompt(&mut self) {
        let viewport = self.inner.viewport;
        view::scroll_to_next_prompt(&mut self.inner.active, &viewport)
    }

    pub fn is_synchronized_update_active(&self) -> bool {
        host::synchronized_update_active(self.inner.modes.synchronized_update_since)
    }

    pub fn take_bell_pending(&mut self) -> bool {
        std::mem::replace(&mut self.effects.bell, false)
    }

    pub fn report_focus_change(
        &mut self,
        focused: bool,
    ) {
        let c1_mode = self.inner.modes.c1_mode;
        let focus_reporting = self.inner.modes.focus_reporting;
        host::report_focus_change(
            &mut self.effects.host_bytes,
            c1_mode,
            focus_reporting,
            focused,
        )
    }

    pub fn take_pending_output(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.effects.host_bytes)
    }

    pub fn pending_output(&mut self) -> Vec<u8> {
        self.take_pending_output()
    }

    pub fn open_search(&mut self) {
        selection::open_search(&mut self.inner.search)
    }

    pub fn search_active(&self) -> bool {
        selection::search_active(&self.inner.search)
    }

    pub fn mouse_report(
        &mut self,
        kind: MouseEventKind,
        button: MouseButton,
        col: u32,
        row: u32,
        mods: MouseModifiers,
    ) -> bool {
        let c1_mode = self.inner.modes.c1_mode;
        let mouse_tracking = self.inner.modes.mouse_tracking;
        let mouse_encoding = self.inner.modes.mouse_encoding;
        host::mouse_report(
            &mut self.effects.host_bytes,
            c1_mode,
            mouse_tracking,
            mouse_encoding,
            kind,
            button,
            col,
            row,
            mods,
        )
    }

    pub fn take_pending_host_resize(&mut self) -> Option<(u32, u32)> {
        self.effects.resize_request.take()
    }

    pub fn set_default_cursor_style(
        &mut self,
        style: CursorStyle,
    ) {
        settings::set_default_cursor_style(&mut self.inner.cursor_style, style)
    }

    pub fn set_palette(
        &mut self,
        palette: ColorPalette,
    ) {
        let Terminal {
            active,
            stash,
            palette: current_palette,
            base_palette,
            dec_color,
            ..
        } = &mut self.inner;
        settings::set_palette(
            active,
            stash,
            current_palette,
            base_palette,
            dec_color,
            palette,
        )
    }

    pub fn set_scrollback_policy(
        &mut self,
        limit: u32,
        strict_altscreen_scrollback: bool,
    ) {
        let Terminal {
            active,
            stash,
            viewport,
            strict_altscreen_scrollback: strict_flag,
            ..
        } = &mut self.inner;
        settings::set_scrollback_policy(
            active,
            stash,
            viewport,
            strict_flag,
            limit,
            strict_altscreen_scrollback,
        )
    }

    pub fn set_default_status_display(
        &mut self,
        status_display: StatusDisplayKind,
    ) {
        let Terminal {
            active,
            stash,
            viewport,
            palette,
            default_status_display,
            ..
        } = &mut self.inner;
        settings::set_default_status_display(
            active,
            stash,
            viewport,
            palette,
            default_status_display,
            status_display,
        )
    }
}

impl Deref for TestTerm {
    type Target = Terminal;

    fn deref(&self) -> &Terminal {
        &self.inner
    }
}

impl DerefMut for TestTerm {
    fn deref_mut(&mut self) -> &mut Terminal {
        &mut self.inner
    }
}

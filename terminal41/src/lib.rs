//! Terminal-emulation core for `term41`.
//!
//! This crate owns terminal state, parsing, screen buffers, host I/O protocol
//! helpers, selection/search state, inline image placement, and DEC/xterm
//! compatibility features. The application crate drives it by feeding PTY
//! bytes through [`TerminalProcessor`] and routing host-originated events
//! through [`HostInput`].

#[macro_use]
extern crate log;

mod apply;
mod charset;
mod color;
mod conformance;
mod cursor;
mod dcs;
mod dec;
mod dispatch;
mod drcs;
mod effects;
mod feature;
mod graphics;
/// Host-bound reports and event encoders.
pub mod host;
mod image;
/// Host clipboard helpers plus keyboard/mouse protocol state reexports.
pub mod io;
pub mod iterm_features;
mod lifecycle_ops;
mod metadata;
mod mode;
mod osc;
mod parser;
mod processing;
/// Shell-integration prompt metadata helpers.
pub mod prompt;
mod protocol_state;
mod report;
mod runtime;
mod screen;
pub mod selection;
/// Runtime settings mutation helpers used by config reload and UI actions.
pub mod settings;
mod snapshot;
mod snapshot_dirty;
mod state;
#[doc(hidden)]
pub mod test_support;
mod thread;
/// Read-only view/navigation helpers for renderer and UI code.
pub mod view;

use config41::ColorPalette;
use config41::CursorStyle;
use config41::EmojiCompatibilityMode;
use config41::FeaturePermissions;
use config41::TerminalLimits;
pub use vte_mode41::TextMode;

pub use crate::conformance::C1Mode;
pub use crate::conformance::ConformanceLevel;
pub use crate::dec::color::ColorSpace as DecColorSpace;
pub use crate::dec::color::DecColorState;
pub use crate::dec::color::LookupTable as DecColorLookupTable;
pub use crate::dec::color::alternate_assignment_for_style as dec_alternate_assignment_for_style;
pub use crate::dec::color::assign_alternate_text_color as dec_assign_alternate_text_color;
pub(crate) use crate::dec::color::report_color_table;
pub use crate::dec::color::select_lookup_table as dec_select_lookup_table;
pub use crate::dec::color::state_from_palette as dec_color_state_from_palette;
pub use crate::dec::color::table_color as dec_table_color;
pub use crate::dec::udk::DecModifierKey;
pub use crate::dec::udk::LocalFunctionKeyControl;
pub use crate::dec::udk::ModifierKeyControl;
pub(crate) use crate::dispatch::CsiAction;
pub(crate) use crate::drcs::DrcsStore;
pub use crate::effects::TerminalEffects;
pub(crate) use crate::feature::apply_status_display_mode;
pub use crate::graphics::KittyFileRequest;
pub use crate::image::PlacedImage;
pub use crate::image::VisibleImage;
pub use crate::image::is_kitty_unicode_placeholder_cell;
pub use crate::io::clipboard::ClipboardRequest;
pub use crate::io::keyboard::KittyFlags;
pub use crate::io::keyboard::KittyKeyboardState;
pub use crate::io::keyboard::KittyKeys;
pub use crate::io::mouse::MouseButton;
pub use crate::io::mouse::MouseEncoding;
pub use crate::io::mouse::MouseEventKind;
pub use crate::io::mouse::MouseModifiers;
pub use crate::io::mouse::MouseTracking;
pub use crate::metadata::CommandMeta;
pub use crate::metadata::ShellIntegrationPhase;
pub use crate::metadata::TerminalMetadata;
pub(crate) use crate::parser::MainCsiAction;
pub(crate) use crate::parser::ParsedCsiAction;
pub use crate::processing::HostInput;
pub use crate::processing::HostInputEffects;
pub use crate::processing::HostMouse;
pub use crate::processing::PasteMode;
pub use crate::processing::TerminalProcessor;
pub use crate::processing::apply_host_input;
pub(crate) use crate::protocol_state::SYNCHRONIZED_UPDATE_TIMEOUT;
pub use crate::protocol_state::TerminalImageState;
pub use crate::protocol_state::TerminalModes;
pub use crate::protocol_state::TerminalProtocolState;
pub use crate::protocol_state::Vt52CursorAddr;
pub(crate) use crate::report::deccir_report;
pub(crate) use crate::report::dectabsr_report;
pub use crate::screen::Screen;
pub use crate::screen::StatusDisplayKind;
pub use crate::screen::grid::Viewport;
pub use crate::screen::hyperlink::HyperlinkRegistry;
pub(crate) use crate::screen::resize_screen;
pub use crate::screen::row::LineAttr;
pub use crate::screen::row::Row;
pub use crate::snapshot::RowSnapshot;
pub use crate::snapshot::SearchSnapshot;
pub use crate::snapshot::TermSnapshot;
pub use crate::snapshot::TermSnapshotInput;
pub use crate::snapshot::TermSnapshotOutput;
pub use crate::snapshot::TermSnapshotPublisher;
pub use crate::snapshot::publish_terminal_snapshot;
pub use crate::snapshot::terminal_snapshot_buffer;
pub use crate::state::Terminal;
pub use crate::thread::TerminalThread;

#[cfg(test)]
mod terminal_effects_tests {
    use crate::ShellIntegrationPhase;
    use crate::test_support::TestTerm;

    #[test]
    fn app_cursor_mode_marks_input_context_changed() {
        let mut term = TestTerm::new_80x24();

        term.process(b"\x1b[?1h");

        assert!(term.active.app_cursor_keys);
        assert!(term.effects.input_context_changed);
    }

    #[test]
    fn alt_screen_mode_marks_input_context_changed() {
        let mut term = TestTerm::new_80x24();

        term.process(b"\x1b[?1049h");

        assert!(term.on_alt_screen);
        assert!(term.effects.input_context_changed);
    }

    #[test]
    fn shell_phase_marks_input_context_changed() {
        let mut term = TestTerm::new_80x24();

        term.process(b"\x1b]133;B\x07");

        assert_eq!(
            term.metadata.shell_integration_phase,
            ShellIntegrationPhase::Command
        );
        assert!(term.effects.input_context_changed);
    }
}

#[cfg(test)]
mod command_block_tests {
    use crate::test_support::TestTerm;

    fn row_text(row: &crate::Row) -> String {
        row.cells.concat()
    }

    #[test]
    fn osc_133_prompt_start_moves_previous_grid_into_own_block() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);

        term.process(b"old");
        term.process(b"\x1b]133;A\x07$ ");

        assert_eq!(term.active.scrollback_blocks.len(), 1);
        assert!(row_text(&term.active.scrollback_blocks[0].grid.rows[0]).starts_with("old"));
        assert!(row_text(&term.active.grid.rows[0]).starts_with("$ "));
    }

    #[test]
    fn primary_ed_2_clears_only_active_block_and_ed_3_drops_completed_blocks() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);

        term.process(b"old");
        term.process(b"\x1b]133;A\x07$ prompt");
        term.process(b"\x1b[2J");

        assert_eq!(term.active.scrollback_blocks.len(), 1);
        assert!(row_text(&term.active.scrollback_blocks[0].grid.rows[0]).starts_with("old"));
        assert!(row_text(&term.active.grid.rows[0]).trim().is_empty());

        term.process(b"\x1b[3J");

        assert!(term.active.scrollback_blocks.is_empty());
    }

    #[test]
    fn alt_screen_ignores_command_block_splitting() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);

        term.process(b"\x1b[?1049h");
        term.process(b"alt");
        term.process(b"\x1b]133;A\x07");

        assert!(term.active.scrollback_blocks.is_empty());
        assert!(row_text(&term.active.grid.rows[0]).starts_with("alt"));
    }

    #[test]
    fn prompt_restart_reuses_prompt_only_block() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);

        term.process(b"\x1b]133;A\x07");
        term.process(b"\x1b]133;A\x07$ ");

        assert!(term.active.scrollback_blocks.is_empty());
        assert!(term.active.grid.rows[0].prompt_start);
        assert!(row_text(&term.active.grid.rows[0]).starts_with("$ "));
    }

    #[test]
    fn prompt_start_drops_empty_block_without_prompt_metadata() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);

        term.process(b"old");
        term.process(b"\x1b]133;A\x07$ ");
        term.process(b"\x1b]133;A\x07");

        assert_eq!(term.active.scrollback_blocks.len(), 2);
        assert!(row_text(&term.active.scrollback_blocks[0].grid.rows[0]).starts_with("old"));
        assert!(row_text(&term.active.scrollback_blocks[1].grid.rows[0]).starts_with("$ "));
        assert!(term.active.grid.rows[0].prompt_start);
    }

    #[test]
    fn prompt_redraw_drops_stale_wrapped_command_tail_after_line_clear() {
        let mut term = TestTerm::new(8, 6, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ old\x1b]133;B\x07");
        term.process(b"\r\n\x1b]133;C\x07out\x1b]133;D;0\x07");
        term.process(b"\r\x1b]133;A\x07$ \x1b]133;B\x07abcdefghijk");
        term.process(b"\x1b[1;8H\x1b[1K");
        term.process(b"\r\x1b]133;A\x07$ \x1b]133;B\x07abcdefghijk");
        term.process(b"\r\n\x1b]133;C\x07running");

        assert_eq!(term.active.scrollback_blocks.len(), 1);
        assert!(row_text(&term.active.scrollback_blocks[0].grid.rows[0]).starts_with("$ old"));
        assert!(
            row_text(&term.active.grid.rows[0]).starts_with("$ abc"),
            "{}",
            row_text(&term.active.grid.rows[0])
        );
        assert!(
            row_text(&term.active.grid.rows[1]).starts_with("ghijk"),
            "{}",
            row_text(&term.active.grid.rows[1])
        );
    }

    #[test]
    fn prompt_redraw_drops_unfinished_wrapped_command_block() {
        let mut term = TestTerm::new(8, 6, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ old\x1b]133;B\x07");
        term.process(b"\r\n\x1b]133;C\x07out\x1b]133;D;0\x07");
        term.process(b"\r\x1b]133;A\x07$ \x1b]133;B\x07abcdefghijk");
        term.process(b"\r\x1b]133;A\x07$ \x1b]133;B\x07abcdefghijk");
        term.process(b"\r\n\x1b]133;C\x07running");

        assert_eq!(term.active.scrollback_blocks.len(), 1);
        assert!(row_text(&term.active.scrollback_blocks[0].grid.rows[0]).starts_with("$ old"));
        assert!(row_text(&term.active.grid.rows[0]).starts_with("$ abc"));
        assert!(row_text(&term.active.grid.rows[1]).starts_with("ghijk"));
    }

    #[test]
    fn prompt_restart_preserves_finished_empty_command_block() {
        let mut term = TestTerm::new(10, 3, 100, 16, 8);

        term.process(b"\x1b]133;A\x07");
        term.process(b"\x1b]133;B\x07");
        term.process(b"\x1b]133;C\x07");
        term.process(b"\x1b]133;D;130\x07");
        term.process(b"\x1b]133;A\x07$ ");

        assert_eq!(term.active.scrollback_blocks.len(), 1);
        assert_eq!(
            term.active.scrollback_blocks[0].grid.rows[0].exit_status,
            Some(130)
        );
        assert!(row_text(&term.active.grid.rows[0]).starts_with("$ "));
    }

    #[test]
    fn csi_edit_after_cursor_position_expands_active_block_grid() {
        let mut term = TestTerm::new(10, 5, 100, 16, 8);

        term.process(b"\x1b]133;A\x07");
        term.process(b"\x1b[4;1H\x1b[Kx");

        assert!(term.active.grid.rows.len() >= 4);
        assert!(row_text(&term.active.grid.rows[3]).starts_with("x"));
    }

    #[test]
    fn resize_preserves_completed_command_blocks() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ one\x1b]133;B\x07");
        term.process(b"\r\n\x1b]133;C\x07output");
        term.process(b"\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ two\x1b]133;B\x07");

        assert_eq!(term.active.scrollback_blocks.len(), 1);
        term.resize(6, 6);

        assert_eq!(term.active.scrollback_blocks.len(), 1);
        assert!(row_text(&term.active.scrollback_blocks[0].grid.rows[0]).starts_with("$ one"));
    }

    #[test]
    fn resize_taller_keeps_completed_command_blocks_visible() {
        let mut term = TestTerm::new(10, 4, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ one\x1b]133;B\x07");
        term.process(b"\r\n\x1b]133;C\x07output");
        term.process(b"\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ two\x1b]133;B\x07");
        term.resize(10, 10);

        let (mut publisher, mut output) = crate::terminal_snapshot_buffer(&mut term.inner);
        crate::publish_terminal_snapshot(&mut term.inner, &mut publisher);
        output.update();
        let snap = output.read();
        let text = snap
            .rows
            .iter()
            .map(|row| row.cells.concat())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("$ one"));
        assert!(text.contains("output"));
        assert!(text.contains("$ two"));
    }

    #[test]
    fn resize_wider_reflows_completed_command_block_wraps() {
        let mut term = TestTerm::new(6, 4, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ x\x1b]133;B\x07");
        term.process(b"\r\n\x1b]133;C\x07abcdefghi");
        term.process(b"\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ y\x1b]133;B\x07");
        term.resize(12, 4);

        let block = &term.active.scrollback_blocks[0];
        assert!(block.grid.rows.iter().all(|row| row.len() == 12));
        assert!(
            block
                .grid
                .rows
                .iter()
                .any(|row| row_text(row).starts_with("abcdefghi"))
        );
    }

    #[test]
    fn resize_narrower_reflows_completed_command_block_wraps() {
        let mut term = TestTerm::new(12, 4, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ x\x1b]133;B\x07");
        term.process(b"\r\n\x1b]133;C\x07abcdefghi");
        term.process(b"\x1b]133;D;0\x07");
        term.process(b"\x1b]133;A\x07$ y\x1b]133;B\x07");
        term.resize(6, 4);

        let block_text = term.active.scrollback_blocks[0]
            .grid
            .rows
            .iter()
            .map(row_text)
            .collect::<Vec<_>>();

        assert!(block_text.iter().any(|row| row.starts_with("abcdef")));
        assert!(block_text.iter().any(|row| row.starts_with("ghi")));
    }
}

#[cfg(test)]
mod emoji_compatibility_tests {
    use super::*;
    use crate::test_support::TestTerm;

    const BASH_ZWJ_EMOJI: &str = "👩🏼\u{200D}❤\u{FE0F}\u{200D}💋\u{200D}👩🏽";

    #[test]
    fn auto_uses_legacy_width_inside_osc_133_command_phase() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);

        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        term.process(BASH_ZWJ_EMOJI.as_bytes());

        assert_eq!(term.cursor(), (0, 13));
        term.process(&[0x08; 11]);
        assert_eq!(term.cursor(), (0, 2));
    }

    #[test]
    fn off_keeps_normal_cluster_width_even_inside_command_phase() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);
        term.emoji_compatibility_mode = EmojiCompatibilityMode::Off;

        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        term.process(BASH_ZWJ_EMOJI.as_bytes());

        assert_eq!(term.cursor(), (0, 4));
    }

    #[test]
    fn on_uses_legacy_width_without_shell_integration() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);
        term.emoji_compatibility_mode = EmojiCompatibilityMode::On;

        term.process(BASH_ZWJ_EMOJI.as_bytes());

        assert_eq!(term.cursor(), (0, 11));
    }

    #[test]
    fn mode_cycles_in_requested_order() {
        let mut term = TestTerm::new(40, 3, 100, 16, 8);

        assert_eq!(term.emoji_compatibility_mode, EmojiCompatibilityMode::Auto);
        assert_eq!(
            term.cycle_emoji_compatibility_mode(),
            EmojiCompatibilityMode::Off
        );
        assert_eq!(
            term.cycle_emoji_compatibility_mode(),
            EmojiCompatibilityMode::On
        );
        assert_eq!(
            term.cycle_emoji_compatibility_mode(),
            EmojiCompatibilityMode::Auto
        );
    }
}

use commands41::CommandLineView;
use terminal41::ShellIntegrationPhase;

use super::InputEndpoint;
use super::command_editor_input_context;
use super::write_host_bytes;

#[derive(Default)]
pub(crate) struct ShellEditing {
    pub(crate) suspended: bool,
    prompt: Option<u64>,
    sent: Option<(String, usize)>,
    confirmed: Option<commands41::CommandEditor>,
}

pub(crate) fn adopt_shell_input(target: &mut InputEndpoint) {
    let terminal = target.terminal.lock();
    prepare_shell_input(
        &terminal,
        &mut target.command_editor,
        &mut target.shell_editing,
    );
}

fn prepare_shell_input(
    terminal: &terminal41::Terminal,
    editor: &mut commands41::CommandEditor,
    editing: &mut ShellEditing,
) {
    let prompt = terminal41::view::shell_prompt_document_row(terminal);
    if terminal.on_alt_screen
        || matches!(
            terminal.metadata.shell_integration_phase,
            ShellIntegrationPhase::Output | ShellIntegrationPhase::Finished
        )
    {
        editing.suspended = true;
        return;
    }
    if terminal.metadata.shell_integration_phase != ShellIntegrationPhase::Command {
        return;
    }
    if editing.sent.is_some() && !editing.suspended {
        return;
    }
    if (editing.suspended && editing.prompt == prompt)
        || !terminal41::view::shell_prompt_is_empty(terminal)
    {
        return;
    }
    editor.clear();
    editing.prompt = prompt;
    editing.suspended = false;
    editing.sent = Some((String::new(), 0));
    editing.confirmed = Some(editor.clone());
}

pub(crate) fn shell_editing_active(target: &InputEndpoint) -> bool {
    !target.shell_editing.suspended && target.shell_editing.sent.is_some()
}

pub(crate) fn suspend_shell_editing(target: &mut InputEndpoint) {
    target.shell_editing.suspended = true;
    target.shell_editing.prompt =
        terminal41::view::shell_prompt_document_row(&target.terminal.lock());
}

pub(crate) fn handoff_shell_input(
    target: &mut InputEndpoint,
    bytes: &[u8],
) {
    // Ctrl+L only redraws. Other forwarded bindings may change the shell
    // buffer, which cannot be reconstructed reliably from terminal cells.
    if !bytes.is_empty() && bytes != b"\x0c" {
        suspend_shell_editing(target);
    }
}

pub(crate) fn sync_shell_input(
    target: &mut InputEndpoint,
    view: &CommandLineView,
) -> Result<(), &'static str> {
    if !shell_editing_active(target) {
        return Ok(());
    }
    let terminal = target.terminal.lock();
    if command_editor_input_context(&terminal).is_none() {
        return Ok(());
    }
    let bracketed_paste = terminal.modes.bracketed_paste;
    let app_cursor = terminal41::view::app_cursor_keys(&terminal.active);
    drop(terminal);
    let (text, cursor) = target.shell_editing.sent.as_ref().unwrap();
    let bytes = super::shell_edit::shell_edit_bytes(
        text,
        *cursor,
        &view.text,
        view.cursor,
        bracketed_paste,
        app_cursor,
    );
    match bytes {
        Ok(bytes) => {
            write_host_bytes(target, bytes, true);
            target.shell_editing.sent = Some((view.text.clone(), view.cursor));
            target.shell_editing.confirmed = Some(target.command_editor.clone());
            Ok(())
        }
        Err(error) => {
            if let Some(editor) = &target.shell_editing.confirmed {
                target.command_editor = editor.clone();
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use commands41::CommandEditor;
    use commands41::EditorInput;
    use commands41::EditorSettings;
    use commands41::apply_input;
    use terminal41::test_support::TestTerm;

    use super::*;

    #[test]
    fn acquire_only_an_empty_marked_prompt_and_release_for_output() {
        let mut term = TestTerm::new_80x24();
        let mut editor = CommandEditor::new();
        let mut editing = ShellEditing::default();
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(editing.sent.is_none());
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07already typed");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(editing.sent.is_none());
        term.process(b"\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert_eq!(editing.sent, Some((String::new(), 0)));
        assert!(!editing.suspended);
        apply_input(
            &mut editor,
            EditorInput::Insert("echo hello".into()),
            &EditorSettings::default(),
        );
        term.process(b"echo hello");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(!editor.is_empty());
        term.process(b"\r\x1b[2K\x1b]133;A\x07$ ");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(!editing.suspended);
        term.process(b"\x1b]133;B\x07echo hello");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert_eq!(editor.view(&EditorSettings::default()).text, "echo hello");
        term.process(b"\r\n\x1b]133;C\x07hello\r\n");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(editing.suspended);
        term.process(b"\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(!editing.suspended);
        assert!(editor.is_empty());
    }

    #[test]
    fn suspended_prompt_and_alternate_screen_do_not_reacquire_input() {
        let mut term = TestTerm::new_80x24();
        let mut editor = CommandEditor::new();
        let mut editing = ShellEditing::default();
        term.process(b"\x1b]133;A\x07$ \x1b]133;B\x07");
        prepare_shell_input(&term, &mut editor, &mut editing);
        editing.suspended = true;
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(editing.suspended);
        term.process(b"\x1b[?1049h\x1b]133;A\x07$ \x1b]133;B\x07");
        prepare_shell_input(&term, &mut editor, &mut editing);
        assert!(editing.suspended);
    }
}

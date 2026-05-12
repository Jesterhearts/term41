use config41::CommandEditorConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandEditorConfigSync {
    PreserveRuntimeEnabled,
    ApplyConfiguredEnabled,
}

pub(super) fn synced_command_editor_config(
    configured: &CommandEditorConfig,
    runtime: &CommandEditorConfig,
    sync: CommandEditorConfigSync,
) -> CommandEditorConfig {
    let mut next = configured.clone();
    if sync == CommandEditorConfigSync::PreserveRuntimeEnabled {
        next.enabled = runtime.enabled;
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_sync_preserves_runtime_enabled_state() {
        let mut configured = CommandEditorConfig {
            enabled: false,
            ..CommandEditorConfig::default()
        };
        configured.vim_mode = true;
        let runtime = CommandEditorConfig {
            enabled: true,
            ..CommandEditorConfig::default()
        };

        let synced = synced_command_editor_config(
            &configured,
            &runtime,
            CommandEditorConfigSync::PreserveRuntimeEnabled,
        );

        assert!(synced.enabled);
        assert!(synced.vim_mode);
    }

    #[test]
    fn config_sync_applies_configured_enabled_state() {
        let configured = CommandEditorConfig {
            enabled: false,
            ..CommandEditorConfig::default()
        };
        let runtime = CommandEditorConfig {
            enabled: true,
            ..CommandEditorConfig::default()
        };

        let synced = synced_command_editor_config(
            &configured,
            &runtime,
            CommandEditorConfigSync::ApplyConfiguredEnabled,
        );

        assert!(!synced.enabled);
    }
}

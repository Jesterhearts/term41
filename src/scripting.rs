use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::mpsc;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::thread::Thread;
use std::time::Duration;
use std::time::Instant;

use arc_swap::ArcSwap;
use config41::ScriptPermissions;
use mlua::Function;
use mlua::Lua;
use mlua::LuaOptions;
use mlua::StdLib;
use mlua::Table;
use mlua::Value;
use parking_lot::Mutex;

const MAX_SCRIPT_TEXT_BYTES: usize = 4096;
const SCRIPT_IDLE_UPDATE_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScriptInput {
    pub tab_title: Option<String>,
    pub cwd: Option<String>,
    pub tab_count: usize,
    /// One-based active tab index for Lua callers.
    pub active_tab_index: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ScriptOutput {
    pub title: Option<String>,
    pub status_text: Option<String>,
    pub error: Option<String>,
}

struct ScriptHandle {
    name: String,
    tx: mpsc::SyncSender<ScriptInput>,
    output: Arc<ArcSwap<ScriptOutput>>,
}

#[derive(Default)]
pub(crate) struct ScriptRuntime {
    scripts: Vec<ScriptHandle>,
}

impl ScriptRuntime {
    pub(crate) fn discover(
        scripts_dir: Option<PathBuf>,
        permissions: &BTreeMap<String, ScriptPermissions>,
        render_thread_handle: Arc<OnceLock<Thread>>,
    ) -> Self {
        let Some(scripts_dir) = scripts_dir else {
            return Self::default();
        };
        let scripts = discover_script_files(&scripts_dir);
        let scripts = scripts
            .into_iter()
            .map(|script| {
                spawn_script(
                    script.clone(),
                    permissions.get(&script.name).copied().unwrap_or_default(),
                    render_thread_handle.clone(),
                )
            })
            .collect();
        Self { scripts }
    }

    pub(crate) fn send_input(
        &self,
        input: ScriptInput,
    ) -> bool {
        let mut delivered = true;
        for script in &self.scripts {
            match script.tx.try_send(input.clone()) {
                Ok(()) => {}
                Err(mpsc::TrySendError::Full(_)) => {
                    delivered = false;
                }
                Err(mpsc::TrySendError::Disconnected(_)) => {}
            }
        }
        delivered
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.scripts.is_empty()
    }

    pub(crate) fn output(&self) -> ScriptOutput {
        let mut merged = ScriptOutput::default();
        for script in &self.scripts {
            let output = script.output.load_full();
            if merged.title.is_none() {
                merged.title = output.title.clone();
            }
            if merged.status_text.is_none() {
                merged.status_text = output.status_text.clone();
            }
            if merged.error.is_none() {
                merged.error = output
                    .error
                    .clone()
                    .map(|error| format!("{}: {error}", script.name));
            }
        }
        merged
    }
}

#[derive(Clone)]
struct ScriptFile {
    name: String,
    path: PathBuf,
}

fn discover_script_files(scripts_dir: &Path) -> Vec<ScriptFile> {
    let Ok(entries) = fs::read_dir(scripts_dir) else {
        return Vec::new();
    };
    let mut scripts = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            (path.extension().and_then(|ext| ext.to_str()) == Some("lua")).then_some(path)
        })
        .filter_map(|path| {
            let name = path.file_stem()?.to_str()?.to_owned();
            Some(ScriptFile { name, path })
        })
        .collect::<Vec<_>>();
    scripts.sort_by(|a, b| a.name.cmp(&b.name));
    scripts
}

fn spawn_script(
    script: ScriptFile,
    permissions: ScriptPermissions,
    render_thread_handle: Arc<OnceLock<Thread>>,
) -> ScriptHandle {
    let (tx, rx) = mpsc::sync_channel(1);
    let output = Arc::new(ArcSwap::from_pointee(ScriptOutput::default()));
    let thread_output = output.clone();
    let handle_name = script.name.clone();
    let thread_name = format!("lua-script-{}", script.name);
    let builder = thread::Builder::new().name(thread_name);
    if let Err(e) = builder.spawn(move || {
        run_script(script, permissions, rx, thread_output, render_thread_handle);
    }) {
        publish_script_output(
            &output,
            ScriptOutput {
                error: Some(format!("failed to spawn script thread: {e}")),
                ..ScriptOutput::default()
            },
        );
    }
    ScriptHandle {
        name: handle_name,
        tx,
        output,
    }
}

#[derive(Debug, Default)]
struct ScriptContext {
    input: ScriptInput,
    output: ScriptOutput,
}

fn run_script(
    script: ScriptFile,
    permissions: ScriptPermissions,
    rx: mpsc::Receiver<ScriptInput>,
    output: Arc<ArcSwap<ScriptOutput>>,
    render_thread_handle: Arc<OnceLock<Thread>>,
) {
    let source = match fs::read_to_string(&script.path) {
        Ok(source) => source,
        Err(e) => {
            publish_script_output(
                &output,
                ScriptOutput {
                    error: Some(format!("failed to read {}: {e}", script.path.display())),
                    ..ScriptOutput::default()
                },
            );
            return;
        }
    };

    let lua = match sandboxed_lua(permissions) {
        Ok(lua) => lua,
        Err(e) => {
            publish_script_output(
                &output,
                ScriptOutput {
                    error: Some(format!("failed to create Lua state: {e}")),
                    ..ScriptOutput::default()
                },
            );
            return;
        }
    };
    let context = Arc::new(Mutex::new(ScriptContext::default()));
    if let Err(e) = install_terminal_module(&lua, context.clone()) {
        publish_script_output(
            &output,
            ScriptOutput {
                error: Some(format!("failed to install terminal module: {e}")),
                ..ScriptOutput::default()
            },
        );
        return;
    }
    if let Err(e) = lua.load(&source).set_name(&script.name).exec() {
        publish_script_output(
            &output,
            ScriptOutput {
                error: Some(format!("failed to load script: {e}")),
                ..ScriptOutput::default()
            },
        );
        return;
    }

    publish_context_output(&context, &output, None, &render_thread_handle);
    let mut next_update = Instant::now();
    loop {
        match rx.recv_timeout(next_update.saturating_duration_since(Instant::now())) {
            Ok(input) => {
                set_latest_script_input(&context, input);
                drain_pending_script_input(&context, &rx);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let update_started = Instant::now();
        let result = call_update(&lua);
        let error = result.err().map(|e| e.to_string());
        publish_context_output(&context, &output, error, &render_thread_handle);
        next_update = next_script_update_deadline(update_started, Instant::now());
    }
}

fn set_latest_script_input(
    context: &Arc<Mutex<ScriptContext>>,
    input: ScriptInput,
) {
    context.lock().input = input;
}

fn drain_pending_script_input(
    context: &Arc<Mutex<ScriptContext>>,
    rx: &mpsc::Receiver<ScriptInput>,
) {
    while let Ok(input) = rx.try_recv() {
        set_latest_script_input(context, input);
    }
}

fn next_script_update_deadline(
    update_started: Instant,
    update_finished: Instant,
) -> Instant {
    update_started
        .checked_add(SCRIPT_IDLE_UPDATE_INTERVAL)
        .filter(|deadline| *deadline > update_finished)
        .unwrap_or(update_finished)
}

fn sandboxed_lua(permissions: ScriptPermissions) -> mlua::Result<Lua> {
    let mut libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8;
    if permissions.filesystem {
        libs |= StdLib::IO;
    }
    if permissions.shell
        || permissions.filesystem
        || permissions.process_info
        || permissions.resource_usage
    {
        libs |= StdLib::OS;
    }
    let lua = Lua::new_with(libs, LuaOptions::default())?;
    let globals = lua.globals();
    if !permissions.resource_usage {
        globals.set("collectgarbage", Value::Nil)?;
    }
    if !permissions.filesystem {
        globals.set("dofile", Value::Nil)?;
        globals.set("loadfile", Value::Nil)?;
    }
    if let Ok(os) = globals.get::<Table>("os") {
        os.set("exit", Value::Nil)?;
        os.set("setlocale", Value::Nil)?;
        if !permissions.shell {
            os.set("execute", Value::Nil)?;
        }
        if !permissions.filesystem {
            os.set("remove", Value::Nil)?;
            os.set("rename", Value::Nil)?;
            os.set("tmpname", Value::Nil)?;
        }
        if !permissions.process_info {
            os.set("getenv", Value::Nil)?;
            os.set("date", Value::Nil)?;
            os.set("time", Value::Nil)?;
            os.set("difftime", Value::Nil)?;
        }
        if !permissions.resource_usage {
            os.set("clock", Value::Nil)?;
        }
    }
    Ok(lua)
}

fn install_terminal_module(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<()> {
    let terminal = lua.create_table()?;
    terminal.set(
        "current_tab_title",
        current_tab_title_fn(lua, context.clone())?,
    )?;
    terminal.set("current_cwd", current_cwd_fn(lua, context.clone())?)?;
    terminal.set("tab_count", tab_count_fn(lua, context.clone())?)?;
    terminal.set(
        "active_tab_index",
        active_tab_index_fn(lua, context.clone())?,
    )?;
    terminal.set("info", info_fn(lua, context.clone())?)?;
    terminal.set(
        "set_current_tab_title",
        set_current_tab_title_fn(lua, context.clone())?,
    )?;
    terminal.set("set_status_text", set_status_text_fn(lua, context)?)?;

    let terminal_key = lua.create_registry_value(terminal)?;
    let require = lua.create_function(move |lua, name: String| {
        if name == "terminal" {
            lua.registry_value::<Table>(&terminal_key)
        } else {
            Err(mlua::Error::RuntimeError(format!(
                "module {name:?} is not available"
            )))
        }
    })?;
    lua.globals().set("require", require)?;
    Ok(())
}

fn current_tab_title_fn(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<Function> {
    lua.create_function(move |_, ()| Ok(context.lock().input.tab_title.clone()))
}

fn current_cwd_fn(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<Function> {
    lua.create_function(move |_, ()| Ok(context.lock().input.cwd.clone()))
}

fn tab_count_fn(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<Function> {
    lua.create_function(move |_, ()| Ok(context.lock().input.tab_count))
}

fn active_tab_index_fn(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<Function> {
    lua.create_function(move |_, ()| Ok(context.lock().input.active_tab_index))
}

fn info_fn(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<Function> {
    lua.create_function(move |lua, ()| {
        let input = context.lock().input.clone();
        let table = lua.create_table()?;
        table.set("tab_title", input.tab_title)?;
        table.set("cwd", input.cwd)?;
        table.set("tab_count", input.tab_count)?;
        table.set("active_tab_index", input.active_tab_index)?;
        Ok(table)
    })
}

fn set_current_tab_title_fn(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<Function> {
    lua.create_function(move |_, value: Value| {
        context.lock().output.title = lua_optional_text(value)?;
        Ok(())
    })
}

fn set_status_text_fn(
    lua: &Lua,
    context: Arc<Mutex<ScriptContext>>,
) -> mlua::Result<Function> {
    lua.create_function(move |_, value: Value| {
        context.lock().output.status_text = lua_optional_text(value)?;
        Ok(())
    })
}

fn lua_optional_text(value: Value) -> mlua::Result<Option<String>> {
    match value {
        Value::Nil => Ok(None),
        Value::String(s) => Ok(Some(clamp_script_text(s.to_str()?.as_ref()))),
        other => Err(mlua::Error::RuntimeError(format!(
            "expected string or nil, got {}",
            other.type_name()
        ))),
    }
}

fn clamp_script_text(text: &str) -> String {
    if text.len() <= MAX_SCRIPT_TEXT_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_SCRIPT_TEXT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

fn call_update(lua: &Lua) -> mlua::Result<()> {
    let globals = lua.globals();
    match globals.get::<Value>("update")? {
        Value::Function(update) => update.call::<()>(()),
        Value::Nil => Ok(()),
        other => Err(mlua::Error::RuntimeError(format!(
            "global update must be a function, got {}",
            other.type_name()
        ))),
    }
}

fn publish_context_output(
    context: &Arc<Mutex<ScriptContext>>,
    output: &ArcSwap<ScriptOutput>,
    error: Option<String>,
    render_thread_handle: &Arc<OnceLock<Thread>>,
) {
    let mut script_output = context.lock().output.clone();
    script_output.error = error;
    if publish_script_output_if_changed(output, script_output)
        && let Some(thread) = render_thread_handle.get()
    {
        thread.unpark();
    }
}

fn publish_script_output(
    output: &ArcSwap<ScriptOutput>,
    script_output: ScriptOutput,
) {
    output.store(Arc::new(script_output));
}

fn publish_script_output_if_changed(
    output: &ArcSwap<ScriptOutput>,
    script_output: ScriptOutput,
) -> bool {
    if output.load().as_ref() == &script_output {
        return false;
    }
    publish_script_output(output, script_output);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_sandbox_does_not_expose_filesystem_helpers() {
        let lua = sandboxed_lua(ScriptPermissions::default()).unwrap();
        let globals = lua.globals();

        assert!(matches!(
            globals.get::<Value>("dofile").unwrap(),
            Value::Nil
        ));
        assert!(matches!(
            globals.get::<Value>("loadfile").unwrap(),
            Value::Nil
        ));
        assert!(matches!(
            globals.get::<Value>("collectgarbage").unwrap(),
            Value::Nil
        ));
        assert!(globals.get::<Table>("io").is_err());
    }

    #[test]
    fn resource_usage_permission_exposes_collectgarbage_and_os_clock() {
        let lua = sandboxed_lua(ScriptPermissions {
            resource_usage: true,
            ..ScriptPermissions::default()
        })
        .unwrap();
        let globals = lua.globals();

        assert!(matches!(
            globals.get::<Value>("collectgarbage").unwrap(),
            Value::Function(_)
        ));
        let os = globals.get::<Table>("os").unwrap();
        assert!(matches!(
            os.get::<Value>("clock").unwrap(),
            Value::Function(_)
        ));
        assert!(matches!(os.get::<Value>("time").unwrap(), Value::Nil));
    }

    #[test]
    fn terminal_module_reads_input_and_records_output() {
        let lua = sandboxed_lua(ScriptPermissions::default()).unwrap();
        let context = Arc::new(Mutex::new(ScriptContext {
            input: ScriptInput {
                tab_title: Some("build".to_owned()),
                cwd: Some("/tmp/project".to_owned()),
                tab_count: 3,
                active_tab_index: 2,
            },
            output: ScriptOutput::default(),
        }));
        install_terminal_module(&lua, context.clone()).unwrap();

        lua.load(
            r#"
local terminal = require("terminal")
terminal.set_current_tab_title(terminal.current_tab_title() .. " ok")
terminal.set_status_text(terminal.current_cwd())
"#,
        )
        .exec()
        .unwrap();

        let output = &context.lock().output;
        assert_eq!(output.title.as_deref(), Some("build ok"));
        assert_eq!(output.status_text.as_deref(), Some("/tmp/project"));
    }

    #[test]
    fn publishing_unchanged_output_does_not_store_again() {
        let output = ArcSwap::from_pointee(ScriptOutput {
            title: Some("build".to_owned()),
            status_text: Some("ok".to_owned()),
            error: None,
        });

        assert!(!publish_script_output_if_changed(
            &output,
            ScriptOutput {
                title: Some("build".to_owned()),
                status_text: Some("ok".to_owned()),
                error: None,
            }
        ));
        assert!(publish_script_output_if_changed(
            &output,
            ScriptOutput {
                title: Some("build".to_owned()),
                status_text: Some("done".to_owned()),
                error: None,
            }
        ));
        assert_eq!(output.load().status_text.as_deref(), Some("done"));
    }

    #[test]
    fn next_script_update_deadline_throttles_fast_updates() {
        let started = Instant::now();
        let finished = started + Duration::from_millis(10);

        assert_eq!(
            next_script_update_deadline(started, finished),
            started + SCRIPT_IDLE_UPDATE_INTERVAL
        );
    }

    #[test]
    fn next_script_update_deadline_keeps_slow_updates_continuous() {
        let started = Instant::now();
        let finished = started + SCRIPT_IDLE_UPDATE_INTERVAL + Duration::from_millis(10);

        assert_eq!(next_script_update_deadline(started, finished), finished);
    }

    #[test]
    fn send_input_reports_full_script_mailboxes() {
        let (tx, _rx) = mpsc::sync_channel(1);
        tx.try_send(ScriptInput::default()).unwrap();
        let runtime = ScriptRuntime {
            scripts: vec![ScriptHandle {
                name: "status".to_owned(),
                tx,
                output: Arc::new(ArcSwap::from_pointee(ScriptOutput::default())),
            }],
        };

        assert!(!runtime.send_input(ScriptInput {
            tab_title: Some("build".to_owned()),
            cwd: None,
            tab_count: 1,
            active_tab_index: 1,
        }));
    }
}

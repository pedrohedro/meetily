//! Meeting debrief with Claude Code (headless), shown in the meeting's "Debrief" tab.
//!
//! - Generation: after a summary is saved (or on demand), `claude -p` reads the
//!   meeting through the bundled read-only MCP server and returns a structured
//!   debrief (`--json-schema`), also written as markdown to `~/markdowns/meetings/`.
//! - Actions: see `actions.rs` (prepare with read tools, execute one tool after
//!   explicit confirmation in the UI).
//!
//! Auth is the user's own Claude Code login (macOS Keychain); Meetily never sees
//! or stores Claude credentials. State lives in `store.rs` (one JSON per meeting).

pub mod actions;
pub mod store;

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

use store::{ActionState, Debrief, Status};

/// The plugin's MCP server, embedded so the app doesn't depend on where the repo lives.
const MCP_SCRIPT: &str = include_str!("../../../../claude-plugin/plugins/meetily/mcp/meetily_mcp.py");
const GENERATE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const EXECUTE_TIMEOUT: Duration = Duration::from_secs(3 * 60);

/// Denied in every headless run, whatever the user's settings pre-approve
/// (deny beats allow). The transcript is untrusted speech.
const DENY_ALWAYS: &[&str] = &[
    "Bash",
    "WebFetch",
    "WebSearch",
    "Agent",
    "Task",
    "NotebookEdit",
    "mcp__plugin_playwright_playwright",
];

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn find_claude() -> Option<PathBuf> {
    [
        home().join(".local/bin/claude"),
        PathBuf::from("/opt/homebrew/bin/claude"),
        PathBuf::from("/usr/local/bin/claude"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Apps launched from the Dock get a bare PATH; `uv` (for the MCP server) lives in ~/.local/bin.
fn gui_path() -> String {
    let home = home();
    format!(
        "{}:{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin",
        home.join(".local/bin").display(),
        home.join(".cargo/bin").display()
    )
}

/// The final `result` event of `claude -p --output-format json` (object or event array).
fn result_event(stdout: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(stdout).ok()? {
        Value::Array(events) => events
            .into_iter()
            .rev()
            .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("result")),
        other => Some(other),
    }
}

/// `structured_output` of a successful run; missing means the run failed to validate.
fn structured_output(stdout: &str) -> Option<Value> {
    let result = result_event(stdout)?;
    if result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(true) {
        return None;
    }
    result.get("structured_output").filter(|v| v.is_object()).cloned()
}

fn notify<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::warn!("debrief: notification failed: {}", e);
    }
}

fn emit_updated<R: Runtime>(app: &AppHandle<R>, meeting_id: &str) {
    if let Err(e) = app.emit("debrief-updated", json!({ "meeting_id": meeting_id })) {
        log::warn!("debrief: emit failed: {}", e);
    }
}

/// Meeting ids reach here from IPC too; they become file names and part of the prompt.
fn valid_meeting_id(id: &str) -> bool {
    id.strip_prefix("meeting-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// One debrief operation per meeting at a time (generation, prepare or execute).
static IN_FLIGHT: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

struct InFlight(String);
impl InFlight {
    fn claim(id: &str) -> Option<Self> {
        let mut running = IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        if running.iter().any(|r| r == id) {
            return None;
        }
        running.push(id.to_string());
        Some(Self(id.to_string()))
    }
}
impl Drop for InFlight {
    fn drop(&mut self) {
        IN_FLIGHT.lock().unwrap_or_else(|e| e.into_inner()).retain(|r| r != &self.0);
    }
}

fn debriefs_dir<R: Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {}", e))?
        .join("debriefs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {}", dir.display(), e))?;
    Ok(dir)
}

fn claim(meeting_id: &str) -> Result<InFlight, String> {
    if !valid_meeting_id(meeting_id) {
        return Err(format!("id de reunião inesperado: {:?}", meeting_id));
    }
    InFlight::claim(meeting_id).ok_or_else(|| "já existe uma operação de debrief em andamento para esta reunião".into())
}

/// Every headless run goes through here: same binary, PATH, cwd and containment flags.
/// Why a headless run produced no output. The distinction matters for writes:
/// `NotStarted` means nothing ran, `TimedOut` means a call may have gone out.
#[derive(Debug)]
enum RunError {
    NotStarted(String),
    TimedOut(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotStarted(e) | Self::TimedOut(e) => f.write_str(e),
        }
    }
}

impl From<RunError> for String {
    fn from(e: RunError) -> Self {
        e.to_string()
    }
}

/// `extra_deny` is merged into the single `--disallowedTools` list (one flag, so no
/// doubt about how repeated variadic flags combine).
async fn run_claude(
    dir: &Path,
    prompt: String,
    extra: &[String],
    extra_deny: &[&str],
    timeout: Duration,
) -> Result<std::process::Output, RunError> {
    let claude = find_claude()
        .ok_or_else(|| RunError::NotStarted("Claude Code CLI não encontrado (instale ou faça login no claude)".into()))?;
    let mut cmd = tokio::process::Command::new(claude);
    cmd.arg("-p")
        .arg(prompt)
        .args(extra)
        .args(["--permission-mode", "dontAsk"])
        .arg("--disallowedTools")
        .args(DENY_ALWAYS)
        .args(extra_deny)
        .current_dir(dir)
        .env("PATH", gui_path())
        .kill_on_drop(true);
    // ponytail: kill_on_drop stops the child only while the app runs; if Meetily quits
    // mid-execution the child may still finish, which is why recovery says `unknown`.
    tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| RunError::TimedOut(format!("tempo esgotado ({} s)", timeout.as_secs())))?
        .map_err(|e| RunError::NotStarted(format!("não foi possível iniciar o claude: {}", e)))
}

fn write_error_log(dir: &Path, name: &str, output: &std::process::Output) -> String {
    let log = dir.join(name);
    let _ = std::fs::write(&log, [output.stdout.as_slice(), &output.stderr].concat());
    log.display().to_string()
}

// ---------------------------------------------------------------- generation

fn debrief_schema() -> Value {
    let item = |extra: Value| {
        let mut props = json!({"text": {"type": "string"}, "ts": {"type": "string"}, "quote": {"type": "string"}});
        if let (Some(p), Some(e)) = (props.as_object_mut(), extra.as_object()) {
            p.extend(e.clone());
        }
        json!({"type": "object", "properties": props, "required": ["text"]})
    };
    json!({
        "type": "object",
        "properties": {
            "title": {"type": "string"},
            "summary": {"type": "string"},
            "decisions": {"type": "array", "items": item(json!({}))},
            "tasks": {"type": "array", "items": item(json!({
                "owner": {"type": "string"}, "owner_inferred": {"type": "boolean"}, "deadline": {"type": "string"}
            }))},
            "pending": {"type": "array", "items": item(json!({}))},
            "questions": {"type": "array", "items": item(json!({}))},
            "actions": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "title": {"type": "string"},
                    "destination": {"type": "string", "enum": ["izzyplan-card", "email", "calendar-event", "teams-message", "notion-page", "none"]},
                    "detail": {"type": "string"}
                },
                "required": ["title", "destination"]
            }},
            "markdown_path": {"type": "string"}
        },
        "required": ["summary", "decisions", "tasks", "pending", "questions", "actions", "markdown_path"]
    })
}

/// The markdown must be a real file inside the debrief folder (the path comes from model output).
fn markdown_ok(path: &str, folder: &Path) -> bool {
    let (Ok(file), Ok(folder)) = (Path::new(path).canonicalize(), folder.canonicalize()) else { return false };
    file.is_file() && file.starts_with(folder)
}

/// Splits Claude's structured output into the stored debrief (result + actions).
fn debrief_from_output(meeting_id: &str, mut out: Value, markdown_folder: &Path) -> Result<Debrief, String> {
    let markdown_path = out.get("markdown_path").and_then(|v| v.as_str()).map(String::from);
    if !markdown_path.as_deref().is_some_and(|p| markdown_ok(p, markdown_folder)) {
        return Err(format!("o debrief não gravou o markdown em {} ({:?})", markdown_folder.display(), markdown_path));
    }
    let actions_value = out.as_object_mut().and_then(|o| o.remove("actions")).unwrap_or(json!([]));
    let actions: Vec<store::Action> =
        serde_json::from_value(actions_value).map_err(|e| format!("ações inválidas: {}", e))?;
    let mut debrief = Debrief::new(meeting_id, Status::Completed);
    debrief.markdown_path = markdown_path;
    debrief.result = Some(out);
    debrief.actions = actions;
    Ok(debrief)
}

fn counts(debrief: &Debrief) -> String {
    let n = |k: &str| {
        debrief.result.as_ref().and_then(|r| r.get(k)).and_then(|v| v.as_array()).map_or(0, |a| a.len())
    };
    format!("{} tarefas, {} pendências, {} decisões", n("tasks"), n("pending"), n("decisions"))
}

/// Called when a summary is saved: respects the auto-debrief preference and never
/// regenerates an existing debrief (summary regeneration must not re-spend usage).
pub fn spawn<R: Runtime>(app: AppHandle<R>, meeting_id: String) {
    let Ok(guard) = claim(&meeting_id) else {
        log::info!("debrief: skipping automatic run for {:?}", meeting_id);
        return;
    };
    tauri::async_runtime::spawn(async move {
        let enabled = crate::audio::recording_preferences::load_recording_preferences(&app)
            .await
            .map(|p| p.auto_debrief)
            .unwrap_or(true);
        if enabled {
            generate(&app, &meeting_id, false).await;
        }
        drop(guard);
    });
}

async fn generate<R: Runtime>(app: &AppHandle<R>, meeting_id: &str, force: bool) {
    let dir = match debriefs_dir(app) {
        Ok(dir) => dir,
        Err(e) => return log::error!("debrief: {}", e),
    };
    match store::load(&dir, meeting_id) {
        Ok(Some(existing)) if !force && existing.status == Status::Completed => {
            return log::info!("debrief: {} already done, skipping", meeting_id);
        }
        // Never overwrite a file we can't read: it may hold executed-action history.
        Err(e) => return log::error!("debrief: {}", e),
        _ => {}
    }
    let _ = store::save(&dir, &Debrief::new(meeting_id, Status::Running));
    emit_updated(app, meeting_id);

    let outcome = async {
        let mcp_config = write_mcp_config(&dir)?;
        let schema = debrief_schema().to_string();
        let extra: Vec<String> = [
            "--mcp-config", &mcp_config.to_string_lossy(),
            "--allowedTools",
            "mcp__meetily__*",
            "mcp__plugin_meetily_meetily__*",
            // Scoped: an unscoped Read would let a spoken instruction pull ~/.ssh etc. in.
            "Read(~/markdowns/meetings/**)",
            "Write(~/markdowns/meetings/**)",
            "Edit(~/markdowns/meetings/**)",
            "--max-turns", "30",
            "--output-format", "json",
            "--json-schema", &schema,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        // Generation needs no connectors: hide every claude.ai one.
        let prompt = format!("/meetily:meeting-debrief {}", meeting_id);
        let output = run_claude(&dir, prompt, &extra, &["mcp__claude_ai_*"], GENERATE_TIMEOUT).await?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        match structured_output(&stdout).filter(|_| output.status.success()) {
            Some(out) => debrief_from_output(meeting_id, out, &home().join("markdowns/meetings")),
            None => Err(format!("sem debrief estruturado; veja {}", write_error_log(&dir, &format!("{}.error.log", meeting_id), &output))),
        }
    }
    .await;

    match outcome {
        Ok(debrief) => {
            let msg = counts(&debrief);
            if let Err(e) = store::save(&dir, &debrief) {
                log::error!("debrief: {}", e);
            }
            notify(app, "Debrief da reunião pronto", &msg);
        }
        Err(e) => {
            log::error!("debrief: {} failed: {}", meeting_id, e);
            let mut failed = Debrief::new(meeting_id, Status::Failed);
            failed.error = Some(e.clone());
            let _ = store::save(&dir, &failed);
            notify(app, "Debrief da reunião falhou", &e);
        }
    }
    emit_updated(app, meeting_id);
}

/// Shared by all meetings; written atomically so a concurrent run never reads a half-written script.
fn write_mcp_config(dir: &Path) -> Result<PathBuf, String> {
    let script = dir.join("meetily_mcp.py");
    store::write_atomic(&script, MCP_SCRIPT.as_bytes())?;
    let config = json!({
        "mcpServers": {
            "meetily": {
                "command": "uv",
                "args": ["run", "--quiet", "--script", script.to_string_lossy()]
            }
        }
    });
    let path = dir.join("mcp.json");
    store::write_atomic(&path, config.to_string().as_bytes())?;
    Ok(path)
}

fn write_guard(dir: &Path) -> Result<PathBuf, String> {
    let guard = dir.join("debrief_guard.py");
    store::write_atomic(&guard, actions::GUARD_SCRIPT.as_bytes())?;
    Ok(guard)
}

// ---------------------------------------------------------------- actions

/// Loads an existing debrief or explains why it can't be used.
fn load_existing(dir: &Path, meeting_id: &str) -> Result<Debrief, String> {
    store::load(dir, meeting_id)?.ok_or_else(|| "debrief não encontrado".to_string())
}

/// Persist a finished action step. If the file vanished or became unreadable the
/// action can't be updated; recovery at the next start will settle it.
fn finish_action(dir: &Path, meeting_id: &str, index: usize, next: ActionState, update: impl FnOnce(&mut store::Action)) {
    let saved = load_existing(dir, meeting_id).and_then(|mut debrief| {
        let action = debrief.actions.get_mut(index).ok_or("ação não existe")?;
        update(action);
        debrief.transition(index, next)?;
        store::save(dir, &debrief)
    });
    if let Err(e) = saved {
        log::error!("debrief: could not record action {} of {}: {}", index, meeting_id, e);
    }
}

async fn prepare<R: Runtime>(app: &AppHandle<R>, dir: &Path, meeting_id: &str, index: usize, tools: actions::ToolSet) {
    let drafted = async {
        let debrief = load_existing(dir, meeting_id)?;
        let action = debrief.actions.get(index).cloned().ok_or("ação não existe")?;
        let result = debrief.result.clone().unwrap_or_default();
        let text = |k: &str| result.get(k).and_then(|v| v.as_str()).unwrap_or_default().to_string();
        let prompt = actions::prepare_prompt(&text("title"), &text("summary"), &action.title, &action.detail, &tools);
        let schema = actions::prepare_schema().to_string();
        // The write tool stays visible (its schema is needed to draft the call), but the
        // guard hook refuses it, whatever the user's settings pre-approve.
        let guard = write_guard(dir)?;
        let settings = actions::guard_settings(&tools.execute, &guard, &[]);
        let mut extra = vec!["--allowedTools".to_string()];
        extra.extend(tools.read.iter().cloned());
        extra.extend(
            ["--settings", &settings, "--max-turns", "8", "--output-format", "json", "--json-schema", &schema]
                .iter()
                .map(|s| s.to_string()),
        );
        let output = run_claude(dir, prompt, &extra, &[], PREPARE_TIMEOUT).await?;
        let out = structured_output(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
            format!("sem rascunho; veja {}", write_error_log(dir, &format!("{}.action{}.error.log", meeting_id, index), &output))
        })?;
        actions::validate_prepared(&action.destination, &out)
    }
    .await;

    match drafted {
        Ok(prepared) => finish_action(dir, meeting_id, index, ActionState::Prepared, |a| {
            a.prepared = Some(prepared);
            a.error = None;
        }),
        Err(e) => finish_action(dir, meeting_id, index, ActionState::Failed, |a| a.error = Some(e)),
    }
    emit_updated(app, meeting_id);
}

async fn execute<R: Runtime>(app: &AppHandle<R>, dir: &Path, meeting_id: &str, index: usize, prepared: store::Prepared) {
    let approved = dir.join(format!("{}.action{}.approved.json", meeting_id, index));
    let run = async {
        let guard = write_guard(dir).map_err(RunError::NotStarted)?;
        store::write_atomic(&approved, prepared.arguments.to_string().as_bytes()).map_err(RunError::NotStarted)?;
        // The guard allows the write tool only with arguments equal to the approved payload.
        let settings = actions::guard_settings(&prepared.tool, &guard, &[approved.as_path()]);
        let extra: Vec<String> = [
            "--allowedTools", &prepared.tool,
            "--settings", &settings,
            "--max-turns", "3",
            "--output-format", "stream-json", "--verbose",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        run_claude(dir, actions::execute_prompt(&prepared), &extra, &[], EXECUTE_TIMEOUT).await
    }
    .await;

    let (next, error, outcome) = match run {
        // Nothing was spawned: no write happened, the user may prepare/execute again.
        Err(RunError::NotStarted(e)) => (ActionState::Failed, Some(e), None),
        // The call may have gone out before the timeout.
        Err(RunError::TimedOut(e)) => (ActionState::Unknown, Some(format!("{} — confira no destino antes de refazer", e)), None),
        Ok(output) => {
            let exec = actions::parse_execution(&String::from_utf8_lossy(&output.stdout), &prepared.tool);
            let outcome = json!({
                "approved_input": prepared.arguments,
                "executed_input": exec.calls,
                "result": exec.result,
            });
            let log = || write_error_log(dir, &format!("{}.action{}.error.log", meeting_id, index), &output);
            let (next, error) = match exec.calls.as_slice() {
                [] => (ActionState::Failed, Some(format!("a ferramenta não foi chamada; veja {}", log()))),
                // The guard refused every call: nothing was sent.
                _ if exec.blocked && exec.calls.len() == 1 => (
                    ActionState::Failed,
                    Some("bloqueado: os argumentos não eram os aprovados (nada foi enviado); prepare de novo".into()),
                ),
                _ if exec.tool_error => (ActionState::Unknown, Some("a ferramenta retornou erro — confira no destino antes de refazer".into())),
                [only] if actions::canonical(only) == actions::canonical(&prepared.arguments) => (ActionState::Executed, None),
                [_] => (ActionState::ExecutedWithDiff, Some("executado com argumentos diferentes do aprovado (veja o detalhe)".into())),
                _ => (ActionState::ExecutedWithDiff, Some(format!("a ferramenta foi chamada {} vezes — confira duplicidades no destino", exec.calls.len()))),
            };
            (next, error, Some(outcome))
        }
    };
    let _ = std::fs::remove_file(&approved);
    finish_action(dir, meeting_id, index, next, |a| {
        a.error = error;
        if outcome.is_some() {
            a.outcome = outcome;
        }
    });
    emit_updated(app, meeting_id);
}

// ---------------------------------------------------------------- commands

#[tauri::command]
pub async fn api_get_debrief<R: Runtime>(app: AppHandle<R>, meeting_id: String) -> Result<Option<Debrief>, String> {
    if !valid_meeting_id(&meeting_id) {
        return Err("id de reunião inesperado".into());
    }
    store::load(&debriefs_dir(&app)?, &meeting_id)
}

/// Manual (re)generation from the Debrief tab; ignores the auto-debrief preference.
#[tauri::command]
pub async fn api_run_debrief<R: Runtime>(app: AppHandle<R>, meeting_id: String) -> Result<(), String> {
    let guard = claim(&meeting_id)?;
    let dir = debriefs_dir(&app)?;
    if store::load(&dir, &meeting_id)?.is_some_and(|d| d.has_external_history()) {
        return Err("já há ações executadas neste debrief; regerar apagaria esse histórico".into());
    }
    tauri::async_runtime::spawn(async move {
        generate(&app, &meeting_id, true).await;
        drop(guard);
    });
    Ok(())
}

#[tauri::command]
pub async fn api_prepare_debrief_action<R: Runtime>(app: AppHandle<R>, meeting_id: String, index: usize) -> Result<(), String> {
    let guard = claim(&meeting_id)?;
    let dir = debriefs_dir(&app)?;
    let mut debrief = load_existing(&dir, &meeting_id)?;
    let destination = debrief.actions.get(index).ok_or("ação não existe")?.destination.clone();
    let tools = actions::tools_for(&destination).ok_or("esta ação não tem execução")?;
    debrief.transition(index, ActionState::Preparing)?;
    store::save(&dir, &debrief)?;
    emit_updated(&app, &meeting_id);
    tauri::async_runtime::spawn(async move {
        prepare(&app, &dir, &meeting_id, index, tools).await;
        drop(guard);
    });
    Ok(())
}

/// Only reachable after the user confirmed the prepared payload in the UI.
/// `reviewed` is the exact `{tool, arguments}` the user saw; it must still match the
/// stored draft, so a re-prepare racing with the dialog can't swap the payload.
#[tauri::command]
pub async fn api_execute_debrief_action<R: Runtime>(
    app: AppHandle<R>,
    meeting_id: String,
    index: usize,
    reviewed: Value,
) -> Result<(), String> {
    let guard = claim(&meeting_id)?;
    if find_claude().is_none() {
        return Err("Claude Code CLI não encontrado".into());
    }
    let dir = debriefs_dir(&app)?;
    let mut debrief = load_existing(&dir, &meeting_id)?;
    let action = debrief.actions.get(index).ok_or("ação não existe")?;
    let prepared = action.prepared.clone().ok_or("ação ainda não foi preparada")?;
    let stored = json!({"tool": prepared.tool, "arguments": prepared.arguments});
    if actions::canonical(&stored) != actions::canonical(&reviewed) {
        return Err("o rascunho mudou desde a revisão; revise de novo antes de executar".into());
    }
    // Re-validate against the destination map: the stored file is not trusted blindly.
    actions::validate_prepared(&action.destination, &serde_json::to_value(&prepared).map_err(|e| e.to_string())?)?;
    debrief.transition(index, ActionState::Executing)?;
    store::save(&dir, &debrief)?;
    emit_updated(&app, &meeting_id);
    tauri::async_runtime::spawn(async move {
        execute(&app, &dir, &meeting_id, index, prepared).await;
        drop(guard);
    });
    Ok(())
}

/// At startup: mark work interrupted by a previous quit (see `store::recover`).
pub fn recover_on_startup<R: Runtime>(app: &AppHandle<R>) {
    if let Ok(dir) = debriefs_dir(app) {
        store::recover_all(&dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meeting_id_allowlist() {
        assert!(valid_meeting_id("meeting-224087ea-59a0-48b6-8033-2a570d7af461"));
        assert!(!valid_meeting_id("meeting-"));
        assert!(!valid_meeting_id("meeting-../../etc"));
        assert!(!valid_meeting_id("meeting-a b"));
        assert!(!valid_meeting_id("x-123"));
    }

    #[test]
    fn in_flight_claims_are_exclusive_and_released() {
        let first = InFlight::claim("meeting-t1").unwrap();
        assert!(InFlight::claim("meeting-t1").is_none());
        drop(first);
        assert!(InFlight::claim("meeting-t1").is_some());
    }

    #[test]
    fn structured_output_requires_success_and_object() {
        let ok = r#"{"type":"result","is_error":false,"structured_output":{"a":1}}"#;
        assert_eq!(structured_output(ok), Some(json!({"a": 1})));
        let arr = r#"[{"type":"system"},{"type":"result","is_error":false,"structured_output":{"b":2}}]"#;
        assert_eq!(structured_output(arr), Some(json!({"b": 2})));
        assert_eq!(structured_output(r#"{"type":"result","is_error":true,"structured_output":{"a":1}}"#), None);
        assert_eq!(structured_output(r#"{"type":"result","is_error":false}"#), None);
        assert_eq!(structured_output("not json"), None);
    }

    #[test]
    fn debrief_from_output_requires_markdown_and_splits_actions() {
        let md = std::env::temp_dir().join(format!("debrief-md-{}.md", std::process::id()));
        std::fs::write(&md, "# x").unwrap();
        let out = json!({
            "summary": "s", "decisions": [], "tasks": [{"text": "t"}], "pending": [], "questions": [],
            "actions": [{"title": "Mail", "destination": "email", "detail": "d"}],
            "markdown_path": md.to_string_lossy()
        });
        let folder = md.parent().unwrap().to_path_buf();
        let d = debrief_from_output("meeting-x", out.clone(), &folder).unwrap();
        assert_eq!(d.status, Status::Completed);
        assert_eq!(d.actions.len(), 1);
        assert_eq!(d.actions[0].state, ActionState::Proposed);
        assert!(d.result.as_ref().unwrap().get("actions").is_none());
        assert_eq!(counts(&d), "1 tarefas, 0 pendências, 0 decisões");

        let mut missing = out.clone();
        missing["markdown_path"] = json!("/nope/x.md");
        assert!(debrief_from_output("meeting-x", missing, &folder).is_err());
        // a real file outside the debrief folder is rejected too
        let mut outside = out.clone();
        outside["markdown_path"] = json!("/etc/hosts");
        assert!(debrief_from_output("meeting-x", outside, &folder).is_err());
        let _ = std::fs::remove_file(md);
    }

    #[test]
    fn deny_list_blocks_shell_and_web() {
        for tool in ["Bash", "WebFetch", "WebSearch"] {
            assert!(DENY_ALWAYS.contains(&tool));
        }
    }
}

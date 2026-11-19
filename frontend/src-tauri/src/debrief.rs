//! Auto-debrief: once a meeting summary is saved, run Claude Code headless and
//! read-only to extract decisions, tasks and pending promises into
//! `~/markdowns/meetings/`. Nothing outside that folder is written; follow-up
//! actions are applied later, interactively, with `/meetily:meeting-debrief apply`.
//!
//! Auth is the user's own Claude Code login (macOS Keychain); Meetily never sees
//! or stores Claude credentials.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tauri::{AppHandle, Manager, Runtime};
use tauri_plugin_notification::NotificationExt;

/// The plugin's MCP server, embedded so the app doesn't depend on where the repo lives.
const MCP_SCRIPT: &str = include_str!("../../../claude-plugin/plugins/meetily/mcp/meetily_mcp.py");
const TIMEOUT: Duration = Duration::from_secs(15 * 60);

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

/// Pulls `(path, counts)` out of the skill's final `DEBRIEF: <path> | tarefas=N | ...` line.
pub fn parse_debrief_line(text: &str) -> Option<(String, String)> {
    let line = text.lines().rev().find_map(|l| l.trim().strip_prefix("DEBRIEF:"))?;
    let mut parts = line.split('|').map(str::trim);
    let path = parts.next().filter(|p| !p.is_empty())?.to_string();
    let counts = parts.collect::<Vec<_>>().join(", ");
    Some((path, counts))
}

/// `claude -p --output-format json` prints either a result object or an event array.
fn result_text(stdout: &str) -> Option<(bool, String)> {
    let value: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let result = match &value {
        serde_json::Value::Array(events) => events
            .iter()
            .rev()
            .find(|e| e.get("type").and_then(|t| t.as_str()) == Some("result"))?,
        other => other,
    };
    let is_error = result.get("is_error").and_then(|v| v.as_bool()).unwrap_or(true);
    let text = result.get("result").and_then(|v| v.as_str()).unwrap_or_default().to_string();
    Some((is_error, text))
}

fn notify<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::warn!("debrief: notification failed: {}", e);
    }
}

/// Meeting ids reach here from IPC too; they become file names and part of the prompt.
fn valid_meeting_id(id: &str) -> bool {
    id.strip_prefix("meeting-")
        .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'))
}

/// Meetings with a debrief currently running (summary regeneration must not start a second one).
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

pub fn spawn<R: Runtime>(app: AppHandle<R>, meeting_id: String) {
    if !valid_meeting_id(&meeting_id) {
        log::warn!("debrief: refusing unexpected meeting id {:?}", meeting_id);
        return;
    }
    tauri::async_runtime::spawn(async move {
        let Some(_claim) = InFlight::claim(&meeting_id) else {
            log::info!("debrief: {} already running, skipping", meeting_id);
            return;
        };
        match run(&app, &meeting_id).await {
            Ok(Some(msg)) => notify(&app, "Debrief da reunião pronto", &msg),
            Ok(None) => {}
            Err(e) => {
                log::error!("debrief: {} failed: {}", meeting_id, e);
                notify(&app, "Debrief da reunião falhou", &e);
            }
        }
    });
}

async fn run<R: Runtime>(app: &AppHandle<R>, meeting_id: &str) -> Result<Option<String>, String> {
    let enabled = crate::audio::recording_preferences::load_recording_preferences(app)
        .await
        .map(|p| p.auto_debrief)
        .unwrap_or(true);
    if !enabled {
        return Ok(None);
    }

    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app data dir: {}", e))?
        .join("debriefs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {}", dir.display(), e))?;

    // One debrief per meeting: regenerating the summary must not re-spend Claude usage.
    let done = dir.join(format!("{}.json", meeting_id));
    if done.exists() {
        log::info!("debrief: {} already done, skipping", meeting_id);
        return Ok(None);
    }

    let claude = find_claude().ok_or("Claude Code CLI não encontrado (instale ou faça login no claude)")?;
    let mcp_config = write_mcp_config(&dir)?;

    log::info!("debrief: starting for {}", meeting_id);
    let mut cmd = tokio::process::Command::new(claude);
    cmd.arg("-p")
        .arg(format!("/meetily:meeting-debrief {}", meeting_id))
        .arg("--mcp-config")
        .arg(&mcp_config)
        .arg("--allowedTools")
        .args([
            "mcp__meetily__*",
            "mcp__plugin_meetily_meetily__*",
            // Scoped: the transcript is untrusted speech; an unscoped Read would let a
            // spoken instruction pull ~/.ssh etc. into the debrief.
            "Read(~/markdowns/meetings/**)",
            "Write(~/markdowns/meetings/**)",
            "Edit(~/markdowns/meetings/**)",
        ])
        .args(["--permission-mode", "dontAsk", "--max-turns", "30", "--output-format", "json"])
        .current_dir(&dir)
        .env("PATH", gui_path())
        .kill_on_drop(true);

    let output = tokio::time::timeout(TIMEOUT, cmd.output())
        .await
        .map_err(|_| "tempo esgotado (15 min)".to_string())?
        .map_err(|e| format!("não foi possível iniciar o claude: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let (is_error, text) = result_text(&stdout).unwrap_or((true, String::new()));
    // Success means the skill reported a file that actually exists; anything else
    // (plugin missing, skill drifted) stays retryable and is surfaced as a failure.
    let produced = parse_debrief_line(&text).filter(|(path, _)| Path::new(path).is_file());
    let Some((path, counts)) = produced.filter(|_| output.status.success() && !is_error) else {
        let log = dir.join(format!("{}.error.log", meeting_id));
        let _ = std::fs::write(&log, [stdout.as_bytes(), &output.stderr].concat());
        return Err(format!("veja {}", log.display()));
    };
    std::fs::write(&done, stdout.as_bytes()).map_err(|e| format!("write {}: {}", done.display(), e))?;

    let file = Path::new(&path).file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or(path);
    Ok(Some(format!("{} — {}", counts, file)))
}

fn write_mcp_config(dir: &Path) -> Result<PathBuf, String> {
    let script = dir.join("meetily_mcp.py");
    std::fs::write(&script, MCP_SCRIPT).map_err(|e| format!("write {}: {}", script.display(), e))?;
    let config = serde_json::json!({
        "mcpServers": {
            "meetily": {
                "command": "uv",
                "args": ["run", "--quiet", "--script", script.to_string_lossy()]
            }
        }
    });
    let path = dir.join("mcp.json");
    std::fs::write(&path, config.to_string()).map_err(|e| format!("write {}: {}", path.display(), e))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_final_debrief_line() {
        let text = "blah\nDEBRIEF: /Users/x/markdowns/meetings/2026-11-18-a.md | tarefas=4 | pendencias=3 | decisoes=8\n";
        let (path, counts) = parse_debrief_line(text).unwrap();
        assert_eq!(path, "/Users/x/markdowns/meetings/2026-11-18-a.md");
        assert_eq!(counts, "tarefas=4, pendencias=3, decisoes=8");
        assert!(parse_debrief_line("no marker here").is_none());
        assert!(parse_debrief_line("DEBRIEF:  | tarefas=1").is_none());
    }

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
    fn reads_result_from_object_or_event_array() {
        let obj = r#"{"type":"result","is_error":false,"result":"ok"}"#;
        assert_eq!(result_text(obj), Some((false, "ok".to_string())));
        let arr = r#"[{"type":"system"},{"type":"result","is_error":true,"result":"boom"}]"#;
        assert_eq!(result_text(arr), Some((true, "boom".to_string())));
        assert_eq!(result_text("not json"), None);
    }
}

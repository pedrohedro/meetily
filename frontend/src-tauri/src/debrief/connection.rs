//! Claude Code connection status and test, for Settings > Claude.
//!
//! Everything comes from the user's own `claude` CLI: Meetily never reads, stores or
//! forwards credentials, and never shows the account e-mail or organization ids
//! (only the fields in `AuthInfo`).

use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Runtime};

const STATUS_TIMEOUT: Duration = Duration::from_secs(20);
const TEST_TIMEOUT: Duration = Duration::from_secs(90);
const SUMMARY_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Default, PartialEq, Serialize)]
pub struct ClaudeStatus {
    pub installed: bool,
    pub path: Option<String>,
    /// Locations that were checked, so "not found" is actionable.
    pub searched: Vec<String>,
    pub version: Option<String>,
    pub logged_in: Option<bool>,
    pub auth_method: Option<String>,
    pub plan: Option<String>,
    pub login_expires_at: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, PartialEq)]
pub struct AuthInfo {
    pub logged_in: bool,
    pub auth_method: Option<String>,
    pub plan: Option<String>,
    pub login_expires_at: Option<String>,
}

/// Parses `claude auth status --json`, keeping only non-identifying fields.
pub fn parse_auth_status(json: &str) -> Result<AuthInfo, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| format!("status ilegível: {}", e))?;
    let text = |k: &str| value.get(k).and_then(|v| v.as_str()).map(String::from);
    Ok(AuthInfo {
        logged_in: value.get("loggedIn").and_then(|v| v.as_bool()).ok_or("status sem o campo loggedIn")?,
        auth_method: text("authMethod"),
        plan: text("subscriptionType"),
        login_expires_at: text("loginExpiresAt"),
    })
}

/// `2.1.330 (Claude Code)` -> `2.1.330 (Claude Code)`; empty output -> None.
fn parse_version(stdout: &str) -> Option<String> {
    stdout.lines().map(str::trim).find(|l| !l.is_empty()).map(String::from)
}

async fn cli(path: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    let mut cmd = tokio::process::Command::new(path);
    cmd.args(args)
        .current_dir(super::home())
        .env("PATH", super::gui_path())
        .kill_on_drop(true);
    tokio::time::timeout(STATUS_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("tempo esgotado ({} s)", STATUS_TIMEOUT.as_secs()))?
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn api_claude_status() -> ClaudeStatus {
    let searched = super::claude_candidates().iter().map(|p| p.display().to_string()).collect();
    let Some(path) = super::find_claude() else {
        return ClaudeStatus { searched, error: Some("Claude Code não encontrado".into()), ..Default::default() };
    };
    let (version, auth) = tokio::join!(cli(&path, &["--version"]), cli(&path, &["auth", "status", "--json"]));

    let mut status = ClaudeStatus {
        installed: true,
        path: Some(path.display().to_string()),
        searched,
        version: version.ok().and_then(|o| parse_version(&String::from_utf8_lossy(&o.stdout))),
        ..Default::default()
    };
    // `auth status` may exit non-zero when logged out, so read stdout either way.
    match auth.and_then(|o| parse_auth_status(&String::from_utf8_lossy(&o.stdout))) {
        Ok(info) => {
            status.logged_in = Some(info.logged_in);
            status.auth_method = info.auth_method;
            status.plan = info.plan;
            status.login_expires_at = info.login_expires_at;
        }
        Err(e) => status.error = Some(e),
    }
    status
}

#[derive(Debug, Serialize)]
pub struct TestResult {
    pub ok: bool,
    pub duration_ms: u128,
    pub message: String,
}

fn short(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 300 {
        format!("{}…", one_line.chars().take(300).collect::<String>())
    } else {
        one_line
    }
}

/// A real, minimal round trip through the same launcher the debrief uses (same binary,
/// PATH, deny list and permission mode), so a pass means the debrief can run.
#[tauri::command]
pub async fn api_claude_test<R: Runtime>(app: AppHandle<R>) -> Result<TestResult, String> {
    if super::find_claude().is_none() {
        return Err("Claude Code não encontrado".into());
    }
    let dir = super::debriefs_dir(&app)?;
    let extra: Vec<String> = ["--max-turns", "2", "--output-format", "json"].iter().map(|s| s.to_string()).collect();
    let started = Instant::now();
    let run = super::run_claude(&dir, "Reply with the single word OK.".into(), &extra, &["mcp__claude_ai_*"], TEST_TIMEOUT).await;
    let duration_ms = started.elapsed().as_millis();

    let (ok, message) = match run {
        Err(e) => (false, e.to_string()),
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let result = super::result_event(&stdout);
            let is_error = result.as_ref().and_then(|r| r.get("is_error")).and_then(|v| v.as_bool()).unwrap_or(true);
            let text = result.as_ref().and_then(|r| r.get("result")).and_then(|v| v.as_str()).unwrap_or_default();
            if output.status.success() && !is_error && !text.trim().is_empty() {
                (true, format!("Claude respondeu em {:.1} s", duration_ms as f64 / 1000.0))
            } else {
                let detail = if text.trim().is_empty() { String::from_utf8_lossy(&output.stderr).into_owned() } else { text.to_string() };
                (false, short(&detail))
            }
        }
    };
    Ok(TestResult { ok, duration_ms, message })
}

#[cfg(test)]
mod tests {
    #[test]
    fn claude_code_is_a_summary_provider() {
        use crate::summary::llm_client::LLMProvider;
        assert_eq!(LLMProvider::from_str("claude-code"), Ok(LLMProvider::ClaudeCode));
        assert_eq!(LLMProvider::from_str("claude"), Ok(LLMProvider::Claude)); // API-key provider untouched
    }

    /// Real round trip through the logged-in CLI: `cargo test -- --ignored claude_code_summary`.
    #[tokio::test]
    #[ignore]
    async fn claude_code_summary_round_trip() {
        let out = complete("Reply with one word.", "Say: pong", "default", None).await.unwrap();
        assert!(out.to_lowercase().contains("pong"), "got: {out}");
    }

    use super::*;

    #[test]
    fn parses_status_and_drops_identifiers() {
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"team","loginExpiresAt":"2026-12-15T02:08:22.874Z",
            "email":"someone@example.com","orgId":"abc","orgName":"Acme","accountUuid":"u"}"#;
        let info = parse_auth_status(json).unwrap();
        assert_eq!(
            info,
            AuthInfo {
                logged_in: true,
                auth_method: Some("claude.ai".into()),
                plan: Some("team".into()),
                login_expires_at: Some("2026-12-15T02:08:22.874Z".into()),
            }
        );
        // the type has no field for identifiers, so none can leak to the UI
        assert!(!format!("{:?}", info).contains("example.com"));
    }

    #[test]
    fn logged_out_and_garbage() {
        assert!(!parse_auth_status(r#"{"loggedIn":false}"#).unwrap().logged_in);
        assert!(parse_auth_status(r#"{"nothing":1}"#).is_err());
        assert!(parse_auth_status("Not logged in").is_err());
    }

    #[test]
    fn version_and_short() {
        assert_eq!(parse_version("2.1.330 (Claude Code)\n").as_deref(), Some("2.1.330 (Claude Code)"));
        assert_eq!(parse_version("\n  \n"), None);
        assert!(short(&"x ".repeat(400)).ends_with('…'));
        assert_eq!(short("a\n b   c"), "a b c");
    }
}


/// One tool-less completion through the user's own Claude Code login, used as a
/// summary provider. The transcript goes on stdin (argv would hit the OS limit),
/// runs in an empty directory with no tools, MCP servers, hooks or skills, so the
/// model can only answer with text.
pub(crate) async fn complete(
    system: &str,
    user: &str,
    model: &str,
    cancel: Option<&tokio_util::sync::CancellationToken>,
) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;

    let claude = super::find_claude()
        .ok_or("Claude Code CLI não encontrado: instale e rode `claude auth login`")?;
    let dir = std::env::temp_dir().join("meetily-claude-summary");
    std::fs::create_dir_all(&dir).map_err(|e| format!("pasta temporária: {}", e))?;

    let mut cmd = tokio::process::Command::new(claude);
    cmd.arg("-p")
        .args(["--tools", ""])
        .args(["--setting-sources", "project"])
        .args(["--strict-mcp-config", "--disable-slash-commands", "--no-session-persistence"])
        .args(["--max-turns", "1", "--permission-mode", "dontAsk"])
        .args(["--system-prompt", system]);
    if !model.is_empty() && model != "default" {
        cmd.args(["--model", model]);
    }
    cmd.current_dir(&dir)
        .env("PATH", super::gui_path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| format!("não foi possível iniciar o claude: {}", e))?;
    let mut stdin = child.stdin.take().ok_or("stdin do claude indisponível")?;
    let input = user.to_owned();
    let writer = tokio::spawn(async move {
        let _ = stdin.write_all(input.as_bytes()).await;
        // stdin closes when dropped here
    });

    let run = async {
        tokio::time::timeout(SUMMARY_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| format!("tempo esgotado ({} s)", SUMMARY_TIMEOUT.as_secs()))?
            .map_err(|e| format!("falha ao ler a saída do claude: {}", e))
    };
    let output = match cancel {
        Some(token) => tokio::select! {
            r = run => r?,
            _ = token.cancelled() => return Err("Summary generation was cancelled".to_string()),
        },
        None => run.await?,
    };
    let _ = writer.await;

    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let tail: String = err.trim().chars().rev().take(300).collect::<Vec<_>>().into_iter().rev().collect();
        return Err(format!("claude saiu com {}: {}", output.status, tail));
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return Err("claude não devolveu texto".to_string());
    }
    Ok(text)
}

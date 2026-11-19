//! Per-meeting debrief state in `app_data_dir/debriefs/<meeting_id>.json`.
//!
//! A JSON file instead of a DB table on purpose: the upstream app runs
//! `sqlx::migrate!` without `ignore_missing`, so a fork-only migration would stop
//! the official build from opening the database after a rollback.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Completed,
    Failed,
}

/// `failed` only happens before an external write starts. Once a write may have
/// happened the state is terminal (`executed`, `executed_with_diff`, `unknown`),
/// so the UI never offers a retry that could duplicate a card or a message.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionState {
    Proposed,
    Preparing,
    Prepared,
    Executing,
    Executed,
    ExecutedWithDiff,
    Failed,
    Unknown,
}

impl ActionState {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Executed | Self::ExecutedWithDiff | Self::Unknown)
    }
}

fn proposed() -> ActionState {
    ActionState::Proposed
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prepared {
    pub tool: String,
    pub arguments: Value,
    pub human_summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    pub title: String,
    pub destination: String,
    #[serde(default)]
    pub detail: String,
    #[serde(default = "proposed")]
    pub state: ActionState,
    #[serde(default)]
    pub prepared: Option<Prepared>,
    /// What actually ran: `{executed_input, result}` (both payloads kept for audit).
    #[serde(default)]
    pub outcome: Option<Value>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Debrief {
    pub meeting_id: String,
    pub status: Status,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub markdown_path: Option<String>,
    /// Structured debrief (summary, decisions, tasks, pending, questions).
    #[serde(default)]
    pub result: Option<Value>,
    #[serde(default)]
    pub actions: Vec<Action>,
    pub updated_at: String,
}

impl Debrief {
    pub fn new(meeting_id: &str, status: Status) -> Self {
        Self {
            meeting_id: meeting_id.to_string(),
            status,
            error: None,
            markdown_path: None,
            result: None,
            actions: Vec::new(),
            updated_at: now(),
        }
    }

    /// Regenerating replaces the action list, so refuse once anything may have been written outside.
    pub fn has_external_history(&self) -> bool {
        self.actions.iter().any(|a| a.state.is_terminal() || a.state == ActionState::Executing)
    }

    pub fn transition(&mut self, index: usize, to: ActionState) -> Result<(), String> {
        use ActionState::*;
        let action = self.actions.get_mut(index).ok_or_else(|| format!("ação {} não existe", index))?;
        let from = action.state;
        let allowed = matches!(
            (from, to),
            (Proposed | Failed | Prepared, Preparing)
                | (Preparing, Prepared | Failed)
                | (Prepared, Executing)
                | (Executing, Executed | ExecutedWithDiff | Failed | Unknown)
        );
        if !allowed {
            return Err(format!("transição inválida da ação {}: {:?} -> {:?}", index, from, to));
        }
        action.state = to;
        self.updated_at = now();
        Ok(())
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn path(dir: &Path, meeting_id: &str) -> PathBuf {
    dir.join(format!("{}.json", meeting_id))
}

/// `Ok(None)`: no debrief yet. A file from the previous version (the raw CLI result
/// envelope) is a completed debrief without structured data. Anything else that
/// doesn't parse is an error: treating corruption as "empty" would let a regenerate
/// erase the record of actions that were already executed.
pub fn load(dir: &Path, meeting_id: &str) -> Result<Option<Debrief>, String> {
    let file = path(dir, meeting_id);
    let text = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {}", file.display(), e)),
    };
    if let Ok(debrief) = serde_json::from_str::<Debrief>(&text) {
        return Ok(Some(debrief));
    }
    // The previous version stored `claude -p --output-format json` verbatim: a result
    // object, or (as observed on this machine) an array of events ending in one.
    let is_result = |v: &Value| v.get("type").and_then(|t| t.as_str()) == Some("result");
    let legacy = match serde_json::from_str::<Value>(&text) {
        Ok(Value::Array(events)) => events.iter().any(is_result),
        Ok(v) => is_result(&v),
        Err(_) => false,
    };
    if legacy {
        return Ok(Some(Debrief::new(meeting_id, Status::Completed)));
    }
    Err(format!("arquivo de debrief ilegível: {}", file.display()))
}

/// Atomic and durable: temp file, fsync, rename, fsync the directory. An action is
/// marked `executing` through here before its write is spawned, so a power loss
/// must not roll the file back to `prepared` (that would allow a second execution).
pub fn write_atomic(target: &Path, body: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let dir = target.parent().ok_or("caminho sem diretório")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {}", dir.display(), e))?;
    let tmp = target.with_extension("tmp");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("create {}: {}", tmp.display(), e))?;
    file.write_all(body).and_then(|_| file.sync_all()).map_err(|e| format!("write {}: {}", tmp.display(), e))?;
    std::fs::rename(&tmp, target).map_err(|e| format!("rename {}: {}", target.display(), e))?;
    std::fs::File::open(dir).and_then(|d| d.sync_all()).map_err(|e| format!("sync {}: {}", dir.display(), e))
}

pub fn save(dir: &Path, debrief: &Debrief) -> Result<(), String> {
    let body = serde_json::to_vec_pretty(debrief).map_err(|e| e.to_string())?;
    write_atomic(&path(dir, &debrief.meeting_id), &body)
}

/// At startup nothing is running, so leftovers are interrupted work: a generation
/// becomes failed, a preparation goes back to proposed, and an execution becomes
/// `unknown` (the write may or may not have happened; never retried automatically).
pub fn recover(debrief: &mut Debrief) -> bool {
    let mut changed = false;
    if debrief.status == Status::Running {
        debrief.status = Status::Failed;
        debrief.error = Some("interrompido (o app foi fechado durante o debrief)".into());
        changed = true;
    }
    for action in &mut debrief.actions {
        let next = match action.state {
            ActionState::Preparing => ActionState::Proposed,
            ActionState::Executing => ActionState::Unknown,
            _ => continue,
        };
        action.state = next;
        if next == ActionState::Unknown {
            action.error = Some("execução interrompida: confira no destino antes de refazer".into());
        }
        changed = true;
    }
    changed
}

pub fn recover_all(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_suffix(".json").filter(|id| id.starts_with("meeting-")) else { continue };
        if let Ok(Some(mut debrief)) = load(dir, id) {
            if recover(&mut debrief) {
                if let Err(e) = save(dir, &debrief) {
                    log::warn!("debrief: recovery of {} failed: {}", id, e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_actions(states: &[ActionState]) -> Debrief {
        let mut d = Debrief::new("meeting-x", Status::Completed);
        d.actions = states
            .iter()
            .map(|s| Action {
                title: "t".into(),
                destination: "email".into(),
                detail: String::new(),
                state: *s,
                prepared: None,
                outcome: None,
                error: None,
            })
            .collect();
        d
    }

    #[test]
    fn happy_path_and_reprepare() {
        use ActionState::*;
        let mut d = with_actions(&[Proposed]);
        d.transition(0, Preparing).unwrap();
        d.transition(0, Prepared).unwrap();
        d.transition(0, Preparing).unwrap(); // re-draft before executing is fine
        d.transition(0, Prepared).unwrap();
        d.transition(0, Executing).unwrap();
        d.transition(0, Executed).unwrap();
    }

    #[test]
    fn terminal_states_never_become_executable_again() {
        use ActionState::*;
        for terminal in [Executed, ExecutedWithDiff, Unknown] {
            let mut d = with_actions(&[terminal]);
            for to in [Preparing, Prepared, Executing, Failed] {
                assert!(d.transition(0, to).is_err(), "{:?} -> {:?} must be rejected", terminal, to);
            }
        }
        // cannot skip confirmation: proposed/failed straight to executing is rejected
        assert!(with_actions(&[Proposed]).transition(0, Executing).is_err());
        assert!(with_actions(&[Failed]).transition(0, Executing).is_err());
        assert!(with_actions(&[Proposed]).transition(5, Preparing).is_err());
    }

    #[test]
    fn recovery_marks_interrupted_work() {
        use ActionState::*;
        let mut d = with_actions(&[Preparing, Executing, Prepared]);
        d.status = Status::Running;
        assert!(recover(&mut d));
        assert_eq!(d.status, Status::Failed);
        assert_eq!(d.actions[0].state, Proposed);
        assert_eq!(d.actions[1].state, Unknown);
        assert_eq!(d.actions[2].state, Prepared);
        assert!(d.has_external_history());
        assert!(!recover(&mut d)); // idempotent
    }

    #[test]
    fn save_load_roundtrip_and_legacy_file() {
        let dir = std::env::temp_dir().join(format!("debrief-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = with_actions(&[ActionState::Prepared]);
        save(&dir, &d).unwrap();
        assert_eq!(load(&dir, "meeting-x").unwrap(), Some(d));
        assert!(!path(&dir, "meeting-x").with_extension("tmp").exists());

        std::fs::write(path(&dir, "meeting-old"), r#"{"type":"result","is_error":false}"#).unwrap();
        let legacy = load(&dir, "meeting-old").unwrap().unwrap();
        assert_eq!(legacy.status, Status::Completed);
        assert!(legacy.result.is_none());
        std::fs::write(path(&dir, "meeting-old2"), r#"[{"type":"system"},{"type":"result","is_error":false}]"#).unwrap();
        assert_eq!(load(&dir, "meeting-old2").unwrap().unwrap().status, Status::Completed);
        assert_eq!(load(&dir, "meeting-none").unwrap(), None);

        // corruption is an error, never an empty debrief that a regenerate could overwrite
        std::fs::write(path(&dir, "meeting-bad"), "{\"meeting_id\": \"meeting-bad\", \"actions\": [tru").unwrap();
        assert!(load(&dir, "meeting-bad").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

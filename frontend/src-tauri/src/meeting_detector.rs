//! Auto-record Microsoft Teams calls.
//!
//! Signal: a process whose bundle id starts with `com.microsoft.teams2` is running
//! audio *input* (CoreAudio `kAudioProcessPropertyIsRunningInput`). Measured on
//! Teams 2 (macOS 27): the helper `com.microsoft.teams2.modulehost` holds the mic
//! for the whole call, keeps it while muted, and releases it on leaving.
//!
//! Only recordings this detector started are stopped by it; a manual recording
//! is never touched.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

const TEAMS_BUNDLE_PREFIX: &str = "com.microsoft.teams2";
const POLL_EVERY: Duration = Duration::from_secs(3);
/// Mic must be held this long before recording (ignores quick device tests).
const START_AFTER: Duration = Duration::from_secs(5);
/// Mic must be released this long before stopping (survives drop + rejoin).
const STOP_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Start,
    Stop,
}

/// Debounces the raw "in call" signal into start/stop edges.
#[derive(Debug, Default)]
pub struct Debouncer {
    active: bool,
    in_since: Option<Instant>,
    out_since: Option<Instant>,
}

impl Debouncer {
    pub fn step(&mut self, in_call: bool, now: Instant) -> Option<Action> {
        if in_call {
            self.out_since = None;
            if self.active {
                return None;
            }
            let since = *self.in_since.get_or_insert(now);
            if now.duration_since(since) >= START_AFTER {
                self.active = true;
                self.in_since = None;
                return Some(Action::Start);
            }
        } else {
            self.in_since = None;
            if !self.active {
                return None;
            }
            let since = *self.out_since.get_or_insert(now);
            if now.duration_since(since) >= STOP_AFTER {
                self.active = false;
                self.out_since = None;
                return Some(Action::Stop);
            }
        }
        None
    }
}

#[cfg(target_os = "macos")]
fn teams_using_mic() -> bool {
    use cidre::core_audio::hardware::System;
    // ponytail: polls every 3s; switch to a CoreAudio property listener if CPU ever matters.
    let Ok(processes) = System::processes() else {
        return false;
    };
    processes.iter().any(|p| {
        p.is_running_input().unwrap_or(false)
            && p.bundle_id()
                .map(|id| id.to_string().starts_with(TEAMS_BUNDLE_PREFIX))
                .unwrap_or(false)
    })
}

#[cfg(not(target_os = "macos"))]
fn teams_using_mic() -> bool {
    false
}

async fn enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    crate::audio::recording_preferences::load_recording_preferences(app)
        .await
        .map(|p| p.auto_record_teams)
        .unwrap_or(false)
}

fn notify<R: Runtime>(app: &AppHandle<R>, title: &str, body: &str) {
    if let Err(e) = app.notification().builder().title(title).body(body).show() {
        log::warn!("meeting_detector: notification failed: {}", e);
    }
}

pub fn start<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let mut debouncer = Debouncer::default();
        let mut we_started = false;
        let mut tick = tokio::time::interval(POLL_EVERY);
        loop {
            tick.tick().await;
            if !enabled(&app).await {
                debouncer = Debouncer::default();
                continue;
            }
            let recording = crate::audio::recording_commands::is_recording().await;
            if we_started && !recording {
                we_started = false; // stopped by the user meanwhile
            }

            let in_call = tokio::task::spawn_blocking(teams_using_mic)
                .await
                .unwrap_or(false);

            match debouncer.step(in_call, Instant::now()) {
                Some(Action::Start) if !recording => {
                    let name = format!("Teams {}", chrono::Local::now().format("%Y-%m-%d %H:%M"));
                    log::info!("meeting_detector: Teams call detected, starting recording '{}'", name);
                    match crate::audio::recording_commands::start_recording_with_meeting_name(
                        app.clone(),
                        Some(name),
                    )
                    .await
                    {
                        Ok(()) => {
                            we_started = true;
                            notify(&app, "Gravando reunião do Teams", "Avise os participantes que a reunião está sendo gravada.");
                        }
                        Err(e) => log::error!("meeting_detector: failed to start recording: {}", e),
                    }
                }
                Some(Action::Stop) if we_started && recording => {
                    log::info!("meeting_detector: Teams call ended, stopping recording");
                    we_started = false;
                    if let Err(e) = crate::tray::stop_recording_from_backend(&app).await {
                        log::error!("meeting_detector: {}", e);
                    }
                }
                _ => {}
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(base: Instant, secs: u64) -> Instant {
        base + Duration::from_secs(secs)
    }

    #[test]
    fn starts_after_hold_and_stops_after_release() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        assert_eq!(d.step(true, at(t, 0)), None);
        assert_eq!(d.step(true, at(t, 3)), None);
        assert_eq!(d.step(true, at(t, 6)), Some(Action::Start));
        assert_eq!(d.step(true, at(t, 9)), None); // stays active, no repeat
        assert_eq!(d.step(false, at(t, 12)), None);
        assert_eq!(d.step(false, at(t, 40)), None); // 28s out
        assert_eq!(d.step(false, at(t, 42)), Some(Action::Stop));
        assert_eq!(d.step(false, at(t, 45)), None);
    }

    #[test]
    fn short_blips_do_nothing() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        // device test: mic held 3s then released
        assert_eq!(d.step(true, at(t, 0)), None);
        assert_eq!(d.step(true, at(t, 3)), None);
        assert_eq!(d.step(false, at(t, 6)), None);
        // the hold timer restarts instead of accumulating
        assert_eq!(d.step(true, at(t, 9)), None);
        assert_eq!(d.step(true, at(t, 12)), None);
        assert_eq!(d.step(true, at(t, 14)), Some(Action::Start));
    }

    #[test]
    fn rejoin_within_grace_keeps_one_recording() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.step(true, at(t, 0));
        assert_eq!(d.step(true, at(t, 5)), Some(Action::Start));
        assert_eq!(d.step(false, at(t, 10)), None); // dropped
        assert_eq!(d.step(false, at(t, 30)), None);
        assert_eq!(d.step(true, at(t, 35)), None); // rejoined within 30s
        assert_eq!(d.step(false, at(t, 40)), None); // out timer restarted
        assert_eq!(d.step(false, at(t, 69)), None);
        assert_eq!(d.step(false, at(t, 70)), Some(Action::Stop));
    }
}

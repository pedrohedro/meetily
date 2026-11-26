//! Auto-record Microsoft Teams calls.
//!
//! Signal: a process whose bundle id starts with `com.microsoft.teams2` is running
//! audio *input* (CoreAudio `kAudioProcessPropertyIsRunningInput`). Measured on
//! Teams 2 (macOS 27): the helper `com.microsoft.teams2.modulehost` holds the mic
//! for the whole call, keeps it while muted, and releases it on leaving.
//!
//! With the toggle on, the detector owns the recording for the length of a call:
//! it starts one when a call begins, and if a recording is already running (e.g. you
//! pressed record by hand) it adopts it and stops it when the call ends. Recordings
//! that never overlapped a Teams call are never touched.

use std::time::{Duration, Instant};

use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;

const TEAMS_BUNDLE_PREFIX: &str = "com.microsoft.teams2";
const POLL_EVERY: Duration = Duration::from_secs(3);
/// Mic must be held this long before recording (ignores quick device tests).
const START_AFTER: Duration = Duration::from_secs(5);
/// Mic must be released this long before stopping (survives drop + rejoin).
const STOP_AFTER: Duration = Duration::from_secs(30);
/// Wait after any recording ends before auto-starting another (frontend save window).
const COOLDOWN: Duration = Duration::from_secs(90);

/// What to do when a call has been held long enough to count as started.
#[derive(Debug, PartialEq, Eq)]
pub enum StartDecision {
    Begin,
    /// A recording is already running: take it over so the call's end stops it.
    Adopt,
    /// A recording just ended and is still being saved: try again later.
    Retry,
}

pub fn decide_start(recording: bool, cooling_down: bool) -> StartDecision {
    match (recording, cooling_down) {
        (false, false) => StartDecision::Begin,
        (true, _) => StartDecision::Adopt,
        (false, true) => StartDecision::Retry,
    }
}

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
    /// Forget a Start the caller could not act on, so the next ticks retry it.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

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

/// Reads only the flag; `load_recording_preferences` logs at info on every call,
/// which would add ~29k lines/day at a 3s poll.
fn enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    use tauri_plugin_store::StoreExt;
    app.store("recording_preferences.json")
        .ok()
        .and_then(|store| store.get("preferences"))
        .and_then(|prefs| prefs.get("auto_record_teams").and_then(|v| v.as_bool()))
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
        let mut was_recording = false;
        let mut last_recording_end: Option<Instant> = None;
        let mut tick = tokio::time::interval(POLL_EVERY);
        loop {
            tick.tick().await;
            let now = Instant::now();
            let recording = crate::audio::recording_commands::is_recording().await;
            if was_recording && !recording {
                last_recording_end = Some(now);
                we_started = false; // whoever stopped it, we no longer own a recording
            }
            was_recording = recording;

            if !enabled(&app) {
                debouncer.reset();
                we_started = false;
                continue;
            }

            let in_call = tokio::task::spawn_blocking(teams_using_mic)
                .await
                .unwrap_or(false);

            match debouncer.step(in_call, now) {
                Some(Action::Start) => {
                    // The frontend saves a stopped meeting asynchronously (up to ~60s);
                    // starting the next one before that finishes mixes their transcripts.
                    let cooling_down = last_recording_end.is_some_and(|t| now.duration_since(t) < COOLDOWN);
                    match decide_start(recording, cooling_down) {
                        StartDecision::Retry => {
                            debouncer.reset(); // retry on later ticks
                            continue;
                        }
                        StartDecision::Adopt => {
                            log::info!("meeting_detector: Teams call detected during a running recording, will stop it when the call ends");
                            we_started = true;
                            notify(&app, "Reunião do Teams detectada", "A gravação em andamento vai parar quando a call terminar.");
                            continue;
                        }
                        StartDecision::Begin => {}
                    }
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
                            was_recording = true;
                            notify(&app, "Gravando reunião do Teams", "Avise os participantes que a reunião está sendo gravada.");
                        }
                        Err(e) => {
                            log::error!("meeting_detector: failed to start recording: {}", e);
                            notify(&app, "Não foi possível gravar a reunião do Teams", &e);
                            debouncer.reset();
                        }
                    }
                }
                Some(Action::Stop) if we_started && recording => {
                    log::info!("meeting_detector: Teams call ended, stopping recording");
                    match crate::tray::stop_recording_from_backend(&app).await {
                        Ok(()) => we_started = false,
                        Err(e) => {
                            log::error!("meeting_detector: {}", e);
                            notify(&app, "Não foi possível parar a gravação", &e);
                        }
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
    fn running_recording_is_adopted_not_ignored() {
        assert_eq!(decide_start(false, false), StartDecision::Begin);
        assert_eq!(decide_start(true, false), StartDecision::Adopt);
        assert_eq!(decide_start(true, true), StartDecision::Adopt);
        assert_eq!(decide_start(false, true), StartDecision::Retry);
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
    fn reset_after_skipped_start_retries() {
        let t = Instant::now();
        let mut d = Debouncer::default();
        d.step(true, at(t, 0));
        assert_eq!(d.step(true, at(t, 5)), Some(Action::Start));
        d.reset(); // caller couldn't start (manual recording running / cooldown)
        assert_eq!(d.step(true, at(t, 8)), None);
        assert_eq!(d.step(true, at(t, 13)), Some(Action::Start));
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

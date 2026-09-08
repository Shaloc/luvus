//! Explicit local appearance changes fan out through the existing control API.
//! This is not config replication: inbound patches and config reloads never
//! fan out, and discovery/reconnect alone never overwrites another owner.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use super::{io_jobs::IoJobs, App};
use crate::session::remote::{self, ConnectionScope, HostStatus, RemoteSession};

struct PreferenceRequest {
    value: String,
    revision: u64,
    hosts: Vec<String>,
}

#[derive(Clone, Copy)]
enum Preference {
    Theme,
    Language,
}

impl Preference {
    fn key(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Language => "language",
        }
    }

    fn state(self, app: &App) -> &RemotePreferenceSync {
        match self {
            Self::Theme => &app.remote_theme_sync,
            Self::Language => &app.remote_language_sync,
        }
    }

    fn state_mut(self, app: &mut App) -> &mut RemotePreferenceSync {
        match self {
            Self::Theme => &mut app.remote_theme_sync,
            Self::Language => &mut app.remote_language_sync,
        }
    }

    fn value(self, app: &App) -> &str {
        match self {
            Self::Theme => &app.config.theme,
            Self::Language => &app.config.language,
        }
    }

    fn revision(self, app: &App) -> u64 {
        match self {
            Self::Theme => app.theme_selection_revision,
            Self::Language => app.remote_language_sync.revision,
        }
    }

    fn done(self, app: &App) -> &'static str {
        match self {
            Self::Theme => app.catalog.settings.theme_sync_done,
            Self::Language => app.catalog.settings.language_sync_done,
        }
    }

    fn failed(self, app: &App) -> &'static str {
        match self {
            Self::Theme => app.catalog.settings.theme_sync_failed,
            Self::Language => app.catalog.settings.language_sync_failed,
        }
    }
}

#[derive(Default)]
pub(super) struct RemotePreferenceSync {
    pub(super) revision: u64,
    // Reuse the bounded app-job executor on a separate lane: SSH deadlines
    // must not delay the local config persistence queue.
    jobs: IoJobs,
    inflight: bool,
    pending: Option<PreferenceRequest>,
    scopes: HashMap<String, Arc<ConnectionScope>>,
}

impl Drop for RemotePreferenceSync {
    fn drop(&mut self) {
        for scope in self.scopes.values() {
            scope.cancel();
        }
    }
}

fn connected_hosts(selected: &[String], statuses: &[HostStatus]) -> Vec<String> {
    statuses
        .iter()
        .filter(|status| {
            selected.contains(&status.host)
                && status.error.is_none()
                && status.sessions.iter().any(|session| session.running)
        })
        .map(|status| status.host.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn sync_host_preference(
    host: &str,
    preference: Preference,
    value: &str,
    scope: &Arc<ConnectionScope>,
) -> Result<(), String> {
    if !scope.wait_for_retry(Duration::ZERO) {
        return Err("remote preference sync cancelled".into());
    }
    // The host was already connected when the user changed the setting. Refresh
    // its inventory off-loop so other running named owners update live too.
    // Listing is read-only; every subsequent request is existing-only.
    let sessions = remote::list_host_sessions(host)?;
    let mut updated = false;
    let mut failures = Vec::new();
    for session in sessions.iter().filter(|session| session.running) {
        if !scope.wait_for_retry(Duration::ZERO) {
            return Err("remote preference sync cancelled".into());
        }
        let result = (|| {
            let target = RemoteSession::new(host, &session.name)?;
            let response = remote::request_control_in_scope(
                &target,
                "config.patch",
                serde_json::json!({"patch": {(preference.key()): value}}),
                Duration::from_secs(3),
                1024 * 1024,
                Arc::clone(scope),
            )?;
            if let Some(error) = response.get("error") {
                return Err(error.to_string());
            }
            if response.get("result").is_none() {
                return Err("missing config response".into());
            }
            Ok(())
        })();
        match result {
            Ok(()) => updated = true,
            Err(error) => failures.push(format!("{}: {error}", session.name)),
        }
    }
    if !failures.is_empty() {
        Err(failures.join("; "))
    } else if updated {
        Ok(())
    } else {
        Err("no running remote sessions".into())
    }
}

impl App {
    pub(super) fn sync_theme_to_connected_hosts(&mut self) {
        self.sync_preference_to_connected_hosts(Preference::Theme);
    }

    pub(super) fn sync_language_to_connected_hosts(&mut self) {
        self.sync_preference_to_connected_hosts(Preference::Language);
    }

    fn sync_preference_to_connected_hosts(&mut self, preference: Preference) {
        let request = PreferenceRequest {
            value: preference.value(self).to_string(),
            revision: preference.revision(self),
            hosts: connected_hosts(&self.config.remote_hosts, &self.remote_host_status),
        };
        preference.state_mut(self).pending = Some(request);
        self.start_pending_preference_sync(preference);
    }

    pub(super) fn cancel_unselected_preference_sync(&mut self) {
        for state in [&mut self.remote_theme_sync, &mut self.remote_language_sync] {
            state.scopes.retain(|host, scope| {
                if self.config.remote_hosts.contains(host) {
                    true
                } else {
                    scope.cancel();
                    false
                }
            });
        }
    }

    fn start_pending_preference_sync(&mut self, preference: Preference) {
        if preference.state(self).inflight {
            return;
        }
        let Some(mut request) = preference.state_mut(self).pending.take() else {
            return;
        };
        if request.revision != preference.revision(self) || request.value != preference.value(self)
        {
            return;
        }
        let connected = connected_hosts(&self.config.remote_hosts, &self.remote_host_status);
        request.hosts.retain(|host| connected.contains(host));
        if request.hosts.is_empty() {
            return;
        }
        preference.state_mut(self).scopes = request
            .hosts
            .iter()
            .map(|host| (host.clone(), Arc::new(ConnectionScope::default())))
            .collect();
        let scopes = preference.state(self).scopes.clone();
        let tx = self.app_tx.clone();
        let accepted = preference.state_mut(self).jobs.submit(tx, move || {
            let mut results = Vec::new();
            for hosts in request.hosts.chunks(4) {
                results.extend(std::thread::scope(|threads| {
                    let jobs: Vec<_> = hosts
                        .iter()
                        .map(|host| {
                            let scope = &scopes[host];
                            let value = &request.value;
                            (
                                host.clone(),
                                threads.spawn(move || {
                                    sync_host_preference(host, preference, value, scope)
                                }),
                            )
                        })
                        .collect();
                    jobs.into_iter()
                        .map(|(host, job)| {
                            (
                                host,
                                job.join().unwrap_or_else(|_| {
                                    Err("remote preference worker failed".into())
                                }),
                            )
                        })
                        .collect::<Vec<_>>()
                }));
            }
            Box::new(move |app| {
                preference.state_mut(app).inflight = false;
                preference.state_mut(app).scopes.clear();
                // Arrow-key theme previews can queue many changes. Keep only
                // the latest pending selection; old receipts cannot report
                // success for a newer selection or a deselected host.
                if request.revision == preference.revision(app)
                    && request.value == preference.value(app)
                {
                    results.retain(|(host, _)| app.config.remote_hosts.contains(host));
                    let failures: Vec<_> = results
                        .iter()
                        .filter_map(|(host, result)| {
                            result
                                .as_ref()
                                .err()
                                .map(|error| format!("{host}: {error}"))
                        })
                        .collect();
                    if !failures.is_empty() {
                        app.show_toast(
                            preference
                                .failed(app)
                                .replace("{error}", &failures.join("; ")),
                        );
                    } else if !results.is_empty() {
                        let hosts = results
                            .iter()
                            .map(|(host, _)| host.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        app.show_toast(preference.done(app).replace("{hosts}", &hosts));
                    }
                }
                app.start_pending_preference_sync(preference);
                true
            })
        });
        match accepted {
            Ok(()) => preference.state_mut(self).inflight = true,
            Err(error) => {
                preference.state_mut(self).scopes.clear();
                self.show_toast(preference.failed(self).replace("{error}", error));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{persist, session::remote::HostSession};

    fn status(host: &str, running: bool, error: Option<&str>) -> HostStatus {
        HostStatus {
            host: host.into(),
            sessions: vec![HostSession {
                name: "default".into(),
                running,
            }],
            error: error.map(str::to_string),
        }
    }

    #[test]
    fn theme_sync_targets_only_selected_connected_running_hosts_once() {
        let selected = ["active", "offline", "stopped"].map(str::to_string);
        let statuses = [
            status("active", true, None),
            status("active", true, None),
            status("offline", true, Some("disconnected")),
            status("stopped", false, None),
            status("unselected", true, None),
        ];
        assert_eq!(connected_hosts(&selected, &statuses), vec!["active"]);
    }

    #[test]
    fn preference_sync_during_host_admission_keeps_already_connected_hosts() {
        let _env = persist::test_env("preference-sync-admission");
        let (tx, _rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.config.remote_hosts = vec!["active".into(), "installing".into()];
        app.remote_host_status = vec![status("active", true, None)];
        app.remote_discovery_inflight = Some(app.remote_registry_generation);
        app.start_merged_remote_sessions();
        app.remote_theme_sync.inflight = true;
        app.remote_language_sync.inflight = true;
        app.apply_theme("one-light");
        app.apply_language_locally("zh");
        app.sync_language_to_connected_hosts();
        for state in [&app.remote_theme_sync, &app.remote_language_sync] {
            assert_eq!(state.pending.as_ref().unwrap().hosts, ["active"]);
        }
    }

    #[test]
    fn language_sync_is_surgical_and_independent_of_pending_theme_changes() {
        let _env = persist::test_env("language-sync");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.config.remote_hosts = vec!["active".into()];
        app.remote_host_status = vec![status("active", true, None)];
        app.remote_theme_sync.inflight = true;
        app.remote_language_sync.inflight = true;
        app.apply_theme("one-light");
        for language in ["ja", "zh"] {
            app.apply_language_locally(language);
            app.sync_language_to_connected_hosts();
        }
        assert_eq!(
            app.remote_theme_sync.pending.as_ref().unwrap().value,
            "one-light"
        );
        let language = app.remote_language_sync.pending.as_ref().unwrap();
        assert_eq!(language.value, "zh");
        assert_eq!(language.revision, app.remote_language_sync.revision);
        app.remote_theme_sync.pending = None;
        app.remote_language_sync.pending = None;
        let generation = app.remote_registry_generation;
        app.file_tree.scroll = 17;
        let mut expected = serde_json::to_value(&app.config).unwrap();
        expected["language"] = "en".into();
        expected["theme"] = "one-dark".into();
        app.dispatch(
            "config.patch",
            &serde_json::json!({"patch":{"language":"en","theme":"one-dark"}}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&app.config).unwrap(), expected);
        assert_eq!(app.remote_registry_generation, generation);
        assert_eq!(app.file_tree.scroll, 17);
        assert!(app.remote_theme_sync.pending.is_none());
        assert!(app.remote_language_sync.pending.is_none());
        assert!(app
            .dispatch(
                "config.patch",
                &serde_json::json!({"patch":{"language":"zh","theme":"not-installed"}})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&app.config).unwrap(), expected);
        app.flush_config_for_test(&rx);
        assert_eq!(crate::config::load().language, "en");
    }

    #[test]
    fn theme_sync_inbound_patch_is_surgical_and_never_rebroadcasts() {
        let _env = persist::test_env("theme-sync-inbound");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.config.remote_hosts = vec!["active".into()];
        app.remote_host_status = vec![status("active", true, None)];
        app.remote_theme_sync.inflight = true;
        app.file_tree.scroll = 17;
        let generation = app.remote_registry_generation;
        let mut expected = serde_json::to_value(&app.config).unwrap();
        expected["theme"] = "one-light".into();
        app.dispatch(
            "config.patch",
            &serde_json::json!({"patch":{"theme":"one-light"}}),
        )
        .unwrap();
        assert_eq!(serde_json::to_value(&app.config).unwrap(), expected);
        assert_eq!(app.file_tree.scroll, 17);
        assert_eq!(app.remote_registry_generation, generation);
        assert!(app.remote_theme_sync.pending.is_none());
        assert!(app
            .dispatch(
                "config.patch",
                &serde_json::json!({"patch":{"theme":"not-installed"}})
            )
            .is_err());
        assert_eq!(serde_json::to_value(&app.config).unwrap(), expected);
        app.flush_config_for_test(&rx);
        assert_eq!(crate::config::load().theme, "one-light");
    }

    #[test]
    fn theme_sync_coalesces_previews_and_cancels_deselected_hosts() {
        let _env = persist::test_env("theme-sync-queue");
        let (tx, rx) = std::sync::mpsc::channel();
        let mut app = App::new(80, 24, tx).unwrap();
        app.config.remote_hosts = vec!["active".into()];
        app.remote_host_status = vec![status("active", true, None)];
        // Hold the queue without creating any network worker in this unit test.
        app.remote_theme_sync.inflight = true;
        app.apply_theme("one-dark");
        app.apply_theme("one-light");
        let pending = app.remote_theme_sync.pending.as_ref().unwrap();
        assert_eq!(pending.value, "one-light");
        assert_eq!(pending.revision, app.theme_selection_revision);
        assert_eq!(pending.hosts, vec!["active"]);
        let scope = Arc::new(ConnectionScope::default());
        app.remote_theme_sync
            .scopes
            .insert("active".into(), scope.clone());
        app.config.remote_hosts.clear();
        app.cancel_unselected_preference_sync();
        assert!(!scope.wait_for_retry(Duration::ZERO));
        app.remote_theme_sync.inflight = false;
        app.start_pending_preference_sync(Preference::Theme);
        assert!(app.remote_theme_sync.pending.is_none());
        assert!(!app.remote_theme_sync.inflight);
        app.flush_config_for_test(&rx);
    }
}

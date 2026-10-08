//! Drain remote topology events independently of slow SSH snapshot requests.
//! Keep one sequence watermark, not a queue of redundant full refreshes.

use std::io::{BufRead, BufReader, Write};
use std::sync::{mpsc, Condvar, Mutex};

use serde_json::{json, Value};

use super::{remote_snapshot_at, RemoteDisplay, RemoteSessionSnapshot, REMOTE_RESPONSE_TIMEOUT};
use crate::event::AppEvent;
use crate::session::remote::{self, ConnectionScope, RemoteSession};

#[derive(Default)]
struct PendingEvents {
    sequence: u64,
    resync: bool,
    error: Option<String>,
}

#[derive(Default)]
struct SessionEvents {
    pending: Mutex<PendingEvents>,
    ready: Condvar,
}

#[derive(Debug, PartialEq)]
enum Refresh {
    Snapshot,
    Resubscribe,
}

impl SessionEvents {
    fn record(&self, sequence: u64, resync: bool) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.sequence = pending.sequence.max(sequence);
            pending.resync |= resync;
            self.ready.notify_one();
        }
    }

    fn fail(&self, error: String) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.error = Some(error);
            self.ready.notify_one();
        }
    }

    fn next(&self, covered: u64) -> Result<Refresh, String> {
        let pending = self
            .pending
            .lock()
            .map_err(|_| "remote event state lock failed")?;
        let mut pending = self
            .ready
            .wait_while(pending, |pending| {
                pending.sequence <= covered && !pending.resync && pending.error.is_none()
            })
            .map_err(|_| "remote event state lock failed")?;
        // A closed stream must not leave us fetching every queued old event
        // before exposing the error. Resync ends the reader deliberately.
        if let Some(error) = pending.error.take() {
            return Err(error);
        }
        Ok(if pending.resync {
            Refresh::Resubscribe
        } else {
            Refresh::Snapshot
        })
    }
}

fn affects_snapshot(name: &str) -> bool {
    matches!(
        name,
        "workspace.created"
            | "agent.history_changed"
            | "agent.title_changed"
            | "agent.pin_changed"
            | "automation.created"
            | "automation.updated"
            | "automation.rebound"
            | "automation.enabled"
            | "automation.disabled"
            | "automation.deleted"
            | "automation.run_queued"
            | "automation.run_materialized"
            | "automation.run_started"
            | "automation.run_finished"
            | "automation.run_failed"
            | "automation.run_updated"
            | "task.updated"
            | "workspace.closed"
            | "workspace.renamed"
            | "workspace.metadata_reported"
            | "pane.agent_status_changed"
            | "pane.created"
            | "pane.closed"
            | "pane.moved"
            | "pane.renamed"
            | "pane.swapped"
            | "pane.focused"
            | "tab.created"
            | "tab.closed"
            | "tab.renamed"
            | "tab.focused"
            | "terminal.created"
            | "terminal.metadata_changed"
            | "terminal.moved"
    )
}

fn read_session_events(reader: &mut impl BufRead, events: &SessionEvents) -> Result<(), String> {
    loop {
        let line = crate::ipc::api::read_stream_frame(reader)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "remote event subscription closed".to_string())?;
        let event: Value = serde_json::from_str(&line).map_err(|error| error.to_string())?;
        let name = event
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let resync = name == "events.resync_required";
        if resync || affects_snapshot(name) {
            let sequence = event
                .get("sequence")
                .and_then(Value::as_u64)
                .ok_or_else(|| "remote event is missing its sequence".to_string())?;
            events.record(sequence, resync);
            if resync {
                return Ok(());
            }
        }
    }
}

/// Used for both subscription-ACK resync and normal event-driven refreshes.
fn refresh_snapshot(
    owner: &Option<String>,
    load: impl FnOnce() -> Result<RemoteSessionSnapshot, String>,
    publish: impl FnOnce(RemoteSessionSnapshot) -> Result<(), String>,
) -> Result<u64, String> {
    let snapshot = load()?;
    if *owner != snapshot.display.server_generation {
        // Watcher recovery refreshes every display's owner fence, including
        // legacy per-workspace channels. Resubscribing only to events would
        // leave those channels rejecting all frames from the new owner.
        return Err("remote session owner changed".into());
    }
    let covered = snapshot.event_sequence;
    publish(snapshot)?;
    Ok(covered)
}

/// A fresh snapshot covers all earlier events, including ones received while
/// SSH was responding. A later event remains pending for the next refresh.
fn refresh_snapshots(
    events: &SessionEvents,
    mut covered: u64,
    owner: &Option<String>,
    mut load: impl FnMut() -> Result<RemoteSessionSnapshot, String>,
    mut publish: impl FnMut(RemoteSessionSnapshot) -> Result<(), String>,
) -> Result<u64, String> {
    loop {
        let action = events.next(covered)?;
        covered = refresh_snapshot(owner, &mut load, &mut publish)?;
        if action == Refresh::Resubscribe {
            return Ok(covered);
        }
    }
}

pub(super) fn watch_remote_session(
    target: &RemoteSession,
    display: &RemoteDisplay,
    mut after_sequence: u64,
    generation: u64,
    scope: &ConnectionScope,
    app_tx: &mpsc::Sender<AppEvent>,
) -> Result<(), String> {
    let location = display.location;
    let owner = display.server_generation.clone();
    let publish = |snapshot| {
        app_tx
            .send(AppEvent::RemoteSessionDiscovered {
                generation,
                target: target.clone(),
                result: Ok(snapshot),
            })
            .map_err(|_| "local session closed".to_string())
    };
    loop {
        // The extra scope cancels only this subscription when a refresh fails
        // or needs resubscription. The existing parent scope still owns user
        // cancellation and the generation fence for the whole remote session.
        let subscription_scope = ConnectionScope::default();
        let mut connection = remote::connect_control_at(target, location)?
            .in_scope(scope)?
            .in_scope(&subscription_scope)?;
        let deadline = connection.deadline(REMOTE_RESPONSE_TIMEOUT);
        writeln!(
            connection,
            "{}",
            json!({
                "id":"remote-session-events", "method":"events.subscribe",
                "params":{"after_sequence":after_sequence},
            })
        )
        .map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(connection);
        let response = crate::ipc::api::read_response_frame(&mut reader)
            .map_err(|error| deadline.error("remote event subscription", error))?;
        drop(deadline);
        let response: Value = serde_json::from_str(&response).map_err(|error| error.to_string())?;
        if let Some(error) = response.get("error") {
            if error.get("code").and_then(Value::as_str) == Some("resync_required") {
                after_sequence = refresh_snapshot(
                    &owner,
                    || remote_snapshot_at(target, location, scope),
                    publish,
                )?;
                continue;
            }
            return Err(format!("remote event subscription failed: {error}"));
        }

        let events = SessionEvents::default();
        after_sequence = std::thread::scope(|workers| {
            std::thread::Builder::new()
                .name("luvus-remote-events".into())
                .spawn_scoped(workers, || {
                    if let Err(error) = read_session_events(&mut reader, &events) {
                        events.fail(error);
                    }
                })
                .map_err(|error| format!("could not start remote event reader: {error}"))?;
            let result = refresh_snapshots(
                &events,
                after_sequence,
                &owner,
                || remote_snapshot_at(target, location, scope),
                publish,
            );
            // Wake and join the blocking reader on every normal/error return;
            // no reader or its SSH child may outlive this subscription.
            subscription_scope.cancel();
            result
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::time::Duration;

    fn snapshot(sequence: u64, owner: &str) -> RemoteSessionSnapshot {
        RemoteSessionSnapshot {
            event_sequence: sequence,
            workspaces: Vec::new(),
            display: RemoteDisplay {
                server_generation: Some(owner.into()),
                ..Default::default()
            },
        }
    }

    #[cfg(unix)]
    #[test]
    fn slow_snapshot_does_not_block_event_reads_or_replay_covered_refreshes() {
        use std::net::Shutdown;
        use std::os::unix::net::UnixStream;

        let (mut writer, reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let events = SessionEvents::default();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (published_tx, published_rx) = mpsc::channel();
        std::thread::scope(|workers| {
            workers.spawn(|| {
                if let Err(error) = read_session_events(&mut BufReader::new(reader), &events) {
                    events.fail(error);
                }
            });
            let events = &events;
            let refresh = workers.spawn(move || {
                let mut calls = 0;
                refresh_snapshots(
                    events,
                    0,
                    &Some("boot".into()),
                    || {
                        calls += 1;
                        started_tx.send(calls).unwrap();
                        if calls == 1 {
                            release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                            Ok(snapshot(100, "boot"))
                        } else {
                            assert_eq!(calls, 2, "covered events must not reopen SSH");
                            Ok(snapshot(101, "boot"))
                        }
                    },
                    |snapshot| {
                        published_tx.send(snapshot.event_sequence).unwrap();
                        if snapshot.event_sequence == 101 {
                            Err("fixture complete".into())
                        } else {
                            Ok(())
                        }
                    },
                )
            });
            writeln!(writer, "{}", json!({"event":"pane.renamed","sequence":1})).unwrap();
            assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
            for sequence in 2..=100 {
                writeln!(
                    writer,
                    "{}",
                    json!({"event":"pane.renamed","sequence":sequence})
                )
                .unwrap();
            }
            // The real reader must consume the whole burst while the snapshot
            // loader is still parked. No event-sized queue grows behind it.
            let (pending, timeout) = events
                .ready
                .wait_timeout_while(
                    events.pending.lock().unwrap(),
                    Duration::from_secs(2),
                    |pending| pending.sequence < 100,
                )
                .unwrap();
            assert!(!timeout.timed_out());
            assert_eq!(pending.sequence, 100);
            drop(pending);
            release_tx.send(()).unwrap();
            assert_eq!(
                published_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                100
            );
            assert!(matches!(
                started_rx.try_recv(),
                Err(mpsc::TryRecvError::Empty)
            ));
            writeln!(
                writer,
                "{}",
                json!({"event":"agent.history_changed","sequence":101})
            )
            .unwrap();
            assert_eq!(started_rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
            assert_eq!(
                published_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                101
            );
            assert_eq!(refresh.join().unwrap().unwrap_err(), "fixture complete");
            writer.shutdown(Shutdown::Both).unwrap();
        });
    }

    #[test]
    fn resync_replaces_the_subscription_sequence() {
        let events = SessionEvents::default();
        events.record(40, true);
        let mut published = Vec::new();
        let fence = refresh_snapshots(
            &events,
            30,
            &Some("old".into()),
            || Ok(snapshot(50, "old")),
            |snapshot| {
                published.push(snapshot.event_sequence);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(fence, 50);
        assert_eq!(published, [50]);
    }

    #[test]
    fn snapshot_owner_change_requests_full_watcher_recovery() {
        for stream_closed in [false, true] {
            let events = SessionEvents::default();
            events.record(40, false);
            let result = refresh_snapshots(
                &events,
                30,
                &Some("old".into()),
                || {
                    if stream_closed {
                        events.fail("old owner stream closed while loading snapshot".into());
                    }
                    Ok(snapshot(2, "new"))
                },
                |_| panic!("new owner metadata needs full watcher recovery first"),
            );
            // WatcherClosed owns resetting legacy per-workspace display metadata,
            // too. Simply resubscribing leaves its old server-generation fence.
            assert_eq!(result.unwrap_err(), "remote session owner changed");
        }
    }

    #[test]
    fn ack_resync_snapshot_checks_owner_before_publication() {
        let owner = Some("old".into());
        for new_owner in ["old", "new"] {
            let mut published = Vec::new();
            let result = refresh_snapshot(
                &owner,
                || Ok(snapshot(50, new_owner)),
                |snapshot| {
                    published.push(snapshot.event_sequence);
                    Ok(())
                },
            );
            if new_owner == "old" {
                assert_eq!(result.unwrap(), 50);
                assert_eq!(published, [50]);
            } else {
                assert_eq!(result.unwrap_err(), "remote session owner changed");
                assert!(published.is_empty());
            }
        }
    }

    #[test]
    fn closed_subscription_reports_error_before_fetching_stale_events() {
        let events = SessionEvents::default();
        events.record(100, false);
        events.fail("event frame read failed: SSH fixture closed".into());
        let error = refresh_snapshots(
            &events,
            0,
            &None,
            || panic!("closed subscription must not drain old snapshot requests"),
            |_| Ok(()),
        )
        .unwrap_err();
        assert_eq!(error, "event frame read failed: SSH fixture closed");
    }

    #[test]
    fn reader_skips_output_events_but_retains_status_resync_and_protocol_errors() {
        let events = SessionEvents::default();
        let mut wire = std::io::Cursor::new(
            b"{\"event\":\"terminal.output_ready\",\"sequence\":100}\n\
              {\"event\":\"agent.title_changed\",\"sequence\":101}\n\
              {\"event\":\"events.resync_required\",\"sequence\":102}\n"
                .to_vec(),
        );
        read_session_events(&mut wire, &events).unwrap();
        let pending = events.pending.lock().unwrap();
        assert_eq!(pending.sequence, 102);
        assert!(pending.resync);
        drop(pending);
        assert_eq!(events.next(102).unwrap(), Refresh::Resubscribe);
        for wire in ["{bad}\n", "{\"event\":\"pane.renamed\"}\n", "partial"] {
            assert!(read_session_events(
                &mut std::io::Cursor::new(wire),
                &SessionEvents::default()
            )
            .is_err());
        }
    }
}

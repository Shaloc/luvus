//! Bounded display input. The app only queues messages; the existing SSH writer
//! owns encoding and writes. A sleeping deadline guard interrupts a stuck pipe
//! on every platform without polling healthy/idle connections.

use std::io::{self, Write};
use std::process::Child;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::ipc::protocol::{self, ClientMessage};

const MAX_MESSAGES: usize = 256;
const MAX_BYTES: usize = 32 * 1024 * 1024;
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);

struct Queued {
    message: ClientMessage,
    bytes: usize,
    budget: Arc<AtomicUsize>,
}

impl Drop for Queued {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Default)]
struct WriteState {
    deadline: Mutex<(bool, Option<Instant>)>,
    wake: Condvar,
    failed: AtomicBool,
    closing: AtomicBool,
}

impl WriteState {
    fn stop(&self) {
        if let Ok(mut state) = self.deadline.lock() {
            state.0 = true;
            self.wake.notify_all();
        }
    }
}

type Failure = Arc<dyn Fn(String) + Send + Sync>;

pub struct RemoteInput {
    sender: mpsc::SyncSender<Queued>,
    budget: Arc<AtomicUsize>,
    state: Arc<WriteState>,
    failure: Failure,
    #[cfg(test)]
    observer: Option<mpsc::Sender<ClientMessage>>,
}

impl RemoteInput {
    pub(crate) fn spawn(
        writer: impl Write + Send + 'static,
        child: Arc<Mutex<Child>>,
        report: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        Self::with_writer(writer, WRITE_TIMEOUT, move |error| {
            report(error);
            if let Ok(mut child) = child.lock() {
                let _ = child.kill();
            }
        })
    }

    fn with_writer(
        mut writer: impl Write + Send + 'static,
        timeout: Duration,
        failure: impl Fn(String) + Send + Sync + 'static,
    ) -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Queued>(MAX_MESSAGES);
        let budget = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(WriteState::default());
        let failure: Failure = Arc::new(failure);
        let guard = state.clone();
        let timed_out = failure.clone();
        std::thread::spawn(move || {
            let Ok(mut deadline) = guard.deadline.lock() else {
                return;
            };
            loop {
                if deadline.0 {
                    return;
                }
                if let Some(at) = deadline.1 {
                    if Instant::now() >= at {
                        drop(deadline);
                        if !guard.failed.swap(true, Ordering::AcqRel) {
                            timed_out("SSH display input write timed out".into());
                        }
                        return;
                    }
                    let Ok((next, _)) = guard
                        .wake
                        .wait_timeout(deadline, at.saturating_duration_since(Instant::now()))
                    else {
                        return;
                    };
                    deadline = next;
                } else {
                    let Ok(next) = guard.wake.wait(deadline) else {
                        return;
                    };
                    deadline = next;
                }
            }
        });
        let writing = state.clone();
        let write_failed = failure.clone();
        std::thread::spawn(move || {
            let mut detached = false;
            for queued in receiver {
                if writing.closing.load(Ordering::Acquire) || writing.failed.load(Ordering::Acquire)
                {
                    break;
                }
                if !write_bounded(
                    &mut writer,
                    &queued.message,
                    &writing,
                    timeout,
                    &write_failed,
                ) {
                    break;
                }
                if matches!(queued.message, ClientMessage::Detach) {
                    detached = true;
                    break;
                }
            }
            // Dropping a view (including a stale Ready result) must detach its
            // owner. Discard queued ordinary input, but keep the write deadline
            // alive for an in-flight write and this final Detach.
            if !detached && !writing.failed.load(Ordering::Acquire) {
                write_bounded(
                    &mut writer,
                    &ClientMessage::Detach,
                    &writing,
                    timeout,
                    &write_failed,
                );
            }
            writing.stop();
        });
        Self {
            sender,
            budget,
            state,
            failure,
            #[cfg(test)]
            observer: None,
        }
    }

    pub fn send(&self, message: ClientMessage) -> io::Result<()> {
        #[cfg(test)]
        if let Some(observer) = &self.observer {
            return observer
                .send(message)
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe));
        }
        if self.state.failed.load(Ordering::Acquire) {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        // Upper bound for the owned payload plus its framing/enum overhead.
        // Encoding stays off the app loop, including large image pastes.
        let payload = match &message {
            ClientMessage::Paste(text) | ClientMessage::Command(text) => text.len(),
            ClientMessage::HelloWorkspace { workspace_id, .. }
            | ClientMessage::HelloProjection { workspace_id, .. } => workspace_id.len(),
            ClientMessage::ClipboardImage(image) => {
                image.bytes.len().saturating_add(image.extension.len())
            }
            ClientMessage::ClipboardHelperResult { generation, result } => {
                generation.len().saturating_add(match result {
                    Err(error) => error.len(),
                    Ok(crate::terminal::clipboard::kitten::Status::Installed(version)) => {
                        version.len()
                    }
                    _ => 0,
                })
            }
            _ => 0,
        };
        let bytes = payload.saturating_add(256);
        if self
            .budget
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|total| *total <= MAX_BYTES)
            })
            .is_err()
        {
            return Err(self.queue_failed());
        }
        let queued = Queued {
            message,
            bytes,
            budget: self.budget.clone(),
        };
        self.sender
            .try_send(queued)
            .map_err(|_| self.queue_failed())
    }

    fn queue_failed(&self) -> io::Error {
        if !self.state.failed.swap(true, Ordering::AcqRel) {
            (self.failure)(
                "SSH display input queue is full or closed; input was not replayed".into(),
            );
        }
        self.state.stop();
        io::Error::new(io::ErrorKind::ConnectionAborted, "remote input unavailable")
    }
}

impl Drop for RemoteInput {
    fn drop(&mut self) {
        self.state.closing.store(true, Ordering::Release);
    }
}

fn write_bounded(
    writer: &mut impl Write,
    message: &ClientMessage,
    state: &WriteState,
    timeout: Duration,
    failure: &Failure,
) -> bool {
    if let Ok(mut deadline) = state.deadline.lock() {
        if deadline.0 || state.failed.load(Ordering::Acquire) {
            return false;
        }
        deadline.1 = Some(Instant::now() + timeout);
        state.wake.notify_all();
    }
    let result = protocol::write_message(writer, message);
    if let Ok(mut deadline) = state.deadline.lock() {
        deadline.1 = None;
        state.wake.notify_all();
    }
    if let Err(error) = result {
        if !state.failed.swap(true, Ordering::AcqRel) {
            failure(format!("SSH display input failed: {error}"));
        }
        return false;
    }
    !state.failed.load(Ordering::Acquire)
}

#[cfg(test)]
impl From<mpsc::Sender<ClientMessage>> for RemoteInput {
    fn from(observer: mpsc::Sender<ClientMessage>) -> Self {
        let (sender, _) = mpsc::sync_channel(1);
        Self {
            sender,
            budget: Arc::default(),
            state: Arc::default(),
            failure: Arc::new(|_| {}),
            observer: Some(observer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_idle_input_emits_detach_even_without_a_view() {
        struct Capture(mpsc::Sender<Vec<u8>>);
        impl Write for Capture {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (tx, rx) = mpsc::channel();
        let input = RemoteInput::with_writer(Capture(tx), WRITE_TIMEOUT, |_| {});
        drop(input);
        let mut bytes = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("drop must detach the owner");
        bytes.extend(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        assert!(matches!(
            protocol::read_message::<_, ClientMessage>(&mut bytes.as_slice()).unwrap(),
            ClientMessage::Detach
        ));
    }

    #[test]
    fn dropping_input_discards_queued_keys_but_finishes_a_bounded_detach() {
        struct Gated {
            started: mpsc::Sender<()>,
            release: Option<mpsc::Receiver<()>>,
            done: mpsc::Sender<Vec<u8>>,
            bytes: Vec<u8>,
        }
        impl Write for Gated {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if let Some(release) = self.release.take() {
                    self.started.send(()).unwrap();
                    release.recv().unwrap();
                }
                self.bytes.extend(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        impl Drop for Gated {
            fn drop(&mut self) {
                let _ = self.done.send(std::mem::take(&mut self.bytes));
            }
        }
        let (started, ready) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (done, result) = mpsc::channel();
        let input = RemoteInput::with_writer(
            Gated {
                started,
                release: Some(blocked),
                done,
                bytes: Vec::new(),
            },
            WRITE_TIMEOUT,
            |_| {},
        );
        input
            .send(ClientMessage::Paste("in flight".into()))
            .unwrap();
        ready.recv_timeout(Duration::from_secs(1)).unwrap();
        input
            .send(ClientMessage::Paste("queued, must not replay".into()))
            .unwrap();
        drop(input);
        release.send(()).unwrap();
        let bytes = result.recv_timeout(Duration::from_secs(1)).unwrap();
        let mut bytes = bytes.as_slice();
        assert!(
            matches!(protocol::read_message::<_, ClientMessage>(&mut bytes).unwrap(), ClientMessage::Paste(text) if text == "in flight")
        );
        assert!(matches!(
            protocol::read_message::<_, ClientMessage>(&mut bytes).unwrap(),
            ClientMessage::Detach
        ));
        assert!(bytes.is_empty());
    }

    #[test]
    fn dropping_input_keeps_the_stalled_write_deadline_alive() {
        struct Stalled(mpsc::Receiver<()>);
        impl Write for Stalled {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                let _ = self.0.recv();
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (release, blocked) = mpsc::channel();
        let (failure, failed) = mpsc::channel();
        let input =
            RemoteInput::with_writer(Stalled(blocked), Duration::from_millis(30), move |error| {
                let _ = failure.send(error);
            });
        // Even a final Detach can stall. Teardown must still cancel its child.
        drop(input);
        assert!(failed
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .contains("timed out"));
        drop(release);
    }

    #[test]
    fn remote_input_queue_is_bounded_and_never_flushes_queued_input_after_failure() {
        struct Gated {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
            writes: Arc<AtomicUsize>,
        }
        impl Write for Gated {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.writes.fetch_add(1, Ordering::Relaxed);
                let _ = self.entered.send(());
                let _ = self.release.recv();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (entered, started) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (failure, failed) = mpsc::channel();
        let writes = Arc::new(AtomicUsize::new(0));
        let input = RemoteInput::with_writer(
            Gated {
                entered,
                release: blocked,
                writes: writes.clone(),
            },
            Duration::from_secs(10),
            move |error| {
                let _ = failure.send(error);
            },
        );
        input.send(ClientMessage::Paste("first".into())).unwrap();
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        for _ in 0..MAX_MESSAGES {
            input.send(ClientMessage::Paste("queued".into())).unwrap();
        }
        assert!(input.send(ClientMessage::Paste("overflow".into())).is_err());
        assert!(failed
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .contains("queue"));
        drop(release);
        drop(input);
        // Only the in-flight frame may finish. No queued request can be replayed.
        for _ in 0..2 {
            let _ = started.recv_timeout(Duration::from_secs(1));
        }
        assert!(
            writes.load(Ordering::Relaxed) <= 2,
            "framing header + one payload only"
        );
    }

    #[test]
    fn oversized_input_fails_closed_without_replay() {
        let (failure, failed) = mpsc::channel();
        let input = RemoteInput::with_writer(io::sink(), WRITE_TIMEOUT, move |error| {
            let _ = failure.send(error);
        });
        assert!(input
            .send(ClientMessage::Paste("x".repeat(MAX_BYTES)))
            .is_err());
        assert!(failed
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .contains("queue"));
        assert!(input
            .send(ClientMessage::Paste("must not replay".into()))
            .is_err());
        assert_eq!(input.budget.load(Ordering::Acquire), 0);
    }

    #[test]
    fn stalled_writer_expires_but_an_idle_writer_does_not() {
        struct Stalled(mpsc::Receiver<()>);
        impl Write for Stalled {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                let _ = self.0.recv();
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (release, blocked) = mpsc::channel();
        let (failure, failed) = mpsc::channel();
        let input =
            RemoteInput::with_writer(Stalled(blocked), Duration::from_millis(30), move |error| {
                let _ = failure.send(error);
            });
        assert!(failed.recv_timeout(Duration::from_millis(60)).is_err());
        input.send(ClientMessage::Paste("bounded".into())).unwrap();
        assert!(failed
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .contains("timed out"));
        let _ = release.send(());
        assert!(input
            .send(ClientMessage::Paste("no replay".into()))
            .is_err());
    }
}

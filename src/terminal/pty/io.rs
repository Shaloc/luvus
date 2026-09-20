use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::mpsc;
#[cfg(unix)]
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

use portable_pty::MasterPty;

use crate::event::AppEvent;
use crate::ids::PaneId;
use crate::terminal::vt::VtEngine;

use super::{input::QueuedInput, InputSender};

#[cfg(windows)]
mod split;
#[cfg(unix)]
pub(super) mod unix_actor;

/// Preserve the child and pane identity if terminal processing fails. The
/// poisoned engine must not be reused: report the failure on the app loop so
/// the UI can replace a silently blank pane with an explicit error.
fn run_guarded(id: PaneId, app_tx: mpsc::Sender<AppEvent>, run: impl FnOnce()) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).is_err() {
        let _ = app_tx.send(AppEvent::PtyThreadFailed(id));
    }
}

pub(super) struct InputReceiver {
    receiver: mpsc::Receiver<QueuedInput>,
    #[cfg(unix)]
    wake: Arc<OnceLock<Arc<unix_actor::WakePipe>>>,
}

pub(super) fn input_channel() -> (InputSender, InputReceiver) {
    let (sender, receiver) = InputSender::channel();
    #[cfg(unix)]
    {
        let wake = sender.wake_slot();
        (sender, InputReceiver { receiver, wake })
    }
    #[cfg(windows)]
    {
        (sender, InputReceiver { receiver })
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn start(
    id: PaneId,
    master: &(dyn MasterPty + Send),
    input: InputReceiver,
    engine: Arc<Mutex<dyn VtEngine>>,
    app_tx: mpsc::Sender<AppEvent>,
    data_pending: Arc<AtomicBool>,
    content_revision: Arc<AtomicU64>,
    _cancelled: Arc<AtomicBool>,
) -> io::Result<()> {
    #[cfg(unix)]
    {
        unix_actor::start(
            id,
            master,
            input.receiver,
            input.wake,
            engine,
            app_tx,
            data_pending,
            content_revision,
            _cancelled,
        )
    }
    #[cfg(windows)]
    {
        split::start(
            id,
            master,
            input.receiver,
            engine,
            app_tx,
            data_pending,
            content_revision,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_panic_reports_failure_without_reporting_child_exit() {
        let (tx, rx) = mpsc::channel();
        let engine = Mutex::new(());
        let id = PaneId::alloc();
        run_guarded(id, tx, || {
            let _guard = engine.lock().unwrap();
            panic!("injected terminal failure");
        });
        assert!(engine.is_poisoned());
        assert!(matches!(rx.try_recv(), Ok(AppEvent::PtyThreadFailed(pane)) if pane == id));
        assert!(rx.try_recv().is_err());
    }
}

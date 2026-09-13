//! Owned recovery operation and completion-delivery lifecycle.

use super::SupervisorError;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
};

pub(super) struct Worker {
    completed: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl Worker {
    pub(super) fn spawn<T: Send + 'static>(
        name: &str,
        operation: impl FnOnce() -> T + Send + 'static,
        complete: impl FnOnce(T) + Send + 'static,
    ) -> Result<Self, SupervisorError> {
        let completed = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&completed);
        let thread = thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let result = operation();
                // The owner can receive the callback before this thread exits.
                // Storage work and its locks are finished before delivery; the
                // owner still joins this thread before reuse or disposal.
                flag.store(true, Ordering::Release);
                complete(result);
            })
            .map_err(|_| SupervisorError::WorkerUnavailable)?;
        Ok(Self { completed, thread })
    }

    pub(super) fn is_finished(&self) -> bool {
        // A panicked operation must reach join() and report cleanup failure.
        self.completed.load(Ordering::Acquire) || self.thread.is_finished()
    }

    pub(super) fn join(self) -> Result<(), SupervisorError> {
        self.thread
            .join()
            .map_err(|_| SupervisorError::CleanupUnproven)
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Fixtures assert worker ordering and panic propagation."
)]
mod tests {
    use super::*;
    use crate::launch_supervisor::recovery::RecoveryError;
    use std::{sync::mpsc, time::Duration};

    #[test]
    fn completion_is_visible_before_callback_and_thread_exit() {
        let (release_operation, operation_gate) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let (release_callback, callback_gate) = mpsc::channel();
        let worker = Worker::spawn(
            "recovery-ordering-test",
            move || {
                operation_gate.recv_timeout(Duration::from_secs(5)).unwrap();
                Err::<(), _>(RecoveryError::Expired)
            },
            move |result| {
                completed.send(result).unwrap();
                // Hold this worker alive after delivering the result: CI may
                // deschedule it at exactly this point while a new request arrives.
                callback_gate.recv_timeout(Duration::from_secs(5)).unwrap();
            },
        )
        .unwrap();
        assert!(
            !worker.is_finished(),
            "active storage work must remain exclusive"
        );
        release_operation.send(()).unwrap();
        assert!(matches!(
            completion.recv_timeout(Duration::from_secs(5)).unwrap(),
            Err(RecoveryError::Expired)
        ));
        let ready = worker.is_finished();
        assert!(
            !worker.thread.is_finished(),
            "callback gate must hold the thread alive"
        );
        release_callback.send(()).unwrap();
        worker.join().unwrap();
        assert!(
            ready,
            "delivered completion must admit the next recovery request"
        );
    }

    #[test]
    fn callback_panic_does_not_turn_completion_into_proven_cleanup() {
        let worker = Worker::spawn(
            "recovery-callback-panic",
            || (),
            |()| panic!("callback failed"),
        )
        .unwrap();
        assert_eq!(worker.join(), Err(SupervisorError::CleanupUnproven));
    }

    #[test]
    fn operation_panic_does_not_deliver_success_or_prove_cleanup() {
        let (sender, receiver) = mpsc::channel();
        let worker = Worker::spawn(
            "recovery-operation-panic",
            || panic!("operation failed"),
            move |()| {
                sender.send(()).unwrap();
            },
        )
        .unwrap();
        assert_eq!(worker.join(), Err(SupervisorError::CleanupUnproven));
        assert!(receiver.try_recv().is_err());
    }
}

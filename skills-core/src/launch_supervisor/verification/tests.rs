//! Completion ordering and sticky cleanup failure at the worker boundary.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Fixtures force worker ordering and assert cleanup and panic propagation."
)]

use super::*;
use std::sync::mpsc;

#[test]
fn completion_releases_busy_state_before_callback_returns() {
    completion_ordering(false);
}

#[test]
fn failed_cleanup_is_visible_before_completion_and_stays_sticky() {
    completion_ordering(true);
}

fn completion_ordering(failed_cleanup: bool) {
    let (release_operation, operation_gate) = mpsc::channel();
    let (sender, receiver) = mpsc::channel();
    let (release_callback, callback_gate) = mpsc::channel();
    let cleanup_failed = Arc::new(AtomicBool::new(false));
    let cleanup_flag = Arc::clone(&cleanup_failed);
    let completed = Arc::new(AtomicBool::new(false));
    let completion_flag = Arc::clone(&completed);
    let error = if failed_cleanup {
        SupervisorError::CleanupUnproven
    } else {
        SupervisorError::AuthorizationRejected
    };
    let expected_error = error.clone();
    let mut worker = Worker {
        cancelled: Arc::new(AtomicBool::new(false)),
        completed,
        start_gate: Arc::new(Mutex::new(())),
        cleanup_failed,
        thread: Some(thread::spawn(move || {
            operation_gate.recv_timeout(Duration::from_secs(5)).unwrap();
            finish(
                Err(error),
                &cleanup_flag,
                &completion_flag,
                Box::new(move |result| {
                    sender.send(result).unwrap();
                    // Force the scheduling window: the consumer sees completion
                    // while this thread remains inside the callback.
                    callback_gate.recv_timeout(Duration::from_secs(5)).unwrap();
                }),
            )
        })),
    };
    assert!(
        !worker.finished(),
        "active verification must remain exclusive"
    );
    release_operation.send(()).unwrap();
    let result = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    let ready = worker.finished();
    let cleanup_visible = worker.cleanup_failed.load(Ordering::Acquire);
    let thread_finished = worker.thread.as_ref().unwrap().is_finished();
    release_callback.send(()).unwrap();
    let expected_cleanup = if failed_cleanup {
        Err(SupervisorError::CleanupUnproven)
    } else {
        Ok(())
    };
    assert_eq!(worker.cancel(), expected_cleanup);
    assert_eq!(worker.cancel(), expected_cleanup);
    assert_eq!(result, Err(expected_error));
    assert_eq!(cleanup_visible, failed_cleanup);
    assert!(!thread_finished, "callback gate must hold the worker alive");
    assert!(
        ready,
        "a delivered result must not leave verification falsely busy"
    );
}

#[test]
fn repeated_cancellation_cannot_forget_unproven_cleanup() {
    let mut worker = Worker {
        cancelled: Arc::new(AtomicBool::new(false)),
        completed: Arc::new(AtomicBool::new(false)),
        start_gate: Arc::new(Mutex::new(())),
        cleanup_failed: Arc::new(AtomicBool::new(false)),
        thread: Some(thread::spawn(|| Err(SupervisorError::CleanupUnproven))),
    };
    assert_eq!(worker.cancel(), Err(SupervisorError::CleanupUnproven));
    assert_eq!(worker.cancel(), Err(SupervisorError::CleanupUnproven));
}

#[test]
fn callback_panic_preserves_cleanup_failure_after_completion() {
    let cleanup_failed = Arc::new(AtomicBool::new(false));
    let cleanup_flag = Arc::clone(&cleanup_failed);
    let completed = Arc::new(AtomicBool::new(false));
    let completion_flag = Arc::clone(&completed);
    let mut worker = Worker {
        cancelled: Arc::new(AtomicBool::new(false)),
        completed,
        start_gate: Arc::new(Mutex::new(())),
        cleanup_failed,
        thread: Some(thread::spawn(move || {
            finish(
                Err(SupervisorError::AuthorizationRejected),
                &cleanup_flag,
                &completion_flag,
                Box::new(|_| panic!("callback failed")),
            )
        })),
    };
    assert_eq!(worker.cancel(), Err(SupervisorError::CleanupUnproven));
    assert!(worker.finished());
    assert_eq!(worker.cancel(), Err(SupervisorError::CleanupUnproven));
}

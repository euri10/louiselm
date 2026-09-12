//! Cleanup failure must remain sticky across every later lifecycle operation.

use super::*;

#[test]
fn repeated_cancellation_cannot_forget_unproven_cleanup() {
    let mut worker = Worker {
        cancelled: Arc::new(AtomicBool::new(false)),
        start_gate: Arc::new(Mutex::new(())),
        cleanup_failed: Arc::new(AtomicBool::new(false)),
        thread: Some(thread::spawn(|| Err(SupervisorError::CleanupUnproven))),
    };
    assert_eq!(worker.cancel(), Err(SupervisorError::CleanupUnproven));
    assert_eq!(worker.cancel(), Err(SupervisorError::CleanupUnproven));
}

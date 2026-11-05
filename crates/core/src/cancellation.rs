use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::{CoreError, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelOutcome {
    Accepted,
    TooLate,
    Finished,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationStatus {
    Preparing,
    CancelRequested,
    Committing,
    Succeeded,
    Canceled,
    Failed,
}

const PREPARING: u8 = 0;
const CANCEL_REQUESTED: u8 = 1;
const COMMITTING: u8 = 2;
const SUCCEEDED: u8 = 3;
const CANCELED: u8 = 4;
const FAILED: u8 = 5;

/// One control belongs to one operation. It never acquires the project mutex.
pub struct OperationControl {
    state: AtomicU8,
    claimed: AtomicBool,
}

impl Default for OperationControl {
    fn default() -> Self {
        Self::new()
    }
}

impl OperationControl {
    pub fn new() -> Self {
        Self {
            state: AtomicU8::new(PREPARING),
            claimed: AtomicBool::new(false),
        }
    }

    /// A control is single-use even when two worker threads receive the same Arc.
    pub(crate) fn claim(&self) -> Result<()> {
        self.claimed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|_| CoreError::invalid("Operation control has already been used"))
    }

    pub fn status(&self) -> OperationStatus {
        match self.state.load(Ordering::Acquire) {
            PREPARING => OperationStatus::Preparing,
            CANCEL_REQUESTED => OperationStatus::CancelRequested,
            COMMITTING => OperationStatus::Committing,
            SUCCEEDED => OperationStatus::Succeeded,
            CANCELED => OperationStatus::Canceled,
            FAILED => OperationStatus::Failed,
            _ => unreachable!("invalid operation state"),
        }
    }

    pub fn request_cancel(&self) -> CancelOutcome {
        loop {
            match self.state.load(Ordering::Acquire) {
                PREPARING => {
                    if self
                        .state
                        .compare_exchange(
                            PREPARING,
                            CANCEL_REQUESTED,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return CancelOutcome::Accepted;
                    }
                }
                CANCEL_REQUESTED => return CancelOutcome::Accepted,
                COMMITTING => return CancelOutcome::TooLate,
                _ => return CancelOutcome::Finished,
            }
        }
    }

    /// Call while waiting for locks and between bounded preparation steps.
    pub fn check(&self) -> Result<()> {
        match self.state.load(Ordering::Acquire) {
            PREPARING | COMMITTING => Ok(()),
            CANCEL_REQUESTED => Err(CoreError::new("CANCELED", "Operation canceled")),
            _ => Err(CoreError::invalid("Operation control has already finished")),
        }
    }

    /// Race cancellation immediately before the first project publication.
    /// Subsequent publications in the same operation are already committed.
    pub fn begin_commit(&self) -> Result<()> {
        loop {
            match self.state.load(Ordering::Acquire) {
                PREPARING => {
                    if self
                        .state
                        .compare_exchange(
                            PREPARING,
                            COMMITTING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return Ok(());
                    }
                }
                COMMITTING => return Ok(()),
                CANCEL_REQUESTED => return Err(CoreError::new("CANCELED", "Operation canceled")),
                _ => return Err(CoreError::invalid("Operation control has already finished")),
            }
        }
    }

    /// The owner calls this once, after the real worker result is known.
    pub fn finish<T>(&self, result: Result<T>) -> Result<T> {
        let mut result = Some(result);
        loop {
            let current = self.state.load(Ordering::Acquire);
            if current >= SUCCEEDED {
                return Err(CoreError::invalid("Operation control has already finished"));
            }
            let canceled = match result
                .as_ref()
                .expect("result is retained until completion")
            {
                Ok(_) => current == CANCEL_REQUESTED,
                Err(error) => error.code == "CANCELED" && current != COMMITTING,
            };
            let next = if canceled {
                CANCELED
            } else if result
                .as_ref()
                .expect("result is retained until completion")
                .is_ok()
            {
                SUCCEEDED
            } else {
                FAILED
            };
            if self
                .state
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return if canceled {
                    Err(CoreError::new("CANCELED", "Operation canceled"))
                } else {
                    result.take().expect("result is retained until completion")
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn canceled_before_commit_cannot_publish() {
        let control = OperationControl::new();
        assert_eq!(control.request_cancel(), CancelOutcome::Accepted);
        assert_eq!(control.request_cancel(), CancelOutcome::Accepted);
        assert_eq!(control.begin_commit().unwrap_err().code, "CANCELED");
        assert_eq!(
            control
                .finish::<()>(Err(CoreError::new("CANCELED", "canceled")))
                .unwrap_err()
                .code,
            "CANCELED"
        );
        assert_eq!(control.status(), OperationStatus::Canceled);
        assert_eq!(control.request_cancel(), CancelOutcome::Finished);
    }

    #[test]
    fn completed_commit_is_never_relabelled_as_canceled() {
        let control = OperationControl::new();
        control.begin_commit().unwrap();
        assert_eq!(control.request_cancel(), CancelOutcome::TooLate);
        control.check().unwrap();
        assert_eq!(control.finish(Ok(42)).unwrap(), 42);
        assert_eq!(control.status(), OperationStatus::Succeeded);
    }

    #[test]
    fn cancel_and_commit_have_one_winner() {
        for _ in 0..128 {
            let control = Arc::new(OperationControl::new());
            let barrier = Arc::new(Barrier::new(2));
            let worker_control = Arc::clone(&control);
            let worker_barrier = Arc::clone(&barrier);
            let worker = std::thread::spawn(move || {
                worker_barrier.wait();
                worker_control.begin_commit()
            });
            barrier.wait();
            let cancel = control.request_cancel();
            let commit = worker.join().unwrap();
            match cancel {
                CancelOutcome::Accepted => {
                    assert_eq!(commit.unwrap_err().code, "CANCELED");
                    assert!(control
                        .finish::<()>(Err(CoreError::new("CANCELED", "canceled")))
                        .is_err());
                    assert_eq!(control.status(), OperationStatus::Canceled);
                }
                CancelOutcome::TooLate => {
                    commit.unwrap();
                    control.finish(Ok(())).unwrap();
                    assert_eq!(control.status(), OperationStatus::Succeeded);
                }
                CancelOutcome::Finished => panic!("worker has not finished the control"),
            }
        }
    }

    #[test]
    fn cancellation_wins_against_a_prepared_read_only_result() {
        let control = OperationControl::new();
        assert_eq!(control.request_cancel(), CancelOutcome::Accepted);
        assert_eq!(control.finish(Ok(1)).unwrap_err().code, "CANCELED");
        assert_eq!(control.status(), OperationStatus::Canceled);
    }
    #[test]
    fn a_control_cannot_be_claimed_by_two_workers() {
        let control = Arc::new(OperationControl::new());
        let barrier = Arc::new(Barrier::new(2));
        let other_control = Arc::clone(&control);
        let other_barrier = Arc::clone(&barrier);
        let other = std::thread::spawn(move || {
            other_barrier.wait();
            other_control.claim().is_ok()
        });
        barrier.wait();
        let this = control.claim().is_ok();
        assert_ne!(this, other.join().unwrap());
    }
}

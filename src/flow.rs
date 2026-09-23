//! Per-stream send credit gate (W3 flow control).
//!
//! A gate starts **unbounded** (v1 behavior) so a session never blocks
//! before negotiation completes. Once the peer's VERSION frame is known,
//! [`CreditGate::enable`] flips it to bounded mode with the advertised
//! window; from then on [`CreditGate::acquire`] throttles the sender and
//! incoming WINDOW frames replenish credit via [`CreditGate::release`].
//!
//! [`CreditGate::close`] wakes every waiter and passes it through: the
//! stream/session is going away, so remaining sends must fail at the
//! socket instead of dead-locking on credit that will never arrive.

use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateState {
    /// Peer has not negotiated (or stream closed past bounded mode).
    Unbounded,
    Bounded { available: i64, window: i64 },
    /// Stream/session finished: acquire() passes without consuming.
    Closed,
}

#[derive(Debug)]
pub(crate) struct CreditGate {
    state: Mutex<GateState>,
    notify: Notify,
}

impl CreditGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(GateState::Unbounded),
            notify: Notify::new(),
        })
    }

    /// Enters bounded mode with `window` bytes of credit. Idempotent: the
    /// first VERSION wins, later ones cannot shrink/grow a live window.
    pub(crate) fn enable(&self, window: i64) {
        let window = window.max(1);
        let mut state = self.state.lock().unwrap();
        if matches!(*state, GateState::Unbounded) {
            *state = GateState::Bounded {
                available: window,
                window,
            };
        }
        // No waiters can exist while Unbounded, but waking is harmless.
        drop(state);
        self.notify.notify_waiters();
    }

    /// Credits `n` consumed bytes back to the window (capped at the
    /// advertised maximum so a buggy peer cannot inflate it).
    pub(crate) fn release(&self, n: i64) {
        if n <= 0 {
            return;
        }
        let mut state = self.state.lock().unwrap();
        if let GateState::Bounded {
            ref mut available,
            window,
        } = *state
        {
            *available = (*available + n).min(window);
        }
        drop(state);
        self.notify.notify_one();
    }

    /// Unblocks all waiters and switches to pass-through mode.
    pub(crate) fn close(&self) {
        *self.state.lock().unwrap() = GateState::Closed;
        self.notify.notify_waiters();
    }

    /// Waits until `n` bytes of credit are available. Unbounded/Closed
    /// states return immediately. `n` is clamped to the window so an
    /// oversized frame can never wait for credit that can never arrive.
    pub(crate) async fn acquire(&self, n: usize) {
        let n = n as i64;
        loop {
            {
                let mut state = self.state.lock().unwrap();
                match *state {
                    GateState::Unbounded | GateState::Closed => return,
                    GateState::Bounded {
                        ref mut available,
                        window,
                    } => {
                        let need = n.min(window);
                        if *available >= need {
                            *available -= need;
                            return;
                        }
                    }
                }
            }
            // Re-check after registering: release() between the check and
            // the await stores one permit (notify_one), so no wakeup is lost.
            self.notify.notified().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unbounded_gate_passes_immediately() {
        let gate = CreditGate::new();
        gate.acquire(usize::MAX).await;
    }

    #[tokio::test]
    async fn bounded_gate_throttles_until_release() {
        let gate = CreditGate::new();
        gate.enable(100);
        gate.acquire(60).await;

        let mut waiting = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.acquire(60).await;
                1
            }
        });
        // The waiter needs 60 but only 40 remain: it must not finish yet.
        let poll =
            tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting).await;
        assert!(poll.is_err(), "acquire passed without credit");

        gate.release(60);
        let done = tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .expect("release did not wake waiter")
            .unwrap();
        assert_eq!(done, 1);
    }

    #[tokio::test]
    async fn release_is_capped_at_window() {
        let gate = CreditGate::new();
        gate.enable(100);
        gate.acquire(100).await;
        gate.release(1_000_000);
        gate.acquire(100).await;
        // Capped: another full window is NOT available.
        let pending = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            gate.acquire(1),
        )
        .await;
        assert!(pending.is_err(), "window inflated beyond advertised max");
        gate.close();
    }

    #[tokio::test]
    async fn close_unblocks_waiters_and_passes_through() {
        let gate = CreditGate::new();
        gate.enable(1);
        gate.acquire(1).await;

        let waiting = tokio::spawn({
            let gate = gate.clone();
            async move {
                gate.acquire(64 * 1024).await;
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        gate.close();
        tokio::time::timeout(std::time::Duration::from_secs(2), waiting)
            .await
            .expect("close did not wake waiter")
            .unwrap();
        // Closed stays pass-through.
        gate.acquire(usize::MAX).await;
    }

    #[tokio::test]
    async fn oversized_acquire_clamps_to_window() {
        let gate = CreditGate::new();
        gate.enable(64);
        // Request larger than the window: served from a full window
        // instead of waiting forever.
        gate.acquire(64 * 1024).await;
        gate.close();
    }

    #[tokio::test]
    async fn enable_is_idempotent() {
        let gate = CreditGate::new();
        gate.enable(100);
        gate.acquire(100).await;
        // A second VERSION must not refill the window.
        gate.enable(100);
        let pending =
            tokio::time::timeout(std::time::Duration::from_millis(50), gate.acquire(1)).await;
        assert!(pending.is_err(), "re-enable refilled a live window");
        gate.close();
    }
}

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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;

const UNBOUNDED_TAG: u32 = 0;
const CLOSED_TAG: u32 = u32::MAX;
const MAX_WINDOW: i64 = (u32::MAX - 1) as i64;

#[inline(always)]
const fn pack_state(window: u32, available: u32) -> u64 {
    ((window as u64) << 32) | (available as u64)
}

#[inline(always)]
const fn unpack_state(raw: u64) -> (u32, u32) {
    ((raw >> 32) as u32, raw as u32)
}

#[derive(Debug)]
pub(crate) struct CreditGate {
    /// Packed `(window: u32, available: u32)`:
    /// - `window == 0`: Unbounded (initial fast path)
    /// - `window == u32::MAX`: Closed (pass-through)
    /// - `1..=u32::MAX-1`: Bounded with `available <= window`
    state: AtomicU64,
    notify: Notify,
}

impl CreditGate {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: AtomicU64::new(pack_state(UNBOUNDED_TAG, 0)),
            notify: Notify::new(),
        })
    }

    /// Enters bounded mode with `window` bytes of credit. Idempotent: the
    /// first VERSION wins, later ones cannot shrink/grow a live window.
    pub(crate) fn enable(&self, window: i64) {
        let w = window.clamp(1, MAX_WINDOW) as u32;
        if self
            .state
            .compare_exchange(
                pack_state(UNBOUNDED_TAG, 0),
                pack_state(w, w),
                Ordering::AcqRel,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            self.notify.notify_waiters();
        }
    }

    /// Credits `n` consumed bytes back to the window (capped at the
    /// advertised maximum so a buggy peer cannot inflate it).
    #[inline]
    pub(crate) fn release(&self, n: i64) {
        if n <= 0 {
            return;
        }
        let add = n.min(MAX_WINDOW) as u64;
        let mut cur = self.state.load(Ordering::Relaxed);
        loop {
            let (win, avail) = unpack_state(cur);
            if win == UNBOUNDED_TAG || win == CLOSED_TAG {
                return;
            }
            let new_avail = ((avail as u64) + add).min(win as u64) as u32;
            if new_avail == avail {
                return;
            }
            match self.state.compare_exchange_weak(
                cur,
                pack_state(win, new_avail),
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => {
                    self.notify.notify_one();
                    return;
                }
                Err(actual) => cur = actual,
            }
        }
    }

    /// Unblocks all waiters and switches to pass-through mode.
    pub(crate) fn close(&self) {
        self.state
            .store(pack_state(CLOSED_TAG, 0), Ordering::Release);
        self.notify.notify_waiters();
    }

    /// Waits until `n` bytes of credit are available. Unbounded/Closed
    /// states return immediately. `n` is clamped to the window so an
    /// oversized frame can never wait for credit that can never arrive.
    #[inline]
    pub(crate) async fn acquire(&self, n: usize) {
        // Fast path: try to acquire without constructing or registering a Notified waiter.
        let mut cur = self.state.load(Ordering::Acquire);
        loop {
            let (win, avail) = unpack_state(cur);
            if win == UNBOUNDED_TAG || win == CLOSED_TAG {
                return;
            }
            let need = n.min(win as usize) as u32;
            if avail < need {
                break;
            }
            match self.state.compare_exchange_weak(
                cur,
                pack_state(win, avail - need),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => cur = actual,
            }
        }

        // Slow path: register waiter BEFORE re-checking state so neither
        // `release()` (`notify_one`) nor `close()`/`enable()` (`notify_waiters`)
        // can fire in the gap between the state check and `.await`.
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let mut cur = self.state.load(Ordering::Acquire);
            loop {
                let (win, avail) = unpack_state(cur);
                if win == UNBOUNDED_TAG || win == CLOSED_TAG {
                    return;
                }
                let need = n.min(win as usize) as u32;
                if avail < need {
                    break;
                }
                match self.state.compare_exchange_weak(
                    cur,
                    pack_state(win, avail - need),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                ) {
                    Ok(_) => return,
                    Err(actual) => cur = actual,
                }
            }

            notified.await;
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

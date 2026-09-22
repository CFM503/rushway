//! Process-wide forwarding statistics (GoWay `Statistics` parity).
//!
//! Hot counters are plain relaxed atomics so relay tasks never contend on a
//! mutex; speed floats are written once per monitor tick and read by the TUI
//! or the `[STATS]` line.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

#[repr(align(64))]
struct HotCounters {
    active_conns: AtomicI64,
    bytes_up: AtomicI64,
    bytes_down: AtomicI64,
}

#[repr(align(64))]
struct SpeedWords {
    up_bits: AtomicU64,
    down_bits: AtomicU64,
}

static HOT: HotCounters = HotCounters {
    active_conns: AtomicI64::new(0),
    bytes_up: AtomicI64::new(0),
    bytes_down: AtomicI64::new(0),
};

static SPEEDS: SpeedWords = SpeedWords {
    up_bits: AtomicU64::new(0),
    down_bits: AtomicU64::new(0),
};

/// RAII guard: increments the active-connection gauge for the lifetime of a
/// spawned proxy/transport task. Mirrors GoWay `tryAcquireConn`/`releaseConn`
/// bookkeeping (the actual capacity limit remains the accept-loop semaphore).
pub struct ConnGuard;

impl ConnGuard {
    #[must_use]
    pub fn new() -> Self {
        HOT.active_conns.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Default for ConnGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let _ = HOT
            .active_conns
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                if v > 0 {
                    Some(v - 1)
                } else {
                    Some(0)
                }
            });
    }
}

/// Records relayed bytes: `up` is client→remote, `down` is remote→client.
#[inline]
pub fn add_bytes(up: i64, down: i64) {
    if up > 0 {
        HOT.bytes_up.fetch_add(up, Ordering::Relaxed);
    }
    if down > 0 {
        HOT.bytes_down.fetch_add(down, Ordering::Relaxed);
    }
}

#[inline]
pub fn active_conns() -> i64 {
    HOT.active_conns.load(Ordering::Relaxed)
}

#[inline]
pub fn totals() -> (i64, i64) {
    (
        HOT.bytes_up.load(Ordering::Relaxed),
        HOT.bytes_down.load(Ordering::Relaxed),
    )
}

fn set_speeds(up_mbps: f64, down_mbps: f64) {
    SPEEDS.up_bits.store(up_mbps.to_bits(), Ordering::Relaxed);
    SPEEDS
        .down_bits
        .store(down_mbps.to_bits(), Ordering::Relaxed);
}

pub fn speeds() -> (f64, f64) {
    (
        f64::from_bits(SPEEDS.up_bits.load(Ordering::Relaxed)),
        f64::from_bits(SPEEDS.down_bits.load(Ordering::Relaxed)),
    )
}

/// Spawns the GoWay `monitorStats` equivalent: recompute speeds every tick,
/// publish them atomically, and either drive the TUI refresh or print the
/// single-line `[STATS]` status (suppressed while the TUI owns the screen).
pub fn spawn_monitor(tui_enabled: bool, print_stats_line: bool) {
    tokio::spawn(async move {
        let interval_secs: u64 = if tui_enabled { 1 } else { 3 };
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Consume the immediate first tick so the initial window is a full interval.
        ticker.tick().await;
        let mut last_up = 0i64;
        let mut last_down = 0i64;
        loop {
            ticker.tick().await;
            let (curr_up, curr_down) = totals();
            let active = active_conns();
            let secs = interval_secs as f64;
            let up_speed = (curr_up.saturating_sub(last_up)) as f64 / secs / 1024.0 / 1024.0;
            let down_speed = (curr_down.saturating_sub(last_down)) as f64 / secs / 1024.0 / 1024.0;
            last_up = curr_up;
            last_down = curr_down;
            set_speeds(up_speed, down_speed);
            if tui_enabled {
                crate::tui::trigger_refresh();
            } else if print_stats_line {
                use std::io::Write as _;
                print!(
                    "\r\x1b[90m[STATS] Conns: {active} | Up: {up_speed:.2} MB/s | Down: {down_speed:.2} MB/s\x1b[0m"
                );
                let _ = std::io::stdout().flush();
            }
        }
    });
}

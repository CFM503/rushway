//! `-cpuprofile` / `-cpuprofile-duration` support (GoWay `runtime/pprof`
//! parity via the `pprof` crate on Unix signal sampling).
//!
//! Windows builds accept the flags but cannot sample other threads' stacks
//! without DbgHelp suspension, so they warn and skip — documented in SPEC.

use std::path::PathBuf;

/// Starts CPU profiling. On Unix, samples at ~99 Hz into a `pprof` protobuf.
/// `duration_secs = Some(n)` stops and writes after `n` seconds; `None`/`0`
/// keeps sampling until [`finish`] is called at shutdown.
pub fn start(path: PathBuf, duration_secs: Option<u64>) -> anyhow::Result<()> {
    start_imp(path, duration_secs)
}

/// Writes any in-flight profile and stops the sampler. Safe to call twice.
pub fn finish() {
    finish_imp()
}

#[cfg(unix)]
mod imp {
    use anyhow::{anyhow, Result};
    use pprof::protos::Message as _;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::time::Duration;

    type Guard = pprof::ProfilerGuard<'static>;

    static STATE: Mutex<Option<(Guard, PathBuf)>> = Mutex::new(None);

    pub(crate) fn start(path: PathBuf, duration_secs: Option<u64>) -> Result<()> {
        let guard = pprof::ProfilerGuardBuilder::default()
            .frequency(99)
            .build()
            .map_err(|e| anyhow!("failed to start CPU profiler: {e}"))?;
        {
            let mut slot = STATE.lock().unwrap_or_else(|e| e.into_inner());
            if slot.is_some() {
                return Err(anyhow!("CPU profiler already running"));
            }
            *slot = Some((guard, path.clone()));
        }
        tracing::info!(path = %path.display(), "CPU profiler started (99 Hz)");
        if let Some(secs) = duration_secs.filter(|s| *s > 0) {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(secs));
                finish();
            });
        }
        Ok(())
    }

    pub(crate) fn finish() {
        let taken = STATE.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some((guard, path)) = taken else { return };
        match guard.report().build() {
            Ok(report) => match report.pprof() {
                Ok(profile) => {
                    let mut content = Vec::new();
                    if let Err(error) = profile.encode(&mut content) {
                        tracing::warn!(%error, "failed to encode CPU profile");
                        return;
                    }
                    let mut file = match std::fs::File::create(&path) {
                        Ok(f) => f,
                        Err(error) => {
                            tracing::warn!(path = %path.display(), %error, "failed to create CPU profile file");
                            return;
                        }
                    };
                    use std::io::Write as _;
                    if let Err(error) = file.write_all(&content) {
                        tracing::warn!(path = %path.display(), %error, "failed to write CPU profile");
                        return;
                    }
                    tracing::info!(path = %path.display(), "CPU profile saved");
                }
                Err(error) => {
                    tracing::warn!(%error, "failed to encode CPU profile");
                }
            },
            Err(error) => {
                tracing::warn!(%error, "failed to build CPU profile report");
            }
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use anyhow::Result;
    use std::path::PathBuf;

    pub(crate) fn start(path: PathBuf, _duration_secs: Option<u64>) -> Result<()> {
        tracing::warn!(
            path = %path.display(),
            "CPU profiling requires a Unix build (pprof signal sampler); flag accepted, no profile will be written on this platform"
        );
        Ok(())
    }

    pub(crate) fn finish() {}
}

use imp::{finish as finish_imp, start as start_imp};

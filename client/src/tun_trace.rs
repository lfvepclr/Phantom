//! Optional on-device trace of the userspace TCP stack behind the TUN device.
//!
//! The TUN path speaks TCP to the app on one side and a byte stream to the
//! tunnel on the other, so when an app stalls the interesting questions are
//! always the same: what did the app advertise (MSS / window scale / initial
//! window), how many bytes did we inject and in which segments, did the ACKs
//! come back, and why did the flow end.
//!
//! Normal logging is far too noisy (and too expensive) to answer those at
//! packet granularity, so tracing is opt-in: the UI passes a file path through
//! `phantomHarmonySetTrace`, and until it does, every call here returns after a
//! single atomic-free mutex peek. Lines are capped (`MAX_LINES`) so a debugging
//! session can never fill the phone's storage.
//!
//! The sink lives here rather than in `tun.rs` so the hot path only pays for a
//! `format_args!` when tracing is on.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

/// Hard cap on the trace file. 5000 lines is a few hundred KiB — enough for
/// several app launches, small enough to always be readable from the phone.
pub const MAX_LINES: usize = 5000;

struct Sink {
    file: std::fs::File,
    lines: usize,
    started: Instant,
}

static SINK: OnceLock<Mutex<Option<Sink>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Sink>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// Turn tracing on (with a freshly truncated file at `path`) or off.
///
/// Returns an error only when the file cannot be created; tracing stays off in
/// that case so a bad path cannot break the data plane.
pub fn set_path(path: Option<&str>) -> std::io::Result<()> {
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    match path {
        None => {
            *guard = None;
            Ok(())
        }
        Some(p) => {
            let path = PathBuf::from(p);
            let file = std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&path)?;
            let mut file = file;
            let _ = writeln!(
                file,
                "# Phantom TUN trace — {} line cap; timestamps are ms since enable",
                MAX_LINES
            );
            *guard = Some(Sink {
                file,
                lines: 0,
                started: Instant::now(),
            });
            Ok(())
        }
    }
}

/// `true` once `set_path(Some(..))` succeeded and the line budget is not spent.
pub fn is_enabled() -> bool {
    let guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    matches!(&*guard, Some(s) if s.lines < MAX_LINES)
}

/// Append one trace line. A no-op while tracing is disabled.
pub fn log(args: std::fmt::Arguments<'_>) {
    // Cheap pre-check so the common (disabled) path does not format anything.
    if !is_enabled() {
        return;
    }
    let mut guard = slot().lock().unwrap_or_else(|e| e.into_inner());
    let Some(sink) = guard.as_mut() else {
        return;
    };
    if sink.lines >= MAX_LINES {
        return;
    }
    sink.lines += 1;
    let ms = sink.started.elapsed().as_millis();
    let _ = writeln!(sink.file, "[{:>7}ms] {}", ms, args);
}

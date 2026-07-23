//! Progress and event callback types for library consumers.
//!
//! Each library function that produces output accepts an optional
//! [`EventCallback`].  When `None` is passed the function behaves exactly as
//! before (indicatif progress bars, `println!`, `eprintln!` output), unless
//! [`set_silent_output`] is enabled.
//! When `Some(cb)` is passed every message and progress update is routed
//! through the callback instead, making the library embeddable inside async
//! servers or other tools without polluting stdout.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

static SILENT_OUTPUT: AtomicBool = AtomicBool::new(false);

/// A single event emitted by a library operation.
#[derive(Debug, Clone)]
pub enum ProbeEvent {
    /// A human-readable log message.
    Log { msg: String, tag: LogTag },
    /// UF2 write progress: bytes written so far and optional total.
    Progress { written: u64, total: Option<u64> },
    /// A decoded defmt frame string (from `attach` / `watch`).
    Frame(String),
    /// Serial port connected (from `attach` / `watch`).
    Connected { port: String },
    /// Serial port disconnected.
    Disconnected,
}

/// Severity tag for [`ProbeEvent::Log`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTag {
    Info,
    Ok,
    Warn,
    Err,
}

/// Shared callback type.  Wrap your channel sender or logging closure in an
/// [`Arc`] and pass it to any library function that accepts
/// `Option<EventCallback>`.
pub type EventCallback = Arc<dyn Fn(ProbeEvent) + Send + Sync + 'static>;

/// Enable or disable default terminal output (progress bars, `println!`,
/// `eprintln!`) when no event callback is provided.
///
/// This is useful for crates embedding `probe-rp-usb` where the caller wants
/// complete control over presentation.
pub fn set_silent_output(silent: bool) {
    SILENT_OUTPUT.store(silent, Ordering::Relaxed);
}

/// Returns whether silent output mode is currently enabled.
pub fn silent_output_enabled() -> bool {
    SILENT_OUTPUT.load(Ordering::Relaxed)
}

/// Returns `true` when terminal output should be rendered directly by this
/// crate (CLI-style behavior).
#[inline]
pub(crate) fn use_terminal_output(on_event: &Option<EventCallback>) -> bool {
    on_event.is_none() && !silent_output_enabled()
}

/// Convenience helper: if `on_event` is `Some`, invoke the callback with a
/// `Log` event.  When `None`, does nothing (caller is responsible for any
/// indicatif / println output in the `None` branch).
#[inline]
pub fn report(on_event: &Option<EventCallback>, msg: impl Into<String>, tag: LogTag) {
    if let Some(cb) = on_event {
        cb(ProbeEvent::Log {
            msg: msg.into(),
            tag,
        });
    }
}

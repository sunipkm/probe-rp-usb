//! Internal progress-reporting types shared by `flash` and `write`.
//!
//! These are `pub(crate)` only; library consumers interact exclusively
//! through the public [`crate::event`] API.

use indicatif::{ProgressBar, ProgressStyle};
use std::io::{self, Write};
use std::time::Duration;

use crate::event::{EventCallback, LogTag, ProbeEvent};

/// Abstracts over indicatif progress bars (CLI mode) and event callbacks
/// (library-consumer mode).
///
/// Clone before moving into a [`ProgressWriter`] so you can call
/// [`ProgressReporter::finish`] or [`ProgressReporter::abandon`] after the
/// writer is consumed.
#[derive(Clone)]
pub(crate) enum ProgressReporter {
    Bar(ProgressBar),
    Callback { written: u64, total: Option<u64>, cb: EventCallback },
}

impl ProgressReporter {
    /// Determinate reporter for a known total byte count.
    pub(crate) fn progress(on_event: &Option<EventCallback>, total: u64) -> Self {
        match on_event {
            Some(cb) => ProgressReporter::Callback {
                written: 0,
                total: Some(total),
                cb: cb.clone(),
            },
            None => {
                let bar = ProgressBar::new(total);
                bar.set_style(
                    ProgressStyle::with_template(
                        "  Writing UF2  [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
                    )
                    .unwrap()
                    .progress_chars("\u{2588}\u{2589}\u{258a}\u{258b}\u{258c}\u{258d}\u{258e}\u{258f} "),
                );
                ProgressReporter::Bar(bar)
            }
        }
    }

    /// Indeterminate reporter (spinner) for an unknown total byte count.
    pub(crate) fn spinner(on_event: &Option<EventCallback>) -> Self {
        match on_event {
            Some(cb) => ProgressReporter::Callback {
                written: 0,
                total: None,
                cb: cb.clone(),
            },
            None => {
                let bar = ProgressBar::new_spinner();
                bar.enable_steady_tick(Duration::from_millis(80));
                bar.set_style(
                    ProgressStyle::with_template(
                        "{spinner:.cyan} Writing UF2\u{2026} {bytes}",
                    )
                    .unwrap()
                    .tick_strings(crate::ui::tick_chars()),
                );
                ProgressReporter::Bar(bar)
            }
        }
    }

    /// Increment the bytes-written counter and emit a progress event.
    pub(crate) fn inc(&mut self, n: u64) {
        match self {
            ProgressReporter::Bar(bar) => bar.inc(n),
            ProgressReporter::Callback { written, total, cb } => {
                *written += n;
                cb(ProbeEvent::Progress { written: *written, total: *total });
            }
        }
    }

    /// Emit a success completion message.
    pub(crate) fn finish(&self, msg: &str) {
        match self {
            ProgressReporter::Bar(bar) => bar.finish_with_message(msg.to_owned()),
            ProgressReporter::Callback { cb, .. } => {
                cb(ProbeEvent::Log { msg: msg.to_owned(), tag: LogTag::Ok });
            }
        }
    }

    /// Emit a failure/abandon message.
    pub(crate) fn abandon(&self, msg: &str) {
        match self {
            ProgressReporter::Bar(bar) => bar.abandon_with_message(msg.to_owned()),
            ProgressReporter::Callback { cb, .. } => {
                cb(ProbeEvent::Log { msg: msg.to_owned(), tag: LogTag::Err });
            }
        }
    }
}

/// A `Write` adapter that increments a [`ProgressReporter`] with every batch
/// of bytes written.
pub(crate) struct ProgressWriter<W: Write> {
    pub(crate) inner: W,
    reporter: ProgressReporter,
}

impl<W: Write> ProgressWriter<W> {
    pub(crate) fn new(inner: W, reporter: ProgressReporter) -> Self {
        ProgressWriter { inner, reporter }
    }
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.reporter.inc(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

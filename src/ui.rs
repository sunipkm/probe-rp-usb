use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

// indicatif writes progress output to stderr, so check that stream.
static UNICODE_TICKS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏", "✓"];
static ASCII_TICKS: &[&str] = &["-", "\\", "|", "/", "+"];

/// Return the spinner tick strings appropriate for the current terminal.
///
/// Uses braille characters (U+2800 block) + `✓` when the terminal reports
/// Unicode support; falls back to ASCII `-\|/+` otherwise.  The check is
/// done at runtime so Windows Terminal and legacy `cmd.exe` get different
/// results even though both compile as `target_os = "windows"`.
pub fn tick_chars() -> &'static [&'static str] {
    if supports_unicode::on(supports_unicode::Stream::Stderr) {
        UNICODE_TICKS
    } else {
        ASCII_TICKS
    }
}

/// Create a spinner with `msg` that auto-ticks until explicitly finished.
pub fn spinner(msg: impl Into<String>) -> ProgressBar {
    let bar = ProgressBar::new_spinner();
    bar.enable_steady_tick(Duration::from_millis(80));
    bar.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_strings(tick_chars()),
    );
    bar.set_message(msg.into());
    bar
}

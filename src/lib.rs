//! Library interface for `probe-rp-usb`.
//!
//! Exposes the core USB, serial, flash, and UF2 operations so they can be
//! embedded in other tools without going through the command-line interface.
//!
//! Firmware targeted by `probe-rp-usb` should use the
//! [`rp-usb-reset`](https://crates.io/crates/rp-usb-reset) crate to
//! expose the expected USB reset interface. This keeps reset behavior
//! compatible with `probe-rp-usb` and, in normal Windows setups, removes the
//! need for a manual Zadig driver install.
//!
//! # Quick example
//!
//! ```no_run
//! use probe_rp_usb::{bootsel, flash, usb};
//! use probe_rp_usb::Family;
//! use std::path::Path;
//!
//! // Reset into BOOTSEL and flash a firmware ELF in one call.
//! flash::flash(
//!     Path::new("firmware.elf"),
//!     Family::RP2350_ARM_S,
//!     0x1000_0000,
//!     None,  // VID: use default 0x2E8A
//!     None,  // PID: use default 0x0009
//!     10,    // BOOTSEL drive timeout (seconds)
//!     false, // no_wait: wait for device reboot
//!     None,  // event callback
//! ).unwrap();
//!
//! // Optional: suppress default terminal output when embedding as a library.
//! probe_rp_usb::event::set_silent_output(true);
//! ```

pub mod attach;
pub mod bootsel;
pub mod elf;
pub mod event;
pub mod flash;
pub mod picoboot;
pub(crate) mod progress;
pub mod uf2;
pub mod ui;
pub mod usb;
pub mod write;

/// Re-export the UF2 family enum used by the flash and write APIs.
pub use crate::uf2::Family;

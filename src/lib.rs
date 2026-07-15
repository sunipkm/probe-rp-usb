//! Library interface for `probe-rp-usb`.
//!
//! Exposes the core USB, serial, flash, and UF2 operations so they can be
//! embedded in other tools without going through the command-line interface.
//!
//! # Quick example
//!
//! ```no_run
//! use probe_rp_usb::{bootsel, flash, usb};
//! use elf2uf2_core::Family;
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
//! ).unwrap();
//! ```

pub mod attach;
pub mod bootsel;
pub mod flash;
pub mod uf2;
pub mod ui;
pub mod usb;
pub mod write;

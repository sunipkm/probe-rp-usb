use anyhow::{Context, Result};
use elf2uf2_core::Family;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::event::{report, EventCallback, LogTag};
use crate::progress::{ProgressReporter, ProgressWriter};
use crate::{bootsel, ui, uf2, usb};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A raw binary image to write at a specific flash address.
#[derive(Clone)]
pub struct WriteTarget {
    pub path: PathBuf,
    pub address: u32,
}

/// Write one or more raw binary images to flash at their specified addresses.
///
/// Each image is converted to independent UF2 blocks at its target address;
/// no gap-filling is performed between images (UF2 blocks are inherently
/// non-contiguous).  The combined block set is renumbered globally and written
/// as a single UF2 file so the device resets exactly once.
///
/// If `erase_boot` is `true`, a 256-byte block of `0xFF` is prepended at
/// `0x10000000` (the first flash page on RP2040/RP2350).  This invalidates the
/// firmware header *before* the data lands, preventing the device from booting
/// into stale firmware if it resets mid-transfer.
///
/// Pass `on_event: Some(cb)` to receive structured progress events instead of
/// the default indicatif terminal output.
#[allow(clippy::too_many_arguments)]
pub fn write_data(
    targets: &[WriteTarget],
    erase_boot: bool,
    family: Family,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: Option<EventCallback>,
) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();

    // Optionally invalidate the existing firmware header so the device cannot
    // reboot into stale firmware while the data write is in progress.
    const FLASH_START: u32 = 0x1000_0000;
    if erase_boot {
        let blank = [0xFFu8; 256];
        uf2::bin2uf2(blank.as_slice(), &mut buf, FLASH_START, family as u32)
            .context("Failed to generate erase-boot UF2 block")?;
        log::info!("Erase-boot: 256 bytes of 0xFF at 0x{:08x}", FLASH_START);
    }

    // Convert each target binary to UF2 blocks at its absolute address.
    for (i, target) in targets.iter().enumerate() {
        let mut f = File::open(&target.path).with_context(|| {
            format!("Failed to open write target {}", target.path.display())
        })?;
        let mut data = Vec::new();
        f.read_to_end(&mut data).with_context(|| {
            format!("Failed to read write target {}", target.path.display())
        })?;
        log::info!(
            "Write target [{}]: {} @ 0x{:08x} ({} bytes)",
            i,
            target.path.display(),
            target.address,
            data.len()
        );
        uf2::bin2uf2(data.as_slice(), &mut buf, target.address, family as u32)
            .with_context(|| format!("UF2 conversion failed (target {})", i))?;
    }

    // Renumber blocks globally so the device knows when the transfer is complete.
    let total = (buf.len() / 512) as u32;
    log::info!("Total UF2 blocks: {}", total);
    uf2::renumber_blocks(&mut buf, 0, total);

    run_uf2_write(buf, vid, pid, bootsel_timeout_secs, no_wait, "Write", &on_event)
}

/// Erase the entire flash region by writing `0xFF` to every 256-byte page.
///
/// Generates a UF2 file that covers all `flash_size` bytes starting at
/// `base_addr`, with every byte set to `0xFF`.  This restores the flash to its
/// erased state, removing any existing firmware or data.
///
/// Pass `on_event: Some(cb)` to receive structured progress events instead of
/// the default indicatif terminal output.
#[allow(clippy::too_many_arguments)]
pub fn erase_flash(
    flash_size: u32,
    base_addr: u32,
    family: Family,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: Option<EventCallback>,
) -> Result<()> {
    anyhow::ensure!(flash_size > 0, "flash_size must be greater than zero");
    log::info!(
        "Erasing {} bytes (0x{:x}) at 0x{:08x}",
        flash_size, flash_size, base_addr
    );

    let data = vec![0xFFu8; flash_size as usize];
    let mut buf = Vec::new();
    uf2::bin2uf2(data.as_slice(), &mut buf, base_addr, family as u32)
        .context("UF2 conversion failed (erase)")?;

    run_uf2_write(buf, vid, pid, bootsel_timeout_secs, no_wait, "Erase", &on_event)
}

// ---------------------------------------------------------------------------
// Shared write helper
// ---------------------------------------------------------------------------

/// Detect (or wait for) the BOOTSEL drive, write `buf` as a single UF2 file,
/// and optionally wait for the device to reboot.
fn run_uf2_write(
    buf: Vec<u8>,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    op_name: &str,
    on_event: &Option<EventCallback>,
) -> Result<()> {
    let mount = match bootsel::find_bootsel_drive() {
        Some(m) => {
            log::info!("BOOTSEL drive already mounted at {}", m.display());
            report(on_event, format!("BOOTSEL drive: {}", m.display()), LogTag::Info);
            m
        }
        None => {
            log::info!(
                "No BOOTSEL drive found — resetting device (VID {:04x} PID {:04x})",
                vid.unwrap_or(usb::DEFAULT_VID),
                pid.unwrap_or(usb::DEFAULT_PID),
            );
            report(on_event, "Resetting device to BOOTSEL mode\u{2026}", LogTag::Info);
            usb::reset_to_bootsel(vid, pid)?;
            let maybe_spin = if on_event.is_none() {
                Some(ui::spinner("Waiting for BOOTSEL drive\u{2026}"))
            } else {
                report(on_event, "Waiting for BOOTSEL drive\u{2026}", LogTag::Info);
                None
            };
            let m = bootsel::wait_for_bootsel_drive(Duration::from_secs(bootsel_timeout_secs))
                .inspect_err(|_| {
                    if let Some(ref s) = maybe_spin {
                        s.abandon();
                    } else {
                        report(on_event, "Timed out waiting for BOOTSEL drive", LogTag::Err);
                    }
                })?;
            if let Some(spin) = maybe_spin {
                spin.finish_with_message(format!("BOOTSEL drive: {}", m.display()));
            } else {
                report(on_event, format!("BOOTSEL drive: {}", m.display()), LogTag::Ok);
            }
            m
        }
    };

    let out_path = mount.join("out.uf2");
    let out_file = File::create(&out_path)
        .with_context(|| format!("Failed to create {}", out_path.display()))?;

    let reporter = ProgressReporter::progress(on_event, buf.len() as u64);
    let finish_rpt = reporter.clone();
    let mut pw = ProgressWriter::new(out_file, reporter);
    let write_result = pw
        .write_all(&buf)
        .context("Failed to write UF2 to BOOTSEL drive");
    if write_result.is_err() {
        finish_rpt.abandon("Write failed");
    } else {
        finish_rpt.finish("UF2 written");
    }

    if let Err(e) = write_result {
        let _ = fs::remove_file(&out_path);
        return Err(e.context("UF2 write failed; partial file removed"));
    }

    if no_wait {
        log::info!("--no-wait: skipping reboot wait");
        if on_event.is_some() {
            report(on_event, format!("{op_name} complete (device left in BOOTSEL mode)"), LogTag::Ok);
        } else {
            println!("{op_name} complete (device left in BOOTSEL mode)");
        }
        return Ok(());
    }

    let maybe_spin2 = if on_event.is_none() {
        Some(ui::spinner("Waiting for device to reboot\u{2026}"))
    } else {
        report(on_event, "Waiting for device to reboot\u{2026}", LogTag::Info);
        None
    };
    bootsel::wait_for_bootsel_unmount(Duration::from_secs(15))
        .inspect_err(|_| {
            if let Some(ref s) = maybe_spin2 {
                s.abandon();
            } else {
                report(on_event, "Timed out waiting for device to reboot", LogTag::Err);
            }
        })
        .context("Device did not unmount BOOTSEL drive")?;
    if let Some(spin) = maybe_spin2 {
        spin.finish_with_message(format!("{op_name} complete"));
    } else {
        report(on_event, format!("{op_name} complete"), LogTag::Ok);
    }
    Ok(())
}

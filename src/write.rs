use anyhow::{Context, Result};
use elf2uf2_core::Family;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use crate::{bootsel, uf2, ui, usb};

// ---------------------------------------------------------------------------
// Progress-reporting writer (mirrors the one in flash.rs)
// ---------------------------------------------------------------------------

struct ProgressWriter<W: Write> {
    inner: W,
    bar: ProgressBar,
}

impl<W: Write> Write for ProgressWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.bar.inc(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

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
pub fn write_data(
    targets: &[WriteTarget],
    erase_boot: bool,
    family: Family,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
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

    run_uf2_write(buf, vid, pid, bootsel_timeout_secs, no_wait, "Write")
}

/// Erase the entire flash region by writing `0xFF` to every 256-byte page.
///
/// Generates a UF2 file that covers all `flash_size` bytes starting at
/// `base_addr`, with every byte set to `0xFF`.  This restores the flash to its
/// erased state, removing any existing firmware or data.
pub fn erase_flash(
    flash_size: u32,
    base_addr: u32,
    family: Family,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
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

    run_uf2_write(buf, vid, pid, bootsel_timeout_secs, no_wait, "Erase")
}

// ---------------------------------------------------------------------------
// Shared write helper
// ---------------------------------------------------------------------------

/// Detect (or wait for) the BOOTSEL drive, write `buf` as a single UF2 file,
/// and optionally wait for the device to reboot.  `op_name` is used in
/// progress messages (e.g. `"Write"` or `"Erase"`).
fn run_uf2_write(
    buf: Vec<u8>,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    op_name: &str,
) -> Result<()> {
    let mount = match bootsel::find_bootsel_drive() {
        Some(m) => {
            log::info!("BOOTSEL drive already mounted at {}", m.display());
            m
        }
        None => {
            log::info!(
                "No BOOTSEL drive found — resetting device (VID {:04x} PID {:04x})",
                vid.unwrap_or(usb::DEFAULT_VID),
                pid.unwrap_or(usb::DEFAULT_PID),
            );
            usb::reset_to_bootsel(vid, pid)?;
            let spin = ui::spinner("Waiting for BOOTSEL drive…");
            let m = bootsel::wait_for_bootsel_drive(Duration::from_secs(bootsel_timeout_secs))
                .inspect_err(|_| spin.abandon())?;
            spin.finish_with_message(format!("BOOTSEL drive: {}", m.display()));
            m
        }
    };

    let out_path = mount.join("out.uf2");
    let out_file = File::create(&out_path)
        .with_context(|| format!("Failed to create {}", out_path.display()))?;

    let bar = ProgressBar::new(buf.len() as u64);
    bar.set_style(
        ProgressStyle::with_template(
            "  Writing UF2  [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})",
        )
        .unwrap()
        .progress_chars("█▉▊▋▌▍▎▏ "),
    );
    let mut pw = ProgressWriter {
        inner: out_file,
        bar: bar.clone(),
    };
    let write_result = pw
        .write_all(&buf)
        .context("Failed to write UF2 to BOOTSEL drive");
    if write_result.is_err() {
        bar.abandon_with_message("Write failed");
    } else {
        bar.finish_with_message("UF2 written");
    }

    if let Err(e) = write_result {
        let _ = fs::remove_file(&out_path);
        return Err(e.context("UF2 write failed; partial file removed"));
    }

    if no_wait {
        log::info!("--no-wait: skipping reboot wait");
        println!("{op_name} complete (device left in BOOTSEL mode)");
        return Ok(());
    }

    let spin = ui::spinner("Waiting for device to reboot…");
    bootsel::wait_for_bootsel_unmount(Duration::from_secs(15))
        .inspect_err(|_| spin.abandon())
        .context("Device did not unmount BOOTSEL drive")?;
    spin.finish_with_message(format!("{op_name} complete"));
    Ok(())
}

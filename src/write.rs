use anyhow::{Context, Result, anyhow};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::event::{EventCallback, LogTag, report, use_terminal_output};
use crate::progress::{ProgressReporter, ProgressWriter};
use crate::uf2::Family;
use crate::{bootsel, picoboot::PicobootConnection, uf2, ui, usb};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// A raw binary image to write at a specific flash address.
#[derive(Clone)]
pub struct WriteTarget {
    pub path: PathBuf,
    pub address: u32,
}

#[derive(Clone)]
struct WriteRegion {
    label: String,
    address: u32,
    data: Vec<u8>,
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
    _family: Family,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: Option<EventCallback>,
) -> Result<()> {
    let mut regions: Vec<WriteRegion> = Vec::new();

    const FLASH_START: u32 = 0x1000_0000;
    if erase_boot {
        regions.push(WriteRegion {
            label: "erase-boot".to_owned(),
            address: FLASH_START,
            data: vec![0xFFu8; 256],
        });
        log::info!("Erase-boot: 256 bytes of 0xFF at 0x{:08x}", FLASH_START);
    }

    for (i, target) in targets.iter().enumerate() {
        let mut f = File::open(&target.path)
            .with_context(|| format!("Failed to open write target {}", target.path.display()))?;
        let mut data = Vec::new();
        f.read_to_end(&mut data)
            .with_context(|| format!("Failed to read write target {}", target.path.display()))?;
        log::info!(
            "Write target [{}]: {} @ 0x{:08x} ({} bytes)",
            i,
            target.path.display(),
            target.address,
            data.len()
        );
        regions.push(WriteRegion {
            label: target.path.display().to_string(),
            address: target.address,
            data,
        });
    }

    run_picoboot_write(regions, vid, pid, bootsel_timeout_secs, no_wait, &on_event)
}

pub(crate) fn write_regions_data(
    regions: Vec<(String, u32, Vec<u8>)>,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: Option<EventCallback>,
) -> Result<()> {
    let regions = regions
        .into_iter()
        .map(|(label, address, data)| WriteRegion {
            label,
            address,
            data,
        })
        .collect();
    run_picoboot_write(regions, vid, pid, bootsel_timeout_secs, no_wait, &on_event)
}

/// UF2 mass-storage implementation retained as a compatibility backend.
#[allow(clippy::too_many_arguments)]
pub fn write_data_uf2(
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
        let mut f = File::open(&target.path)
            .with_context(|| format!("Failed to open write target {}", target.path.display()))?;
        let mut data = Vec::new();
        f.read_to_end(&mut data)
            .with_context(|| format!("Failed to read write target {}", target.path.display()))?;
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

    run_uf2_write(
        buf,
        vid,
        pid,
        bootsel_timeout_secs,
        no_wait,
        "Write",
        &on_event,
    )
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
    _family: Family,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: Option<EventCallback>,
) -> Result<()> {
    anyhow::ensure!(flash_size > 0, "flash_size must be greater than zero");
    anyhow::ensure!(
        base_addr.is_multiple_of(FLASH_SECTOR_SIZE),
        "base address must be 4096-byte aligned for direct erase"
    );
    anyhow::ensure!(
        flash_size.is_multiple_of(FLASH_SECTOR_SIZE),
        "flash size must be 4096-byte aligned for direct erase"
    );
    log::info!(
        "Erasing {} bytes (0x{:x}) at 0x{:08x}",
        flash_size,
        flash_size,
        base_addr
    );

    run_picoboot_erase(
        flash_size,
        base_addr,
        vid,
        pid,
        bootsel_timeout_secs,
        no_wait,
        &on_event,
    )
}

/// UF2 mass-storage implementation retained as a compatibility backend.
#[allow(clippy::too_many_arguments)]
pub fn erase_flash_uf2(
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
        flash_size,
        flash_size,
        base_addr
    );

    let data = vec![0xFFu8; flash_size as usize];
    let mut buf = Vec::new();
    uf2::bin2uf2(data.as_slice(), &mut buf, base_addr, family as u32)
        .context("UF2 conversion failed (erase)")?;

    run_uf2_write(
        buf,
        vid,
        pid,
        bootsel_timeout_secs,
        no_wait,
        "Erase",
        &on_event,
    )
}

pub fn read_flash(
    address: u32,
    length: u32,
    output: &Path,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    on_event: Option<EventCallback>,
) -> Result<()> {
    let mut connection = open_picoboot(vid, pid, bootsel_timeout_secs, &on_event)?;
    connection.exit_xip().context("Failed to exit XIP mode")?;

    let out_file = File::create(output)
        .with_context(|| format!("Failed to create output file {}", output.display()))?;
    let reporter = ProgressReporter::progress_with_label(&on_event, length as u64, "Reading flash");
    let finish_rpt = reporter.clone();
    let mut writer = ProgressWriter::new(out_file, reporter);

    let mut remaining = length;
    let mut cursor = address;
    const READ_CHUNK: u32 = 4096;
    while remaining > 0 {
        let chunk = remaining.min(READ_CHUNK);
        let data = connection
            .read(cursor, chunk)
            .with_context(|| format!("Failed to read flash at 0x{cursor:08x}"))?;
        writer.write_all(&data)?;
        cursor = cursor
            .checked_add(chunk)
            .ok_or_else(|| anyhow!("Read address overflow"))?;
        remaining -= chunk;
    }

    finish_rpt.finish("Flash read complete");
    if use_terminal_output(&on_event) {
        println!("Read {} bytes to {}", length, output.display());
    } else {
        report(
            &on_event,
            format!("Read {} bytes to {}", length, output.display()),
            LogTag::Ok,
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared write helper
// ---------------------------------------------------------------------------

const FLASH_PAGE_SIZE: u32 = 256;
const FLASH_SECTOR_SIZE: u32 = 4096;

fn open_picoboot(
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    on_event: &Option<EventCallback>,
) -> Result<PicobootConnection> {
    report(on_event, "Opening PICOBOOT interface", LogTag::Info);
    let spinner = if use_terminal_output(on_event) {
        Some(ui::spinner("Opening PICOBOOT interface..."))
    } else {
        None
    };
    let result =
        PicobootConnection::open_after_reset(vid, pid, Duration::from_secs(bootsel_timeout_secs));
    match (&spinner, &result) {
        (Some(spinner), Ok(_)) => spinner.finish_with_message("PICOBOOT interface ready"),
        (Some(spinner), Err(_)) => spinner.abandon_with_message("PICOBOOT interface unavailable"),
        _ => {}
    }
    result
}

fn run_picoboot_write(
    regions: Vec<WriteRegion>,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: &Option<EventCallback>,
) -> Result<()> {
    let total: u64 = regions.iter().map(|region| region.data.len() as u64).sum();
    let reporter = ProgressReporter::progress(on_event, total);
    let finish_rpt = reporter.clone();
    let mut progress = reporter;
    let mut connection = open_picoboot(vid, pid, bootsel_timeout_secs, on_event)?;
    connection.exit_xip().context("Failed to exit XIP mode")?;

    let mut first_addr = None;
    for region in regions {
        if region.data.is_empty() {
            continue;
        }
        first_addr = Some(first_addr.map_or(region.address, |addr: u32| addr.min(region.address)));
        write_region(&mut connection, &region, &mut progress)?;
    }

    finish_rpt.finish("Flash written");

    if no_wait {
        if use_terminal_output(on_event) {
            println!("Write complete (device left in BOOTSEL mode)");
        } else {
            report(
                on_event,
                "Write complete (device left in BOOTSEL mode)",
                LogTag::Ok,
            );
        }
        return Ok(());
    }

    connection
        .reboot_flash_update(first_addr.unwrap_or(0x1000_0000))
        .context("Failed to reboot device after write")?;
    if use_terminal_output(on_event) {
        println!("Write complete");
    } else {
        report(on_event, "Write complete", LogTag::Ok);
    }
    Ok(())
}

fn run_picoboot_erase(
    flash_size: u32,
    base_addr: u32,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: &Option<EventCallback>,
) -> Result<()> {
    let reporter = ProgressReporter::progress(on_event, flash_size as u64);
    let finish_rpt = reporter.clone();
    let mut progress = reporter;
    let mut connection = open_picoboot(vid, pid, bootsel_timeout_secs, on_event)?;
    connection.exit_xip().context("Failed to exit XIP mode")?;

    let end = base_addr
        .checked_add(flash_size)
        .ok_or_else(|| anyhow!("Erase range overflows u32 address space"))?;
    let mut addr = base_addr;
    while addr < end {
        connection
            .flash_erase(addr, FLASH_SECTOR_SIZE)
            .with_context(|| format!("Failed to erase flash sector at 0x{addr:08x}"))?;
        progress.inc(FLASH_SECTOR_SIZE as u64);
        addr += FLASH_SECTOR_SIZE;
    }

    finish_rpt.finish("Flash erased");
    if no_wait {
        if use_terminal_output(on_event) {
            println!("Erase complete (device left in BOOTSEL mode)");
        } else {
            report(
                on_event,
                "Erase complete (device left in BOOTSEL mode)",
                LogTag::Ok,
            );
        }
        return Ok(());
    }

    connection
        .reboot_application()
        .context("Failed to reboot device after erase")?;
    if use_terminal_output(on_event) {
        println!("Erase complete");
    } else {
        report(on_event, "Erase complete", LogTag::Ok);
    }
    Ok(())
}

fn write_region(
    connection: &mut PicobootConnection,
    region: &WriteRegion,
    progress: &mut ProgressReporter,
) -> Result<()> {
    let start = region.address;
    let data_len = u32::try_from(region.data.len()).context("Write target is too large")?;
    let end = start
        .checked_add(data_len)
        .ok_or_else(|| anyhow!("Write range overflows u32 address space"))?;
    let sector_start = align_down(start, FLASH_SECTOR_SIZE);
    let sector_end = align_up(end, FLASH_SECTOR_SIZE)?;

    log::info!(
        "Direct write: {} @ 0x{:08x} ({} bytes), sector range 0x{:08x}..0x{:08x}",
        region.label,
        start,
        region.data.len(),
        sector_start,
        sector_end
    );

    let mut sector_addr = sector_start;
    while sector_addr < sector_end {
        let sector_data_start = sector_addr.max(start);
        let sector_data_end = sector_addr
            .checked_add(FLASH_SECTOR_SIZE)
            .ok_or_else(|| anyhow!("Sector address overflow"))?
            .min(end);

        let sector_offset = usize::try_from(sector_data_start - sector_addr).unwrap();
        let region_offset = usize::try_from(sector_data_start - start).unwrap();
        let copy_len = usize::try_from(sector_data_end - sector_data_start).unwrap();
        let sector_data = if sector_offset == 0 && copy_len == FLASH_SECTOR_SIZE as usize {
            region.data[region_offset..region_offset + copy_len].to_vec()
        } else {
            let mut data = connection
                .read(sector_addr, FLASH_SECTOR_SIZE)
                .with_context(|| format!("Failed to read flash sector at 0x{sector_addr:08x}"))?;
            data[sector_offset..sector_offset + copy_len]
                .copy_from_slice(&region.data[region_offset..region_offset + copy_len]);
            data
        };

        connection
            .flash_erase(sector_addr, FLASH_SECTOR_SIZE)
            .with_context(|| format!("Failed to erase flash sector at 0x{sector_addr:08x}"))?;

        let mut page_addr = sector_addr;
        for page in sector_data.chunks(FLASH_PAGE_SIZE as usize) {
            connection
                .write(page_addr, page)
                .with_context(|| format!("Failed to write flash page at 0x{page_addr:08x}"))?;
            progress.inc(overlap_len(page_addr, FLASH_PAGE_SIZE, start, end) as u64);
            page_addr += FLASH_PAGE_SIZE;
        }

        sector_addr += FLASH_SECTOR_SIZE;
    }
    Ok(())
}

fn overlap_len(addr: u32, len: u32, start: u32, end: u32) -> u32 {
    let page_end = addr.saturating_add(len);
    page_end.min(end).saturating_sub(addr.max(start))
}

fn align_down(value: u32, alignment: u32) -> u32 {
    value & !(alignment - 1)
}

fn align_up(value: u32, alignment: u32) -> Result<u32> {
    let add = alignment - 1;
    let rounded = value
        .checked_add(add)
        .ok_or_else(|| anyhow!("Address alignment overflow"))?;
    Ok(align_down(rounded, alignment))
}

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
            report(
                on_event,
                format!("BOOTSEL drive: {}", m.display()),
                LogTag::Info,
            );
            m
        }
        None => {
            log::info!(
                "No BOOTSEL drive found — resetting device (VID {:04x} PID {:04x})",
                vid.unwrap_or(usb::DEFAULT_VID),
                pid.unwrap_or(usb::DEFAULT_PID),
            );
            report(
                on_event,
                "Resetting device to BOOTSEL mode\u{2026}",
                LogTag::Info,
            );
            usb::reset_to_bootsel(vid, pid)?;
            let maybe_spin = if use_terminal_output(on_event) {
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
                report(
                    on_event,
                    format!("BOOTSEL drive: {}", m.display()),
                    LogTag::Ok,
                );
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
        if use_terminal_output(on_event) {
            println!("{op_name} complete (device left in BOOTSEL mode)");
        } else {
            report(
                on_event,
                format!("{op_name} complete (device left in BOOTSEL mode)"),
                LogTag::Ok,
            );
        }
        return Ok(());
    }

    let maybe_spin2 = if use_terminal_output(on_event) {
        Some(ui::spinner("Waiting for device to reboot\u{2026}"))
    } else {
        report(
            on_event,
            "Waiting for device to reboot\u{2026}",
            LogTag::Info,
        );
        None
    };
    bootsel::wait_for_bootsel_unmount(Duration::from_secs(15))
        .inspect_err(|_| {
            if let Some(ref s) = maybe_spin2 {
                s.abandon();
            } else {
                report(
                    on_event,
                    "Timed out waiting for device to reboot",
                    LogTag::Err,
                );
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

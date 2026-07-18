use anyhow::{Context, Result};
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use crate::event::{EventCallback, LogTag, report};
use crate::progress::{ProgressReporter, ProgressWriter};
use crate::uf2::Family;
use crate::{bootsel, elf, uf2, ui, usb, write};

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// Check whether a file starts with the ELF magic bytes.
fn is_elf_file(f: &mut File) -> Result<bool> {
    let mut magic = [0u8; 4];
    let n = f.read(&mut magic).context("Failed to read file magic")?;
    Ok(n == 4 && magic == ELF_MAGIC)
}

/// Write an ELF or raw binary using the direct PICOBOOT backend.
///
/// If no PICOBOOT interface is detected, the device is first reset into BOOTSEL mode.
/// Input type is detected by ELF magic (`0x7FELF`); everything else is treated as
/// a raw binary placed at `base_addr`.
///
/// Pass `on_event: Some(cb)` to receive structured progress events instead of
/// the default indicatif terminal output.  `None` preserves the existing CLI
/// behavior unchanged.
#[allow(clippy::too_many_arguments)]
pub fn flash(
    input_path: &Path,
    family: Family,
    base_addr: u32,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: Option<EventCallback>,
) -> Result<()> {
    let mut in_file = File::open(input_path)
        .with_context(|| format!("Failed to open input file {}", input_path.display()))?;

    let elf_input = is_elf_file(&mut in_file)?;
    in_file
        .seek(SeekFrom::Start(0))
        .context("Failed to rewind input file")?;

    if elf_input {
        log::info!(
            "ELF -> UF2 layout -> direct PICOBOOT write, family {:?}",
            family
        );
        let mut uf2_data = Vec::new();
        elf::elf2uf2(BufReader::new(in_file), &mut uf2_data, family)
            .map_err(anyhow::Error::from)
            .context("UF2 conversion failed (primary image)")?;
        let regions = uf2::uf2_to_regions(&uf2_data)?
            .into_iter()
            .enumerate()
            .map(|(i, (address, data))| (format!("{}#{i}", input_path.display()), address, data))
            .collect();
        return write::write_regions_data(
            regions,
            vid,
            pid,
            bootsel_timeout_secs,
            no_wait,
            on_event,
        );
    }

    let target = write::WriteTarget {
        path: input_path.to_path_buf(),
        address: base_addr,
    };
    write::write_data(
        &[target],
        false,
        family,
        vid,
        pid,
        bootsel_timeout_secs,
        no_wait,
        on_event,
    )
}

/// UF2 mass-storage implementation retained as a compatibility backend.
#[allow(clippy::too_many_arguments)]
pub fn flash_uf2(
    input_path: &Path,
    family: Family,
    base_addr: u32,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
    on_event: Option<EventCallback>,
) -> Result<()> {
    let mount = match bootsel::find_bootsel_drive() {
        Some(m) => {
            log::info!("BOOTSEL drive already mounted at {}", m.display());
            report(
                &on_event,
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
                &on_event,
                "Resetting device to BOOTSEL mode\u{2026}",
                LogTag::Info,
            );
            usb::reset_to_bootsel(vid, pid)?;
            let maybe_spin = if on_event.is_none() {
                Some(ui::spinner("Waiting for BOOTSEL drive\u{2026}"))
            } else {
                report(&on_event, "Waiting for BOOTSEL drive\u{2026}", LogTag::Info);
                None
            };
            let m = bootsel::wait_for_bootsel_drive(Duration::from_secs(bootsel_timeout_secs))
                .inspect_err(|_| {
                    if let Some(ref s) = maybe_spin {
                        s.abandon();
                    } else {
                        report(
                            &on_event,
                            "Timed out waiting for BOOTSEL drive",
                            LogTag::Err,
                        );
                    }
                })?;
            if let Some(spin) = maybe_spin {
                spin.finish_with_message(format!("BOOTSEL drive: {}", m.display()));
            } else {
                report(
                    &on_event,
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

    let mut in_file = File::open(input_path)
        .with_context(|| format!("Failed to open input file {}", input_path.display()))?;

    let elf_input = is_elf_file(&mut in_file)?;
    in_file
        .seek(SeekFrom::Start(0))
        .context("Failed to rewind input file")?;

    // For inputs under 16 MiB, convert the entire UF2 into a Vec<u8> first so
    // the exact byte count is known before any write starts.  This gives a
    // determinate progress bar with a reliable ETA.  Inputs at or above the
    // threshold are streamed directly to avoid excessive memory use (spinner).
    const IN_MEMORY_THRESHOLD: u64 = 16 * 1024 * 1024;
    let file_size = in_file.metadata().map(|m| m.len()).unwrap_or(u64::MAX);

    let write_result: Result<()> = if file_size < IN_MEMORY_THRESHOLD {
        // --- In-memory path: convert fully, then write with exact byte count ---
        let mut buf: Vec<u8> = Vec::new();

        let convert = if elf_input {
            log::info!("ELF → UF2 (in-memory, family {:?})", family);
            elf::elf2uf2(BufReader::new(in_file), &mut buf, family).map_err(anyhow::Error::from)
        } else {
            log::info!(
                "Raw binary → UF2 (in-memory, base 0x{:08x}, family {:?})",
                base_addr,
                family
            );
            uf2::bin2uf2(in_file, &mut buf, base_addr, family as u32)
        };
        if let Err(e) = convert {
            return Err(e.context("UF2 conversion failed (primary image)"));
        }

        let reporter = ProgressReporter::progress(&on_event, buf.len() as u64);
        let finish_rpt = reporter.clone();
        let mut pw = ProgressWriter::new(out_file, reporter);
        let r = pw
            .write_all(&buf)
            .context("Failed to write UF2 to BOOTSEL drive");
        if r.is_err() {
            finish_rpt.abandon("Write failed");
        } else {
            finish_rpt.finish("UF2 written");
        }
        r
    } else {
        // --- Streaming path: output size unknown, use spinner / unbounded callback ---
        let reporter = ProgressReporter::spinner(&on_event);
        let finish_rpt = reporter.clone();
        let pw = ProgressWriter::new(out_file, reporter);
        let r = if elf_input {
            log::info!("ELF → UF2 (streaming, family {:?})", family);
            elf::elf2uf2(BufReader::new(in_file), pw, family).map_err(anyhow::Error::from)
        } else {
            log::info!(
                "Raw binary → UF2 (streaming, base 0x{:08x}, family {:?})",
                base_addr,
                family
            );
            uf2::bin2uf2(in_file, pw, base_addr, family as u32)
        };
        if r.is_err() {
            finish_rpt.abandon("Write failed");
        } else {
            finish_rpt.finish("UF2 written");
        }
        r
    };

    if let Err(e) = write_result {
        let _ = fs::remove_file(&out_path);
        return Err(e.context("Flash failed; partial UF2 file removed"));
    }

    if no_wait {
        log::info!("--no-wait: skipping reboot wait");
        if on_event.is_some() {
            report(
                &on_event,
                "Flash complete (device left in BOOTSEL mode)",
                LogTag::Ok,
            );
        } else {
            println!("Flash complete (device left in BOOTSEL mode)");
        }
        return Ok(());
    }

    let maybe_spin2 = if on_event.is_none() {
        Some(ui::spinner("Waiting for device to reboot\u{2026}"))
    } else {
        report(
            &on_event,
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
                    &on_event,
                    "Timed out waiting for device to reboot",
                    LogTag::Err,
                );
            }
        })
        .context("Device did not unmount BOOTSEL drive after flashing")?;
    if let Some(spin) = maybe_spin2 {
        spin.finish_with_message("Flash complete");
    } else {
        report(&on_event, "Flash complete", LogTag::Ok);
    }
    Ok(())
}

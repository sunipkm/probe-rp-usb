use anyhow::{Context, Result};
use elf2uf2_core::Family;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::Duration;

use crate::{bootsel, uf2, ui, usb};

// ---------------------------------------------------------------------------
// Progress-reporting writer
// ---------------------------------------------------------------------------

/// Wraps any `Write` implementation and advances a `ProgressBar` with every
/// byte written, so callers need not know about progress reporting.
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

const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];

/// Check whether a file starts with the ELF magic bytes.
fn is_elf_file(f: &mut File) -> Result<bool> {
    let mut magic = [0u8; 4];
    let n = f.read(&mut magic).context("Failed to read file magic")?;
    Ok(n == 4 && magic == ELF_MAGIC)
}

/// Convert an ELF or raw binary to UF2 and write it to the mounted BOOTSEL drive.
///
/// If no BOOTSEL drive is detected, the device is first reset into BOOTSEL mode.
/// Input type is detected by ELF magic (`0x7FELF`); everything else is treated as
/// a raw binary placed at `base_addr`.
pub fn flash(
    input_path: &Path,
    family: Family,
    base_addr: u32,
    vid: Option<u16>,
    pid: Option<u16>,
    bootsel_timeout_secs: u64,
    no_wait: bool,
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
            elf2uf2_core::elf2uf2(BufReader::new(in_file), &mut buf, family)
                .map_err(anyhow::Error::from)
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
        let r = pw
            .write_all(&buf)
            .context("Failed to write UF2 to BOOTSEL drive");
        if r.is_err() {
            bar.abandon_with_message("Write failed");
        } else {
            bar.finish_with_message("UF2 written");
        }
        r
    } else {
        // --- Streaming path: output size unknown, show spinner with byte count ---
        let bar = ProgressBar::new_spinner();
        bar.enable_steady_tick(Duration::from_millis(80));
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} Writing UF2… {bytes}")
                .unwrap()
                .tick_strings(ui::tick_chars()),
        );
        let pw = ProgressWriter {
            inner: out_file,
            bar: bar.clone(),
        };
        let r = if elf_input {
            log::info!("ELF → UF2 (streaming, family {:?})", family);
            elf2uf2_core::elf2uf2(BufReader::new(in_file), pw, family).map_err(anyhow::Error::from)
        } else {
            log::info!(
                "Raw binary → UF2 (streaming, base 0x{:08x}, family {:?})",
                base_addr,
                family
            );
            uf2::bin2uf2(in_file, pw, base_addr, family as u32)
        };
        if r.is_err() {
            bar.abandon_with_message("Write failed");
        } else {
            bar.finish_with_message("UF2 written");
        }
        r
    };

    if let Err(e) = write_result {
        let _ = fs::remove_file(&out_path);
        return Err(e.context("Flash failed; partial UF2 file removed"));
    }

    if no_wait {
        log::info!("--no-wait: skipping reboot wait");
        println!("Flash complete (device left in BOOTSEL mode)");
        return Ok(());
    }

    let spin = ui::spinner("Waiting for device to reboot…");
    bootsel::wait_for_bootsel_unmount(Duration::from_secs(15))
        .inspect_err(|_| spin.abandon())
        .context("Device did not unmount BOOTSEL drive after flashing")?;
    spin.finish_with_message("Flash complete");
    Ok(())
}

use anyhow::{Context, Result};
use elf2uf2_core::Family;
use std::fs::{self, File};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use crate::{bootsel, uf2, usb};

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
            println!("Reset sent. Waiting for BOOTSEL drive...");
            let m = bootsel::wait_for_bootsel_drive(Duration::from_secs(10))?;
            println!("BOOTSEL drive mounted at: {}", m.display());
            m
        }
    };

    let out_path = mount.join("out.uf2");
    let out_file =
        File::create(&out_path).with_context(|| format!("Failed to create {}", out_path.display()))?;

    let mut in_file = File::open(input_path)
        .with_context(|| format!("Failed to open input file {}", input_path.display()))?;

    let elf_input = is_elf_file(&mut in_file)?;
    in_file
        .seek(SeekFrom::Start(0))
        .context("Failed to rewind input file")?;

    let result = if elf_input {
        log::info!("ELF input detected — converting via elf2uf2-core (family {:?})", family);
        elf2uf2_core::elf2uf2(BufReader::new(in_file), out_file, family)
            .map_err(anyhow::Error::from)
    } else {
        log::info!(
            "Raw binary input detected — converting via bin2uf2 (base 0x{:08x}, family {:?})",
            base_addr,
            family
        );
        uf2::bin2uf2(in_file, out_file, base_addr, family as u32)
    };

    if let Err(e) = result {
        let _ = fs::remove_file(&out_path);
        return Err(e.context("Flash failed; partial UF2 file removed"));
    }

    println!("Flash complete.");
    Ok(())
}

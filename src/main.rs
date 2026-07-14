mod attach;
mod bootsel;
mod flash;
mod uf2;
mod usb;

use anyhow::Result;
use clap::{Parser, Subcommand};
use elf2uf2_core::Family;
use std::path::PathBuf;
use std::time::Duration;

/// Parse a `u16` from a decimal or `0x…` hex string.
fn parse_u16_hex(s: &str) -> Result<u16, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u16::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u16>().map_err(|e| e.to_string())
    }
}

/// Parse a `u32` from a decimal or `0x…` hex string.
fn parse_u32_hex(s: &str) -> Result<u32, String> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        u32::from_str_radix(hex, 16).map_err(|e| e.to_string())
    } else {
        s.parse::<u32>().map_err(|e| e.to_string())
    }
}

#[derive(Parser)]
#[command(
    name = "chickadee-probe",
    about = "RP2040/RP2350 flashing and defmt debug tool",
    long_about = None,
)]
struct Cli {
    /// USB Vendor ID (decimal or 0x-prefixed hex).
    /// Default: 0x2E8A (Raspberry Pi). When omitted, devices with VID 0xC0DE are
    /// also probed as a fallback.
    #[arg(long, global = true, value_parser = parse_u16_hex)]
    vid: Option<u16>,

    /// USB Product ID in app mode (decimal or 0x-prefixed hex).
    /// Default: 0x0009 (pico_stdio_usb). When omitted together with --vid, the
    /// 0xC0DE fallback scan accepts any PID.
    #[arg(long, global = true, value_parser = parse_u16_hex)]
    pid: Option<u16>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Check whether a BOOTSEL USB storage drive is currently mounted and print its path
    Check,

    /// Reset the device into BOOTSEL mode and wait for the storage drive to appear
    Reset,

    /// Convert an ELF or raw binary to UF2 and flash it to the device
    ///
    /// If no BOOTSEL drive is mounted the device is reset automatically before flashing.
    Flash {
        /// Input firmware file (ELF detected by magic bytes; anything else treated as raw binary)
        input: PathBuf,

        /// UF2 family to embed in the UF2 blocks
        #[arg(long, value_enum, default_value = "rp2350-arm-s")]
        family: Family,

        /// Flash base address used when the input is a raw binary (ignored for ELF)
        #[arg(long, value_parser = parse_u32_hex, default_value = "0x10000000")]
        address: u32,
    },

    /// Attach to the device's last serial port and decode defmt output
    Attach {
        /// ELF file built with defmt (provides the symbol table for decoding)
        elf: PathBuf,

        /// Override the auto-detected serial port
        #[arg(long)]
        port: Option<String>,
    },

    /// Like `attach` but reconnects automatically whenever the device resets or is reflashed
    Watch {
        /// ELF file built with defmt (loaded once; reused across reconnects)
        elf: PathBuf,

        /// Override the auto-detected serial port
        #[arg(long)]
        port: Option<String>,
    },

    /// Flash the firmware and immediately enter watch mode (equivalent to `probe-rs run`)
    Run {
        /// Input firmware file (ELF detected by magic bytes; anything else treated as raw binary)
        input: PathBuf,

        /// UF2 family to embed in the UF2 blocks
        #[arg(long, value_enum, default_value = "rp2350-arm-s")]
        family: Family,

        /// Flash base address used when the input is a raw binary (ignored for ELF)
        #[arg(long, value_parser = parse_u32_hex, default_value = "0x10000000")]
        address: u32,

        /// Override the auto-detected serial port
        #[arg(long)]
        port: Option<String>,
    },
}

fn main() -> Result<()> {
    env_logger::init();
    let cli = Cli::parse();

    match cli.command {
        Cmd::Check => match bootsel::find_bootsel_drive() {
            Some(path) => println!("{}", path.display()),
            None => anyhow::bail!("No BOOTSEL drive found"),
        },

        Cmd::Reset => {
            usb::reset_to_bootsel(cli.vid, cli.pid)?;
            println!("Reset sent. Waiting for BOOTSEL drive...");
            let path = bootsel::wait_for_bootsel_drive(Duration::from_secs(10))?;
            println!("BOOTSEL drive mounted at: {}", path.display());
        }

        Cmd::Flash {
            input,
            family,
            address,
        } => {
            flash::flash(&input, family, address, cli.vid, cli.pid)?;
        }

        Cmd::Attach { elf, port } => {
            let port = resolve_port(port, cli.vid, cli.pid)?;
            attach::attach(&elf, &port)?;
        }

        Cmd::Watch { elf, port } => {
            attach::watch(&elf, port, cli.vid, cli.pid)?;
        }

        Cmd::Run {
            input,
            family,
            address,
            port,
        } => {
            flash::flash(&input, family, address, cli.vid, cli.pid)?;
            attach::watch(&input, port, cli.vid, cli.pid)?;
        }
    }

    Ok(())
}

/// Resolve the serial port: use the override if provided, else auto-detect by VID/PID.
/// When neither --vid nor --pid was specified, the fallback VID 0xC0DE is also scanned.
fn resolve_port(
    port_override: Option<String>,
    vid: Option<u16>,
    pid: Option<u16>,
) -> Result<String> {
    if let Some(p) = port_override {
        return Ok(p);
    }
    attach::find_serial_port(vid, pid).ok_or({
        match vid {
            None => anyhow::anyhow!(
                "No serial port found for VID 0x2E8A (default) or VID 0xC0DE (fallback). \
                 Is the device connected and running firmware?"
            ),
            Some(v) => anyhow::anyhow!(
                "No serial port found for VID {:04x} and PID {:04x}. \
                 Is the device connected and running firmware?",
                v,
                pid.unwrap_or(usb::DEFAULT_PID),
            ),
        }
    })
}

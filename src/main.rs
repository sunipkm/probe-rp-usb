use probe_rp_usb::{attach, bootsel, flash, ui, usb, write};

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

/// Parse a `FILE@OFFSET` write target.  The offset is added to `--base` later.
///
/// Uses `rsplit_once('@')` so paths containing `@` are handled correctly.
fn parse_write_target(s: &str) -> Result<write::WriteTarget, String> {
    let (path_part, offset_part) = s
        .rsplit_once('@')
        .ok_or_else(|| format!("expected FILE@OFFSET, got {s:?}"))?;
    let address = parse_u32_hex(offset_part)?;
    Ok(write::WriteTarget {
        path: PathBuf::from(path_part),
        address,
    })
}

#[derive(Parser)]
#[command(
    name = "probe-rp-usb",
    version = option_env!("VERGEN_GIT_DESCRIBE").unwrap_or(env!("CARGO_PKG_VERSION")),
    about = "RP2040/RP2350 flashing and defmt debug tool",
    long_about = None,
)]
struct Cli {
    /// USB Vendor ID (decimal or 0x-prefixed hex).
    /// Default: 0x2E8A (Raspberry Pi). When omitted, devices with VID 0xC0DE or 0xC001 are
    /// also probed as a fallback.
    #[arg(long, global = true, value_parser = parse_u16_hex)]
    vid: Option<u16>,

    /// USB Product ID in app mode (decimal or 0x-prefixed hex).
    /// Default: 0x0009 (pico_stdio_usb). When omitted together with --vid, the
    /// 0xC0DE/0xC001 fallback scan accepts any PID.
    #[arg(long, global = true, value_parser = parse_u16_hex)]
    pid: Option<u16>,

    /// Seconds to wait for the BOOTSEL drive to appear after a reset.
    #[arg(long, global = true, default_value = "10")]
    bootsel_timeout: u64,

    /// Serial port baud rate used for defmt output.
    #[arg(long, global = true, default_value = "115200")]
    baud: u32,

    /// Serial read timeout in milliseconds (how long to wait for data before polling again).
    #[arg(long, global = true, default_value = "100")]
    read_timeout_ms: u64,

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

        /// Do not wait for the device to reboot after flashing (leaves device in BOOTSEL mode).
        /// Useful when writing data partitions that should not trigger a firmware reset.
        #[arg(long)]
        no_wait: bool,
    },

    /// Write one or more raw binary images to flash at specific addresses
    ///
    /// Each FILE@OFFSET argument specifies a binary file and its offset relative
    /// to --base (default 0x0, i.e. offsets are absolute addresses).  All images
    /// are written as a single UF2 file so the device resets exactly once.
    Write {
        /// One or more `FILE@OFFSET` targets, e.g. `data.bin@0x100000`.
        /// Offsets are added to --base to produce the final flash address.
        #[arg(required = true, value_parser = parse_write_target, value_name = "FILE@OFFSET")]
        targets: Vec<write::WriteTarget>,

        /// Base address added to every offset (decimal or 0x-prefixed hex).
        /// Use this to address regions relative to a partition start.
        /// Default: 0x0 (offsets are treated as absolute flash addresses).
        #[arg(long, value_parser = parse_u32_hex, default_value = "0x0")]
        base: u32,

        /// UF2 family to embed in the UF2 blocks
        #[arg(long, value_enum, default_value = "rp2350-arm-s")]
        family: Family,

        /// Prepend a 256-byte block of 0xFF at 0x10000000 (the start of flash)
        /// to invalidate the existing firmware header before the data is written.
        /// This prevents the device from booting stale firmware if it resets
        /// mid-transfer.
        #[arg(long)]
        erase_boot: bool,
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

    /// Erase the entire flash by writing 0xFF to every page
    ///
    /// Generates a UF2 file that covers all FLASH_SIZE bytes starting at --base,
    /// with every byte set to 0xFF.  This restores flash to its erased state,
    /// removing any existing firmware or data.
    Erase {
        /// Total flash size in bytes (decimal or 0x-prefixed hex).
        /// Common values: 0x200000 (2 MiB), 0x400000 (4 MiB), 0x800000 (8 MiB).
        #[arg(value_parser = parse_u32_hex)]
        flash_size: u32,

        /// Flash start address (decimal or 0x-prefixed hex)
        #[arg(long, value_parser = parse_u32_hex, default_value = "0x10000000")]
        base: u32,

        /// UF2 family to embed in the UF2 blocks
        #[arg(long, value_enum, default_value = "rp2350-arm-s")]
        family: Family,
    },
}

fn main() {
    env_logger::init();
    if let Err(e) = run(Cli::parse()) {
        eprintln!("Error: {e:?}");
        #[cfg(target_os = "linux")]
        if is_permission_error(&e) {
            eprintln!("\n{}", usb::udev_hint());
        }
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Cmd::Check => match bootsel::find_bootsel_drive() {
            Some(path) => println!("{}", path.display()),
            None => anyhow::bail!("No BOOTSEL drive found"),
        },

        Cmd::Reset => {
            usb::reset_to_bootsel(cli.vid, cli.pid)?;
            let spin = ui::spinner("Waiting for BOOTSEL drive…");
            let path = bootsel::wait_for_bootsel_drive(Duration::from_secs(cli.bootsel_timeout))
                .inspect_err(|_| spin.abandon())?;
            spin.finish_with_message(format!("BOOTSEL drive: {}", path.display()));
        }

        Cmd::Flash {
            input,
            family,
            address,
            no_wait,
        } => {
            flash::flash(
                &input,
                family,
                address,
                cli.vid,
                cli.pid,
                cli.bootsel_timeout,
                no_wait,
            )?;
        }

        Cmd::Write {
            targets,
            base,
            family,
            erase_boot,
        } => {
            let targets: Vec<write::WriteTarget> = targets
                .into_iter()
                .map(|t| write::WriteTarget {
                    path: t.path,
                    address: base.wrapping_add(t.address),
                })
                .collect();
            write::write_data(
                &targets,
                erase_boot,
                family,
                cli.vid,
                cli.pid,
                cli.bootsel_timeout,
            )?;
        }

        Cmd::Attach { elf, port } => {
            let port = resolve_port(port, cli.vid, cli.pid)?;
            attach::attach(&elf, &port, cli.baud, cli.read_timeout_ms)?;
        }

        Cmd::Watch { elf, port } => {
            attach::watch(&elf, port, cli.vid, cli.pid, cli.baud, cli.read_timeout_ms)?;
        }

        Cmd::Run {
            input,
            family,
            address,
            port,
        } => {
            flash::flash(
                &input,
                family,
                address,
                cli.vid,
                cli.pid,
                cli.bootsel_timeout,
                false,
            )?;
            attach::watch(
                &input,
                port,
                cli.vid,
                cli.pid,
                cli.baud,
                cli.read_timeout_ms,
            )?;
        }

        Cmd::Erase {
            flash_size,
            base,
            family,
        } => {
            write::erase_flash(
                flash_size,
                base,
                family,
                cli.vid,
                cli.pid,
                cli.bootsel_timeout,
            )?;
        }
    }

    Ok(())
}

/// Return `true` when any error in the chain looks like a USB/serial permission denial.
/// Used on Linux to decide whether to print the udev setup hint.
#[cfg(target_os = "linux")]
fn is_permission_error(e: &anyhow::Error) -> bool {
    e.chain().any(|cause| {
        let msg = cause.to_string();
        msg.contains("Permission denied") || msg.contains("Access denied")
    })
}

/// Resolve the serial port: use the override if provided, else auto-detect by VID/PID.
/// When neither --vid nor --pid was specified, the fallback VIDs 0xC0DE and 0xC001 are also scanned.
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
                "No serial port found for VID 0x2E8A (default) or VID 0xC0DE/0xC001 (fallback). \
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

use anyhow::{Context, Result};
use defmt_decoder::{DecodeError, StreamDecoder, Table};
use serialport::SerialPortType;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::usb::{DEFAULT_PID, DEFAULT_VID, FALLBACK_VID};

/// Scan available serial ports, filter by VID and (optionally) PID, sort by name, return last.
fn find_port_by_vid_pid(vid: u16, pid: Option<u16>) -> Option<String> {
    let mut ports: Vec<String> = serialport::available_ports()
        .ok()?
        .into_iter()
        .filter_map(|p| {
            if let SerialPortType::UsbPort(info) = &p.port_type {
                let vid_ok = info.vid == vid;
                let pid_ok = pid.is_none_or(|expected| info.pid == expected);
                if vid_ok && pid_ok {
                    return Some(p.port_name);
                }
            }
            None
        })
        .collect();
    ports.sort();
    ports.into_iter().last()
}

/// Find the last serial port matching the given VID/PID.
///
/// When `vid` is `None` (user did not specify `--vid`):
/// 1. Try the default RPI VID `0x2E8A` with the default/given PID.
/// 2. Fall back to any port with VID `0xC0DE` (any PID).
pub fn find_serial_port(vid: Option<u16>, pid: Option<u16>) -> Option<String> {
    let primary_vid = vid.unwrap_or(DEFAULT_VID);
    let primary_pid = pid.unwrap_or(DEFAULT_PID);

    if let Some(port) = find_port_by_vid_pid(primary_vid, Some(primary_pid)) {
        return Some(port);
    }

    if vid.is_none()
        && let Some(port) = find_port_by_vid_pid(FALLBACK_VID, None) {
            log::info!("Primary serial port not found; using fallback VID {:04x}", FALLBACK_VID);
            return Some(port);
        }

    None
}

/// Load the defmt `Table` from an ELF file.
fn load_table(elf_path: &Path) -> Result<Table> {
    let elf_bytes = fs::read(elf_path)
        .with_context(|| format!("Failed to read ELF: {}", elf_path.display()))?;
    Table::parse(&elf_bytes)
        .context("Failed to parse defmt table from ELF")?
        .ok_or_else(|| anyhow::anyhow!("ELF file contains no .defmt section — was it built with defmt?"))
}

/// Open `port_name`, feed received bytes through the `StreamDecoder`, and print decoded frames.
///
/// Returns when the serial port closes/errors.  `DecodeError::UnexpectedEof` signals that
/// more bytes are needed (normal); `DecodeError::Malformed` is handled based on the
/// encoding's recovery capability.
fn run_decode_loop(table: &Table, port_name: &str) -> Result<()> {
    let mut decoder = table.new_stream_decoder();

    let mut port = serialport::new(port_name, 115200)
        .timeout(Duration::from_millis(100))
        .open()
        .with_context(|| format!("Failed to open serial port {}", port_name))?;

    let mut buf = [0u8; 1024];

    loop {
        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                decoder.received(&buf[..n]);
                drain_frames(&mut *decoder, table)?;
            }
            Err(ref e) if e.kind() == ErrorKind::TimedOut => {
                // No data in this 100 ms window — normal.
            }
            Err(e) => {
                return Err(anyhow::Error::from(e).context("Serial read error"));
            }
        }
    }
}

/// Drain all currently decodable frames from `decoder`, printing each one.
fn drain_frames(decoder: &mut dyn StreamDecoder, table: &Table) -> Result<()> {
    loop {
        match decoder.decode() {
            Ok(frame) => {
                println!("{}", frame.display(true));
            }
            Err(DecodeError::UnexpectedEof) => break,
            Err(DecodeError::Malformed) => {
                if table.encoding().can_recover() {
                    log::warn!("Malformed defmt frame skipped (encoding can recover)");
                } else {
                    return Err(anyhow::anyhow!(
                        "Malformed defmt frame — encoding cannot recover; aborting"
                    ));
                }
                break;
            }
        }
    }
    Ok(())
}

/// Attach to the serial port and decode defmt output.  Exits when the port is closed or on error.
pub fn attach(elf_path: &Path, port_name: &str) -> Result<()> {
    let table = load_table(elf_path)?;
    println!("Attached to {} (Ctrl+C to quit)", port_name);
    run_decode_loop(&table, port_name)
}

/// Like `attach`, but reconnects automatically whenever the device disconnects.
///
/// The defmt `Table` is loaded once and reused across reconnects so no ELF re-read is needed.
/// If `port_override` is `None`, the port is discovered by VID/PID on each (re-)connection.
pub fn watch(
    elf_path: &Path,
    port_override: Option<String>,
    vid: Option<u16>,
    pid: Option<u16>,
) -> Result<()> {
    let table = load_table(elf_path)?;

    loop {
        let port_name = match port_override.as_deref() {
            Some(p) => p.to_owned(),
            None => match wait_for_serial_port(vid, pid, Duration::from_secs(30)) {
                Some(p) => p,
                None => {
                    let pid_str = pid
                        .map(|p| format!("PID {:04x}", p))
                        .unwrap_or_else(|| "any PID".into());
                    eprintln!(
                        "Timed out waiting for serial port \
                         (VID {:04x} {} / fallback VID {:04x}) — retrying",
                        vid.unwrap_or(DEFAULT_VID),
                        pid_str,
                        FALLBACK_VID,
                    );
                    continue;
                }
            },
        };

        println!("Connecting to {}...", port_name);

        match run_decode_loop(&table, &port_name) {
            Ok(()) => break,
            Err(e) => {
                eprintln!("Disconnected: {:#}", e);
                if port_override.is_some() {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    Ok(())
}

/// Poll until a serial port appears (using the fallback scan when `vid` is `None`), or timeout.
fn wait_for_serial_port(vid: Option<u16>, pid: Option<u16>, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(port) = find_serial_port(vid, pid) {
            return Some(port);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}

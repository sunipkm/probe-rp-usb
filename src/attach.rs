use anyhow::{Context, Result};
use defmt_decoder::{DecodeError, StreamDecoder, Table};
use serialport::SerialPortType;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::event::{EventCallback, LogTag, ProbeEvent};
use crate::usb::{self, DEFAULT_PID, DEFAULT_VID, FALLBACK_VIDS};

/// Return a sort key that orders port names naturally, treating a trailing digit
/// run as a number.  This ensures "COM10" sorts after "COM3" on Windows, and
/// "/dev/ttyACM10" after "/dev/ttyACM2" on Linux.
fn port_sort_key(name: &str) -> (String, u64) {
    let split = name.len()
        - name
            .as_bytes()
            .iter()
            .rev()
            .take_while(|b| b.is_ascii_digit())
            .count();
    let (prefix, num_str) = name.split_at(split);
    (prefix.to_lowercase(), num_str.parse().unwrap_or(0))
}

/// Find the serial port for a specific USB CDC interface by matching the
/// interface number reported by the OS.
///
/// `ctrl` is the CDC Control interface number; `data` is the CDC Data interface
/// number (always `ctrl + 1`).  Windows and Linux report the *control* number;
/// macOS reports the *data* number.  Accepting both makes this function
/// platform-agnostic.
fn find_port_by_interface(vid: u16, pid: u16, ctrl: u8, data: u8) -> Option<String> {
    serialport::available_ports()
        .ok()?
        .into_iter()
        .find_map(|p| {
            if let SerialPortType::UsbPort(info) = p.port_type
                && info.vid == vid
                && info.pid == pid
                && matches!(info.interface, Some(n) if n == ctrl || n == data)
            {
                return Some(p.port_name);
            }
            None
        })
}

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
    ports.sort_by_key(|p| port_sort_key(p));
    ports.into_iter().last()
}

/// Find the serial port for defmt output, using the most specific method available.
///
/// Discovery order:
/// 1. **Interface string descriptor** (robust): open the USB device, find the
///    CDC-ACM interface whose `iInterface` string contains "defmt", and return
///    the OS serial-port name bound to that exact interface.  Requires the
///    firmware to label the interface (e.g. `iInterface = "defmt"`) and, on
///    Windows, the WinUSB driver to be installed for the device.
/// 2. **VID/PID heuristic** (fallback): pick the highest-numbered serial port
///    matching the given VID/PID by natural sort.  Works without string
///    descriptors but relies on the defmt port being the last enumerated one.
/// 3. **Fallback VID `0xC0DE`** (fallback, only when `--vid` is not set).
pub fn find_serial_port(vid: Option<u16>, pid: Option<u16>) -> Option<String> {
    let primary_vid = vid.unwrap_or(DEFAULT_VID);
    let primary_pid = pid.unwrap_or(DEFAULT_PID);

    // 1. Interface string descriptor — precise, platform-agnostic.
    if let Some((ctrl, data)) = usb::find_defmt_interface(primary_vid, primary_pid)
        && let Some(port) = find_port_by_interface(primary_vid, primary_pid, ctrl, data)
    {
        return Some(port);
    }
    // Descriptor found but port not yet visible (device still enumerating) —
    // fall through so the heuristic can retry on the next poll cycle.

    // 2. VID/PID heuristic.
    if let Some(port) = find_port_by_vid_pid(primary_vid, Some(primary_pid)) {
        return Some(port);
    }

    // 3. Fallback VIDs.
    if vid.is_none() {
        for &fvid in FALLBACK_VIDS {
            if let Some(port) = find_port_by_vid_pid(fvid, None) {
                log::info!(
                    "Primary serial port not found; using fallback VID {:04x}",
                    fvid
                );
                return Some(port);
            }
        }
    }

    None
}

/// Load the defmt `Table` from an ELF file.
fn load_table(elf_path: &Path) -> Result<Table> {
    let elf_bytes = fs::read(elf_path)
        .with_context(|| format!("Failed to read ELF: {}", elf_path.display()))?;
    Table::parse(&elf_bytes)
        .context("Failed to parse defmt table from ELF")?
        .ok_or_else(|| {
            anyhow::anyhow!("ELF file contains no .defmt section — was it built with defmt?")
        })
}

/// Open `port_name`, feed received bytes through the `StreamDecoder`, and
/// print decoded frames (or route them through `on_event` when `Some`).
///
/// Returns when the serial port closes/errors or `stop_flag` is set to `true`.
fn run_decode_loop(
    table: &Table,
    port_name: &str,
    baud: u32,
    read_timeout_ms: u64,
    on_event: &Option<EventCallback>,
    stop_flag: &Option<Arc<AtomicBool>>,
) -> Result<()> {
    let mut decoder = table.new_stream_decoder();

    let mut port = serialport::new(port_name, baud)
        .timeout(Duration::from_millis(read_timeout_ms))
        .open()
        .with_context(|| format!("Failed to open serial port {}", port_name))?;

    let mut buf = [0u8; 1024];

    loop {
        // Check the stop flag after each read-timeout window (max 100 ms latency).
        if stop_flag
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
        {
            return Ok(());
        }

        match port.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => {
                decoder.received(&buf[..n]);
                drain_frames(&mut *decoder, table, on_event)?;
            }
            Err(ref e) if e.kind() == ErrorKind::TimedOut => {
                // No data in this read_timeout_ms window — normal.
            }
            Err(e) => {
                return Err(anyhow::Error::from(e).context("Serial read error"));
            }
        }
    }
}

/// Drain all currently decodable frames from `decoder`, routing each one
/// through `on_event` (when `Some`) or printing to stdout (when `None`).
fn drain_frames(
    decoder: &mut dyn StreamDecoder,
    table: &Table,
    on_event: &Option<EventCallback>,
) -> Result<()> {
    loop {
        match decoder.decode() {
            Ok(frame) => {
                let text = frame.display(true).to_string();
                match on_event {
                    Some(cb) => cb(ProbeEvent::Frame(text)),
                    None => println!("{}", text),
                }
            }
            Err(DecodeError::UnexpectedEof) => break,
            Err(DecodeError::Malformed) => {
                if table.encoding().can_recover() {
                    match on_event {
                        Some(cb) => cb(ProbeEvent::Log {
                            msg: "Malformed defmt frame skipped (encoding can recover)".to_owned(),
                            tag: LogTag::Warn,
                        }),
                        None => log::warn!("Malformed defmt frame skipped (encoding can recover)"),
                    }
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

/// Attach to the serial port and decode defmt output.
///
/// Returns when the port closes, on error, or when `stop_flag` is set.
/// Pass `on_event: Some(cb)` to receive frames via callback instead of stdout.
pub fn attach(
    elf_path: &Path,
    port_name: &str,
    baud: u32,
    read_timeout_ms: u64,
    on_event: Option<EventCallback>,
    stop_flag: Option<Arc<AtomicBool>>,
) -> Result<()> {
    let table = load_table(elf_path)?;
    match &on_event {
        Some(cb) => cb(ProbeEvent::Connected {
            port: port_name.to_owned(),
        }),
        None => println!("Attached to {} (Ctrl+C to quit)", port_name),
    }
    run_decode_loop(
        &table,
        port_name,
        baud,
        read_timeout_ms,
        &on_event,
        &stop_flag,
    )
}

/// Like `attach`, but reconnects automatically whenever the device disconnects.
///
/// The defmt `Table` is loaded once and reused across reconnects.
/// Returns when `stop_flag` is set, a fatal error occurs, or (if
/// `port_override` is set) the connection closes.
#[allow(clippy::too_many_arguments)]
pub fn watch(
    elf_path: &Path,
    port_override: Option<String>,
    vid: Option<u16>,
    pid: Option<u16>,
    baud: u32,
    read_timeout_ms: u64,
    on_event: Option<EventCallback>,
    stop_flag: Option<Arc<AtomicBool>>,
) -> Result<()> {
    let table = load_table(elf_path)?;

    loop {
        // Check stop flag before each reconnect attempt.
        if stop_flag
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
        {
            return Ok(());
        }

        let port_name = match port_override.as_deref() {
            Some(p) => p.to_owned(),
            None => match wait_for_serial_port(vid, pid, Duration::from_secs(30), &stop_flag) {
                Some(p) => p,
                None => {
                    // Either timed out or stop_flag was set.
                    if stop_flag
                        .as_ref()
                        .is_some_and(|f| f.load(Ordering::Relaxed))
                    {
                        return Ok(());
                    }
                    let pid_str = pid
                        .map(|p| format!("PID {:04x}", p))
                        .unwrap_or_else(|| "any PID".into());
                    let fallback_str: Vec<String> =
                        FALLBACK_VIDS.iter().map(|v| format!("{v:04x}")).collect();
                    let msg = format!(
                        "Timed out waiting for serial port \
                         (VID {:04x} {} / fallback VID {}) — retrying",
                        vid.unwrap_or(DEFAULT_VID),
                        pid_str,
                        fallback_str.join("/"),
                    );
                    match &on_event {
                        Some(cb) => cb(ProbeEvent::Log {
                            msg,
                            tag: LogTag::Warn,
                        }),
                        None => eprintln!("{}", msg),
                    }
                    continue;
                }
            },
        };

        match &on_event {
            Some(cb) => cb(ProbeEvent::Connected {
                port: port_name.clone(),
            }),
            None => println!("Connecting to {}...", port_name),
        }

        match run_decode_loop(
            &table,
            &port_name,
            baud,
            read_timeout_ms,
            &on_event,
            &stop_flag,
        ) {
            Ok(()) => break,
            Err(e) => {
                match &on_event {
                    Some(cb) => cb(ProbeEvent::Disconnected),
                    None => eprintln!("Disconnected: {:#}", e),
                }
                if port_override.is_some() {
                    std::thread::sleep(Duration::from_secs(1));
                }
            }
        }
    }

    Ok(())
}

/// Poll until a serial port appears or `timeout` elapses.
/// Returns `None` if the timeout elapses OR `stop_flag` is set.
fn wait_for_serial_port(
    vid: Option<u16>,
    pid: Option<u16>,
    timeout: Duration,
    stop_flag: &Option<Arc<AtomicBool>>,
) -> Option<String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if stop_flag
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
        {
            return None;
        }
        if let Some(port) = find_serial_port(vid, pid) {
            return Some(port);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    None
}

use anyhow::{Context, Result, anyhow};
use nusb::transfer::{ControlOut, ControlType, Recipient};
use nusb::{DeviceInfo, MaybeFuture};
use std::time::Duration;

use crate::picoboot::{PRODUCT_ID_RP_USBBOOT, PicobootConnection};

/// USB class/subclass for CDC Abstract Control Model (serial) interfaces.
const CDC_CLASS: u8 = 0x02;
const CDC_SUBCLASS_ACM: u8 = 0x02;

/// USB language ID for US-English string descriptors.
const LANG_EN_US: u16 = 0x0409;

// ---------------------------------------------------------------------------
// Linux udev hint (embedded at compile time so `cargo install` users get it)
// ---------------------------------------------------------------------------

/// Contents of the bundled udev rules file, compiled into the binary.
#[cfg(target_os = "linux")]
const UDEV_RULES: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/99-probe-rp-usb.rules"
));

/// Human-readable setup hint shown when a USB permission error is detected on Linux.
#[cfg(target_os = "linux")]
pub fn udev_hint() -> String {
    format!(
        "Hint: install the udev rules to grant non-root USB access, then reload:\n\
         \n\
         sudo tee /etc/udev/rules.d/99-probe-rp-usb.rules << 'EOF'\n\
         {UDEV_RULES}\
         EOF\n\
         sudo udevadm control --reload-rules && sudo udevadm trigger\n\
         \n\
         Also ensure your user is in the required groups (log out and in to apply):\n\
         sudo usermod -aG plugdev $USER    # USB device access\n\
         sudo usermod -aG dialout $USER    # serial port access"
    )
}

/// Defaults used when the caller passes `None` for VID/PID.
pub const DEFAULT_VID: u16 = 0x2E8A;
pub const DEFAULT_PID: u16 = 0x0009;

/// Fallback VIDs probed when the user does not explicitly specify `--vid`.
/// Any device advertising one of these vendor IDs is eligible for a reset attempt.
pub const FALLBACK_VIDS: &[u16] = &[0xC0DE, 0xC001];

// Pico USB reset interface constants (from pico-sdk usb_reset_interface.h)
const RESET_INTERFACE_SUBCLASS: u8 = 0x00;
const RESET_INTERFACE_PROTOCOL: u8 = 0x01;
const RESET_REQUEST_BOOTSEL: u8 = 0x01;

/// Find a USB device by VID/PID.
pub fn find_device(vid: u16, pid: u16) -> Option<DeviceInfo> {
    nusb::list_devices()
        .wait()
        .ok()?
        .find(|d| d.vendor_id() == vid && d.product_id() == pid)
}

fn matching_devices(vid: u16, pid: Option<u16>) -> Vec<DeviceInfo> {
    let Ok(devices) = nusb::list_devices().wait() else {
        return Vec::new();
    };

    devices
        .filter(|d| d.vendor_id() == vid && pid.is_none_or(|expected| d.product_id() == expected))
        .collect()
}

fn device_summary(info: &DeviceInfo) -> String {
    format!(
        "bus {} addr {} {:04x}:{:04x}",
        info.bus_id(),
        info.device_address(),
        info.vendor_id(),
        info.product_id(),
    )
}

/// Select a unique USB device from one or more VID/PID selectors.
///
/// Returns `Ok(None)` when no device matches any selector, and returns an
/// error when more than one device matches so the caller can ask the user to
/// narrow the selection.
pub(crate) fn select_unique_device(selectors: &[(u16, Option<u16>)]) -> Result<Option<DeviceInfo>> {
    let mut matches = Vec::new();
    for &(vid, pid) in selectors {
        matches.extend(matching_devices(vid, pid));
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => {
            let devices = matches
                .iter()
                .map(device_summary)
                .collect::<Vec<_>>()
                .join("\n- ");
            Err(anyhow!(
                "Multiple matching USB devices are connected:\n- {devices}\nUse --vid/--pid to select one, or disconnect extra probes before retrying."
            ))
        }
    }
}

/// Inspect the active USB configuration of the device at `vid`/`pid` and find
/// the CDC-ACM control interface whose `iInterface` string descriptor contains
/// "defmt" (case-insensitive).
///
/// Returns `(control_interface_num, data_interface_num)` on success.  The data
/// interface is assumed to be `control_interface_num + 1`, which matches the
/// standard CDC-ACM pairing used by Embassy and the pico-sdk.
///
/// Returns `None` when the device is not found, cannot be opened (e.g. no
/// WinUSB driver on Windows), sets no `iInterface` strings, or has no
/// interface labelled "defmt" — the caller should fall back to the port-name
/// heuristic in that case.
pub fn find_defmt_interface(vid: u16, pid: u16) -> Option<(u8, u8)> {
    let device = find_device(vid, pid)?;
    find_defmt_interface_on_device(&device)
}

/// Inspect a specific USB device and find the CDC-ACM control interface whose
/// `iInterface` string descriptor contains "defmt" (case-insensitive).
pub fn find_defmt_interface_on_device(device: &DeviceInfo) -> Option<(u8, u8)> {
    let device = device.open().wait().ok()?;
    let config = device.active_configuration().ok()?;

    for iface in config.interface_alt_settings() {
        // Only examine CDC Control (ACM) interfaces — class 0x02, subclass 0x02.
        if iface.class() != CDC_CLASS || iface.subclass() != CDC_SUBCLASS_ACM {
            continue;
        }
        let ctrl_num = iface.interface_number();
        let Some(str_idx) = iface.string_index() else {
            continue;
        };
        let Ok(label) = device
            .get_string_descriptor(str_idx, LANG_EN_US, Duration::from_millis(500))
            .wait()
        else {
            continue;
        };
        if label.to_ascii_lowercase().contains("defmt") {
            log::info!(
                "defmt CDC interface identified via string descriptor: \
                 control={ctrl_num} data={} (\"{}\")",
                ctrl_num + 1,
                label,
            );
            return Some((ctrl_num, ctrl_num + 1));
        }
    }
    None
}

/// Find the first USB device with the given VID, regardless of PID.
pub fn find_any_device_with_vid(vid: u16) -> Option<DeviceInfo> {
    nusb::list_devices()
        .wait()
        .ok()?
        .find(|d| d.vendor_id() == vid)
}

/// Reset an app-mode device into BOOTSEL via the USB reset interface class control transfer.
/// The device must expose a vendor-class interface (0xFF / 0x00 / 0x01).
fn reboot_via_reset_interface(info: &DeviceInfo) -> Result<()> {
    let device = info.open().wait().context("Failed to open USB device")?;
    let config = device
        .active_configuration()
        .map_err(|e| anyhow!("{}", e))?;

    let iface_num = config
        .interface_alt_settings()
        .find(|iface| {
            iface.class() == 0xFF
                && iface.subclass() == RESET_INTERFACE_SUBCLASS
                && iface.protocol() == RESET_INTERFACE_PROTOCOL
        })
        .map(|iface| iface.interface_number())
        .ok_or_else(|| anyhow!("USB reset interface not found on device"))?;

    let interface = device
        .claim_interface(iface_num)
        .wait()
        .context("Failed to claim reset interface")?;

    // Device resets immediately; the transfer response is often not received — ignore the result.
    let _ = interface
        .control_out(
            ControlOut {
                control_type: ControlType::Class,
                recipient: Recipient::Interface,
                request: RESET_REQUEST_BOOTSEL,
                value: 0, // disable_mask 0 = all interfaces enabled in BOOTSEL
                index: iface_num as u16,
                data: &[],
            },
            Duration::from_millis(2000),
        )
        .wait();

    Ok(())
}

/// Reboot a device already in BOOTSEL mode back into BOOTSEL via the PICOBOOT bulk protocol.
fn reboot_via_picoboot(info: &DeviceInfo) -> Result<()> {
    PicobootConnection::open_device(info)?.reboot_bootsel()
}

/// Reboot a device already in BOOTSEL mode into its normal application via the PICOBOOT bulk protocol.
fn reboot_to_normal_via_picoboot(info: &DeviceInfo) -> Result<()> {
    PicobootConnection::open_device(info)?.reboot_application()
}

/// Reset a device to BOOTSEL mode.
///
/// Strategy (applied in order):
/// 1. App-mode device at `vid`/`pid` → USB reset interface.
/// 2. Same `vid` + USBBOOT PID `0x000F` → PICOBOOT `PC_REBOOT2`.
/// 3. If `vid` was **not** specified by the caller, also scan for any device
///    with a fallback VID (`0xC0DE`, `0xC001`) and attempt the reset interface.
pub fn reset_to_bootsel(vid: Option<u16>, pid: Option<u16>) -> Result<()> {
    let primary_vid = vid.unwrap_or(DEFAULT_VID);
    let primary_pid = pid.unwrap_or(DEFAULT_PID);

    if let Some(info) = select_unique_device(&[(primary_vid, Some(primary_pid))])? {
        log::info!(
            "Found app-mode device {:04x}:{:04x} — sending reset interface request",
            primary_vid,
            primary_pid
        );
        return reboot_via_reset_interface(&info);
    }

    if let Some(info) = select_unique_device(&[(primary_vid, Some(PRODUCT_ID_RP_USBBOOT))])? {
        log::info!(
            "Device {:04x}:{:04x} already in BOOTSEL — sending PC_REBOOT2",
            primary_vid,
            PRODUCT_ID_RP_USBBOOT
        );
        return reboot_via_picoboot(&info);
    }

    // When the caller did not pin a specific VID, also probe the fallback vendors.
    if vid.is_none() {
        let fallback_selectors: Vec<(u16, Option<u16>)> = FALLBACK_VIDS
            .iter()
            .copied()
            .map(|fvid| (fvid, None))
            .collect();
        if let Some(info) = select_unique_device(&fallback_selectors)? {
            log::info!(
                "Primary device not found; trying fallback {:04x}:{:04x}",
                info.vendor_id(),
                info.product_id()
            );
            return reboot_via_reset_interface(&info);
        }
    }

    let fallback_hint = if vid.is_none() {
        let vids: Vec<String> = FALLBACK_VIDS.iter().map(|v| format!("{v:04x}")).collect();
        format!(" or any device with VID {}", vids.join("/"))
    } else {
        String::new()
    };
    anyhow::bail!(
        "No device found at {:04x}:{:04x} (app mode) or {:04x}:{:04x} (BOOTSEL){}",
        primary_vid,
        primary_pid,
        primary_vid,
        PRODUCT_ID_RP_USBBOOT,
        fallback_hint,
    )
}

/// Reboot a device to normal application mode.
///
/// Strategy (applied in order):
/// 1. App-mode device at `vid`/`pid` → already in normal mode, return success.
/// 2. Same `vid` + USBBOOT PID `0x000F` → PICOBOOT `PC_REBOOT2` normal reboot.
/// 3. If `vid` was **not** specified by the caller, also scan fallback VIDs
///    (`0xC0DE`, `0xC001`) for either an app-mode or BOOTSEL device.
pub fn reboot_to_normal(vid: Option<u16>, pid: Option<u16>) -> Result<()> {
    let primary_vid = vid.unwrap_or(DEFAULT_VID);
    let primary_pid = pid.unwrap_or(DEFAULT_PID);

    if select_unique_device(&[(primary_vid, Some(primary_pid))])?.is_some() {
        log::info!(
            "Device {:04x}:{:04x} already in normal application mode",
            primary_vid,
            primary_pid
        );
        return Ok(());
    }

    if let Some(info) = select_unique_device(&[(primary_vid, Some(PRODUCT_ID_RP_USBBOOT))])? {
        log::info!(
            "Device {:04x}:{:04x} in BOOTSEL — sending PC_REBOOT2 normal reboot",
            primary_vid,
            PRODUCT_ID_RP_USBBOOT
        );
        return reboot_to_normal_via_picoboot(&info);
    }

    // When the caller did not pin a specific VID, also probe the fallback vendors.
    if vid.is_none() {
        let fallback_bootsel_selectors: Vec<(u16, Option<u16>)> = FALLBACK_VIDS
            .iter()
            .copied()
            .map(|fvid| (fvid, Some(PRODUCT_ID_RP_USBBOOT)))
            .collect();
        if let Some(info) = select_unique_device(&fallback_bootsel_selectors)? {
            log::info!(
                "Primary device not found; rebooting fallback BOOTSEL device {:04x}:{:04x}",
                info.vendor_id(),
                info.product_id()
            );
            return reboot_to_normal_via_picoboot(&info);
        }

        let fallback_normal_selectors: Vec<(u16, Option<u16>)> = FALLBACK_VIDS
            .iter()
            .copied()
            .map(|fvid| (fvid, None))
            .collect();
        if let Some(info) = select_unique_device(&fallback_normal_selectors)? {
            log::info!(
                "Fallback device {:04x}:{:04x} already in normal application mode",
                info.vendor_id(),
                info.product_id()
            );
            return Ok(());
        }
    }

    let fallback_hint = if vid.is_none() {
        let vids: Vec<String> = FALLBACK_VIDS.iter().map(|v| format!("{v:04x}")).collect();
        format!(" or any device with VID {}", vids.join("/"))
    } else {
        String::new()
    };
    anyhow::bail!(
        "No device found at {:04x}:{:04x} (app mode) or {:04x}:{:04x} (BOOTSEL){}",
        primary_vid,
        primary_pid,
        primary_vid,
        PRODUCT_ID_RP_USBBOOT,
        fallback_hint,
    )
}

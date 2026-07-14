use anyhow::{Context, Result, anyhow};
use nusb::descriptors::TransferType;
use nusb::transfer::{Bulk, ControlOut, ControlType, Direction, In, Out, Recipient};
use nusb::{DeviceInfo, MaybeFuture};
use std::io::{Read, Write};
use std::time::Duration;

/// BOOTSEL (USB boot ROM) product ID — same for all RP-series chips.
const PRODUCT_ID_RP_USBBOOT: u16 = 0x000F;

/// Defaults used when the caller passes `None` for VID/PID.
pub const DEFAULT_VID: u16 = 0x2E8A;
pub const DEFAULT_PID: u16 = 0x0009;

/// Fallback VID probed when the user does not explicitly specify `--vid`.
/// Any device advertising this vendor ID is eligible for a reset attempt.
pub const FALLBACK_VID: u16 = 0xC0DE;

// Pico USB reset interface constants (from pico-sdk usb_reset_interface.h)
const RESET_INTERFACE_SUBCLASS: u8 = 0x00;
const RESET_INTERFACE_PROTOCOL: u8 = 0x01;
const RESET_REQUEST_BOOTSEL: u8 = 0x01;

// PICOBOOT protocol constants (from pico-sdk boot/picoboot.h)
const PICOBOOT_MAGIC: u32 = 0x431FD10B;
const PC_REBOOT2: u8 = 0x0A;
const PICOBOOT_REBOOT2_CMD_SIZE: u8 = 16;
const REBOOT2_FLAG_REBOOT_TYPE_BOOTSEL: u32 = 0x02;

/// Find a USB device by VID/PID.
pub fn find_device(vid: u16, pid: u16) -> Option<DeviceInfo> {
    nusb::list_devices()
        .wait()
        .ok()?
        .find(|d| d.vendor_id() == vid && d.product_id() == pid)
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

/// Build a 32-byte PICOBOOT PC_REBOOT2 command packet (little-endian).
fn build_picoboot_reboot2_cmd() -> [u8; 32] {
    let mut buf = [0u8; 32];
    buf[0..4].copy_from_slice(&PICOBOOT_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&1u32.to_le_bytes()); // dToken
    buf[8] = PC_REBOOT2;
    buf[9] = PICOBOOT_REBOOT2_CMD_SIZE;
    // buf[10..16]: _unused + dTransferLength = 0
    buf[16..20].copy_from_slice(&REBOOT2_FLAG_REBOOT_TYPE_BOOTSEL.to_le_bytes());
    buf[20..24].copy_from_slice(&500u32.to_le_bytes()); // dDelayMS
    // buf[24..32]: dParam0 + dParam1 = 0
    buf
}

/// Reboot a device already in BOOTSEL mode back into BOOTSEL via the PICOBOOT bulk protocol.
fn reboot_via_picoboot(info: &DeviceInfo) -> Result<()> {
    let device = info.open().wait().context("Failed to open USB device")?;
    let config = device
        .active_configuration()
        .map_err(|e| anyhow!("{}", e))?;

    let (iface_num, out_ep, in_ep) = config
        .interface_alt_settings()
        .filter(|iface| iface.class() == 0xFF)
        .find_map(|iface| {
            let endpoints: Vec<_> = iface.endpoints().collect();
            let out_addr = endpoints
                .iter()
                .find(|ep| {
                    ep.transfer_type() == TransferType::Bulk && ep.direction() == Direction::Out
                })?
                .address();
            let in_addr = endpoints
                .iter()
                .find(|ep| {
                    ep.transfer_type() == TransferType::Bulk && ep.direction() == Direction::In
                })?
                .address();
            Some((iface.interface_number(), out_addr, in_addr))
        })
        .ok_or_else(|| anyhow!("PICOBOOT interface not found on device"))?;

    let interface = device
        .claim_interface(iface_num)
        .wait()
        .context("Failed to claim PICOBOOT interface")?;

    let cmd = build_picoboot_reboot2_cmd();

    let mut writer = interface
        .endpoint::<Bulk, Out>(out_ep)
        .context("Failed to get bulk OUT endpoint")?
        .writer(64);
    writer.write_all(&cmd)?;
    writer.flush()?;

    // Read the zero-length ACK; device resets right after, so errors are expected — ignore.
    let mut reader = interface
        .endpoint::<Bulk, In>(in_ep)
        .context("Failed to get bulk IN endpoint")?
        .reader(64);
    let _ = reader.read(&mut [0u8; 1]);

    Ok(())
}

/// Reset a device to BOOTSEL mode.
///
/// Strategy (applied in order):
/// 1. App-mode device at `vid`/`pid` → USB reset interface.
/// 2. Same `vid` + USBBOOT PID `0x000F` → PICOBOOT `PC_REBOOT2`.
/// 3. If `vid` was **not** specified by the caller, also scan for any device
///    with VID `0xC0DE` and attempt the reset interface on it.
pub fn reset_to_bootsel(vid: Option<u16>, pid: Option<u16>) -> Result<()> {
    let primary_vid = vid.unwrap_or(DEFAULT_VID);
    let primary_pid = pid.unwrap_or(DEFAULT_PID);

    if let Some(info) = find_device(primary_vid, primary_pid) {
        log::info!(
            "Found app-mode device {:04x}:{:04x} — sending reset interface request",
            primary_vid,
            primary_pid
        );
        return reboot_via_reset_interface(&info);
    }

    if let Some(info) = find_device(primary_vid, PRODUCT_ID_RP_USBBOOT) {
        log::info!(
            "Device {:04x}:{:04x} already in BOOTSEL — sending PC_REBOOT2",
            primary_vid,
            PRODUCT_ID_RP_USBBOOT
        );
        return reboot_via_picoboot(&info);
    }

    // When the caller did not pin a specific VID, also probe the fallback vendor.
    if vid.is_none()
        && let Some(info) = find_any_device_with_vid(FALLBACK_VID)
    {
        log::info!(
            "Primary device not found; trying fallback {:04x}:{:04x}",
            info.vendor_id(),
            info.product_id()
        );
        return reboot_via_reset_interface(&info);
    }

    let fallback_hint = if vid.is_none() {
        format!(" or any device with VID {:04x}", FALLBACK_VID)
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

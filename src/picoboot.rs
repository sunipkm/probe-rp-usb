use anyhow::{Context, Result, anyhow};
use nusb::descriptors::TransferType;
use nusb::transfer::{Bulk, ControlIn, ControlType, Direction, In, Out, Recipient};
use nusb::{DeviceInfo, Interface, MaybeFuture};
use std::io::{Read, Write};
use std::time::Duration;

use crate::usb::{
    DEFAULT_VID, FALLBACK_VIDS, ensure_winusb_driver, reset_to_bootsel, select_unique_device,
};

/// BOOTSEL/PICOBOOT product ID for RP2xxx boot ROM devices.
pub const PRODUCT_ID_RP_USBBOOT: u16 = 0x000F;

const PICOBOOT_MAGIC: u32 = 0x431F_D10B;

const PICOBOOT_IF_CMD_STATUS: u8 = 0x42;

const PC_FLASH_ERASE: u8 = 0x03;
const PC_READ: u8 = 0x84;
const PC_WRITE: u8 = 0x05;
const PC_EXIT_XIP: u8 = 0x06;
const PC_REBOOT2: u8 = 0x0A;

const REBOOT2_FLAG_REBOOT_TYPE_NORMAL: u32 = 0x00;
const REBOOT2_FLAG_REBOOT_TYPE_BOOTSEL: u32 = 0x02;
const REBOOT2_FLAG_REBOOT_TYPE_FLASH_UPDATE: u32 = 0x04;

const PICOBOOT_REBOOT2_CMD_SIZE: u8 = 16;
const PICOBOOT_RANGE_CMD_SIZE: u8 = 8;

const COMMAND_PACKET_SIZE: usize = 32;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(3);
const DATA_TIMEOUT: Duration = Duration::from_secs(10);
const STATUS_TIMEOUT: Duration = Duration::from_secs(1);
const BULK_PACKET_SIZE: usize = 64;

#[derive(Debug, Clone)]
pub struct CommandStatus {
    pub token: u32,
    pub status_code: u32,
    pub command_id: u8,
    pub in_progress: bool,
}

pub struct PicobootConnection {
    interface: Interface,
    interface_number: u8,
    out_ep: u8,
    in_ep: u8,
    next_token: u32,
}

impl PicobootConnection {
    pub fn open_after_reset(vid: Option<u16>, pid: Option<u16>, timeout: Duration) -> Result<Self> {
        ensure_winusb_driver(vid, pid).context("Failed to prepare WinUSB driver")?;

        if let Some(connection) = Self::try_open_candidates(vid)? {
            return Ok(connection);
        }

        reset_to_bootsel(vid, pid)?;
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if let Some(connection) = Self::try_open_candidates(vid)? {
                return Ok(connection);
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        anyhow::bail!("Timed out waiting for PICOBOOT device");
    }

    pub fn open(vid: Option<u16>) -> Result<Self> {
        ensure_winusb_driver(vid, None).context("Failed to prepare WinUSB driver")?;

        let vid = vid.unwrap_or(DEFAULT_VID);
        let info =
            select_unique_device(&[(vid, Some(PRODUCT_ID_RP_USBBOOT))])?.ok_or_else(|| {
                anyhow!(
                    "No BOOTSEL/PICOBOOT device found at {:04x}:{:04x}",
                    vid,
                    PRODUCT_ID_RP_USBBOOT
                )
            })?;
        Self::open_device(&info)
    }

    fn try_open_candidates(vid: Option<u16>) -> Result<Option<Self>> {
        let mut selectors = vec![(vid.unwrap_or(DEFAULT_VID), Some(PRODUCT_ID_RP_USBBOOT))];
        if vid.is_none() {
            selectors.extend(
                FALLBACK_VIDS
                    .iter()
                    .copied()
                    .map(|candidate_vid| (candidate_vid, Some(PRODUCT_ID_RP_USBBOOT))),
            );
        }

        if let Some(info) = select_unique_device(&selectors)? {
            return Self::open_device(&info).map(Some);
        }

        Ok(None)
    }

    pub fn open_device(info: &DeviceInfo) -> Result<Self> {
        let device = info.open().wait().context("Failed to open USB device")?;
        let config = device
            .active_configuration()
            .map_err(|e| anyhow!("{}", e))?;

        let (interface_number, out_ep, in_ep) = config
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
            .claim_interface(interface_number)
            .wait()
            .context("Failed to claim PICOBOOT interface")?;

        Ok(Self {
            interface,
            interface_number,
            out_ep,
            in_ep,
            next_token: 1,
        })
    }

    pub fn reboot_bootsel(&mut self) -> Result<()> {
        let token = self.next_token();
        let command = build_reboot2_command(token, REBOOT2_FLAG_REBOOT_TYPE_BOOTSEL, 500, 0, 0);
        let _ = self.command(command, None, true);
        Ok(())
    }

    pub fn reboot_application(&mut self) -> Result<()> {
        let token = self.next_token();
        let command = build_reboot2_command(token, REBOOT2_FLAG_REBOOT_TYPE_NORMAL, 500, 0, 0);
        let _ = self.command(command, None, true);
        Ok(())
    }

    pub fn reboot_flash_update(&mut self, updated_addr: u32) -> Result<()> {
        let token = self.next_token();
        let command = build_reboot2_command(
            token,
            REBOOT2_FLAG_REBOOT_TYPE_FLASH_UPDATE,
            500,
            updated_addr,
            0,
        );
        let _ = self.command(command, None, true);
        Ok(())
    }

    pub fn exit_xip(&mut self) -> Result<()> {
        let token = self.next_token();
        self.command(
            build_command(token, PC_EXIT_XIP, 0, 0, [0; 16]),
            None,
            false,
        )?;
        Ok(())
    }

    pub fn flash_erase(&mut self, addr: u32, len: u32) -> Result<()> {
        let token = self.next_token();
        let command = build_range_command(token, PC_FLASH_ERASE, addr, len, 0);
        self.command(command, None, false)?;
        Ok(())
    }

    pub fn write(&mut self, addr: u32, data: &[u8]) -> Result<()> {
        let len = u32::try_from(data.len()).context("PICOBOOT write is too large")?;
        let token = self.next_token();
        let command = build_range_command(token, PC_WRITE, addr, len, len);
        self.command(command, Some(data), false)?;
        Ok(())
    }

    pub fn read(&mut self, addr: u32, len: u32) -> Result<Vec<u8>> {
        let token = self.next_token();
        let command = build_range_command(token, PC_READ, addr, len, len);
        self.command(command, None, false)
    }

    pub fn command_status(&self) -> Result<CommandStatus> {
        let data = self
            .interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Interface,
                    request: PICOBOOT_IF_CMD_STATUS,
                    value: 0,
                    index: self.interface_number as u16,
                    length: 16,
                },
                STATUS_TIMEOUT,
            )
            .wait()
            .context("Failed to read PICOBOOT command status")?;

        parse_command_status(&data)
    }

    fn next_token(&mut self) -> u32 {
        let token = self.next_token;
        self.next_token = self.next_token.wrapping_add(1).max(1);
        token
    }

    fn command(
        &mut self,
        command: [u8; COMMAND_PACKET_SIZE],
        out_data: Option<&[u8]>,
        ignore_ack_error: bool,
    ) -> Result<Vec<u8>> {
        let command_id = command[8];
        let transfer_len = u32::from_le_bytes(command[12..16].try_into().unwrap()) as usize;
        let is_in = command_id & 0x80 != 0;

        {
            let mut writer = self
                .interface
                .endpoint::<Bulk, Out>(self.out_ep)
                .context("Failed to get PICOBOOT bulk OUT endpoint")?
                .writer(BULK_PACKET_SIZE)
                .with_write_timeout(COMMAND_TIMEOUT);
            writer
                .write_all(&command)
                .context("Timed out writing PICOBOOT command packet")?;
            writer
                .flush()
                .context("Timed out flushing PICOBOOT command packet")?;
        }

        let mut in_data = Vec::new();
        if transfer_len != 0 {
            if is_in {
                in_data.resize(transfer_len, 0);
                let mut reader = self
                    .interface
                    .endpoint::<Bulk, In>(self.in_ep)
                    .context("Failed to get PICOBOOT bulk IN endpoint")?
                    .reader(BULK_PACKET_SIZE)
                    .with_read_timeout(DATA_TIMEOUT);
                reader
                    .read_exact(&mut in_data)
                    .context("Timed out reading PICOBOOT IN data")?;
            } else {
                let data = out_data.ok_or_else(|| anyhow!("PICOBOOT command requires OUT data"))?;
                anyhow::ensure!(
                    data.len() == transfer_len,
                    "PICOBOOT OUT data length mismatch: expected {}, got {}",
                    transfer_len,
                    data.len()
                );
                let mut writer = self
                    .interface
                    .endpoint::<Bulk, Out>(self.out_ep)
                    .context("Failed to get PICOBOOT bulk OUT endpoint")?
                    .writer(BULK_PACKET_SIZE)
                    .with_write_timeout(DATA_TIMEOUT);
                writer
                    .write_all(data)
                    .context("Timed out writing PICOBOOT OUT data")?;
                writer
                    .flush()
                    .context("Timed out flushing PICOBOOT OUT data")?;
            }
        }

        let ack = if is_in {
            let mut writer = self
                .interface
                .endpoint::<Bulk, Out>(self.out_ep)
                .context("Failed to get PICOBOOT bulk OUT endpoint")?
                .writer(BULK_PACKET_SIZE)
                .with_write_timeout(COMMAND_TIMEOUT);
            writer.flush_end()
        } else {
            let mut reader = self
                .interface
                .endpoint::<Bulk, In>(self.in_ep)
                .context("Failed to get PICOBOOT bulk IN endpoint")?
                .reader(BULK_PACKET_SIZE)
                .with_read_timeout(COMMAND_TIMEOUT);
            let mut ack = Vec::new();
            reader
                .until_short_packet()
                .read_to_end(&mut ack)
                .map(|_| ())
        };

        if let Err(e) = ack {
            if ignore_ack_error {
                return Ok(in_data);
            }
            if let Ok(status) = self.command_status() {
                if status.status_code == 0 {
                    return Ok(in_data);
                }
                return Err(anyhow!(
                    "PICOBOOT command 0x{:02x} failed: status {} ({})",
                    status.command_id,
                    status.status_code,
                    status_name(status.status_code)
                ))
                .context(e);
            }
            return Err(e.into());
        }

        Ok(in_data)
    }
}

fn build_command(
    token: u32,
    command_id: u8,
    command_size: u8,
    transfer_len: u32,
    args: [u8; 16],
) -> [u8; COMMAND_PACKET_SIZE] {
    let mut buf = [0u8; COMMAND_PACKET_SIZE];
    buf[0..4].copy_from_slice(&PICOBOOT_MAGIC.to_le_bytes());
    buf[4..8].copy_from_slice(&token.to_le_bytes());
    buf[8] = command_id;
    buf[9] = command_size;
    buf[12..16].copy_from_slice(&transfer_len.to_le_bytes());
    buf[16..32].copy_from_slice(&args);
    buf
}

fn build_range_command(
    token: u32,
    command_id: u8,
    addr: u32,
    len: u32,
    transfer_len: u32,
) -> [u8; COMMAND_PACKET_SIZE] {
    let mut args = [0u8; 16];
    args[0..4].copy_from_slice(&addr.to_le_bytes());
    args[4..8].copy_from_slice(&len.to_le_bytes());
    build_command(
        token,
        command_id,
        PICOBOOT_RANGE_CMD_SIZE,
        transfer_len,
        args,
    )
}

fn build_reboot2_command(
    token: u32,
    flags: u32,
    delay_ms: u32,
    param0: u32,
    param1: u32,
) -> [u8; COMMAND_PACKET_SIZE] {
    let mut args = [0u8; 16];
    args[0..4].copy_from_slice(&flags.to_le_bytes());
    args[4..8].copy_from_slice(&delay_ms.to_le_bytes());
    args[8..12].copy_from_slice(&param0.to_le_bytes());
    args[12..16].copy_from_slice(&param1.to_le_bytes());
    build_command(token, PC_REBOOT2, PICOBOOT_REBOOT2_CMD_SIZE, 0, args)
}

fn parse_command_status(data: &[u8]) -> Result<CommandStatus> {
    anyhow::ensure!(
        data.len() == 16,
        "PICOBOOT command status length mismatch: expected 16, got {}",
        data.len()
    );
    Ok(CommandStatus {
        token: u32::from_le_bytes(data[0..4].try_into().unwrap()),
        status_code: u32::from_le_bytes(data[4..8].try_into().unwrap()),
        command_id: data[8],
        in_progress: data[9] != 0,
    })
}

fn status_name(code: u32) -> &'static str {
    match code {
        0 => "OK",
        1 => "UNKNOWN_CMD",
        2 => "INVALID_CMD_LENGTH",
        3 => "INVALID_TRANSFER_LENGTH",
        4 => "INVALID_ADDRESS",
        5 => "BAD_ALIGNMENT",
        6 => "INTERLEAVED_WRITE",
        7 => "REBOOTING",
        8 => "UNKNOWN_ERROR",
        9 => "INVALID_STATE",
        10 => "NOT_PERMITTED",
        11 => "INVALID_ARG",
        12 => "BUFFER_TOO_SMALL",
        13 => "PRECONDITION_NOT_MET",
        14 => "MODIFIED_DATA",
        15 => "INVALID_DATA",
        16 => "NOT_FOUND",
        17 => "UNSUPPORTED_MODIFICATION",
        _ => "UNKNOWN_STATUS",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_reboot2_bootsel_command() {
        let command = build_reboot2_command(1, REBOOT2_FLAG_REBOOT_TYPE_BOOTSEL, 500, 0, 0);
        assert_eq!(&command[0..4], &PICOBOOT_MAGIC.to_le_bytes());
        assert_eq!(&command[4..8], &1u32.to_le_bytes());
        assert_eq!(command[8], PC_REBOOT2);
        assert_eq!(command[9], 16);
        assert_eq!(&command[12..16], &0u32.to_le_bytes());
        assert_eq!(&command[16..20], &2u32.to_le_bytes());
        assert_eq!(&command[20..24], &500u32.to_le_bytes());
    }

    #[test]
    fn serializes_flash_erase_range_command() {
        let command = build_range_command(7, PC_FLASH_ERASE, 0x1000_0000, 4096, 0);
        assert_eq!(command[8], PC_FLASH_ERASE);
        assert_eq!(command[9], 8);
        assert_eq!(&command[12..16], &0u32.to_le_bytes());
        assert_eq!(&command[16..20], &0x1000_0000u32.to_le_bytes());
        assert_eq!(&command[20..24], &4096u32.to_le_bytes());
    }

    #[test]
    fn serializes_write_range_command() {
        let command = build_range_command(8, PC_WRITE, 0x1000_0100, 256, 256);
        assert_eq!(command[8], PC_WRITE);
        assert_eq!(command[9], 8);
        assert_eq!(&command[12..16], &256u32.to_le_bytes());
        assert_eq!(&command[16..20], &0x1000_0100u32.to_le_bytes());
        assert_eq!(&command[20..24], &256u32.to_le_bytes());
    }

    #[test]
    fn serializes_read_range_command() {
        let command = build_range_command(9, PC_READ, 0x1000_0000, 128, 128);
        assert_eq!(command[8], PC_READ);
        assert_eq!(command[9], 8);
        assert_eq!(&command[12..16], &128u32.to_le_bytes());
        assert_eq!(&command[16..20], &0x1000_0000u32.to_le_bytes());
        assert_eq!(&command[20..24], &128u32.to_le_bytes());
    }

    #[test]
    fn parses_command_status() {
        let mut data = [0u8; 16];
        data[0..4].copy_from_slice(&12u32.to_le_bytes());
        data[4..8].copy_from_slice(&10u32.to_le_bytes());
        data[8] = PC_WRITE;
        data[9] = 1;

        let status = parse_command_status(&data).unwrap();
        assert_eq!(status.token, 12);
        assert_eq!(status.status_code, 10);
        assert_eq!(status.command_id, PC_WRITE);
        assert!(status.in_progress);
    }
}

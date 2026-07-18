use anyhow::Result;
use std::io::{Read, Write};

// See https://github.com/microsoft/uf2/blob/master/utils/uf2families.json for list.
#[derive(Debug, Clone, Copy, clap::ValueEnum, Default)]
#[repr(u32)]
#[allow(non_camel_case_types)]
pub enum Family {
    /// Raspberry Pi RP2040
    RP2040 = 0xe48bff56,

    #[default]
    /// Raspberry Pi Microcontrollers: Absolute (unpartitioned) download
    RP2XXX_ABSOLUTE = 0xe48bff57,

    /// Raspberry Pi Microcontrollers: Data partition download
    RP2XXX_DATA = 0xe48bff58,

    /// Raspberry Pi RP2350, Secure Arm image
    RP2350_ARM_S = 0xe48bff59,

    /// Raspberry Pi RP2350, RISC-V image
    RP2350_RISCV = 0xe48bff5a,

    /// Raspberry Pi RP2350, Non-secure Arm image
    RP2350_ARM_NS = 0xe48bff5b,
}

/// Re-numbers every 512-byte UF2 block in `data` in-place.
///
/// After combining several independent UF2 byte buffers (each with their own
/// block numbering) into one, call this to stamp the correct sequential
/// `block_no` (starting at `start_block`) and the global `num_blocks` count
/// across all blocks.
pub fn renumber_blocks(data: &mut [u8], start_block: u32, num_blocks: u32) {
    for (i, block) in data.chunks_exact_mut(512).enumerate() {
        let block_no = start_block + i as u32;
        block[20..24].copy_from_slice(&block_no.to_le_bytes());
        block[24..28].copy_from_slice(&num_blocks.to_le_bytes());
    }
}

/// Merges a list of `(base_address, data)` raw binary regions into a single
/// contiguous flat buffer.
///
/// Regions are sorted by address.  Any gaps between them are filled with
/// `fill_byte` (typically `0xff` to match erased flash).  Returns
/// `(merged_base_address, merged_data)`.
///
/// Returns an error if any two regions overlap.
pub fn merge_regions(
    mut regions: Vec<(u32, Vec<u8>)>,
    fill_byte: u8,
) -> anyhow::Result<(u32, Vec<u8>)> {
    anyhow::ensure!(!regions.is_empty(), "no binary regions to merge");

    regions.sort_by_key(|(addr, _)| *addr);

    // Detect overlaps between adjacent (sorted) regions.
    for w in regions.windows(2) {
        let (addr_a, data_a) = &w[0];
        let (addr_b, _) = &w[1];
        let end_a = addr_a
            .checked_add(data_a.len() as u32)
            .ok_or_else(|| anyhow::anyhow!("address overflow in region at 0x{addr_a:08x}"))?;
        anyhow::ensure!(
            *addr_b >= end_a,
            "overlapping binary regions: 0x{addr_a:08x}..0x{end_a:08x} and 0x{addr_b:08x}"
        );
    }

    let base = regions[0].0;
    let (last_addr, last_data) = regions.last().unwrap();
    let end = last_addr
        .checked_add(last_data.len() as u32)
        .ok_or_else(|| anyhow::anyhow!("address overflow in last region"))?;

    let total_len = (end - base) as usize;
    let mut buf = vec![fill_byte; total_len];
    for (addr, data) in &regions {
        let offset = (addr - base) as usize;
        buf[offset..offset + data.len()].copy_from_slice(data);
    }

    Ok((base, buf))
}

/// Decode UF2 blocks into sorted, contiguous binary regions.
pub fn uf2_to_regions(data: &[u8]) -> anyhow::Result<Vec<(u32, Vec<u8>)>> {
    anyhow::ensure!(
        data.len().is_multiple_of(512),
        "UF2 data length is not a multiple of 512"
    );

    let mut blocks = Vec::new();
    for block in data.chunks_exact(512) {
        let start0 = u32::from_le_bytes(block[0..4].try_into().unwrap());
        let start1 = u32::from_le_bytes(block[4..8].try_into().unwrap());
        let target_addr = u32::from_le_bytes(block[12..16].try_into().unwrap());
        let payload_size = u32::from_le_bytes(block[16..20].try_into().unwrap());
        let end = u32::from_le_bytes(block[508..512].try_into().unwrap());

        anyhow::ensure!(start0 == UF2_MAGIC_START0, "Invalid UF2 start magic 0");
        anyhow::ensure!(start1 == UF2_MAGIC_START1, "Invalid UF2 start magic 1");
        anyhow::ensure!(end == UF2_MAGIC_END, "Invalid UF2 end magic");
        anyhow::ensure!(
            payload_size as usize <= UF2_DATA_FIELD_SIZE,
            "UF2 payload is too large: {}",
            payload_size
        );

        let payload_end = 32 + payload_size as usize;
        blocks.push((target_addr, block[32..payload_end].to_vec()));
    }

    blocks.sort_by_key(|(addr, _)| *addr);

    let mut regions: Vec<(u32, Vec<u8>)> = Vec::new();
    for (addr, payload) in blocks {
        if payload.is_empty() {
            continue;
        }
        if let Some((region_addr, region_data)) = regions.last_mut() {
            let region_end = region_addr
                .checked_add(region_data.len() as u32)
                .ok_or_else(|| anyhow::anyhow!("UF2 region address overflow"))?;
            anyhow::ensure!(addr >= region_end, "overlapping UF2 blocks at 0x{addr:08x}");
            if addr == region_end {
                region_data.extend_from_slice(&payload);
                continue;
            }
        }
        regions.push((addr, payload));
    }

    Ok(regions)
}

const UF2_MAGIC_START0: u32 = 0x0A324655;
const UF2_MAGIC_START1: u32 = 0x9E5D5157;
const UF2_MAGIC_END: u32 = 0x0AB16F30;
const UF2_FLAG_FAMILY_ID_PRESENT: u32 = 0x00002000;

/// Number of data bytes carried per UF2 block.
pub(crate) const UF2_PAYLOAD_SIZE: u32 = 256;

/// Data field size in a UF2 block (payload + padding to fill 476 bytes).
const UF2_DATA_FIELD_SIZE: usize = 476;

/// Convert a raw binary blob to UF2 format.
///
/// The binary is split into 256-byte pages starting at `base_addr`.  Each page
/// is written as a 512-byte UF2 block.  `family_id` should be the numeric
/// value of the target device's UF2 family (e.g. `Family::RP2350_ARM_S as u32`).
pub fn bin2uf2(
    mut input: impl Read,
    mut output: impl Write,
    base_addr: u32,
    family_id: u32,
) -> Result<()> {
    let mut data = Vec::new();
    input.read_to_end(&mut data)?;

    let num_blocks = (data.len() as u32).div_ceil(UF2_PAYLOAD_SIZE);

    for (block_no, chunk) in data.chunks(UF2_PAYLOAD_SIZE as usize).enumerate() {
        let target_addr = base_addr + (block_no as u32) * UF2_PAYLOAD_SIZE;
        write_block(
            &mut output,
            target_addr,
            chunk,
            block_no as u32,
            num_blocks,
            family_id,
        )?;
    }

    Ok(())
}

pub(crate) fn write_block(
    output: &mut impl Write,
    target_addr: u32,
    payload: &[u8],
    block_no: u32,
    num_blocks: u32,
    family_id: u32,
) -> std::io::Result<()> {
    // 32-byte header
    output.write_all(&UF2_MAGIC_START0.to_le_bytes())?;
    output.write_all(&UF2_MAGIC_START1.to_le_bytes())?;
    output.write_all(&UF2_FLAG_FAMILY_ID_PRESENT.to_le_bytes())?;
    output.write_all(&target_addr.to_le_bytes())?;
    output.write_all(&UF2_PAYLOAD_SIZE.to_le_bytes())?;
    output.write_all(&block_no.to_le_bytes())?;
    output.write_all(&num_blocks.to_le_bytes())?;
    output.write_all(&family_id.to_le_bytes())?;

    // 476-byte data field: payload zero-padded to UF2_DATA_FIELD_SIZE
    let mut data_field = [0u8; UF2_DATA_FIELD_SIZE];
    let copy_len = payload.len().min(UF2_PAYLOAD_SIZE as usize);
    data_field[..copy_len].copy_from_slice(&payload[..copy_len]);
    output.write_all(&data_field)?;

    // 4-byte footer
    output.write_all(&UF2_MAGIC_END.to_le_bytes())?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_contiguous_uf2_blocks_to_one_region() {
        let data = vec![0xA5u8; 300];
        let mut uf2 = Vec::new();
        bin2uf2(data.as_slice(), &mut uf2, 0x1000_0000, 0xE48B_FF56).unwrap();

        let regions = uf2_to_regions(&uf2).unwrap();
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].0, 0x1000_0000);
        assert_eq!(regions[0].1.len(), 512);
        assert_eq!(&regions[0].1[..300], &data);
        assert_eq!(regions[0].1[300..], vec![0u8; 212]);
    }
}

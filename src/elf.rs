use ::elf::{ElfStream, ParseError, abi::PT_LOAD, endian::AnyEndian, segment::ProgramHeader};
use std::cmp::min;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Seek, Write};
use thiserror::Error;

use crate::uf2::{Family, UF2_PAYLOAD_SIZE, write_block};

const FLASH_SECTOR_ERASE_SIZE: u64 = 4096;

const MAIN_RAM_START_RP2040: u64 = 0x2000_0000;
const MAIN_RAM_END_RP2040: u64 = 0x2004_2000;
const MAIN_RAM_START_RP2350: u64 = 0x2000_0000;
const MAIN_RAM_END_RP2350: u64 = 0x2008_2000;
const FLASH_START_RP2040: u64 = 0x1000_0000;
const FLASH_END_RP2040: u64 = 0x1500_0000;
const FLASH_START_RP2350: u64 = 0x1000_0000;
const FLASH_END_RP2350: u64 = 0x1500_0000;
const XIP_SRAM_START_RP2040: u64 = 0x1500_0000;
const XIP_SRAM_END_RP2040: u64 = 0x1500_4000;
const XIP_SRAM_START_RP2350: u64 = 0x13ff_c000;
const XIP_SRAM_END_RP2350: u64 = 0x1400_0000;
const MAIN_RAM_BANKED_START_RP2040: u64 = 0x2100_0000;
const MAIN_RAM_BANKED_END_RP2040: u64 = 0x2104_0000;
const ROM_START_RP2040: u64 = 0x0000_0000;
const ROM_END_RP2040: u64 = 0x0000_4000;
const ROM_START_RP2350: u64 = 0x0000_0000;
const ROM_END_RP2350: u64 = 0x0000_8000;

type PageMap = BTreeMap<u64, Vec<PageFragment>>;

#[derive(Error, Debug)]
pub enum Elf2Uf2Error {
    #[error("Failed to get address ranges from elf")]
    FailedToGetPagesFromRanges(AddressRangesFromElfError),
    #[error("Failed to open elf file")]
    FailedToOpenElfFile(ParseError),
    #[error("Failed to realize pages for elf file")]
    FailedToRealizePages(ParseError),
    #[error("Failed to write to output")]
    FailedToWrite(std::io::Error),
    #[error("The input file has no memory pages")]
    InputFileNoMemoryPages,
    #[error("B0/B1 Boot ROM does not support direct entry into XIP_SRAM")]
    DirectEntryIntoXipSram,
    #[error("A RAM binary should have an entry point at the beginning: {0:#08x} (not {1:#08x})")]
    RamBinaryEntryPoint(u32, u32),
    #[error("entry point is not in mapped part of file")]
    EntryPointNotMapped,
}

/// Converts an ELF file into UF2 format.
pub fn elf2uf2(
    input: impl Read + Seek,
    output: impl Write,
    family: Family,
) -> std::result::Result<(), Elf2Uf2Error> {
    let mut elf =
        ElfStream::<AnyEndian, _>::open_stream(input).map_err(Elf2Uf2Error::FailedToOpenElfFile)?;
    let pages = build_page_map(&elf, family)?;
    write_elf_output(&mut elf, &pages, output, family)
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum AddressRangeType {
    Contents,
    NoContents,
    Ignore,
}

#[derive(Copy, Clone, Debug)]
struct AddressRange {
    typ: AddressRangeType,
    from: u64,
    to: u64,
}

impl AddressRange {
    const fn new(from: u64, to: u64, typ: AddressRangeType) -> Self {
        Self { typ, from, to }
    }
}

const RP2040_ADDRESS_RANGES_FLASH: &[AddressRange] = &[
    AddressRange::new(
        FLASH_START_RP2040,
        FLASH_END_RP2040,
        AddressRangeType::Contents,
    ),
    AddressRange::new(
        MAIN_RAM_START_RP2040,
        MAIN_RAM_END_RP2040,
        AddressRangeType::NoContents,
    ),
    AddressRange::new(
        MAIN_RAM_BANKED_START_RP2040,
        MAIN_RAM_BANKED_END_RP2040,
        AddressRangeType::NoContents,
    ),
];

const RP2040_ADDRESS_RANGES_RAM: &[AddressRange] = &[
    AddressRange::new(
        MAIN_RAM_START_RP2040,
        MAIN_RAM_END_RP2040,
        AddressRangeType::Contents,
    ),
    AddressRange::new(
        XIP_SRAM_START_RP2040,
        XIP_SRAM_END_RP2040,
        AddressRangeType::Contents,
    ),
    AddressRange::new(ROM_START_RP2040, ROM_END_RP2040, AddressRangeType::Ignore),
];

const RP2350_ADDRESS_RANGES_FLASH: &[AddressRange] = &[
    AddressRange::new(
        FLASH_START_RP2350,
        FLASH_END_RP2350,
        AddressRangeType::Contents,
    ),
    AddressRange::new(
        MAIN_RAM_START_RP2350,
        MAIN_RAM_END_RP2350,
        AddressRangeType::NoContents,
    ),
];

const RP2350_ADDRESS_RANGES_RAM: &[AddressRange] = &[
    AddressRange::new(
        MAIN_RAM_START_RP2350,
        MAIN_RAM_END_RP2350,
        AddressRangeType::Contents,
    ),
    AddressRange::new(
        XIP_SRAM_START_RP2350,
        XIP_SRAM_END_RP2350,
        AddressRangeType::Contents,
    ),
    AddressRange::new(ROM_START_RP2350, ROM_END_RP2350, AddressRangeType::Ignore),
];

#[derive(Copy, Clone, Debug)]
struct PageFragment {
    segment: ProgramHeader,
    file_offset: u64,
    page_offset: u64,
    bytes: u64,
}

#[derive(Error, Debug)]
pub enum AddressRangesFromElfError {
    #[error("No segments in ELF")]
    NoSegments,
    #[error("In-memory segments overlap")]
    SegmentsOverlap,
    #[error("ELF contains memory contents for uninitialized memory at {0:08x}")]
    ContentsForUninitializedMemory(u64),
    #[error("Memory segment {0:#08x}->{1:#08x} is outside of valid address range for device")]
    SegmentInvalidForDevice(u64, u64),
}

trait AddressRangesExt<'a>: IntoIterator<Item = &'a AddressRange> + Clone {
    fn range_for(&self, addr: u64) -> Option<&'a AddressRange> {
        self.clone()
            .into_iter()
            .find(|range| range.from <= addr && range.to > addr)
    }

    fn is_address_initialized(&self, addr: u64) -> bool {
        self.range_for(addr)
            .is_some_and(|range| matches!(range.typ, AddressRangeType::Contents))
    }

    fn check_address_range(
        &self,
        addr: u64,
        vaddr: u64,
        size: u64,
        uninitialized: bool,
    ) -> std::result::Result<AddressRange, AddressRangesFromElfError> {
        for range in self.clone().into_iter() {
            if range.from <= addr && range.to >= addr + size {
                if range.typ == AddressRangeType::NoContents && !uninitialized {
                    return Err(AddressRangesFromElfError::ContentsForUninitializedMemory(
                        addr,
                    ));
                }
                log::debug!(
                    "{} segment {:#08x}->{:#08x} ({:#08x}->{:#08x})",
                    if uninitialized {
                        "Uninitialized"
                    } else {
                        "Mapped"
                    },
                    addr,
                    addr + size,
                    vaddr,
                    vaddr + size
                );
                return Ok(*range);
            }
        }
        Err(AddressRangesFromElfError::SegmentInvalidForDevice(
            addr,
            addr + size,
        ))
    }

    fn check_elf32_ph_entries(
        &self,
        file: &ElfStream<AnyEndian, impl Read + Seek>,
    ) -> std::result::Result<PageMap, AddressRangesFromElfError> {
        let mut pages = PageMap::new();

        for segment in file.segments() {
            if segment.p_type == PT_LOAD && segment.p_memsz > 0 {
                let mapped_size = min(segment.p_filesz, segment.p_memsz);

                if mapped_size > 0 {
                    let address_range = self.check_address_range(
                        segment.p_paddr,
                        segment.p_vaddr,
                        mapped_size,
                        false,
                    )?;

                    if address_range.typ != AddressRangeType::Contents {
                        log::debug!("ignored");
                        continue;
                    }

                    let mut addr = segment.p_paddr;
                    let mut remaining = mapped_size;
                    let mut file_offset = segment.p_offset;
                    while remaining > 0 {
                        let off = addr & (u64::from(UF2_PAYLOAD_SIZE) - 1);
                        let len = min(remaining, u64::from(UF2_PAYLOAD_SIZE) - off);
                        let fragments = pages.entry(addr - off).or_default();

                        for fragment in fragments.iter() {
                            if (off < fragment.page_offset + fragment.bytes)
                                != ((off + len) <= fragment.page_offset)
                            {
                                return Err(AddressRangesFromElfError::SegmentsOverlap);
                            }
                        }

                        fragments.push(PageFragment {
                            segment: *segment,
                            file_offset,
                            page_offset: off,
                            bytes: len,
                        });
                        addr += len;
                        file_offset += len;
                        remaining -= len;
                    }

                    if segment.p_memsz > segment.p_filesz {
                        self.check_address_range(
                            segment.p_paddr + segment.p_filesz,
                            segment.p_vaddr + segment.p_filesz,
                            segment.p_memsz - segment.p_filesz,
                            true,
                        )?;
                    }
                }
            }
        }

        Ok(pages)
    }
}

impl<'a, T> AddressRangesExt<'a> for T where T: IntoIterator<Item = &'a AddressRange> + Clone {}

fn build_page_map(
    elf: &ElfStream<AnyEndian, impl Read + Seek>,
    family: Family,
) -> std::result::Result<PageMap, Elf2Uf2Error> {
    let ram_style = is_ram_binary(elf, family).ok_or(Elf2Uf2Error::EntryPointNotMapped)?;

    if ram_style {
        log::debug!("Detected RAM binary");
    } else {
        log::debug!("Detected FLASH binary");
    }

    let (
        address_ranges_ram,
        address_ranges_flash,
        main_ram_start,
        main_ram_end,
        xip_sram_start,
        xip_sram_end,
    ) = match family {
        Family::RP2040 => (
            RP2040_ADDRESS_RANGES_RAM,
            RP2040_ADDRESS_RANGES_FLASH,
            MAIN_RAM_START_RP2040,
            MAIN_RAM_END_RP2040,
            XIP_SRAM_START_RP2040,
            XIP_SRAM_END_RP2040,
        ),
        Family::RP2XXX_ABSOLUTE
        | Family::RP2XXX_DATA
        | Family::RP2350_ARM_S
        | Family::RP2350_RISCV
        | Family::RP2350_ARM_NS => (
            RP2350_ADDRESS_RANGES_RAM,
            RP2350_ADDRESS_RANGES_FLASH,
            MAIN_RAM_START_RP2350,
            MAIN_RAM_END_RP2350,
            XIP_SRAM_START_RP2350,
            XIP_SRAM_END_RP2350,
        ),
    };

    let valid_ranges = if ram_style {
        address_ranges_ram
    } else {
        address_ranges_flash
    };

    let mut pages = valid_ranges
        .check_elf32_ph_entries(elf)
        .map_err(Elf2Uf2Error::FailedToGetPagesFromRanges)?;

    if pages.is_empty() {
        return Err(Elf2Uf2Error::InputFileNoMemoryPages);
    }

    if ram_style {
        let mut expected_ep_main_ram = u64::from(u32::MAX);
        let mut expected_ep_xip_sram = u64::from(u32::MAX);

        pages.keys().copied().for_each(|addr| {
            if addr >= main_ram_start && addr <= main_ram_end {
                expected_ep_main_ram = expected_ep_main_ram.min(addr) | 0x1;
            } else if addr >= xip_sram_start && addr < xip_sram_end {
                expected_ep_xip_sram = expected_ep_xip_sram.min(addr) | 0x1;
            }
        });

        let expected_ep = if expected_ep_main_ram != u64::from(u32::MAX) {
            expected_ep_main_ram
        } else {
            expected_ep_xip_sram
        };

        if expected_ep == expected_ep_xip_sram {
            return Err(Elf2Uf2Error::DirectEntryIntoXipSram);
        } else if elf.ehdr.e_entry != expected_ep {
            return Err(Elf2Uf2Error::RamBinaryEntryPoint(
                expected_ep as u32,
                elf.ehdr.e_entry as u32,
            ));
        }
    } else {
        let touched_sectors: HashSet<u64> = pages
            .keys()
            .map(|addr| addr / FLASH_SECTOR_ERASE_SIZE)
            .collect();

        let last_page_addr = *pages.last_key_value().unwrap().0;
        for sector in touched_sectors {
            let mut page = sector * FLASH_SECTOR_ERASE_SIZE;

            while page < (sector + 1) * FLASH_SECTOR_ERASE_SIZE {
                if page < last_page_addr && !pages.contains_key(&page) {
                    pages.insert(page, Vec::new());
                }
                page += u64::from(UF2_PAYLOAD_SIZE);
            }
        }
    }

    Ok(pages)
}

fn is_ram_binary(file: &ElfStream<AnyEndian, impl Read + Seek>, family: Family) -> Option<bool> {
    let entry = file.ehdr.e_entry;

    let (address_ranges_ram, address_ranges_flash) = match family {
        Family::RP2040 => (RP2040_ADDRESS_RANGES_RAM, RP2040_ADDRESS_RANGES_FLASH),
        Family::RP2XXX_ABSOLUTE
        | Family::RP2XXX_DATA
        | Family::RP2350_ARM_S
        | Family::RP2350_RISCV
        | Family::RP2350_ARM_NS => (RP2350_ADDRESS_RANGES_RAM, RP2350_ADDRESS_RANGES_FLASH),
    };

    for segment in file.segments() {
        if segment.p_type == PT_LOAD && segment.p_memsz > 0 {
            let mapped_size = segment.p_filesz.min(segment.p_memsz);
            if mapped_size > 0 && entry >= segment.p_vaddr && entry < segment.p_vaddr + mapped_size
            {
                let effective_entry = entry + segment.p_paddr - segment.p_vaddr;
                if address_ranges_ram.is_address_initialized(effective_entry) {
                    return Some(true);
                } else if address_ranges_flash.is_address_initialized(effective_entry) {
                    return Some(false);
                }
            }
        }
    }

    None
}

fn write_elf_output(
    elf_file: &mut ElfStream<AnyEndian, impl Read + Seek>,
    pages: &PageMap,
    mut output: impl Write,
    family: Family,
) -> std::result::Result<(), Elf2Uf2Error> {
    let num_blocks = pages.len() as u32;

    for (page_num, (target_addr, fragments)) in pages.iter().enumerate() {
        log::debug!("Page {} / {} {:#08x}", page_num, num_blocks, target_addr);

        let mut block_data = [0u8; UF2_PAYLOAD_SIZE as usize];
        realize_page(elf_file, fragments, &mut block_data)
            .map_err(Elf2Uf2Error::FailedToRealizePages)?;
        write_block(
            &mut output,
            *target_addr as u32,
            &block_data,
            page_num as u32,
            num_blocks,
            family as u32,
        )
        .map_err(Elf2Uf2Error::FailedToWrite)?;
    }

    Ok(())
}

fn realize_page(
    file: &mut ElfStream<AnyEndian, impl Read + Seek>,
    fragments: &[PageFragment],
    buf: &mut [u8],
) -> std::result::Result<(), ParseError> {
    for frag in fragments {
        let data = file.segment_data(&frag.segment)?;
        debug_assert!(frag.page_offset < u64::from(UF2_PAYLOAD_SIZE));
        debug_assert!(frag.page_offset + frag.bytes <= u64::from(UF2_PAYLOAD_SIZE));

        let start = (frag.file_offset - frag.segment.p_offset) as usize;
        let end = start + frag.bytes as usize;
        buf[frag.page_offset as usize..(frag.page_offset + frag.bytes) as usize]
            .copy_from_slice(&data[start..end]);
    }

    Ok(())
}

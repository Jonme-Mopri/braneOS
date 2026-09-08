//! PCI configuration-space discovery and resource decoding.
//!
//! Bare metal prefers PCIe ECAM regions discovered through ACPI MCFG and
//! falls back to PCI Configuration Mechanism #1 (CF8/CFC). Both paths are
//! serialized across CPUs. Enumeration starts at bus zero, follows PCI-to-PCI
//! bridges, visits all functions of multifunction devices, and records a
//! fixed-size inventory without allocating from the kernel heap.

#![allow(dead_code)]

use spin::Mutex;

#[cfg(target_os = "none")]
use core::sync::atomic::{AtomicU8, Ordering};
#[cfg(target_os = "none")]
use x86_64::instructions::port::Port;
#[cfg(target_os = "none")]
use x86_64::structures::paging::{
    OffsetPageTable, Page, PageTableFlags, PhysFrame, Size4KiB, Translate,
};
#[cfg(target_os = "none")]
use x86_64::{PhysAddr, VirtAddr};

#[cfg(target_os = "none")]
use crate::memory::frame_allocator::BitmapFrameAllocator;

const CONFIG_ADDRESS_PORT: u16 = 0xCF8;
const CONFIG_DATA_PORT: u16 = 0xCFC;
const INVALID_VENDOR_ID: u16 = 0xFFFF;
const MCFG_HEADER_LEN: usize = 44;
const MCFG_ENTRY_LEN: usize = 16;
const MCFG_MAX_LEN: usize = 4096;

#[cfg(target_os = "none")]
const ECAM_VIRTUAL_BASE: u64 = 0xFFFF_9000_0000_0000;
#[cfg(target_os = "none")]
const PCI_MMIO_VIRTUAL_BASE: u64 = 0xFFFF_A000_0000_0000;
const PAGE_SIZE: u64 = 4096;
const MAX_MMIO_APERTURE_SIZE: u64 = 64 * 1024 * 1024;
#[cfg(target_os = "none")]
const PCI_MMIO_WINDOW_SIZE: u64 = 512 * 1024 * 1024;

pub const MAX_PCI_DEVICES: usize = 256;
pub const MAX_PCI_BARS: usize = 6;
pub const MAX_ECAM_REGIONS: usize = 8;
#[cfg(target_os = "none")]
const MAX_PCI_MMIO_MAPPINGS: usize = 16;
pub const COMMAND_IO_SPACE: u16 = 1 << 0;
pub const COMMAND_MEMORY_SPACE: u16 = 1 << 1;
pub const COMMAND_BUS_MASTER: u16 = 1 << 2;

static CONFIG_ACCESS_LOCK: Mutex<()> = Mutex::new(());
#[cfg(target_os = "none")]
static ACTIVE_CONFIG_BACKEND: AtomicU8 = AtomicU8::new(0);
#[cfg(target_os = "none")]
static ACTIVE_ECAM_REGIONS: Mutex<EcamRegions> = Mutex::new(EcamRegions::new());
#[cfg(target_os = "none")]
static PCI_MMIO_MAPPINGS: Mutex<MmioMappingState> = Mutex::new(MmioMappingState::new());

/// A PCI bus/device/function tuple.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PciAddress {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl PciAddress {
    pub const fn new(bus: u8, device: u8, function: u8) -> Option<Self> {
        if device < 32 && function < 8 {
            Some(Self {
                bus,
                device,
                function,
            })
        } else {
            None
        }
    }

    const fn config_address(self, offset: u16) -> Option<u32> {
        if offset > 0xFC {
            return None;
        }
        Some(
            0x8000_0000
                | ((self.bus as u32) << 16)
                | ((self.device as u32) << 11)
                | ((self.function as u32) << 8)
                | ((offset as u32) & 0xFC),
        )
    }
}

/// One ACPI MCFG allocation structure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamRegion {
    pub physical_base: u64,
    pub segment_group: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

impl EcamRegion {
    pub const fn contains(self, segment_group: u16, bus: u8) -> bool {
        self.segment_group == segment_group && bus >= self.start_bus && bus <= self.end_bus
    }

    pub fn config_address(self, address: PciAddress, offset: u16) -> Option<u64> {
        if !self.contains(0, address.bus) || offset > 0xFFC || offset & 3 != 0 {
            return None;
        }
        self.physical_base
            .checked_add(u64::from(address.bus - self.start_bus) << 20)?
            .checked_add(u64::from(address.device) << 15)?
            .checked_add(u64::from(address.function) << 12)?
            .checked_add(u64::from(offset))
    }

    fn byte_len(self) -> Option<u64> {
        let bus_count = u64::from(self.end_bus - self.start_bus) + 1;
        bus_count.checked_shl(20)
    }
}

/// Fixed-capacity, validated collection of PCIe ECAM allocations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EcamRegions {
    regions: [Option<EcamRegion>; MAX_ECAM_REGIONS],
    count: usize,
}

impl EcamRegions {
    pub const fn new() -> Self {
        Self {
            regions: [None; MAX_ECAM_REGIONS],
            count: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn iter(&self) -> impl Iterator<Item = &EcamRegion> {
        self.regions[..self.count].iter().flatten()
    }

    pub fn region_for(&self, segment_group: u16, bus: u8) -> Option<EcamRegion> {
        self.iter()
            .find(|region| region.contains(segment_group, bus))
            .copied()
    }

    pub fn config_address(&self, address: PciAddress, offset: u16) -> Option<u64> {
        self.region_for(0, address.bus)?
            .config_address(address, offset)
    }

    /// Parse and validate a complete ACPI MCFG table.
    pub fn parse_mcfg(table: &[u8]) -> Result<Self, McfgError> {
        if table.len() < MCFG_HEADER_LEN || &table[..4] != b"MCFG" {
            return Err(McfgError::InvalidHeader);
        }
        let declared_len = u32::from_le_bytes([table[4], table[5], table[6], table[7]]) as usize;
        if !(MCFG_HEADER_LEN..=MCFG_MAX_LEN).contains(&declared_len)
            || declared_len > table.len()
            || !(declared_len - MCFG_HEADER_LEN).is_multiple_of(MCFG_ENTRY_LEN)
        {
            return Err(McfgError::InvalidLength);
        }
        if table[..declared_len]
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte))
            != 0
        {
            return Err(McfgError::InvalidChecksum);
        }

        let entry_count = (declared_len - MCFG_HEADER_LEN) / MCFG_ENTRY_LEN;
        if entry_count == 0 || entry_count > MAX_ECAM_REGIONS {
            return Err(McfgError::UnsupportedRegionCount);
        }

        let mut result = Self::new();
        for index in 0..entry_count {
            let offset = MCFG_HEADER_LEN + index * MCFG_ENTRY_LEN;
            let entry = &table[offset..offset + MCFG_ENTRY_LEN];
            let region = EcamRegion {
                physical_base: u64::from_le_bytes([
                    entry[0], entry[1], entry[2], entry[3], entry[4], entry[5], entry[6], entry[7],
                ]),
                segment_group: u16::from_le_bytes([entry[8], entry[9]]),
                start_bus: entry[10],
                end_bus: entry[11],
            };
            if region.physical_base == 0
                || region.physical_base & ((1 << 20) - 1) != 0
                || region.start_bus > region.end_bus
                || region
                    .byte_len()
                    .and_then(|length| region.physical_base.checked_add(length))
                    .is_none()
            {
                return Err(McfgError::InvalidRegion);
            }
            if result.iter().any(|existing| {
                let bus_overlap = existing.segment_group == region.segment_group
                    && existing.start_bus <= region.end_bus
                    && region.start_bus <= existing.end_bus;
                let existing_end = existing.physical_base + existing.byte_len().unwrap();
                let region_end = region.physical_base + region.byte_len().unwrap();
                let physical_overlap =
                    existing.physical_base < region_end && region.physical_base < existing_end;
                bus_overlap || physical_overlap
            }) {
                return Err(McfgError::OverlappingRegions);
            }
            result.regions[result.count] = Some(region);
            result.count += 1;
        }
        Ok(result)
    }
}

impl Default for EcamRegions {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McfgError {
    InvalidHeader,
    InvalidLength,
    InvalidChecksum,
    UnsupportedRegionCount,
    InvalidRegion,
    OverlappingRegions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciConfigBackend {
    Legacy,
    Ecam,
}

/// One decoded Base Address Register.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBar {
    Unused,
    Io { base: u32 },
    Memory32 { base: u32, prefetchable: bool },
    Memory64 { base: u64, prefetchable: bool },
    UpperHalf64,
    Reserved { raw: u32 },
}

impl PciBar {
    pub const fn base(self) -> Option<u64> {
        match self {
            Self::Io { base } | Self::Memory32 { base, .. } => Some(base as u64),
            Self::Memory64 { base, .. } => Some(base),
            Self::Unused | Self::UpperHalf64 | Self::Reserved { .. } => None,
        }
    }

    pub const fn is_io(self) -> bool {
        matches!(self, Self::Io { .. })
    }

    pub const fn is_memory(self) -> bool {
        matches!(self, Self::Memory32 { .. } | Self::Memory64 { .. })
    }
}

/// One assigned BAR together with the aperture size measured through the PCI
/// sizing protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciBarResource {
    pub index: u8,
    pub bar: PciBar,
    pub size: u64,
}

impl PciBarResource {
    pub const fn base(self) -> Option<u64> {
        self.bar.base()
    }

    pub fn end_exclusive(self) -> Option<u64> {
        self.base()?.checked_add(self.size)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PciBarProbeError {
    InvalidIndex,
    UnusableBar,
    InvalidSize,
    AddressOverflow,
}

/// A PCI MMIO aperture mapped into the kernel's dedicated uncached window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MmioMapping {
    pub physical_base: u64,
    pub virtual_base: u64,
    pub size: u64,
    mapped_physical_base: u64,
    mapped_virtual_base: u64,
    mapped_len: u64,
}

impl MmioMapping {
    #[cfg(test)]
    pub(crate) const fn for_test(physical_base: u64, virtual_base: u64, size: u64) -> Self {
        Self {
            physical_base,
            virtual_base,
            size,
            mapped_physical_base: physical_base,
            mapped_virtual_base: virtual_base,
            mapped_len: size,
        }
    }

    pub fn contains(self, offset: u64, width: u64) -> bool {
        width != 0
            && offset
                .checked_add(width)
                .is_some_and(|end| end <= self.size)
    }

    pub fn virtual_address(self, offset: u64, width: u64) -> Option<u64> {
        self.contains(offset, width)
            .then(|| self.virtual_base.checked_add(offset))?
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioMapError {
    NotMemoryBar,
    InvalidSize,
    AddressOverflow,
    ApertureTooLarge,
    WindowExhausted,
    MappingConflict,
    MappingTableFull,
    MapFailed,
}

#[cfg(target_os = "none")]
#[derive(Clone, Copy)]
struct MmioMappingState {
    mappings: [Option<MmioMapping>; MAX_PCI_MMIO_MAPPINGS],
    count: usize,
    next_offset: u64,
}

#[cfg(target_os = "none")]
impl MmioMappingState {
    const fn new() -> Self {
        Self {
            mappings: [None; MAX_PCI_MMIO_MAPPINGS],
            count: 0,
            next_offset: 0,
        }
    }

    fn existing(&self, physical_base: u64, size: u64) -> Option<MmioMapping> {
        self.mappings[..self.count]
            .iter()
            .flatten()
            .find(|mapping| mapping.physical_base == physical_base && mapping.size == size)
            .copied()
    }
}

/// A discovered PCI function and the resources advertised by its header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PciDevice {
    pub address: PciAddress,
    pub vendor_id: u16,
    pub device_id: u16,
    pub command: u16,
    pub status: u16,
    pub revision_id: u8,
    pub programming_interface: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub header_type: u8,
    pub multifunction: bool,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    pub bars: [PciBar; MAX_PCI_BARS],
    pub bar_sizes: [u64; MAX_PCI_BARS],
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
    pub secondary_bus: Option<u8>,
}

impl PciDevice {
    pub const EMPTY: Self = Self {
        address: PciAddress {
            bus: 0,
            device: 0,
            function: 0,
        },
        vendor_id: 0,
        device_id: 0,
        command: 0,
        status: 0,
        revision_id: 0,
        programming_interface: 0,
        subclass: 0,
        class_code: 0,
        header_type: 0,
        multifunction: false,
        subsystem_vendor_id: 0,
        subsystem_id: 0,
        bars: [PciBar::Unused; MAX_PCI_BARS],
        bar_sizes: [0; MAX_PCI_BARS],
        interrupt_line: 0xFF,
        interrupt_pin: 0,
        secondary_bus: None,
    };

    pub const fn bar(self, index: usize) -> Option<PciBar> {
        if index < MAX_PCI_BARS {
            Some(self.bars[index])
        } else {
            None
        }
    }

    pub fn bar_resource(self, index: usize) -> Option<PciBarResource> {
        let bar = self.bar(index)?;
        let size = self.bar_sizes[index];
        (bar.is_io() || bar.is_memory()).then_some(())?;
        (size != 0).then_some(PciBarResource {
            index: index as u8,
            bar,
            size,
        })
    }

    pub const fn is_pci_bridge(self) -> bool {
        self.class_code == 0x06 && self.subclass == 0x04
    }
}

/// Fixed-capacity PCI inventory.
#[derive(Clone, Copy)]
pub struct PciInventory {
    devices: [Option<PciDevice>; MAX_PCI_DEVICES],
    count: usize,
    overflowed: bool,
    buses_scanned: usize,
}

impl PciInventory {
    pub const fn new() -> Self {
        Self {
            devices: [None; MAX_PCI_DEVICES],
            count: 0,
            overflowed: false,
            buses_scanned: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub const fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub const fn buses_scanned(&self) -> usize {
        self.buses_scanned
    }

    pub fn devices(&self) -> impl Iterator<Item = &PciDevice> {
        self.devices[..self.count].iter().flatten()
    }

    pub fn find(&self, mut predicate: impl FnMut(&PciDevice) -> bool) -> Option<PciDevice> {
        self.devices().find(|device| predicate(device)).copied()
    }

    fn push(&mut self, device: PciDevice) {
        if self.count == MAX_PCI_DEVICES {
            self.overflowed = true;
            return;
        }
        self.devices[self.count] = Some(device);
        self.count += 1;
    }

    fn clear(&mut self) {
        self.devices.fill(None);
        self.count = 0;
        self.overflowed = false;
        self.buses_scanned = 0;
    }
}

impl Default for PciInventory {
    fn default() -> Self {
        Self::new()
    }
}

/// Backend used by the topology walker. Tests provide an in-memory backend.
pub trait PciConfigAccess {
    fn read_u32(&mut self, address: PciAddress, offset: u16) -> u32;
    fn write_u32(&mut self, address: PciAddress, offset: u16, value: u32);
}

pub struct LegacyConfigAccess;

impl PciConfigAccess for LegacyConfigAccess {
    fn read_u32(&mut self, address: PciAddress, offset: u16) -> u32 {
        #[cfg(target_os = "none")]
        {
            let Some(config_address) = address.config_address(offset) else {
                return u32::MAX;
            };
            unsafe {
                let mut address_port = Port::<u32>::new(CONFIG_ADDRESS_PORT);
                let mut data_port = Port::<u32>::new(CONFIG_DATA_PORT);
                address_port.write(config_address);
                data_port.read()
            }
        }
        #[cfg(not(target_os = "none"))]
        {
            let _ = (address, offset);
            u32::MAX
        }
    }

    fn write_u32(&mut self, address: PciAddress, offset: u16, value: u32) {
        #[cfg(target_os = "none")]
        {
            let Some(config_address) = address.config_address(offset) else {
                return;
            };
            unsafe {
                let mut address_port = Port::<u32>::new(CONFIG_ADDRESS_PORT);
                let mut data_port = Port::<u32>::new(CONFIG_DATA_PORT);
                address_port.write(config_address);
                data_port.write(value);
            }
        }
        #[cfg(not(target_os = "none"))]
        {
            let _ = (address, offset, value);
        }
    }
}

#[cfg(target_os = "none")]
struct EcamConfigAccess<'a> {
    regions: EcamRegions,
    mapper: &'a mut OffsetPageTable<'static>,
    frame_allocator: &'a mut BitmapFrameAllocator,
    mapping_failed: bool,
}

#[cfg(target_os = "none")]
impl EcamConfigAccess<'_> {
    fn config_virtual_address(address: PciAddress, offset: u16) -> Option<u64> {
        ECAM_VIRTUAL_BASE
            .checked_add(u64::from(address.bus) << 20)?
            .checked_add(u64::from(address.device) << 15)?
            .checked_add(u64::from(address.function) << 12)?
            .checked_add(u64::from(offset))
    }

    fn ensure_function_mapped(&mut self, address: PciAddress) -> Option<(u64, u64)> {
        let physical = self.regions.config_address(address, 0)?;
        let virtual_address = Self::config_virtual_address(address, 0)?;
        let page = Page::from_start_address(VirtAddr::new(virtual_address)).ok()?;
        let frame = PhysFrame::from_start_address(PhysAddr::new(physical)).ok()?;
        match self.mapper.translate_addr(page.start_address()) {
            Some(mapped) if mapped == frame.start_address() => {}
            Some(_) => {
                self.mapping_failed = true;
                return None;
            }
            None => {
                let flags = PageTableFlags::PRESENT
                    | PageTableFlags::WRITABLE
                    | PageTableFlags::NO_EXECUTE
                    | PageTableFlags::NO_CACHE;
                if crate::memory::paging::map_page(
                    self.mapper,
                    page,
                    frame,
                    flags,
                    self.frame_allocator,
                )
                .is_err()
                {
                    self.mapping_failed = true;
                    return None;
                }
            }
        }
        Some((physical, virtual_address))
    }
}

#[cfg(target_os = "none")]
impl PciConfigAccess for EcamConfigAccess<'_> {
    fn read_u32(&mut self, address: PciAddress, offset: u16) -> u32 {
        if offset > 0xFFC || offset & 3 != 0 {
            return u32::MAX;
        }
        let Some((_, virtual_base)) = self.ensure_function_mapped(address) else {
            return u32::MAX;
        };
        let Some(register) = virtual_base.checked_add(u64::from(offset)) else {
            self.mapping_failed = true;
            return u32::MAX;
        };
        unsafe { core::ptr::read_volatile(register as *const u32) }
    }

    fn write_u32(&mut self, address: PciAddress, offset: u16, value: u32) {
        if offset > 0xFFC || offset & 3 != 0 {
            return;
        }
        let Some((_, virtual_base)) = self.ensure_function_mapped(address) else {
            return;
        };
        let Some(register) = virtual_base.checked_add(u64::from(offset)) else {
            self.mapping_failed = true;
            return;
        };
        unsafe { core::ptr::write_volatile(register as *mut u32, value) };
    }
}

fn enable_command_bits(device: PciDevice, bits: u16) -> u16 {
    let command = device.command | bits;
    #[cfg(target_os = "none")]
    {
        let _guard = CONFIG_ACCESS_LOCK.lock();
        if ACTIVE_CONFIG_BACKEND.load(Ordering::Acquire) == 1
            && ACTIVE_ECAM_REGIONS
                .lock()
                .config_address(device.address, 0x04)
                .is_some()
        {
            if let Some(register) = EcamConfigAccess::config_virtual_address(device.address, 0x04) {
                unsafe { core::ptr::write_volatile(register as *mut u16, command) };
            }
        } else {
            unsafe {
                let mut address_port = Port::<u32>::new(CONFIG_ADDRESS_PORT);
                let mut command_port = Port::<u16>::new(CONFIG_DATA_PORT);
                address_port.write(
                    device
                        .address
                        .config_address(0x04)
                        .expect("command register is in legacy config range"),
                );
                command_port.write(command);
            }
        }
    }
    command
}

/// Enable I/O-port decoding and DMA for a legacy or transitional function.
///
/// Only the 16-bit command register is written, avoiding write-one-to-clear
/// status bits in the upper half of the configuration dword.
pub fn enable_io_bus_mastering(device: PciDevice) -> u16 {
    enable_command_bits(device, COMMAND_IO_SPACE | COMMAND_BUS_MASTER)
}

/// Enable MMIO decoding and DMA for a PCIe function such as xHCI.
pub fn enable_memory_bus_mastering(device: PciDevice) -> u16 {
    enable_command_bits(device, COMMAND_MEMORY_SPACE | COMMAND_BUS_MASTER)
}

/// Compatibility name used by the first virtio transport implementation.
pub fn enable_legacy_io_bus_mastering(device: PciDevice) -> u16 {
    enable_io_bus_mastering(device)
}

fn bar_size_from_mask(mask: u64, width_bits: u8) -> Option<u64> {
    let size = match width_bits {
        32 => u64::from((!(mask as u32)).wrapping_add(1)),
        64 => (!mask).wrapping_add(1),
        _ => return None,
    };
    (size != 0 && size.is_power_of_two()).then_some(size)
}

/// Measure one BAR by temporarily disabling address decoding, writing the
/// sizing mask and restoring both the BAR and Command register before return.
///
/// Callers must serialize the complete transaction against other config-space
/// users. The bare-metal initializer holds `CONFIG_ACCESS_LOCK` while probing.
pub fn probe_bar_with(
    access: &mut impl PciConfigAccess,
    device: PciDevice,
    index: usize,
) -> Result<PciBarResource, PciBarProbeError> {
    let Some(bar) = device.bar(index) else {
        return Err(PciBarProbeError::InvalidIndex);
    };
    if !bar.is_io() && !bar.is_memory() {
        return Err(PciBarProbeError::UnusableBar);
    }
    if matches!(bar, PciBar::Memory64 { .. }) && index + 1 >= MAX_PCI_BARS {
        return Err(PciBarProbeError::InvalidIndex);
    }

    let address = device.address;
    let low_offset = 0x10 + index as u16 * 4;
    let original_command = access.read_u32(address, 0x04) as u16;
    let original_low = access.read_u32(address, low_offset);
    let original_high =
        matches!(bar, PciBar::Memory64 { .. }).then(|| access.read_u32(address, low_offset + 4));

    // Status occupies the upper half of this dword and is write-one-to-clear;
    // writing zero there preserves it while I/O and memory decoding are off.
    access.write_u32(
        address,
        0x04,
        u32::from(original_command & !(COMMAND_IO_SPACE | COMMAND_MEMORY_SPACE)),
    );
    access.write_u32(address, low_offset, u32::MAX);
    if original_high.is_some() {
        access.write_u32(address, low_offset + 4, u32::MAX);
    }
    let mask_low = access.read_u32(address, low_offset);
    let mask_high = original_high.map(|_| access.read_u32(address, low_offset + 4));

    // Restore resources before interpreting the result so every error path
    // leaves the device exactly as it was found.
    access.write_u32(address, low_offset, original_low);
    if let Some(original) = original_high {
        access.write_u32(address, low_offset + 4, original);
    }
    access.write_u32(address, 0x04, u32::from(original_command));

    let size = match bar {
        PciBar::Io { .. } => bar_size_from_mask(u64::from(mask_low & 0xFFFF_FFFC), 32),
        PciBar::Memory32 { .. } => bar_size_from_mask(u64::from(mask_low & 0xFFFF_FFF0), 32),
        PciBar::Memory64 { .. } => {
            let mask =
                (u64::from(mask_high.unwrap_or(0)) << 32) | u64::from(mask_low & 0xFFFF_FFF0);
            bar_size_from_mask(mask, 64)
        }
        _ => None,
    }
    .ok_or(PciBarProbeError::InvalidSize)?;
    let base = bar.base().ok_or(PciBarProbeError::UnusableBar)?;
    if base & (size - 1) != 0 {
        return Err(PciBarProbeError::InvalidSize);
    }
    base.checked_add(size)
        .ok_or(PciBarProbeError::AddressOverflow)?;

    Ok(PciBarResource {
        index: index as u8,
        bar,
        size,
    })
}

fn probe_inventory_bars(access: &mut impl PciConfigAccess, inventory: &mut PciInventory) {
    for slot in inventory.devices[..inventory.count].iter_mut() {
        let Some(device) = slot.as_mut() else {
            continue;
        };
        let snapshot = *device;
        let mut index = 0;
        while index < MAX_PCI_BARS {
            if let Ok(resource) = probe_bar_with(access, snapshot, index) {
                device.bar_sizes[index] = resource.size;
            }
            index += if matches!(snapshot.bars[index], PciBar::Memory64 { .. }) {
                2
            } else {
                1
            };
        }
    }
}

fn align_up(value: u64, alignment: u64) -> Option<u64> {
    value
        .checked_add(alignment - 1)
        .map(|rounded| rounded & !(alignment - 1))
}

fn plan_mmio_mapping(
    resource: PciBarResource,
    mapped_virtual_base: u64,
) -> Result<MmioMapping, MmioMapError> {
    if !resource.bar.is_memory() {
        return Err(MmioMapError::NotMemoryBar);
    }
    if resource.size == 0
        || !resource.size.is_power_of_two()
        || resource.base().is_none()
        || resource.base().unwrap() & (resource.size - 1) != 0
    {
        return Err(MmioMapError::InvalidSize);
    }
    if resource.size > MAX_MMIO_APERTURE_SIZE {
        return Err(MmioMapError::ApertureTooLarge);
    }
    if mapped_virtual_base & (PAGE_SIZE - 1) != 0 {
        return Err(MmioMapError::MappingConflict);
    }

    let physical_base = resource.base().unwrap();
    let physical_offset = physical_base & (PAGE_SIZE - 1);
    let mapped_physical_base = physical_base - physical_offset;
    let mapped_len = align_up(
        physical_offset
            .checked_add(resource.size)
            .ok_or(MmioMapError::AddressOverflow)?,
        PAGE_SIZE,
    )
    .ok_or(MmioMapError::AddressOverflow)?;
    mapped_physical_base
        .checked_add(mapped_len)
        .filter(|end| *end <= (1u64 << 52))
        .ok_or(MmioMapError::AddressOverflow)?;
    let virtual_base = mapped_virtual_base
        .checked_add(physical_offset)
        .ok_or(MmioMapError::AddressOverflow)?;
    mapped_virtual_base
        .checked_add(mapped_len)
        .ok_or(MmioMapError::AddressOverflow)?;

    Ok(MmioMapping {
        physical_base,
        virtual_base,
        size: resource.size,
        mapped_physical_base,
        mapped_virtual_base,
        mapped_len,
    })
}

/// Map a measured memory BAR into a bounded, uncached kernel window.
/// Exact duplicate mappings are reused; overlapping aliases are rejected.
#[cfg(target_os = "none")]
pub fn map_bar_mmio(
    mapper: &mut OffsetPageTable<'static>,
    frame_allocator: &mut BitmapFrameAllocator,
    resource: PciBarResource,
) -> Result<MmioMapping, MmioMapError> {
    let physical_base = resource.base().ok_or(MmioMapError::NotMemoryBar)?;
    let mut state = PCI_MMIO_MAPPINGS.lock();
    if let Some(existing) = state.existing(physical_base, resource.size) {
        return Ok(existing);
    }
    if state.count == MAX_PCI_MMIO_MAPPINGS {
        return Err(MmioMapError::MappingTableFull);
    }
    let physical_end = physical_base
        .checked_add(resource.size)
        .ok_or(MmioMapError::AddressOverflow)?;
    if state.mappings[..state.count]
        .iter()
        .flatten()
        .any(|mapping| {
            let existing_end = mapping.physical_base + mapping.size;
            mapping.physical_base < physical_end && physical_base < existing_end
        })
    {
        return Err(MmioMapError::MappingConflict);
    }

    let mapped_virtual_base = PCI_MMIO_VIRTUAL_BASE
        .checked_add(state.next_offset)
        .ok_or(MmioMapError::AddressOverflow)?;
    let mapping = plan_mmio_mapping(resource, mapped_virtual_base)?;
    let next_offset = state
        .next_offset
        .checked_add(mapping.mapped_len)
        .filter(|offset| *offset <= PCI_MMIO_WINDOW_SIZE)
        .ok_or(MmioMapError::WindowExhausted)?;

    let page_count = mapping.mapped_len / PAGE_SIZE;
    for index in 0..page_count {
        let virtual_address = mapping.mapped_virtual_base + index * PAGE_SIZE;
        if mapper
            .translate_addr(VirtAddr::new(virtual_address))
            .is_some()
        {
            return Err(MmioMapError::MappingConflict);
        }
    }

    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::NO_CACHE;
    for index in 0..page_count {
        let virtual_address = mapping.mapped_virtual_base + index * PAGE_SIZE;
        let physical_address = mapping.mapped_physical_base + index * PAGE_SIZE;
        let page = Page::<Size4KiB>::from_start_address(VirtAddr::new(virtual_address))
            .map_err(|_| MmioMapError::AddressOverflow)?;
        let frame = PhysFrame::<Size4KiB>::from_start_address(PhysAddr::new(physical_address))
            .map_err(|_| MmioMapError::AddressOverflow)?;
        if crate::memory::paging::map_page(mapper, page, frame, flags, frame_allocator).is_err() {
            for rollback in 0..index {
                let rollback_page = Page::<Size4KiB>::from_start_address(VirtAddr::new(
                    mapping.mapped_virtual_base + rollback * PAGE_SIZE,
                ))
                .expect("planned MMIO page must stay aligned");
                let _ = crate::memory::paging::unmap_page(mapper, rollback_page);
            }
            return Err(MmioMapError::MapFailed);
        }
    }

    let mapping_index = state.count;
    state.mappings[mapping_index] = Some(mapping);
    state.count += 1;
    state.next_offset = next_offset;
    Ok(mapping)
}

fn read_u16(access: &mut impl PciConfigAccess, address: PciAddress, offset: u16) -> u16 {
    let value = access.read_u32(address, offset & 0xFC);
    ((value >> ((offset & 2) * 8)) & 0xFFFF) as u16
}

fn function_exists(access: &mut impl PciConfigAccess, address: PciAddress) -> bool {
    read_u16(access, address, 0x00) != INVALID_VENDOR_ID
}

fn decode_bars(
    access: &mut impl PciConfigAccess,
    address: PciAddress,
    header_type: u8,
) -> [PciBar; MAX_PCI_BARS] {
    let mut bars = [PciBar::Unused; MAX_PCI_BARS];
    let limit = match header_type & 0x7F {
        0x00 => 6,
        0x01 => 2,
        _ => 0,
    };
    let mut index = 0;
    while index < limit {
        let raw = access.read_u32(address, 0x10 + (index as u16 * 4));
        if raw == 0 || raw == u32::MAX {
            index += 1;
            continue;
        }
        if raw & 1 != 0 {
            bars[index] = PciBar::Io {
                base: raw & 0xFFFF_FFFC,
            };
            index += 1;
            continue;
        }

        let prefetchable = raw & 0x8 != 0;
        match (raw >> 1) & 0x3 {
            0x0 | 0x1 => {
                bars[index] = PciBar::Memory32 {
                    base: raw & 0xFFFF_FFF0,
                    prefetchable,
                };
                index += 1;
            }
            0x2 if index + 1 < limit => {
                let upper = access.read_u32(address, 0x10 + ((index + 1) as u16 * 4));
                bars[index] = PciBar::Memory64 {
                    base: ((upper as u64) << 32) | ((raw & 0xFFFF_FFF0) as u64),
                    prefetchable,
                };
                bars[index + 1] = PciBar::UpperHalf64;
                index += 2;
            }
            _ => {
                bars[index] = PciBar::Reserved { raw };
                index += 1;
            }
        }
    }
    bars
}

fn read_device(access: &mut impl PciConfigAccess, address: PciAddress) -> Option<PciDevice> {
    let identity = access.read_u32(address, 0x00);
    let vendor_id = identity as u16;
    if vendor_id == INVALID_VENDOR_ID {
        return None;
    }
    let command_status = access.read_u32(address, 0x04);
    let class_revision = access.read_u32(address, 0x08);
    let header = access.read_u32(address, 0x0C);
    let raw_header_type = (header >> 16) as u8;
    let header_type = raw_header_type & 0x7F;
    let subsystem = if header_type == 0 {
        access.read_u32(address, 0x2C)
    } else {
        0
    };
    let interrupt = access.read_u32(address, 0x3C);
    let secondary_bus = if header_type == 1 {
        let buses = access.read_u32(address, 0x18);
        let secondary = (buses >> 8) as u8;
        (secondary != 0).then_some(secondary)
    } else {
        None
    };

    Some(PciDevice {
        address,
        vendor_id,
        device_id: (identity >> 16) as u16,
        command: command_status as u16,
        status: (command_status >> 16) as u16,
        revision_id: class_revision as u8,
        programming_interface: (class_revision >> 8) as u8,
        subclass: (class_revision >> 16) as u8,
        class_code: (class_revision >> 24) as u8,
        header_type,
        multifunction: raw_header_type & 0x80 != 0,
        subsystem_vendor_id: subsystem as u16,
        subsystem_id: (subsystem >> 16) as u16,
        bars: decode_bars(access, address, header_type),
        bar_sizes: [0; MAX_PCI_BARS],
        interrupt_line: interrupt as u8,
        interrupt_pin: (interrupt >> 8) as u8,
        secondary_bus,
    })
}

fn enqueue_bus(
    bus: u8,
    pending: &mut [u8; 256],
    pending_len: &mut usize,
    queued: &mut [bool; 256],
) {
    if queued[bus as usize] || *pending_len == pending.len() {
        return;
    }
    queued[bus as usize] = true;
    pending[*pending_len] = bus;
    *pending_len += 1;
}

fn enumerate_into(access: &mut impl PciConfigAccess, inventory: &mut PciInventory) {
    inventory.clear();
    let mut pending = [0u8; 256];
    let mut queued = [false; 256];
    let mut pending_len = 0usize;
    let mut cursor = 0usize;
    enqueue_bus(0, &mut pending, &mut pending_len, &mut queued);

    while cursor < pending_len {
        let bus = pending[cursor];
        cursor += 1;
        inventory.buses_scanned += 1;

        for device_number in 0..32u8 {
            let function_zero = PciAddress {
                bus,
                device: device_number,
                function: 0,
            };
            let Some(first) = read_device(access, function_zero) else {
                continue;
            };
            let function_count = if first.multifunction { 8 } else { 1 };

            for function in 0..function_count {
                let address = PciAddress {
                    bus,
                    device: device_number,
                    function,
                };
                let pci_device = if function == 0 {
                    first
                } else {
                    if !function_exists(access, address) {
                        continue;
                    }
                    let Some(device) = read_device(access, address) else {
                        continue;
                    };
                    device
                };

                if pci_device.is_pci_bridge() {
                    if let Some(secondary) = pci_device.secondary_bus {
                        enqueue_bus(secondary, &mut pending, &mut pending_len, &mut queued);
                    }
                }
                // A multifunction host bridge may expose one root bus per
                // function even without a type-1 PCI bridge header.
                if bus == 0
                    && device_number == 0
                    && function != 0
                    && pci_device.class_code == 0x06
                    && pci_device.subclass == 0x00
                {
                    enqueue_bus(function, &mut pending, &mut pending_len, &mut queued);
                }
                inventory.push(pci_device);
            }
        }
    }
}

/// Enumerate all PCI functions reachable from the root bus.
///
/// This value-returning form is primarily useful for host tests. Bare metal
/// enumerates directly into the static inventory to avoid placing the large
/// fixed-capacity table on the small boot stack.
pub fn enumerate_with(access: &mut impl PciConfigAccess) -> PciInventory {
    let mut inventory = PciInventory::new();
    enumerate_into(access, &mut inventory);
    inventory
}

pub static PCI_INVENTORY: Mutex<PciInventory> = Mutex::new(PciInventory::new());

/// Discover PCI topology through ECAM when ACPI supplied a usable segment-zero
/// region, otherwise through the legacy CF8/CFC backend.
#[cfg(target_os = "none")]
pub fn init(
    mapper: &mut OffsetPageTable<'static>,
    frame_allocator: &mut BitmapFrameAllocator,
    ecam_regions: EcamRegions,
) -> (usize, usize, bool, PciConfigBackend) {
    let mut inventory = PCI_INVENTORY.lock();
    let mut backend = PciConfigBackend::Legacy;
    let _config_guard = CONFIG_ACCESS_LOCK.lock();

    if ecam_regions.region_for(0, 0).is_some() {
        let mut access = EcamConfigAccess {
            regions: ecam_regions,
            mapper,
            frame_allocator,
            mapping_failed: false,
        };
        enumerate_into(&mut access, &mut inventory);
        if !access.mapping_failed && !inventory.is_empty() {
            probe_inventory_bars(&mut access, &mut inventory);
            backend = PciConfigBackend::Ecam;
        }
    }

    if backend == PciConfigBackend::Legacy {
        let mut access = LegacyConfigAccess;
        enumerate_into(&mut access, &mut inventory);
        probe_inventory_bars(&mut access, &mut inventory);
    }

    if backend == PciConfigBackend::Ecam {
        *ACTIVE_ECAM_REGIONS.lock() = ecam_regions;
        ACTIVE_CONFIG_BACKEND.store(1, Ordering::Release);
    } else {
        *ACTIVE_ECAM_REGIONS.lock() = EcamRegions::new();
        ACTIVE_CONFIG_BACKEND.store(0, Ordering::Release);
    }

    (
        inventory.len(),
        inventory.buses_scanned(),
        inventory.overflowed(),
        backend,
    )
}

/// Find one function in the boot-time inventory.
pub fn find_device(predicate: impl FnMut(&PciDevice) -> bool) -> Option<PciDevice> {
    PCI_INVENTORY.lock().find(predicate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::vec;
    use std::vec::Vec;

    #[derive(Default)]
    struct MockConfig {
        registers: BTreeMap<(u8, u8, u8, u8), u32>,
        probe_masks: BTreeMap<(u8, u8, u8, u8), u32>,
    }

    impl MockConfig {
        fn write(&mut self, address: PciAddress, offset: u8, value: u32) {
            self.registers.insert(
                (address.bus, address.device, address.function, offset & 0xFC),
                value,
            );
        }

        fn set_probe_mask(&mut self, address: PciAddress, offset: u8, mask: u32) {
            self.probe_masks.insert(
                (address.bus, address.device, address.function, offset & 0xFC),
                mask,
            );
        }

        fn add_device(
            &mut self,
            address: PciAddress,
            vendor: u16,
            device: u16,
            class: u8,
            subclass: u8,
            header_type: u8,
        ) {
            self.write(address, 0x00, ((device as u32) << 16) | vendor as u32);
            self.write(
                address,
                0x08,
                ((class as u32) << 24) | ((subclass as u32) << 16),
            );
            self.write(address, 0x0C, (header_type as u32) << 16);
        }
    }

    impl PciConfigAccess for MockConfig {
        fn read_u32(&mut self, address: PciAddress, offset: u16) -> u32 {
            self.registers
                .get(&(
                    address.bus,
                    address.device,
                    address.function,
                    (offset & 0xFC) as u8,
                ))
                .copied()
                .unwrap_or(u32::MAX)
        }

        fn write_u32(&mut self, address: PciAddress, offset: u16, value: u32) {
            let key = (
                address.bus,
                address.device,
                address.function,
                (offset & 0xFC) as u8,
            );
            let stored = if value == u32::MAX {
                self.probe_masks.get(&key).copied().unwrap_or(value)
            } else {
                value
            };
            self.registers.insert(key, stored);
        }
    }

    fn address(bus: u8, device: u8, function: u8) -> PciAddress {
        PciAddress::new(bus, device, function).unwrap()
    }

    fn mcfg_table(regions: &[EcamRegion]) -> Vec<u8> {
        let length = MCFG_HEADER_LEN + regions.len() * MCFG_ENTRY_LEN;
        let mut table = vec![0u8; length];
        table[..4].copy_from_slice(b"MCFG");
        table[4..8].copy_from_slice(&(length as u32).to_le_bytes());
        table[8] = 1;
        for (index, region) in regions.iter().enumerate() {
            let offset = MCFG_HEADER_LEN + index * MCFG_ENTRY_LEN;
            table[offset..offset + 8].copy_from_slice(&region.physical_base.to_le_bytes());
            table[offset + 8..offset + 10].copy_from_slice(&region.segment_group.to_le_bytes());
            table[offset + 10] = region.start_bus;
            table[offset + 11] = region.end_bus;
        }
        let sum = table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        table[9] = sum.wrapping_neg();
        table
    }

    #[test]
    fn parses_mcfg_regions_and_resolves_extended_config_address() {
        let table = mcfg_table(&[
            EcamRegion {
                physical_base: 0xE000_0000,
                segment_group: 0,
                start_bus: 0,
                end_bus: 63,
            },
            EcamRegion {
                physical_base: 0xE400_0000,
                segment_group: 0,
                start_bus: 64,
                end_bus: 127,
            },
        ]);
        let regions = EcamRegions::parse_mcfg(&table).unwrap();

        assert_eq!(regions.len(), 2);
        assert_eq!(
            regions.config_address(address(65, 3, 2), 0xA40),
            Some(0xE400_0000 + (1 << 20) + (3 << 15) + (2 << 12) + 0xA40)
        );
    }

    #[test]
    fn rejects_mcfg_checksum_alignment_and_overlaps() {
        let first = EcamRegion {
            physical_base: 0xE000_0000,
            segment_group: 0,
            start_bus: 0,
            end_bus: 63,
        };
        let mut bad_checksum = mcfg_table(&[first]);
        bad_checksum[20] ^= 1;
        assert_eq!(
            EcamRegions::parse_mcfg(&bad_checksum),
            Err(McfgError::InvalidChecksum)
        );

        let misaligned = EcamRegion {
            physical_base: 0xE000_1000,
            ..first
        };
        assert_eq!(
            EcamRegions::parse_mcfg(&mcfg_table(&[misaligned])),
            Err(McfgError::InvalidRegion)
        );

        let overlap = EcamRegion {
            physical_base: 0xF000_0000,
            start_bus: 32,
            end_bus: 95,
            ..first
        };
        assert_eq!(
            EcamRegions::parse_mcfg(&mcfg_table(&[first, overlap])),
            Err(McfgError::OverlappingRegions)
        );
    }

    #[test]
    fn follows_bridges_and_multifunction_devices() {
        let mut config = MockConfig::default();
        config.add_device(address(0, 1, 0), 0x1234, 0x0001, 0x02, 0x00, 0x00);
        config.add_device(address(0, 2, 0), 0x1234, 0x0002, 0x0C, 0x03, 0x80);
        config.add_device(address(0, 2, 1), 0x1234, 0x0003, 0x0C, 0x03, 0x00);
        config.add_device(address(0, 3, 0), 0x1234, 0x0004, 0x06, 0x04, 0x01);
        config.write(address(0, 3, 0), 0x18, 2 << 8);
        config.add_device(address(2, 0, 0), 0xABCD, 0x0005, 0x01, 0x08, 0x00);

        let inventory = enumerate_with(&mut config);

        assert_eq!(inventory.len(), 5);
        assert_eq!(inventory.buses_scanned(), 2);
        assert!(inventory
            .devices()
            .any(|device| device.address == address(0, 2, 1)));
        assert!(inventory
            .devices()
            .any(|device| device.address == address(2, 0, 0)));
    }

    #[test]
    fn decodes_io_memory32_and_memory64_bars() {
        let mut config = MockConfig::default();
        let device = address(0, 1, 0);
        config.add_device(device, 0x1234, 0x5678, 0x01, 0x00, 0x00);
        config.write(device, 0x10, 0x0000_C001);
        config.write(device, 0x14, 0xFEBF_0008);
        config.write(device, 0x18, 0x0000_1004);
        config.write(device, 0x1C, 0x0000_0001);

        let inventory = enumerate_with(&mut config);
        let discovered = inventory.devices().next().unwrap();

        assert_eq!(discovered.bars[0], PciBar::Io { base: 0xC000 });
        assert_eq!(
            discovered.bars[1],
            PciBar::Memory32 {
                base: 0xFEBF_0000,
                prefetchable: true,
            }
        );
        assert_eq!(
            discovered.bars[2],
            PciBar::Memory64 {
                base: 0x1_0000_1000,
                prefetchable: false,
            }
        );
        assert_eq!(discovered.bars[3], PciBar::UpperHalf64);
    }

    #[test]
    fn enables_io_decoding_and_bus_mastering_without_dropping_command_bits() {
        let mut device = PciDevice::EMPTY;
        device.command = COMMAND_MEMORY_SPACE | (1 << 6);

        assert_eq!(
            enable_legacy_io_bus_mastering(device),
            COMMAND_MEMORY_SPACE | COMMAND_IO_SPACE | COMMAND_BUS_MASTER | (1 << 6)
        );
    }

    #[test]
    fn probes_bar_sizes_and_restores_config_registers() {
        let mut config = MockConfig::default();
        let address = address(0, 5, 0);
        config.add_device(address, 0x1234, 0x5678, 0x0C, 0x03, 0x00);
        config.write(address, 0x04, 0x0000_0043);
        config.write(address, 0x10, 0xFEBF_0000);
        config.write(address, 0x14, 0x0000_0004);
        config.write(address, 0x18, 0x0000_0001);
        config.set_probe_mask(address, 0x10, 0xFFFF_0000);
        config.set_probe_mask(address, 0x14, 0xFFFF_C004);
        config.set_probe_mask(address, 0x18, 0xFFFF_FFFF);

        let inventory = enumerate_with(&mut config);
        let device = *inventory.devices().next().unwrap();
        let memory32 = probe_bar_with(&mut config, device, 0).unwrap();
        let memory64 = probe_bar_with(&mut config, device, 1).unwrap();

        assert_eq!(memory32.size, 64 * 1024);
        assert_eq!(memory32.base(), Some(0xFEBF_0000));
        assert_eq!(memory64.size, 16 * 1024);
        assert_eq!(memory64.base(), Some(0x1_0000_0000));
        assert_eq!(config.read_u32(address, 0x04), 0x0000_0043);
        assert_eq!(config.read_u32(address, 0x10), 0xFEBF_0000);
        assert_eq!(config.read_u32(address, 0x14), 0x0000_0004);
        assert_eq!(config.read_u32(address, 0x18), 0x0000_0001);
    }

    #[test]
    fn rejects_invalid_bar_mask_after_restoring_device() {
        let mut config = MockConfig::default();
        let address = address(0, 6, 0);
        config.add_device(address, 0x1234, 0x5678, 0x01, 0x00, 0x00);
        config.write(address, 0x04, 0x0000_0003);
        config.write(address, 0x10, 0x8000_0000);
        config.set_probe_mask(address, 0x10, 0);
        let device = *enumerate_with(&mut config).devices().next().unwrap();

        assert_eq!(
            probe_bar_with(&mut config, device, 0),
            Err(PciBarProbeError::InvalidSize)
        );
        assert_eq!(config.read_u32(address, 0x04), 0x0000_0003);
        assert_eq!(config.read_u32(address, 0x10), 0x8000_0000);
    }

    #[test]
    fn plans_page_bounded_uncached_mmio_aperture() {
        let mapping = plan_mmio_mapping(
            PciBarResource {
                index: 0,
                bar: PciBar::Memory32 {
                    base: 0xFEBF_0100,
                    prefetchable: false,
                },
                size: 0x100,
            },
            0xFFFF_A000_0000_0000,
        )
        .unwrap();

        assert_eq!(mapping.mapped_physical_base, 0xFEBF_0000);
        assert_eq!(mapping.virtual_base, 0xFFFF_A000_0000_0100);
        assert_eq!(mapping.mapped_len, PAGE_SIZE);
        assert!(mapping.contains(0xFC, 4));
        assert!(!mapping.contains(0xFD, 4));
    }
}

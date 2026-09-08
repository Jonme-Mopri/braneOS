//! xHCI discovery, reset and initial runtime structures.
//!
//! The controller is brought to the Running state with a Device Context Base
//! Address Array, Command Ring and one polling Event Ring. Supported Protocol
//! capabilities and root ports are inspected, and the first connected device
//! is reset, assigned a slot and addressed through its default control pipe.
//! HID transfers and MSI/MSI-X are intentionally left to the next increments.

#![allow(dead_code)]

#[cfg(target_os = "none")]
use core::sync::atomic::{fence, Ordering};

use spin::{Mutex, Once};

use crate::dma::{DmaError, DmaRegion};
#[cfg(target_os = "none")]
use crate::memory::frame_allocator::BitmapFrameAllocator;
use crate::memory::frame_allocator::FRAME_SIZE;
use crate::pci::{self, MmioMapping, PciDevice};

const SERIAL_BUS_CLASS: u8 = 0x0C;
const USB_SUBCLASS: u8 = 0x03;
const XHCI_PROGRAMMING_INTERFACE: u8 = 0x30;

// Capability registers, relative to BAR0.
const CAPLENGTH_HCIVERSION: u64 = 0x00;
const HCSPARAMS1: u64 = 0x04;
const HCSPARAMS2: u64 = 0x08;
const HCCPARAMS1: u64 = 0x10;
const DBOFF: u64 = 0x14;
const RTSOFF: u64 = 0x18;
const MIN_CAPABILITY_LENGTH: u8 = 0x20;

// Operational registers, relative to the operational base.
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const PAGESIZE: u64 = 0x08;
const CRCR: u64 = 0x18;
const DCBAAP: u64 = 0x30;
const CONFIG: u64 = 0x38;
const PORT_REGISTER_BASE: u64 = 0x400;
const PORT_REGISTER_STRIDE: u64 = 0x10;
const PORTSC: u64 = 0;

// Primary interrupter registers, relative to the runtime base.
const ERSTSZ: u64 = 0x28;
const ERSTBA: u64 = 0x30;
const ERDP: u64 = 0x38;

const USBCMD_RUN_STOP: u32 = 1 << 0;
const USBCMD_HOST_CONTROLLER_RESET: u32 = 1 << 1;
const USBSTS_HOST_CONTROLLER_HALTED: u32 = 1 << 0;
const USBSTS_CONTROLLER_NOT_READY: u32 = 1 << 11;
const USBSTS_HOST_CONTROLLER_ERROR: u32 = 1 << 12;

const PORTSC_CURRENT_CONNECT_STATUS: u32 = 1 << 0;
const PORTSC_PORT_ENABLED: u32 = 1 << 1;
const PORTSC_PORT_RESET: u32 = 1 << 4;
const PORTSC_PORT_POWER: u32 = 1 << 9;
const PORTSC_SPEED_SHIFT: u32 = 10;
const PORTSC_SPEED_MASK: u32 = 0xF;
const PORTSC_CHANGE_BITS: u32 = 0x7F << 17;
const PORTSC_WARM_PORT_RESET: u32 = 1 << 31;

const TRB_TYPE_LINK: u32 = 6;
const TRB_TYPE_ENABLE_SLOT_COMMAND: u32 = 9;
const TRB_TYPE_ADDRESS_DEVICE_COMMAND: u32 = 11;
const TRB_TYPE_NO_OP_COMMAND: u32 = 23;
const TRB_TYPE_COMMAND_COMPLETION_EVENT: u32 = 33;
const TRB_CYCLE: u32 = 1 << 0;
const LINK_TRB_TOGGLE_CYCLE: u32 = 1 << 1;
const TRB_TYPE_SHIFT: u32 = 10;
const TRB_TYPE_MASK: u32 = 0x3F;
const COMPLETION_CODE_SUCCESS: u8 = 1;
const EVENT_HANDLER_BUSY: u64 = 1 << 3;
const COMMAND_RING_TRBS: usize = FRAME_SIZE / core::mem::size_of::<Trb>();
const COMMAND_RING_USABLE_TRBS: usize = COMMAND_RING_TRBS - 1;
const EVENT_RING_TRBS: usize = FRAME_SIZE / core::mem::size_of::<Trb>();
const MAX_POLL_SPINS: usize = 10_000_000;
const EXTENDED_CAPABILITY_SUPPORTED_PROTOCOL: u8 = 2;
const SUPPORTED_PROTOCOL_NAME_USB: u32 = 0x2042_5355;
const MAX_SUPPORTED_PROTOCOLS: usize = 8;
const CONTEXTS_PER_DEVICE: usize = 32;
const INPUT_CONTEXTS_PER_DEVICE: usize = CONTEXTS_PER_DEVICE + 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XhciControllerInfo {
    pub pci_device: PciDevice,
    pub mmio: MmioMapping,
    pub capability_length: u8,
    pub interface_version: u16,
    pub max_device_slots: u8,
    pub max_interrupters: u16,
    pub max_ports: u8,
    pub page_size: usize,
    pub scratchpad_buffers: u16,
    pub enabled_slots: u8,
    pub command_ring_trbs: usize,
    pub event_ring_trbs: usize,
    pub supported_protocols: u8,
    pub connected_ports: u8,
    pub addressed_port: u8,
    pub addressed_slot: u8,
    pub addressed_speed_id: u8,
    pub command_probe_completed: bool,
    pub running: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XhciInitError {
    AlreadyInitialized,
    MissingBar0,
    MmioMap(pci::MmioMapError),
    Dma(DmaError),
    RegisterOutsideBar,
    InvalidCapabilityLength,
    InvalidCapabilities,
    InvalidRegisterLayout,
    UnsupportedPageSize,
    ScratchpadArrayTooLarge,
    ControllerNotReadyTimeout,
    ControllerHaltTimeout,
    ControllerResetTimeout,
    ControllerStartTimeout,
    CommandCompletionTimeout,
    InvalidCommandCompletion,
    CommandFailed(u8),
    InvalidExtendedCapability,
    PortResetTimeout,
    PortNotEnabled,
    ControllerError,
}

impl From<DmaError> for XhciInitError {
    fn from(error: DmaError) -> Self {
        Self::Dma(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RegisterLayout {
    operational_base: u64,
    runtime_base: u64,
    doorbell_base: u64,
}

impl RegisterLayout {
    fn new(
        mmio: MmioMapping,
        capability_length: u8,
        runtime_offset: u32,
        doorbell_offset: u32,
    ) -> Result<Self, XhciInitError> {
        let layout = Self {
            operational_base: u64::from(capability_length),
            runtime_base: u64::from(runtime_offset & !0x1F),
            doorbell_base: u64::from(doorbell_offset & !0x03),
        };
        let required = [
            (layout.operational_base + CONFIG, 4),
            (layout.operational_base + CRCR, 8),
            (layout.operational_base + DCBAAP, 8),
            (layout.runtime_base + ERDP, 8),
            (layout.doorbell_base, 4),
        ];
        if layout.runtime_base == 0
            || layout.doorbell_base == 0
            || required
                .into_iter()
                .any(|(offset, width)| !mmio.contains(offset, width))
        {
            return Err(XhciInitError::InvalidRegisterLayout);
        }
        Ok(layout)
    }

    const fn operational(self, offset: u64) -> u64 {
        self.operational_base + offset
    }

    const fn runtime(self, offset: u64) -> u64 {
        self.runtime_base + offset
    }

    fn port(self, port_id: u8, offset: u64) -> Result<u64, XhciInitError> {
        if port_id == 0 {
            return Err(XhciInitError::InvalidCapabilities);
        }
        self.operational_base
            .checked_add(PORT_REGISTER_BASE)
            .and_then(|base| base.checked_add(u64::from(port_id - 1) * PORT_REGISTER_STRIDE))
            .and_then(|base| base.checked_add(offset))
            .ok_or(XhciInitError::InvalidRegisterLayout)
    }
}

#[repr(C, align(16))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Trb {
    parameter: u64,
    status: u32,
    control: u32,
}

impl Trb {
    const fn link(ring_physical_base: u64) -> Self {
        Self {
            parameter: ring_physical_base,
            status: 0,
            control: (TRB_TYPE_LINK << TRB_TYPE_SHIFT) | LINK_TRB_TOGGLE_CYCLE | TRB_CYCLE,
        }
    }

    const fn no_op_command() -> Self {
        Self {
            parameter: 0,
            status: 0,
            control: TRB_TYPE_NO_OP_COMMAND << TRB_TYPE_SHIFT,
        }
    }

    const fn enable_slot_command(slot_type: u8) -> Self {
        Self {
            parameter: 0,
            status: 0,
            control: (TRB_TYPE_ENABLE_SLOT_COMMAND << TRB_TYPE_SHIFT)
                | ((slot_type as u32 & 0x1F) << 16),
        }
    }

    const fn address_device_command(input_context: u64, slot_id: u8) -> Self {
        Self {
            parameter: input_context,
            status: 0,
            control: (TRB_TYPE_ADDRESS_DEVICE_COMMAND << TRB_TYPE_SHIFT) | ((slot_id as u32) << 24),
        }
    }

    const fn with_cycle(mut self, cycle: bool) -> Self {
        self.control &= !TRB_CYCLE;
        if cycle {
            self.control |= TRB_CYCLE;
        }
        self
    }

    const fn trb_type(self) -> u32 {
        (self.control >> TRB_TYPE_SHIFT) & TRB_TYPE_MASK
    }

    const fn completion_code(self) -> u8 {
        (self.status >> 24) as u8
    }

    const fn slot_id(self) -> u8 {
        (self.control >> 24) as u8
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SupportedProtocol {
    major: u8,
    minor: u8,
    port_offset: u8,
    port_count: u8,
    slot_type: u8,
}

impl SupportedProtocol {
    fn contains(self, port_id: u8) -> bool {
        let first = u16::from(self.port_offset);
        let end = first + u16::from(self.port_count);
        let port = u16::from(port_id);
        port >= first && port < end
    }
}

#[derive(Debug, Clone, Copy)]
struct SupportedProtocols {
    entries: [Option<SupportedProtocol>; MAX_SUPPORTED_PROTOCOLS],
    count: u8,
}

impl SupportedProtocols {
    const fn new() -> Self {
        Self {
            entries: [None; MAX_SUPPORTED_PROTOCOLS],
            count: 0,
        }
    }

    fn push(&mut self, protocol: SupportedProtocol) -> Result<(), XhciInitError> {
        let index = usize::from(self.count);
        let entry = self
            .entries
            .get_mut(index)
            .ok_or(XhciInitError::InvalidExtendedCapability)?;
        *entry = Some(protocol);
        self.count += 1;
        Ok(())
    }

    fn for_port(self, port_id: u8) -> Option<SupportedProtocol> {
        self.entries
            .into_iter()
            .flatten()
            .find(|protocol| protocol.contains(port_id))
    }
}

struct XhciDevice {
    slot_id: u8,
    port_id: u8,
    speed_id: u8,
    device_context: DmaRegion,
    input_context: DmaRegion,
    endpoint_zero_ring: DmaRegion,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EventRingSegmentTableEntry {
    ring_segment_base: u64,
    ring_segment_size: u32,
    reserved: u32,
}

struct XhciController {
    info: XhciControllerInfo,
    layout: RegisterLayout,
    dcbaa: DmaRegion,
    command_ring: DmaRegion,
    event_ring: DmaRegion,
    event_ring_segment_table: DmaRegion,
    scratchpad_array: Option<DmaRegion>,
    scratchpad_storage: Option<DmaRegion>,
    command_enqueue_index: usize,
    command_cycle: bool,
    event_dequeue_index: usize,
    event_cycle: bool,
    first_device: Option<XhciDevice>,
}

static XHCI_CONTROLLER: Once<Mutex<XhciController>> = Once::new();

pub fn find_controller() -> Option<PciDevice> {
    pci::find_device(|device| {
        device.class_code == SERIAL_BUS_CLASS
            && device.subclass == USB_SUBCLASS
            && device.programming_interface == XHCI_PROGRAMMING_INTERFACE
    })
}

fn parse_capabilities(
    pci_device: PciDevice,
    mmio: MmioMapping,
    capability_and_version: u32,
    structural_parameters: u32,
) -> Result<XhciControllerInfo, XhciInitError> {
    let capability_length = capability_and_version as u8;
    let interface_version = (capability_and_version >> 16) as u16;
    if capability_length < MIN_CAPABILITY_LENGTH
        || capability_length & 3 != 0
        || u64::from(capability_length) >= mmio.size
    {
        return Err(XhciInitError::InvalidCapabilityLength);
    }

    let max_device_slots = structural_parameters as u8;
    let max_interrupters = ((structural_parameters >> 8) & 0x7FF) as u16;
    let max_ports = (structural_parameters >> 24) as u8;
    if interface_version < 0x0090
        || max_device_slots == 0
        || max_interrupters == 0
        || max_ports == 0
    {
        return Err(XhciInitError::InvalidCapabilities);
    }

    Ok(XhciControllerInfo {
        pci_device,
        mmio,
        capability_length,
        interface_version,
        max_device_slots,
        max_interrupters,
        max_ports,
        page_size: 0,
        scratchpad_buffers: 0,
        enabled_slots: 0,
        command_ring_trbs: 0,
        event_ring_trbs: 0,
        supported_protocols: 0,
        connected_ports: 0,
        addressed_port: 0,
        addressed_slot: 0,
        addressed_speed_id: 0,
        command_probe_completed: false,
        running: false,
    })
}

fn decode_supported_protocol(
    header: u32,
    name: u32,
    ports: u32,
    slot: u32,
) -> Result<Option<SupportedProtocol>, XhciInitError> {
    if header as u8 != EXTENDED_CAPABILITY_SUPPORTED_PROTOCOL {
        return Ok(None);
    }
    if name != SUPPORTED_PROTOCOL_NAME_USB {
        return Err(XhciInitError::InvalidExtendedCapability);
    }
    let protocol = SupportedProtocol {
        major: (header >> 24) as u8,
        minor: (header >> 16) as u8,
        port_offset: ports as u8,
        port_count: (ports >> 8) as u8,
        slot_type: (slot & 0x1F) as u8,
    };
    if protocol.major < 2 || protocol.port_offset == 0 || protocol.port_count == 0 {
        return Err(XhciInitError::InvalidExtendedCapability);
    }
    Ok(Some(protocol))
}

const fn endpoint_zero_max_packet_size(speed_id: u8) -> u16 {
    match speed_id {
        3 => 64,
        4 | 5 => 512,
        _ => 8,
    }
}

const fn scratchpad_count(structural_parameters_2: u32) -> u16 {
    let high = ((structural_parameters_2 >> 21) & 0x1F) as u16;
    let low = ((structural_parameters_2 >> 27) & 0x1F) as u16;
    (high << 5) | low
}

fn select_page_size(page_size_mask: u16, scratchpads: u16) -> Option<usize> {
    let pointer_bytes = usize::from(scratchpads).checked_mul(core::mem::size_of::<u64>())?;
    (0..16).find_map(|bit| {
        if page_size_mask & (1 << bit) == 0 {
            return None;
        }
        let size = FRAME_SIZE.checked_shl(bit)?;
        (pointer_bytes <= size).then_some(size)
    })
}

#[cfg(target_os = "none")]
fn mmio_address(mapping: MmioMapping, offset: u64, width: u64) -> Result<u64, XhciInitError> {
    mapping
        .virtual_address(offset, width)
        .ok_or(XhciInitError::RegisterOutsideBar)
}

#[cfg(target_os = "none")]
fn read_mmio_u32(mapping: MmioMapping, offset: u64) -> Result<u32, XhciInitError> {
    let address = mmio_address(mapping, offset, core::mem::size_of::<u32>() as u64)?;
    Ok(unsafe { core::ptr::read_volatile(address as *const u32) })
}

#[cfg(target_os = "none")]
fn write_mmio_u32(mapping: MmioMapping, offset: u64, value: u32) -> Result<(), XhciInitError> {
    let address = mmio_address(mapping, offset, core::mem::size_of::<u32>() as u64)?;
    unsafe { core::ptr::write_volatile(address as *mut u32, value) };
    Ok(())
}

#[cfg(target_os = "none")]
fn write_mmio_u64(mapping: MmioMapping, offset: u64, value: u64) -> Result<(), XhciInitError> {
    mmio_address(mapping, offset, core::mem::size_of::<u64>() as u64)?;
    write_mmio_u32(mapping, offset, value as u32)?;
    write_mmio_u32(mapping, offset + 4, (value >> 32) as u32)
}

#[cfg(target_os = "none")]
fn wait_for_bits(
    mapping: MmioMapping,
    offset: u64,
    mask: u32,
    expected: u32,
    timeout: XhciInitError,
) -> Result<(), XhciInitError> {
    for _ in 0..MAX_POLL_SPINS {
        if read_mmio_u32(mapping, offset)? & mask == expected {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(timeout)
}

#[cfg(target_os = "none")]
fn discover_supported_protocols(
    mapping: MmioMapping,
    capability_parameters: u32,
    max_ports: u8,
) -> Result<SupportedProtocols, XhciInitError> {
    let mut protocols = SupportedProtocols::new();
    let mut offset = u64::from((capability_parameters >> 16) as u16) * 4;
    let mut visited = 0usize;

    while offset != 0 {
        visited += 1;
        if visited > 64 || !mapping.contains(offset, 4) {
            return Err(XhciInitError::InvalidExtendedCapability);
        }
        let header = read_mmio_u32(mapping, offset)?;
        if header as u8 == EXTENDED_CAPABILITY_SUPPORTED_PROTOCOL {
            let protocol = decode_supported_protocol(
                header,
                read_mmio_u32(mapping, offset + 4)?,
                read_mmio_u32(mapping, offset + 8)?,
                read_mmio_u32(mapping, offset + 12)?,
            )?
            .ok_or(XhciInitError::InvalidExtendedCapability)?;
            let last_port = u16::from(protocol.port_offset)
                .checked_add(u16::from(protocol.port_count) - 1)
                .ok_or(XhciInitError::InvalidExtendedCapability)?;
            if last_port > u16::from(max_ports) {
                return Err(XhciInitError::InvalidExtendedCapability);
            }
            protocols.push(protocol)?;
        }

        let next = ((header >> 8) & 0xFF) as u64;
        if next == 0 {
            break;
        }
        offset = offset
            .checked_add(next * 4)
            .ok_or(XhciInitError::InvalidExtendedCapability)?;
    }
    Ok(protocols)
}

#[cfg(target_os = "none")]
fn reset_controller(mapping: MmioMapping, layout: RegisterLayout) -> Result<(), XhciInitError> {
    let command = layout.operational(USBCMD);
    let status = layout.operational(USBSTS);

    wait_for_bits(
        mapping,
        status,
        USBSTS_CONTROLLER_NOT_READY,
        0,
        XhciInitError::ControllerNotReadyTimeout,
    )?;

    let current_command = read_mmio_u32(mapping, command)?;
    if current_command & USBCMD_RUN_STOP != 0 {
        write_mmio_u32(mapping, command, current_command & !USBCMD_RUN_STOP)?;
    }
    wait_for_bits(
        mapping,
        status,
        USBSTS_HOST_CONTROLLER_HALTED,
        USBSTS_HOST_CONTROLLER_HALTED,
        XhciInitError::ControllerHaltTimeout,
    )?;

    write_mmio_u32(mapping, command, USBCMD_HOST_CONTROLLER_RESET)?;
    wait_for_bits(
        mapping,
        command,
        USBCMD_HOST_CONTROLLER_RESET,
        0,
        XhciInitError::ControllerResetTimeout,
    )?;
    wait_for_bits(
        mapping,
        status,
        USBSTS_CONTROLLER_NOT_READY,
        0,
        XhciInitError::ControllerNotReadyTimeout,
    )
}

#[cfg(target_os = "none")]
fn reset_port(
    mapping: MmioMapping,
    layout: RegisterLayout,
    port_id: u8,
    protocol: Option<SupportedProtocol>,
) -> Result<u8, XhciInitError> {
    let register = layout.port(port_id, PORTSC)?;
    let current = read_mmio_u32(mapping, register)?;
    if current & PORTSC_CURRENT_CONNECT_STATUS == 0 {
        return Err(XhciInitError::PortNotEnabled);
    }

    // PED and the change flags are write-one-to-clear. Never echo those bits
    // while requesting power/reset or an enabled port can be disabled again.
    let neutral = current & !(PORTSC_PORT_ENABLED | PORTSC_CHANGE_BITS);
    let reset = if protocol.is_some_and(|candidate| candidate.major >= 3) {
        PORTSC_WARM_PORT_RESET
    } else {
        PORTSC_PORT_RESET
    };
    write_mmio_u32(mapping, register, neutral | PORTSC_PORT_POWER | reset)?;
    wait_for_bits(mapping, register, reset, 0, XhciInitError::PortResetTimeout)?;
    wait_for_bits(
        mapping,
        register,
        PORTSC_CURRENT_CONNECT_STATUS | PORTSC_PORT_ENABLED,
        PORTSC_CURRENT_CONNECT_STATUS | PORTSC_PORT_ENABLED,
        XhciInitError::PortNotEnabled,
    )?;

    let completed = read_mmio_u32(mapping, register)?;
    let clear_changes = (completed & !(PORTSC_PORT_ENABLED | PORTSC_CHANGE_BITS))
        | (completed & PORTSC_CHANGE_BITS);
    write_mmio_u32(mapping, register, clear_changes)?;
    Ok(((completed >> PORTSC_SPEED_SHIFT) & PORTSC_SPEED_MASK) as u8)
}

#[cfg(target_os = "none")]
fn allocate_scratchpads(
    frame_allocator: &mut BitmapFrameAllocator,
    physical_memory_offset: u64,
    page_size: usize,
    count: u16,
) -> Result<(Option<DmaRegion>, Option<DmaRegion>), XhciInitError> {
    if count == 0 {
        return Ok((None, None));
    }
    let array_bytes = usize::from(count)
        .checked_mul(core::mem::size_of::<u64>())
        .ok_or(XhciInitError::ScratchpadArrayTooLarge)?;
    if array_bytes > page_size {
        return Err(XhciInitError::ScratchpadArrayTooLarge);
    }
    let storage_bytes = usize::from(count)
        .checked_mul(page_size)
        .ok_or(XhciInitError::ScratchpadArrayTooLarge)?;
    let array = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        array_bytes,
        page_size,
    )?;
    let storage = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        storage_bytes,
        page_size,
    )?;
    for index in 0..usize::from(count) {
        let pointer = array
            .pointer_at::<u64>(index * core::mem::size_of::<u64>())
            .ok_or(XhciInitError::ScratchpadArrayTooLarge)?;
        let physical = storage
            .physical_at(index * page_size)
            .ok_or(XhciInitError::ScratchpadArrayTooLarge)?;
        unsafe { core::ptr::write_volatile(pointer, physical) };
    }
    Ok((Some(array), Some(storage)))
}

#[cfg(target_os = "none")]
type RuntimeRegions = (
    DmaRegion,
    DmaRegion,
    DmaRegion,
    DmaRegion,
    Option<DmaRegion>,
    Option<DmaRegion>,
);

#[cfg(target_os = "none")]
fn initialize_runtime_structures(
    frame_allocator: &mut BitmapFrameAllocator,
    physical_memory_offset: u64,
    page_size: usize,
    scratchpads: u16,
    enabled_slots: u8,
) -> Result<RuntimeRegions, XhciInitError> {
    let dcbaa_bytes = (usize::from(enabled_slots) + 1)
        .checked_mul(core::mem::size_of::<u64>())
        .ok_or(XhciInitError::InvalidCapabilities)?;
    if dcbaa_bytes > page_size {
        return Err(XhciInitError::InvalidCapabilities);
    }

    let dcbaa = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        dcbaa_bytes,
        FRAME_SIZE,
    )?;
    let command_ring = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        FRAME_SIZE,
        FRAME_SIZE,
    )?;
    let event_ring = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        FRAME_SIZE,
        FRAME_SIZE,
    )?;
    let event_ring_segment_table = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        core::mem::size_of::<EventRingSegmentTableEntry>(),
        FRAME_SIZE,
    )?;
    let (scratchpad_array, scratchpad_storage) = allocate_scratchpads(
        frame_allocator,
        physical_memory_offset,
        page_size,
        scratchpads,
    )?;

    let link = Trb::link(command_ring.physical_start());
    let link_pointer = command_ring
        .pointer_at::<Trb>(COMMAND_RING_USABLE_TRBS * core::mem::size_of::<Trb>())
        .ok_or(XhciInitError::InvalidCapabilities)?;
    unsafe { core::ptr::write_volatile(link_pointer, link) };

    let erst_entry = EventRingSegmentTableEntry {
        ring_segment_base: event_ring.physical_start(),
        ring_segment_size: EVENT_RING_TRBS as u32,
        reserved: 0,
    };
    let erst_pointer = event_ring_segment_table
        .pointer_at::<EventRingSegmentTableEntry>(0)
        .ok_or(XhciInitError::InvalidCapabilities)?;
    unsafe { core::ptr::write_volatile(erst_pointer, erst_entry) };

    if let Some(array) = scratchpad_array {
        let dcbaa_zero = dcbaa
            .pointer_at::<u64>(0)
            .ok_or(XhciInitError::InvalidCapabilities)?;
        unsafe { core::ptr::write_volatile(dcbaa_zero, array.physical_start()) };
    }
    fence(Ordering::Release);

    Ok((
        dcbaa,
        command_ring,
        event_ring,
        event_ring_segment_table,
        scratchpad_array,
        scratchpad_storage,
    ))
}

#[cfg(target_os = "none")]
fn write_dma_u32(region: DmaRegion, offset: usize, value: u32) -> Result<(), XhciInitError> {
    let pointer = region
        .pointer_at::<u32>(offset)
        .ok_or(XhciInitError::InvalidCapabilities)?;
    unsafe { core::ptr::write_volatile(pointer, value) };
    Ok(())
}

#[cfg(target_os = "none")]
fn write_dma_u64(region: DmaRegion, offset: usize, value: u64) -> Result<(), XhciInitError> {
    let pointer = region
        .pointer_at::<u64>(offset)
        .ok_or(XhciInitError::InvalidCapabilities)?;
    unsafe { core::ptr::write_volatile(pointer, value) };
    Ok(())
}

#[cfg(target_os = "none")]
fn allocate_device(
    frame_allocator: &mut BitmapFrameAllocator,
    physical_memory_offset: u64,
    dcbaa: DmaRegion,
    context_size: usize,
    slot_id: u8,
    port_id: u8,
    speed_id: u8,
) -> Result<XhciDevice, XhciInitError> {
    let device_context = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        context_size * CONTEXTS_PER_DEVICE,
        FRAME_SIZE,
    )?;
    let input_context = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        context_size * INPUT_CONTEXTS_PER_DEVICE,
        FRAME_SIZE,
    )?;
    let endpoint_zero_ring = DmaRegion::allocate(
        frame_allocator,
        physical_memory_offset,
        FRAME_SIZE,
        FRAME_SIZE,
    )?;

    let transfer_link = Trb::link(endpoint_zero_ring.physical_start());
    let transfer_link_pointer = endpoint_zero_ring
        .pointer_at::<Trb>(COMMAND_RING_USABLE_TRBS * core::mem::size_of::<Trb>())
        .ok_or(XhciInitError::InvalidCapabilities)?;
    unsafe { core::ptr::write_volatile(transfer_link_pointer, transfer_link) };

    // Input Control Context: add the Slot and Endpoint 0 contexts.
    write_dma_u32(input_context, 4, 0b11)?;

    // Slot Context: one valid endpoint, root-port routing and the speed ID
    // reported by PORTSC after reset.
    let slot_context = context_size;
    write_dma_u32(
        input_context,
        slot_context,
        (u32::from(speed_id) << 20) | (1 << 27),
    )?;
    write_dma_u32(input_context, slot_context + 4, u32::from(port_id) << 16)?;

    // Default Control Endpoint Context. DCS starts at one, matching the
    // initial transfer-ring cycle state.
    let endpoint_context = context_size * 2;
    let max_packet_size = endpoint_zero_max_packet_size(speed_id);
    write_dma_u32(
        input_context,
        endpoint_context + 4,
        (u32::from(max_packet_size) << 16) | (3 << 1) | (4 << 3),
    )?;
    write_dma_u64(
        input_context,
        endpoint_context + 8,
        endpoint_zero_ring.physical_start() | 1,
    )?;
    write_dma_u32(input_context, endpoint_context + 16, 8)?;

    write_dma_u64(
        dcbaa,
        usize::from(slot_id) * core::mem::size_of::<u64>(),
        device_context.physical_start(),
    )?;
    fence(Ordering::Release);

    Ok(XhciDevice {
        slot_id,
        port_id,
        speed_id,
        device_context,
        input_context,
        endpoint_zero_ring,
    })
}

#[cfg(target_os = "none")]
impl XhciController {
    fn advance_command_ring(&mut self) -> Result<(), XhciInitError> {
        self.command_enqueue_index += 1;
        if self.command_enqueue_index == COMMAND_RING_USABLE_TRBS {
            let link_pointer = self
                .command_ring
                .pointer_at::<Trb>(COMMAND_RING_USABLE_TRBS * core::mem::size_of::<Trb>())
                .ok_or(XhciInitError::InvalidCapabilities)?;
            unsafe {
                core::ptr::write_volatile(
                    link_pointer,
                    Trb::link(self.command_ring.physical_start()).with_cycle(self.command_cycle),
                )
            };
            self.command_enqueue_index = 0;
            self.command_cycle = !self.command_cycle;
        }
        Ok(())
    }

    fn advance_event_ring(&mut self) -> Result<(), XhciInitError> {
        self.event_dequeue_index += 1;
        if self.event_dequeue_index == EVENT_RING_TRBS {
            self.event_dequeue_index = 0;
            self.event_cycle = !self.event_cycle;
        }
        let dequeue = self
            .event_ring
            .physical_at(self.event_dequeue_index * core::mem::size_of::<Trb>())
            .ok_or(XhciInitError::InvalidCapabilities)?;
        write_mmio_u64(
            self.info.mmio,
            self.layout.runtime(ERDP),
            dequeue | EVENT_HANDLER_BUSY,
        )
    }

    fn submit_command(&mut self, command: Trb) -> Result<Trb, XhciInitError> {
        let command_offset = self.command_enqueue_index * core::mem::size_of::<Trb>();
        let command_physical = self
            .command_ring
            .physical_at(command_offset)
            .ok_or(XhciInitError::InvalidCapabilities)?;
        let command_pointer = self
            .command_ring
            .pointer_at::<Trb>(command_offset)
            .ok_or(XhciInitError::InvalidCapabilities)?;
        unsafe {
            core::ptr::write_volatile(command_pointer, command.with_cycle(self.command_cycle))
        };
        fence(Ordering::Release);
        self.advance_command_ring()?;
        write_mmio_u32(self.info.mmio, self.layout.doorbell_base, 0)?;

        for _ in 0..MAX_POLL_SPINS {
            let event_pointer = self
                .event_ring
                .pointer_at::<Trb>(self.event_dequeue_index * core::mem::size_of::<Trb>())
                .ok_or(XhciInitError::InvalidCapabilities)?;
            let event = unsafe { core::ptr::read_volatile(event_pointer) };
            let event_cycle = event.control & TRB_CYCLE != 0;
            if event_cycle != self.event_cycle {
                core::hint::spin_loop();
                continue;
            }
            fence(Ordering::Acquire);
            self.advance_event_ring()?;

            if event.trb_type() != TRB_TYPE_COMMAND_COMPLETION_EVENT {
                continue;
            }
            if event.parameter & !0xF != command_physical {
                continue;
            }
            if event.completion_code() != COMPLETION_CODE_SUCCESS {
                return Err(XhciInitError::CommandFailed(event.completion_code()));
            }
            return Ok(event);
        }
        Err(XhciInitError::CommandCompletionTimeout)
    }

    fn address_first_connected_device(
        &mut self,
        frame_allocator: &mut BitmapFrameAllocator,
        physical_memory_offset: u64,
        protocols: SupportedProtocols,
        context_size: usize,
    ) -> Result<(), XhciInitError> {
        let mut first_connected = None;
        let mut connected_ports = 0u8;
        for port_id in 1..=self.info.max_ports {
            let port_status = read_mmio_u32(self.info.mmio, self.layout.port(port_id, PORTSC)?)?;
            if port_status & PORTSC_CURRENT_CONNECT_STATUS != 0 {
                connected_ports = connected_ports.saturating_add(1);
                if first_connected.is_none() {
                    first_connected = Some(port_id);
                }
            }
        }
        self.info.connected_ports = connected_ports;

        let Some(port_id) = first_connected else {
            return Ok(());
        };
        let protocol = protocols.for_port(port_id);
        let speed_id = reset_port(self.info.mmio, self.layout, port_id, protocol)?;
        let slot_type = protocol.map_or(0, |candidate| candidate.slot_type);
        let completion = self.submit_command(Trb::enable_slot_command(slot_type))?;
        let slot_id = completion.slot_id();
        if slot_id == 0 || slot_id > self.info.enabled_slots {
            return Err(XhciInitError::InvalidCommandCompletion);
        }

        let device = allocate_device(
            frame_allocator,
            physical_memory_offset,
            self.dcbaa,
            context_size,
            slot_id,
            port_id,
            speed_id,
        )?;
        self.submit_command(Trb::address_device_command(
            device.input_context.physical_start(),
            slot_id,
        ))?;

        self.info.addressed_port = port_id;
        self.info.addressed_slot = slot_id;
        self.info.addressed_speed_id = speed_id;
        self.first_device = Some(device);
        Ok(())
    }
}

/// Discover, reset and start the first xHCI controller with polling rings.
#[cfg(target_os = "none")]
pub fn init(
    mapper: &mut x86_64::structures::paging::OffsetPageTable<'static>,
    frame_allocator: &mut BitmapFrameAllocator,
    physical_memory_offset: u64,
) -> Result<Option<XhciControllerInfo>, XhciInitError> {
    if XHCI_CONTROLLER.get().is_some() {
        return Err(XhciInitError::AlreadyInitialized);
    }
    let Some(device) = find_controller() else {
        return Ok(None);
    };
    let resource = device.bar_resource(0).ok_or(XhciInitError::MissingBar0)?;
    let mmio =
        pci::map_bar_mmio(mapper, frame_allocator, resource).map_err(XhciInitError::MmioMap)?;
    pci::enable_memory_bus_mastering(device);

    let capability_and_version = read_mmio_u32(mmio, CAPLENGTH_HCIVERSION)?;
    let structural_parameters_1 = read_mmio_u32(mmio, HCSPARAMS1)?;
    let structural_parameters_2 = read_mmio_u32(mmio, HCSPARAMS2)?;
    let capability_parameters_1 = read_mmio_u32(mmio, HCCPARAMS1)?;
    let runtime_offset = read_mmio_u32(mmio, RTSOFF)?;
    let doorbell_offset = read_mmio_u32(mmio, DBOFF)?;
    let mut info = parse_capabilities(
        device,
        mmio,
        capability_and_version,
        structural_parameters_1,
    )?;
    let layout = RegisterLayout::new(
        mmio,
        info.capability_length,
        runtime_offset,
        doorbell_offset,
    )?;
    let protocols = discover_supported_protocols(mmio, capability_parameters_1, info.max_ports)?;
    let context_size = if capability_parameters_1 & (1 << 2) != 0 {
        64
    } else {
        32
    };

    reset_controller(mmio, layout)?;

    let page_size_mask = read_mmio_u32(mmio, layout.operational(PAGESIZE))? as u16;
    let scratchpads = scratchpad_count(structural_parameters_2);
    let page_size =
        select_page_size(page_size_mask, scratchpads).ok_or(XhciInitError::UnsupportedPageSize)?;
    let enabled_slots = info.max_device_slots;
    let (
        dcbaa,
        command_ring,
        event_ring,
        event_ring_segment_table,
        scratchpad_array,
        scratchpad_storage,
    ) = initialize_runtime_structures(
        frame_allocator,
        physical_memory_offset,
        page_size,
        scratchpads,
        enabled_slots,
    )?;

    write_mmio_u32(mmio, layout.operational(CONFIG), u32::from(enabled_slots))?;
    write_mmio_u64(mmio, layout.operational(DCBAAP), dcbaa.physical_start())?;
    write_mmio_u64(
        mmio,
        layout.operational(CRCR),
        command_ring.physical_start() | u64::from(TRB_CYCLE),
    )?;
    write_mmio_u32(mmio, layout.runtime(ERSTSZ), 1)?;
    write_mmio_u64(mmio, layout.runtime(ERDP), event_ring.physical_start())?;
    write_mmio_u64(
        mmio,
        layout.runtime(ERSTBA),
        event_ring_segment_table.physical_start(),
    )?;
    fence(Ordering::SeqCst);

    write_mmio_u32(mmio, layout.operational(USBCMD), USBCMD_RUN_STOP)?;
    wait_for_bits(
        mmio,
        layout.operational(USBSTS),
        USBSTS_HOST_CONTROLLER_HALTED,
        0,
        XhciInitError::ControllerStartTimeout,
    )?;
    if read_mmio_u32(mmio, layout.operational(USBSTS))? & USBSTS_HOST_CONTROLLER_ERROR != 0 {
        return Err(XhciInitError::ControllerError);
    }
    info.page_size = page_size;
    info.scratchpad_buffers = scratchpads;
    info.enabled_slots = enabled_slots;
    info.command_ring_trbs = COMMAND_RING_USABLE_TRBS;
    info.event_ring_trbs = EVENT_RING_TRBS;
    info.supported_protocols = protocols.count;
    info.running = true;

    let mut controller = XhciController {
        info,
        layout,
        dcbaa,
        command_ring,
        event_ring,
        event_ring_segment_table,
        scratchpad_array,
        scratchpad_storage,
        command_enqueue_index: 0,
        command_cycle: true,
        event_dequeue_index: 0,
        event_cycle: true,
        first_device: None,
    };
    controller.submit_command(Trb::no_op_command())?;
    controller.info.command_probe_completed = true;
    controller.address_first_connected_device(
        frame_allocator,
        physical_memory_offset,
        protocols,
        context_size,
    )?;
    info = controller.info;
    XHCI_CONTROLLER.call_once(|| Mutex::new(controller));
    Ok(Some(info))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::{PciAddress, PciBar};

    fn device() -> PciDevice {
        let mut device = PciDevice::EMPTY;
        device.address = PciAddress::new(0, 4, 0).unwrap();
        device.class_code = SERIAL_BUS_CLASS;
        device.subclass = USB_SUBCLASS;
        device.programming_interface = XHCI_PROGRAMMING_INTERFACE;
        device.bars[0] = PciBar::Memory64 {
            base: 0xFEBF_0000,
            prefetchable: false,
        };
        device.bar_sizes[0] = 0x4000;
        device
    }

    fn mapping() -> MmioMapping {
        MmioMapping::for_test(0xFEBF_0000, 0xFFFF_A000_0000_0000, 0x4000)
    }

    #[test]
    fn parses_mandatory_capability_registers() {
        let info = parse_capabilities(
            device(),
            mapping(),
            (0x0100 << 16) | 0x40,
            (8 << 24) | (4 << 8) | 32,
        )
        .unwrap();

        assert_eq!(info.capability_length, 0x40);
        assert_eq!(info.interface_version, 0x0100);
        assert_eq!(info.max_device_slots, 32);
        assert_eq!(info.max_interrupters, 4);
        assert_eq!(info.max_ports, 8);
    }

    #[test]
    fn rejects_truncated_or_impossible_capabilities() {
        assert_eq!(
            parse_capabilities(device(), mapping(), (0x0100 << 16) | 0x10, 1),
            Err(XhciInitError::InvalidCapabilityLength)
        );
        assert_eq!(
            parse_capabilities(device(), mapping(), (0x0100 << 16) | 0x40, 0),
            Err(XhciInitError::InvalidCapabilities)
        );
    }

    #[test]
    fn validates_operational_runtime_and_doorbell_windows() {
        let layout = RegisterLayout::new(mapping(), 0x40, 0x1003, 0x2002).unwrap();
        assert_eq!(layout.operational_base, 0x40);
        assert_eq!(layout.runtime_base, 0x1000);
        assert_eq!(layout.doorbell_base, 0x2000);
        assert_eq!(
            RegisterLayout::new(mapping(), 0x40, 0x4000, 0x2000),
            Err(XhciInitError::InvalidRegisterLayout)
        );
    }

    #[test]
    fn decodes_scratchpads_and_selects_a_supported_page_size() {
        let parameters = (0b00001 << 21) | (0b00010 << 27);
        assert_eq!(scratchpad_count(parameters), 34);
        assert_eq!(select_page_size(0b0011, 34), Some(4096));
        assert_eq!(select_page_size(0b0010, 600), Some(8192));
        assert_eq!(select_page_size(0, 0), None);
    }

    #[test]
    fn creates_cyclic_command_ring_link_and_event_table_entry() {
        assert_eq!(core::mem::size_of::<Trb>(), 16);
        assert_eq!(core::mem::align_of::<Trb>(), 16);
        assert_eq!(COMMAND_RING_TRBS, 256);
        let link = Trb::link(0x0123_4000);
        assert_eq!(link.parameter, 0x0123_4000);
        assert_eq!((link.control >> TRB_TYPE_SHIFT) & 0x3F, TRB_TYPE_LINK);
        assert_ne!(link.control & LINK_TRB_TOGGLE_CYCLE, 0);
        assert_ne!(link.control & TRB_CYCLE, 0);

        let command = Trb::no_op_command();
        assert_eq!(command.trb_type(), TRB_TYPE_NO_OP_COMMAND);
        assert_eq!(command.control & TRB_CYCLE, 0);
        assert_ne!(command.with_cycle(true).control & TRB_CYCLE, 0);

        let entry = EventRingSegmentTableEntry {
            ring_segment_base: 0x0456_7000,
            ring_segment_size: EVENT_RING_TRBS as u32,
            reserved: 0,
        };
        assert_eq!(core::mem::size_of_val(&entry), 16);
        assert_eq!(entry.ring_segment_size, 256);
    }

    #[test]
    fn decodes_usb_supported_protocol_and_port_range() {
        let header = (3 << 24) | (0x20 << 16) | (4 << 8) | 2;
        let protocol =
            decode_supported_protocol(header, SUPPORTED_PROTOCOL_NAME_USB, (4 << 8) | 5, 7)
                .unwrap()
                .unwrap();

        assert_eq!(protocol.major, 3);
        assert_eq!(protocol.minor, 0x20);
        assert_eq!(protocol.port_offset, 5);
        assert_eq!(protocol.port_count, 4);
        assert_eq!(protocol.slot_type, 7);
        assert!(!protocol.contains(4));
        assert!(protocol.contains(5));
        assert!(protocol.contains(8));
        assert!(!protocol.contains(9));
    }

    #[test]
    fn rejects_malformed_supported_protocol() {
        assert_eq!(
            decode_supported_protocol(
                (2 << 24) | EXTENDED_CAPABILITY_SUPPORTED_PROTOCOL as u32,
                0,
                (4 << 8) | 1,
                0,
            ),
            Err(XhciInitError::InvalidExtendedCapability)
        );
        assert_eq!(
            decode_supported_protocol(
                (2 << 24) | EXTENDED_CAPABILITY_SUPPORTED_PROTOCOL as u32,
                SUPPORTED_PROTOCOL_NAME_USB,
                0,
                0,
            ),
            Err(XhciInitError::InvalidExtendedCapability)
        );
    }

    #[test]
    fn encodes_slot_and_address_commands() {
        let enable = Trb::enable_slot_command(5).with_cycle(true);
        assert_eq!(enable.trb_type(), TRB_TYPE_ENABLE_SLOT_COMMAND);
        assert_eq!((enable.control >> 16) & 0x1F, 5);
        assert_ne!(enable.control & TRB_CYCLE, 0);

        let address = Trb::address_device_command(0x1234_5000, 9);
        assert_eq!(address.trb_type(), TRB_TYPE_ADDRESS_DEVICE_COMMAND);
        assert_eq!(address.parameter, 0x1234_5000);
        assert_eq!(address.slot_id(), 9);
        assert_eq!(endpoint_zero_max_packet_size(1), 8);
        assert_eq!(endpoint_zero_max_packet_size(3), 64);
        assert_eq!(endpoint_zero_max_packet_size(4), 512);
    }

    #[test]
    fn calculates_root_port_registers() {
        let layout = RegisterLayout::new(mapping(), 0x40, 0x1000, 0x2000).unwrap();
        assert_eq!(layout.port(1, PORTSC), Ok(0x440));
        assert_eq!(layout.port(4, PORTSC), Ok(0x470));
        assert_eq!(
            layout.port(0, PORTSC),
            Err(XhciInitError::InvalidCapabilities)
        );
    }
}

//! xHCI discovery, polling transfers and USB HID boot-keyboard support.
//!
//! The controller is brought to the Running state with a Device Context Base
//! Address Array, Command Ring and one polling Event Ring. Supported Protocol
//! capabilities and root ports are inspected, and the first connected device
//! is enumerated through its default control pipe. A HID boot-keyboard endpoint
//! is configured and kept armed through bounded polling. MSI/MSI-X remains a
//! later Phase 13 increment.

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
const TRB_TYPE_NORMAL: u32 = 1;
const TRB_TYPE_SETUP_STAGE: u32 = 2;
const TRB_TYPE_DATA_STAGE: u32 = 3;
const TRB_TYPE_STATUS_STAGE: u32 = 4;
const TRB_TYPE_ENABLE_SLOT_COMMAND: u32 = 9;
const TRB_TYPE_ADDRESS_DEVICE_COMMAND: u32 = 11;
const TRB_TYPE_CONFIGURE_ENDPOINT_COMMAND: u32 = 12;
const TRB_TYPE_NO_OP_COMMAND: u32 = 23;
const TRB_TYPE_TRANSFER_EVENT: u32 = 32;
const TRB_TYPE_COMMAND_COMPLETION_EVENT: u32 = 33;
const TRB_TYPE_PORT_STATUS_CHANGE_EVENT: u32 = 34;
const TRB_CYCLE: u32 = 1 << 0;
const TRB_INTERRUPT_ON_SHORT_PACKET: u32 = 1 << 2;
const TRB_CHAIN: u32 = 1 << 4;
const TRB_INTERRUPT_ON_COMPLETION: u32 = 1 << 5;
const TRB_IMMEDIATE_DATA: u32 = 1 << 6;
const LINK_TRB_TOGGLE_CYCLE: u32 = 1 << 1;
const TRB_TYPE_SHIFT: u32 = 10;
const TRB_TYPE_MASK: u32 = 0x3F;
const TRB_DIRECTION_IN: u32 = 1 << 16;
const SETUP_TRANSFER_TYPE_SHIFT: u32 = 16;
const SETUP_TRANSFER_TYPE_NO_DATA: u32 = 0;
const SETUP_TRANSFER_TYPE_OUT: u32 = 2;
const SETUP_TRANSFER_TYPE_IN: u32 = 3;
const COMPLETION_CODE_SUCCESS: u8 = 1;
const COMPLETION_CODE_SHORT_PACKET: u8 = 13;
const EVENT_HANDLER_BUSY: u64 = 1 << 3;
const COMMAND_RING_TRBS: usize = FRAME_SIZE / core::mem::size_of::<Trb>();
const COMMAND_RING_USABLE_TRBS: usize = COMMAND_RING_TRBS - 1;
const EVENT_RING_TRBS: usize = FRAME_SIZE / core::mem::size_of::<Trb>();
const MAX_POLL_SPINS: usize = 10_000_000;
const RUNTIME_EVENT_BUDGET: usize = 16;
const EXTENDED_CAPABILITY_SUPPORTED_PROTOCOL: u8 = 2;
const SUPPORTED_PROTOCOL_NAME_USB: u32 = 0x2042_5355;
const MAX_SUPPORTED_PROTOCOLS: usize = 8;
const CONTEXTS_PER_DEVICE: usize = 32;
const INPUT_CONTEXTS_PER_DEVICE: usize = CONTEXTS_PER_DEVICE + 1;
const USB_DESCRIPTOR_DEVICE: u8 = 1;
const USB_DESCRIPTOR_CONFIGURATION: u8 = 2;
const USB_DESCRIPTOR_INTERFACE: u8 = 4;
const USB_DESCRIPTOR_ENDPOINT: u8 = 5;
const USB_REQUEST_GET_DESCRIPTOR: u8 = 6;
const USB_REQUEST_SET_CONFIGURATION: u8 = 9;
const HID_REQUEST_SET_PROTOCOL: u8 = 0x0B;
const USB_REQUEST_TYPE_DEVICE_IN: u8 = 0x80;
const USB_REQUEST_TYPE_DEVICE_OUT: u8 = 0x00;
const USB_REQUEST_TYPE_HID_INTERFACE_OUT: u8 = 0x21;
const USB_CLASS_HID: u8 = 0x03;
const HID_SUBCLASS_BOOT: u8 = 0x01;
const HID_PROTOCOL_KEYBOARD: u8 = 0x01;
const USB_ENDPOINT_DIRECTION_IN: u8 = 0x80;
const USB_ENDPOINT_TRANSFER_TYPE_MASK: u8 = 0x03;
const USB_ENDPOINT_TRANSFER_INTERRUPT: u8 = 0x03;
const HID_BOOT_REPORT_BYTES: usize = 8;

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
    pub device_vendor_id: u16,
    pub device_product_id: u16,
    pub device_configurations: u8,
    pub hid_keyboard_ready: bool,
    pub hid_interface: u8,
    pub hid_endpoint_address: u8,
    pub hid_endpoint_dci: u8,
    pub hid_max_packet_size: u16,
    pub hid_interval: u8,
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
    Busy,
    RingFull,
    TransferTimeout,
    InvalidTransferCompletion,
    TransferFailed(u8),
    InvalidDescriptor,
    DescriptorTooLarge,
    HidKeyboardNotFound,
    InvalidHidReport,
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

    const fn configure_endpoint_command(input_context: u64, slot_id: u8) -> Self {
        Self {
            parameter: input_context,
            status: 0,
            control: (TRB_TYPE_CONFIGURE_ENDPOINT_COMMAND << TRB_TYPE_SHIFT)
                | ((slot_id as u32) << 24),
        }
    }

    fn setup_stage(packet: UsbSetupPacket, direction: TransferDirection) -> Self {
        let transfer_type = match direction {
            TransferDirection::None => SETUP_TRANSFER_TYPE_NO_DATA,
            TransferDirection::Out => SETUP_TRANSFER_TYPE_OUT,
            TransferDirection::In => SETUP_TRANSFER_TYPE_IN,
        };
        Self {
            parameter: u64::from_le_bytes(packet.to_bytes()),
            status: 8,
            control: (TRB_TYPE_SETUP_STAGE << TRB_TYPE_SHIFT)
                | TRB_IMMEDIATE_DATA
                | TRB_CHAIN
                | (transfer_type << SETUP_TRANSFER_TYPE_SHIFT),
        }
    }

    const fn data_stage(buffer: u64, length: u16, direction: TransferDirection) -> Self {
        let direction_bit = match direction {
            TransferDirection::In => TRB_DIRECTION_IN,
            TransferDirection::None | TransferDirection::Out => 0,
        };
        Self {
            parameter: buffer,
            status: length as u32,
            control: (TRB_TYPE_DATA_STAGE << TRB_TYPE_SHIFT)
                | TRB_CHAIN
                | TRB_INTERRUPT_ON_SHORT_PACKET
                | direction_bit,
        }
    }

    const fn status_stage(direction: TransferDirection) -> Self {
        let direction_bit = match direction {
            TransferDirection::In => TRB_DIRECTION_IN,
            TransferDirection::None | TransferDirection::Out => 0,
        };
        Self {
            parameter: 0,
            status: 0,
            control: (TRB_TYPE_STATUS_STAGE << TRB_TYPE_SHIFT)
                | TRB_INTERRUPT_ON_COMPLETION
                | direction_bit,
        }
    }

    const fn normal(buffer: u64, length: u16) -> Self {
        Self {
            parameter: buffer,
            status: length as u32,
            control: (TRB_TYPE_NORMAL << TRB_TYPE_SHIFT)
                | TRB_INTERRUPT_ON_SHORT_PACKET
                | TRB_INTERRUPT_ON_COMPLETION,
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

    const fn endpoint_id(self) -> u8 {
        ((self.control >> 16) & 0x1F) as u8
    }

    const fn transfer_length(self) -> u32 {
        self.status & 0x00FF_FFFF
    }
}

#[repr(C, packed)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsbSetupPacket {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
}

impl UsbSetupPacket {
    const fn new(request_type: u8, request: u8, value: u16, index: u16, length: u16) -> Self {
        Self {
            request_type,
            request,
            value,
            index,
            length,
        }
    }

    const fn to_bytes(self) -> [u8; 8] {
        let value = self.value.to_le_bytes();
        let index = self.index.to_le_bytes();
        let length = self.length.to_le_bytes();
        [
            self.request_type,
            self.request,
            value[0],
            value[1],
            index[0],
            index[1],
            length[0],
            length[1],
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferDirection {
    None,
    Out,
    In,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsbDeviceDescriptor {
    pub usb_version: u16,
    pub max_packet_size_zero: u16,
    pub vendor_id: u16,
    pub product_id: u16,
    pub configurations: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HidKeyboardDescriptor {
    pub configuration_value: u8,
    pub interface_number: u8,
    pub endpoint_address: u8,
    pub max_packet_size: u16,
    pub interval: u8,
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    let value = bytes.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([value[0], value[1]]))
}

pub(crate) fn parse_device_descriptor(
    bytes: &[u8],
    speed_id: u8,
) -> Result<UsbDeviceDescriptor, XhciInitError> {
    if bytes.len() < 18 || bytes[0] != 18 || bytes[1] != USB_DESCRIPTOR_DEVICE {
        return Err(XhciInitError::InvalidDescriptor);
    }
    let encoded_packet_size = bytes[7];
    let max_packet_size_zero = match speed_id {
        1 if matches!(encoded_packet_size, 8 | 16 | 32 | 64) => u16::from(encoded_packet_size),
        2 if encoded_packet_size == 8 => 8,
        3 if encoded_packet_size == 64 => 64,
        4 | 5 if encoded_packet_size == 9 => 512,
        _ => return Err(XhciInitError::InvalidDescriptor),
    };
    let configurations = bytes[17];
    if configurations == 0 {
        return Err(XhciInitError::InvalidDescriptor);
    }
    Ok(UsbDeviceDescriptor {
        usb_version: read_u16(bytes, 2).ok_or(XhciInitError::InvalidDescriptor)?,
        max_packet_size_zero,
        vendor_id: read_u16(bytes, 8).ok_or(XhciInitError::InvalidDescriptor)?,
        product_id: read_u16(bytes, 10).ok_or(XhciInitError::InvalidDescriptor)?,
        configurations,
    })
}

pub(crate) fn configuration_total_length(bytes: &[u8]) -> Result<u16, XhciInitError> {
    if bytes.len() < 9 || bytes[0] < 9 || bytes[1] != USB_DESCRIPTOR_CONFIGURATION {
        return Err(XhciInitError::InvalidDescriptor);
    }
    let total = read_u16(bytes, 2).ok_or(XhciInitError::InvalidDescriptor)?;
    if total < 9 {
        return Err(XhciInitError::InvalidDescriptor);
    }
    if usize::from(total) > FRAME_SIZE {
        return Err(XhciInitError::DescriptorTooLarge);
    }
    Ok(total)
}

pub(crate) fn parse_hid_keyboard_configuration(
    bytes: &[u8],
) -> Result<HidKeyboardDescriptor, XhciInitError> {
    let total = usize::from(configuration_total_length(bytes)?);
    if bytes.len() < total || bytes[0] < 9 || bytes[0] as usize > total {
        return Err(XhciInitError::InvalidDescriptor);
    }
    let configuration_value = bytes[5];
    if configuration_value == 0 {
        return Err(XhciInitError::InvalidDescriptor);
    }

    let mut offset = 0usize;
    let mut boot_keyboard_interface = None;
    while offset < total {
        let header = bytes
            .get(
                offset
                    ..offset
                        .checked_add(2)
                        .ok_or(XhciInitError::InvalidDescriptor)?,
            )
            .ok_or(XhciInitError::InvalidDescriptor)?;
        let length = usize::from(header[0]);
        if length < 2 {
            return Err(XhciInitError::InvalidDescriptor);
        }
        let end = offset
            .checked_add(length)
            .ok_or(XhciInitError::InvalidDescriptor)?;
        if end > total {
            return Err(XhciInitError::InvalidDescriptor);
        }

        match header[1] {
            USB_DESCRIPTOR_INTERFACE => {
                if length < 9 {
                    return Err(XhciInitError::InvalidDescriptor);
                }
                let descriptor = &bytes[offset..end];
                boot_keyboard_interface = (descriptor[3] == 0
                    && descriptor[5] == USB_CLASS_HID
                    && descriptor[6] == HID_SUBCLASS_BOOT
                    && descriptor[7] == HID_PROTOCOL_KEYBOARD)
                    .then_some(descriptor[2]);
            }
            USB_DESCRIPTOR_ENDPOINT => {
                if length < 7 {
                    return Err(XhciInitError::InvalidDescriptor);
                }
                if let Some(interface_number) = boot_keyboard_interface {
                    let descriptor = &bytes[offset..end];
                    let address = descriptor[2];
                    let attributes = descriptor[3];
                    let max_packet_size =
                        read_u16(descriptor, 4).ok_or(XhciInitError::InvalidDescriptor)? & 0x07FF;
                    let interval = descriptor[6];
                    if address & USB_ENDPOINT_DIRECTION_IN != 0
                        && address & 0x0F != 0
                        && attributes & USB_ENDPOINT_TRANSFER_TYPE_MASK
                            == USB_ENDPOINT_TRANSFER_INTERRUPT
                        && max_packet_size >= HID_BOOT_REPORT_BYTES as u16
                        && max_packet_size <= 1024
                        && interval != 0
                    {
                        return Ok(HidKeyboardDescriptor {
                            configuration_value,
                            interface_number,
                            endpoint_address: address,
                            max_packet_size,
                            interval,
                        });
                    }
                }
            }
            _ => {}
        }
        offset = end;
    }
    Err(XhciInitError::HidKeyboardNotFound)
}

const fn endpoint_id(endpoint_address: u8) -> Option<u8> {
    let number = endpoint_address & 0x0F;
    if number == 0 {
        return Some(1);
    }
    if endpoint_address & USB_ENDPOINT_DIRECTION_IN != 0 {
        Some(number * 2 + 1)
    } else {
        Some(number * 2)
    }
}

fn interrupt_interval(speed_id: u8, descriptor_interval: u8) -> Option<u8> {
    if descriptor_interval == 0 {
        return None;
    }
    match speed_id {
        // Full/low-speed bInterval is expressed in frames. Convert it to the
        // closest xHCI power-of-two microframe exponent without exceeding the
        // controller's 0..15 field.
        1 | 2 => {
            let microframes = u32::from(descriptor_interval) * 8;
            Some((31 - microframes.leading_zeros()).min(15) as u8)
        }
        // High/SuperSpeed descriptors already encode a 1-based exponent.
        3..=5 => Some(descriptor_interval.saturating_sub(1).min(15)),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingCursor {
    enqueue_index: usize,
    cycle: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingReservation {
    start_index: usize,
    cycle: bool,
    reached_link: bool,
}

impl RingCursor {
    const fn new() -> Self {
        Self {
            enqueue_index: 0,
            cycle: true,
        }
    }

    fn reserve(&mut self, trb_count: usize) -> Result<RingReservation, XhciInitError> {
        if trb_count == 0
            || trb_count > COMMAND_RING_USABLE_TRBS
            || self.enqueue_index + trb_count > COMMAND_RING_USABLE_TRBS
        {
            return Err(XhciInitError::RingFull);
        }
        let reservation = RingReservation {
            start_index: self.enqueue_index,
            cycle: self.cycle,
            reached_link: self.enqueue_index + trb_count == COMMAND_RING_USABLE_TRBS,
        };
        self.enqueue_index += trb_count;
        if reservation.reached_link {
            self.enqueue_index = 0;
            self.cycle = !self.cycle;
        }
        Ok(reservation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransferOwner {
    Control,
    Hid,
}

#[derive(Debug, Clone, Copy)]
struct PendingCommand {
    trb_pointer: u64,
    expected_slot: Option<u8>,
    completion: Option<Trb>,
}

#[derive(Debug, Clone, Copy)]
struct PendingTransfer {
    owner: TransferOwner,
    trb_pointer: u64,
    short_packet_pointer: Option<u64>,
    slot_id: u8,
    endpoint_id: u8,
    requested_length: u16,
    completion: Option<Trb>,
}

#[derive(Debug, Clone, Copy)]
struct EventDispatchState {
    command: Option<PendingCommand>,
    transfer: Option<PendingTransfer>,
    changed_ports: u64,
    unexpected_events: u32,
}

#[derive(Debug, Clone, Copy)]
struct HidReportCompletion {
    slot_id: u8,
    endpoint_address: u8,
    actual_length: u16,
    report: [u8; HID_BOOT_REPORT_BYTES],
}

impl EventDispatchState {
    const fn new() -> Self {
        Self {
            command: None,
            transfer: None,
            changed_ports: 0,
            unexpected_events: 0,
        }
    }

    fn dispatch(&mut self, event: Trb) {
        let accepted = match event.trb_type() {
            TRB_TYPE_COMMAND_COMPLETION_EVENT => self.command.as_mut().is_some_and(|pending| {
                let pointer_matches = event.parameter & !0xF == pending.trb_pointer;
                let slot_matches = pending
                    .expected_slot
                    .is_none_or(|slot| event.slot_id() == slot);
                if pointer_matches && slot_matches {
                    pending.completion = Some(event);
                    true
                } else {
                    false
                }
            }),
            TRB_TYPE_TRANSFER_EVENT => self.transfer.as_mut().is_some_and(|pending| {
                let pointer = event.parameter & !0xF;
                let pointer_matches = pointer == pending.trb_pointer
                    || (event.completion_code() == COMPLETION_CODE_SHORT_PACKET
                        && pending.short_packet_pointer == Some(pointer));
                if pointer_matches
                    && event.slot_id() == pending.slot_id
                    && event.endpoint_id() == pending.endpoint_id
                {
                    pending.completion = Some(event);
                    true
                } else {
                    false
                }
            }),
            TRB_TYPE_PORT_STATUS_CHANGE_EVENT => {
                let port_id = (event.parameter >> 24) as u8;
                if (1..=64).contains(&port_id) {
                    self.changed_ports |= 1u64 << (port_id - 1);
                    true
                } else {
                    false
                }
            }
            _ => false,
        };
        if !accepted {
            self.unexpected_events = self.unexpected_events.saturating_add(1);
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct HidKeyboardDecoder {
    previous: [u8; HID_BOOT_REPORT_BYTES],
}

impl HidKeyboardDecoder {
    const fn new() -> Self {
        Self {
            previous: [0; HID_BOOT_REPORT_BYTES],
        }
    }

    fn decode(&mut self, report: [u8; HID_BOOT_REPORT_BYTES]) -> HidDecodedKeys {
        let mut decoded = HidDecodedKeys::new();
        if report[2..].iter().any(|usage| (1..=3).contains(usage)) {
            return decoded;
        }
        let shift = report[0] & 0x22 != 0;
        for usage in report[2..].iter().copied().filter(|usage| *usage != 0) {
            if self.previous[2..].contains(&usage) {
                continue;
            }
            if let Some(character) = hid_usage_to_char(usage, shift) {
                decoded.push(character);
            }
        }
        self.previous = report;
        decoded
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HidDecodedKeys {
    characters: [char; 6],
    len: u8,
}

impl HidDecodedKeys {
    const fn new() -> Self {
        Self {
            characters: ['\0'; 6],
            len: 0,
        }
    }

    fn push(&mut self, character: char) {
        if let Some(slot) = self.characters.get_mut(usize::from(self.len)) {
            *slot = character;
            self.len += 1;
        }
    }

    fn iter(self) -> impl Iterator<Item = char> {
        self.characters.into_iter().take(usize::from(self.len))
    }
}

fn hid_usage_to_char(usage: u8, shift: bool) -> Option<char> {
    let (plain, shifted) = match usage {
        0x04..=0x1D => {
            let lower = char::from(b'a' + usage - 0x04);
            return Some(if shift {
                lower.to_ascii_uppercase()
            } else {
                lower
            });
        }
        0x1E => ('1', '!'),
        0x1F => ('2', '@'),
        0x20 => ('3', '#'),
        0x21 => ('4', '$'),
        0x22 => ('5', '%'),
        0x23 => ('6', '^'),
        0x24 => ('7', '&'),
        0x25 => ('8', '*'),
        0x26 => ('9', '('),
        0x27 => ('0', ')'),
        0x28 => ('\n', '\n'),
        0x2A => ('\x08', '\x08'),
        0x2B => ('\t', '\t'),
        0x2C => (' ', ' '),
        0x2D => ('-', '_'),
        0x2E => ('=', '+'),
        0x2F => ('[', '{'),
        0x30 => (']', '}'),
        0x31 => ('\\', '|'),
        0x33 => (';', ':'),
        0x34 => ('\'', '"'),
        0x35 => ('`', '~'),
        0x36 => (',', '<'),
        0x37 => ('.', '>'),
        0x38 => ('/', '?'),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
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

#[derive(Clone, Copy)]
struct XhciDevice {
    slot_id: u8,
    port_id: u8,
    speed_id: u8,
    device_context: DmaRegion,
    input_context: DmaRegion,
    endpoint_zero_ring: DmaRegion,
    endpoint_zero_cursor: RingCursor,
    descriptor_buffer: DmaRegion,
    configuration_value: u8,
    hid_interface: u8,
    hid_endpoint_address: u8,
    hid_endpoint_id: u8,
    hid_interval: u8,
    hid_max_packet_size: u16,
    hid_transfer_ring: Option<DmaRegion>,
    hid_transfer_cursor: RingCursor,
    hid_report_buffer: Option<DmaRegion>,
    hid_transfer_pending: bool,
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
    context_size: usize,
    command_enqueue_index: usize,
    command_cycle: bool,
    event_dequeue_index: usize,
    event_cycle: bool,
    events: EventDispatchState,
    first_device: Option<XhciDevice>,
}

static XHCI_CONTROLLER: Once<Mutex<XhciController>> = Once::new();
static HID_DECODER: Mutex<HidKeyboardDecoder> = Mutex::new(HidKeyboardDecoder::new());

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
        device_vendor_id: 0,
        device_product_id: 0,
        device_configurations: 0,
        hid_keyboard_ready: false,
        hid_interface: 0,
        hid_endpoint_address: 0,
        hid_endpoint_dci: 0,
        hid_max_packet_size: 0,
        hid_interval: 0,
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
fn read_dma_u32(region: DmaRegion, offset: usize) -> Result<u32, XhciInitError> {
    let pointer = region
        .pointer_at::<u32>(offset)
        .ok_or(XhciInitError::InvalidCapabilities)?;
    Ok(unsafe { core::ptr::read_volatile(pointer) })
}

#[cfg(target_os = "none")]
fn clear_dma(region: DmaRegion, bytes: usize) -> Result<(), XhciInitError> {
    if bytes > region.len() {
        return Err(XhciInitError::InvalidCapabilities);
    }
    unsafe { core::ptr::write_bytes(region.virtual_start() as *mut u8, 0, bytes) };
    Ok(())
}

#[cfg(target_os = "none")]
fn copy_from_dma(region: DmaRegion, output: &mut [u8]) -> Result<(), XhciInitError> {
    if output.len() > region.len() {
        return Err(XhciInitError::InvalidCapabilities);
    }
    for (offset, byte) in output.iter_mut().enumerate() {
        let pointer = region
            .pointer_at::<u8>(offset)
            .ok_or(XhciInitError::InvalidCapabilities)?;
        *byte = unsafe { core::ptr::read_volatile(pointer) };
    }
    Ok(())
}

#[cfg(target_os = "none")]
fn copy_context(
    source: DmaRegion,
    source_offset: usize,
    destination: DmaRegion,
    destination_offset: usize,
    bytes: usize,
) -> Result<(), XhciInitError> {
    if !bytes.is_multiple_of(core::mem::size_of::<u32>()) {
        return Err(XhciInitError::InvalidCapabilities);
    }
    for offset in (0..bytes).step_by(core::mem::size_of::<u32>()) {
        let value = read_dma_u32(source, source_offset + offset)?;
        write_dma_u32(destination, destination_offset + offset, value)?;
    }
    Ok(())
}

#[cfg(target_os = "none")]
fn enqueue_transfer_td(
    ring: DmaRegion,
    cursor: &mut RingCursor,
    trbs: &[Trb],
) -> Result<u64, XhciInitError> {
    let reservation = cursor.reserve(trbs.len())?;
    let mut final_physical = 0;
    for (relative, trb) in trbs.iter().copied().enumerate() {
        let index = reservation.start_index + relative;
        let offset = index * core::mem::size_of::<Trb>();
        let pointer = ring
            .pointer_at::<Trb>(offset)
            .ok_or(XhciInitError::InvalidCapabilities)?;
        final_physical = ring
            .physical_at(offset)
            .ok_or(XhciInitError::InvalidCapabilities)?;
        unsafe { core::ptr::write_volatile(pointer, trb.with_cycle(reservation.cycle)) };
    }
    if reservation.reached_link {
        let link_pointer = ring
            .pointer_at::<Trb>(COMMAND_RING_USABLE_TRBS * core::mem::size_of::<Trb>())
            .ok_or(XhciInitError::InvalidCapabilities)?;
        unsafe {
            core::ptr::write_volatile(
                link_pointer,
                Trb::link(ring.physical_start()).with_cycle(reservation.cycle),
            )
        };
    }
    Ok(final_physical)
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
    let descriptor_buffer = DmaRegion::allocate(
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
        endpoint_zero_cursor: RingCursor::new(),
        descriptor_buffer,
        configuration_value: 0,
        hid_interface: 0,
        hid_endpoint_address: 0,
        hid_endpoint_id: 0,
        hid_interval: 0,
        hid_max_packet_size: 0,
        hid_transfer_ring: None,
        hid_transfer_cursor: RingCursor::new(),
        hid_report_buffer: None,
        hid_transfer_pending: false,
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

    fn dispatch_one_event(&mut self) -> Result<bool, XhciInitError> {
        let event_pointer = self
            .event_ring
            .pointer_at::<Trb>(self.event_dequeue_index * core::mem::size_of::<Trb>())
            .ok_or(XhciInitError::InvalidCapabilities)?;
        let event = unsafe { core::ptr::read_volatile(event_pointer) };
        if (event.control & TRB_CYCLE != 0) != self.event_cycle {
            return Ok(false);
        }
        fence(Ordering::Acquire);
        self.events.dispatch(event);
        self.advance_event_ring()?;
        Ok(true)
    }

    fn submit_command(&mut self, command: Trb) -> Result<Trb, XhciInitError> {
        if self.events.command.is_some() {
            return Err(XhciInitError::Busy);
        }
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
        let expected_slot = match command.trb_type() {
            TRB_TYPE_ADDRESS_DEVICE_COMMAND | TRB_TYPE_CONFIGURE_ENDPOINT_COMMAND => {
                Some(command.slot_id())
            }
            _ => None,
        };
        self.events.command = Some(PendingCommand {
            trb_pointer: command_physical,
            expected_slot,
            completion: None,
        });
        fence(Ordering::Release);
        self.advance_command_ring()?;
        write_mmio_u32(self.info.mmio, self.layout.doorbell_base, 0)?;

        for _ in 0..MAX_POLL_SPINS {
            self.dispatch_one_event()?;
            let completion = self
                .events
                .command
                .as_ref()
                .and_then(|pending| pending.completion);
            let Some(event) = completion else {
                core::hint::spin_loop();
                continue;
            };
            self.events.command = None;
            if event.completion_code() != COMPLETION_CODE_SUCCESS {
                return Err(XhciInitError::CommandFailed(event.completion_code()));
            }
            return Ok(event);
        }
        Err(XhciInitError::CommandCompletionTimeout)
    }

    fn wait_for_transfer(&mut self, owner: TransferOwner) -> Result<u16, XhciInitError> {
        for _ in 0..MAX_POLL_SPINS {
            self.dispatch_one_event()?;
            let pending = self
                .events
                .transfer
                .filter(|pending| pending.owner == owner && pending.completion.is_some());
            let Some(pending) = pending else {
                core::hint::spin_loop();
                continue;
            };
            self.events.transfer = None;
            let event = pending
                .completion
                .ok_or(XhciInitError::InvalidTransferCompletion)?;
            return match event.completion_code() {
                COMPLETION_CODE_SUCCESS => Ok(pending.requested_length),
                COMPLETION_CODE_SHORT_PACKET => {
                    let remaining = u16::try_from(event.transfer_length())
                        .map_err(|_| XhciInitError::InvalidTransferCompletion)?;
                    pending
                        .requested_length
                        .checked_sub(remaining)
                        .ok_or(XhciInitError::InvalidTransferCompletion)
                }
                code => Err(XhciInitError::TransferFailed(code)),
            };
        }
        Err(XhciInitError::TransferTimeout)
    }

    fn control_transfer(&mut self, packet: UsbSetupPacket) -> Result<u16, XhciInitError> {
        if self.events.transfer.is_some() || usize::from(packet.length) > FRAME_SIZE {
            return Err(if self.events.transfer.is_some() {
                XhciInitError::Busy
            } else {
                XhciInitError::DescriptorTooLarge
            });
        }
        let direction = if packet.length == 0 {
            TransferDirection::None
        } else if packet.request_type & USB_ENDPOINT_DIRECTION_IN != 0 {
            TransferDirection::In
        } else {
            TransferDirection::Out
        };
        let (ring, mut cursor, buffer, slot_id) = {
            let device = self
                .first_device
                .as_ref()
                .ok_or(XhciInitError::InvalidCapabilities)?;
            (
                device.endpoint_zero_ring,
                device.endpoint_zero_cursor,
                device.descriptor_buffer,
                device.slot_id,
            )
        };
        clear_dma(buffer, usize::from(packet.length))?;

        let setup = Trb::setup_stage(packet, direction);
        let status_direction = match direction {
            TransferDirection::In => TransferDirection::Out,
            TransferDirection::None | TransferDirection::Out => TransferDirection::In,
        };
        let data = Trb::data_stage(buffer.physical_start(), packet.length, direction);
        let status = Trb::status_stage(status_direction);
        let mut stages = [setup, data, status];
        let stage_count = if direction == TransferDirection::None {
            stages[1] = status;
            2
        } else {
            3
        };
        let final_pointer = enqueue_transfer_td(ring, &mut cursor, &stages[..stage_count])?;
        let short_packet_pointer = if direction == TransferDirection::None {
            None
        } else {
            Some(
                final_pointer
                    .checked_sub(core::mem::size_of::<Trb>() as u64)
                    .ok_or(XhciInitError::InvalidCapabilities)?,
            )
        };
        self.first_device
            .as_mut()
            .ok_or(XhciInitError::InvalidCapabilities)?
            .endpoint_zero_cursor = cursor;
        self.events.transfer = Some(PendingTransfer {
            owner: TransferOwner::Control,
            trb_pointer: final_pointer,
            short_packet_pointer,
            slot_id,
            endpoint_id: 1,
            requested_length: packet.length,
            completion: None,
        });
        fence(Ordering::Release);
        write_mmio_u32(
            self.info.mmio,
            self.layout.doorbell_base + u64::from(slot_id) * 4,
            1,
        )?;
        self.wait_for_transfer(TransferOwner::Control)
    }

    fn get_descriptor(&mut self, descriptor_type: u8, length: u16) -> Result<u16, XhciInitError> {
        self.control_transfer(UsbSetupPacket::new(
            USB_REQUEST_TYPE_DEVICE_IN,
            USB_REQUEST_GET_DESCRIPTOR,
            u16::from(descriptor_type) << 8,
            0,
            length,
        ))
    }

    fn configure_hid_endpoint(
        &mut self,
        frame_allocator: &mut BitmapFrameAllocator,
        physical_memory_offset: u64,
        descriptor: HidKeyboardDescriptor,
    ) -> Result<(), XhciInitError> {
        let endpoint_dci = endpoint_id(descriptor.endpoint_address)
            .filter(|endpoint| *endpoint > 1 && *endpoint < CONTEXTS_PER_DEVICE as u8)
            .ok_or(XhciInitError::InvalidDescriptor)?;
        let interval = interrupt_interval(
            self.first_device
                .as_ref()
                .ok_or(XhciInitError::InvalidCapabilities)?
                .speed_id,
            descriptor.interval,
        )
        .ok_or(XhciInitError::InvalidDescriptor)?;
        let transfer_ring = DmaRegion::allocate(
            frame_allocator,
            physical_memory_offset,
            FRAME_SIZE,
            FRAME_SIZE,
        )?;
        let report_buffer = DmaRegion::allocate(
            frame_allocator,
            physical_memory_offset,
            FRAME_SIZE,
            FRAME_SIZE,
        )?;
        let link_pointer = transfer_ring
            .pointer_at::<Trb>(COMMAND_RING_USABLE_TRBS * core::mem::size_of::<Trb>())
            .ok_or(XhciInitError::InvalidCapabilities)?;
        unsafe {
            core::ptr::write_volatile(link_pointer, Trb::link(transfer_ring.physical_start()))
        };

        let device = self
            .first_device
            .as_ref()
            .copied()
            .ok_or(XhciInitError::InvalidCapabilities)?;
        copy_context(
            device.device_context,
            0,
            device.input_context,
            self.context_size,
            self.context_size,
        )?;
        write_dma_u32(device.input_context, 0, 0)?;
        write_dma_u32(
            device.input_context,
            4,
            1 | (1u32 << u32::from(endpoint_dci)),
        )?;
        let slot_context = self.context_size;
        let mut slot_dword_zero = read_dma_u32(device.input_context, slot_context)?;
        slot_dword_zero &= !(0x1F << 27);
        slot_dword_zero |= u32::from(endpoint_dci) << 27;
        write_dma_u32(device.input_context, slot_context, slot_dword_zero)?;

        let endpoint_context = self.context_size * (usize::from(endpoint_dci) + 1);
        write_dma_u32(
            device.input_context,
            endpoint_context,
            u32::from(interval) << 16,
        )?;
        write_dma_u32(
            device.input_context,
            endpoint_context + 4,
            (u32::from(descriptor.max_packet_size) << 16) | (3 << 1) | (7 << 3),
        )?;
        write_dma_u64(
            device.input_context,
            endpoint_context + 8,
            transfer_ring.physical_start() | 1,
        )?;
        write_dma_u32(
            device.input_context,
            endpoint_context + 16,
            u32::from(HID_BOOT_REPORT_BYTES as u16) | (u32::from(descriptor.max_packet_size) << 16),
        )?;
        fence(Ordering::Release);
        self.submit_command(Trb::configure_endpoint_command(
            device.input_context.physical_start(),
            device.slot_id,
        ))?;

        let device = self
            .first_device
            .as_mut()
            .ok_or(XhciInitError::InvalidCapabilities)?;
        device.configuration_value = descriptor.configuration_value;
        device.hid_interface = descriptor.interface_number;
        device.hid_endpoint_address = descriptor.endpoint_address;
        device.hid_endpoint_id = endpoint_dci;
        device.hid_interval = interval;
        device.hid_max_packet_size = descriptor.max_packet_size;
        device.hid_transfer_ring = Some(transfer_ring);
        device.hid_report_buffer = Some(report_buffer);

        self.info.hid_keyboard_ready = true;
        self.info.hid_interface = descriptor.interface_number;
        self.info.hid_endpoint_address = descriptor.endpoint_address;
        self.info.hid_endpoint_dci = endpoint_dci;
        self.info.hid_max_packet_size = descriptor.max_packet_size;
        self.info.hid_interval = interval;
        self.arm_hid_transfer()
    }

    fn enumerate_hid_keyboard(
        &mut self,
        frame_allocator: &mut BitmapFrameAllocator,
        physical_memory_offset: u64,
    ) -> Result<(), XhciInitError> {
        let device_length = self.get_descriptor(USB_DESCRIPTOR_DEVICE, 18)?;
        if device_length < 18 {
            return Err(XhciInitError::InvalidDescriptor);
        }
        let (descriptor_buffer, speed_id) = {
            let device = self
                .first_device
                .as_ref()
                .ok_or(XhciInitError::InvalidCapabilities)?;
            (device.descriptor_buffer, device.speed_id)
        };
        let device_bytes = unsafe {
            core::slice::from_raw_parts(descriptor_buffer.virtual_start() as *const u8, 18)
        };
        let device_descriptor = parse_device_descriptor(device_bytes, speed_id)?;
        self.info.device_vendor_id = device_descriptor.vendor_id;
        self.info.device_product_id = device_descriptor.product_id;
        self.info.device_configurations = device_descriptor.configurations;

        let header_length = self.get_descriptor(USB_DESCRIPTOR_CONFIGURATION, 9)?;
        if header_length < 9 {
            return Err(XhciInitError::InvalidDescriptor);
        }
        let header = unsafe {
            core::slice::from_raw_parts(descriptor_buffer.virtual_start() as *const u8, 9)
        };
        let total_length = configuration_total_length(header)?;
        let actual_length = self.get_descriptor(USB_DESCRIPTOR_CONFIGURATION, total_length)?;
        if actual_length < total_length {
            return Err(XhciInitError::InvalidDescriptor);
        }
        let configuration = unsafe {
            core::slice::from_raw_parts(
                descriptor_buffer.virtual_start() as *const u8,
                usize::from(total_length),
            )
        };
        let hid = parse_hid_keyboard_configuration(configuration)?;

        self.control_transfer(UsbSetupPacket::new(
            USB_REQUEST_TYPE_DEVICE_OUT,
            USB_REQUEST_SET_CONFIGURATION,
            u16::from(hid.configuration_value),
            0,
            0,
        ))?;
        self.control_transfer(UsbSetupPacket::new(
            USB_REQUEST_TYPE_HID_INTERFACE_OUT,
            HID_REQUEST_SET_PROTOCOL,
            0,
            u16::from(hid.interface_number),
            0,
        ))?;
        self.configure_hid_endpoint(frame_allocator, physical_memory_offset, hid)
    }

    fn arm_hid_transfer(&mut self) -> Result<(), XhciInitError> {
        if self.events.transfer.is_some() {
            return Err(XhciInitError::Busy);
        }
        let (ring, mut cursor, report_buffer, slot_id, endpoint_dci, already_pending) = {
            let device = self
                .first_device
                .as_ref()
                .ok_or(XhciInitError::InvalidCapabilities)?;
            (
                device
                    .hid_transfer_ring
                    .ok_or(XhciInitError::HidKeyboardNotFound)?,
                device.hid_transfer_cursor,
                device
                    .hid_report_buffer
                    .ok_or(XhciInitError::HidKeyboardNotFound)?,
                device.slot_id,
                device.hid_endpoint_id,
                device.hid_transfer_pending,
            )
        };
        if already_pending {
            return Err(XhciInitError::Busy);
        }
        clear_dma(report_buffer, HID_BOOT_REPORT_BYTES)?;
        let final_pointer = enqueue_transfer_td(
            ring,
            &mut cursor,
            &[Trb::normal(
                report_buffer.physical_start(),
                HID_BOOT_REPORT_BYTES as u16,
            )],
        )?;
        let device = self
            .first_device
            .as_mut()
            .ok_or(XhciInitError::InvalidCapabilities)?;
        device.hid_transfer_cursor = cursor;
        device.hid_transfer_pending = true;
        self.events.transfer = Some(PendingTransfer {
            owner: TransferOwner::Hid,
            trb_pointer: final_pointer,
            short_packet_pointer: None,
            slot_id,
            endpoint_id: endpoint_dci,
            requested_length: HID_BOOT_REPORT_BYTES as u16,
            completion: None,
        });
        fence(Ordering::Release);
        write_mmio_u32(
            self.info.mmio,
            self.layout.doorbell_base + u64::from(slot_id) * 4,
            u32::from(endpoint_dci),
        )
    }

    fn poll_hid_report(&mut self) -> Result<Option<HidReportCompletion>, XhciInitError> {
        for _ in 0..RUNTIME_EVENT_BUDGET {
            if !self.dispatch_one_event()? {
                break;
            }
        }
        let pending = self
            .events
            .transfer
            .filter(|pending| pending.owner == TransferOwner::Hid && pending.completion.is_some());
        let Some(pending) = pending else {
            return Ok(None);
        };
        self.events.transfer = None;
        let event = pending
            .completion
            .ok_or(XhciInitError::InvalidTransferCompletion)?;
        let actual_length = match event.completion_code() {
            COMPLETION_CODE_SUCCESS => pending.requested_length,
            COMPLETION_CODE_SHORT_PACKET => pending
                .requested_length
                .checked_sub(
                    u16::try_from(event.transfer_length())
                        .map_err(|_| XhciInitError::InvalidTransferCompletion)?,
                )
                .ok_or(XhciInitError::InvalidTransferCompletion)?,
            code => {
                if let Some(device) = self.first_device.as_mut() {
                    device.hid_transfer_pending = false;
                }
                self.info.hid_keyboard_ready = false;
                return Err(XhciInitError::TransferFailed(code));
            }
        };

        let (report_buffer, port_id, endpoint_address) = {
            let device = self
                .first_device
                .as_mut()
                .ok_or(XhciInitError::InvalidCapabilities)?;
            device.hid_transfer_pending = false;
            (
                device
                    .hid_report_buffer
                    .ok_or(XhciInitError::HidKeyboardNotFound)?,
                device.port_id,
                device.hid_endpoint_address,
            )
        };
        let changed_bit = 1u64 << (port_id - 1);
        if self.events.changed_ports & changed_bit != 0 {
            self.events.changed_ports &= !changed_bit;
            let connected = read_mmio_u32(self.info.mmio, self.layout.port(port_id, PORTSC)?)?
                & PORTSC_CURRENT_CONNECT_STATUS
                != 0;
            if !connected {
                self.info.hid_keyboard_ready = false;
                return Ok(None);
            }
        }

        let mut report = [0u8; HID_BOOT_REPORT_BYTES];
        let valid_report = usize::from(actual_length) >= HID_BOOT_REPORT_BYTES;
        if valid_report {
            copy_from_dma(report_buffer, &mut report)?;
        }
        self.arm_hid_transfer()?;
        if !valid_report {
            return Err(XhciInitError::InvalidHidReport);
        }
        Ok(Some(HidReportCompletion {
            slot_id: pending.slot_id,
            endpoint_address,
            actual_length,
            report,
        }))
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
        self.enumerate_hid_keyboard(frame_allocator, physical_memory_offset)
    }
}

/// Consume a bounded number of xHCI events and deliver newly pressed HID keys
/// to the TTY. The controller lock is released before decoding or entering the
/// TTY, so the runtime path cannot invert xHCI and terminal lock ordering.
#[cfg(target_os = "none")]
pub fn poll_hid_once() {
    let result = {
        let Some(controller) = XHCI_CONTROLLER.get() else {
            return;
        };
        controller.lock().poll_hid_report()
    };
    let completion = match result {
        Ok(Some(completion)) => completion,
        Ok(None) => return,
        Err(error) => {
            crate::serial_println!("[xhci] HID polling disabled/ignored: {:?}", error);
            return;
        }
    };

    let decoded = HID_DECODER.lock().decode(completion.report);
    for character in decoded.iter() {
        crate::serial_println!(
            "[xhci] HID report received: slot={}, endpoint=0x{:02X}, len={}, key={}",
            completion.slot_id,
            completion.endpoint_address,
            completion.actual_length,
            character,
        );
        crate::tty::TTY.lock().on_char(character);
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
        context_size,
        command_enqueue_index: 0,
        command_cycle: true,
        event_dequeue_index: 0,
        event_cycle: true,
        events: EventDispatchState::new(),
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

    #[test]
    fn serializes_setup_packets_and_transfer_trbs() {
        assert_eq!(core::mem::size_of::<UsbSetupPacket>(), 8);
        let packet = UsbSetupPacket::new(0x80, 6, 0x0100, 0x0203, 18);
        assert_eq!(packet.to_bytes(), [0x80, 6, 0, 1, 3, 2, 18, 0]);

        let setup = Trb::setup_stage(packet, TransferDirection::In).with_cycle(true);
        assert_eq!(setup.trb_type(), TRB_TYPE_SETUP_STAGE);
        assert_ne!(setup.control & TRB_IMMEDIATE_DATA, 0);
        assert_ne!(setup.control & TRB_CHAIN, 0);
        assert_eq!(
            (setup.control >> SETUP_TRANSFER_TYPE_SHIFT) & 0x3,
            SETUP_TRANSFER_TYPE_IN
        );
        let data = Trb::data_stage(0x1234_5000, 18, TransferDirection::In);
        assert_eq!(data.trb_type(), TRB_TYPE_DATA_STAGE);
        assert_ne!(data.control & TRB_DIRECTION_IN, 0);
        assert_ne!(data.control & TRB_INTERRUPT_ON_SHORT_PACKET, 0);
        let status = Trb::status_stage(TransferDirection::Out);
        assert_eq!(status.trb_type(), TRB_TYPE_STATUS_STAGE);
        assert_ne!(status.control & TRB_INTERRUPT_ON_COMPLETION, 0);
        let normal = Trb::normal(0x2234_5000, 8);
        assert_eq!(normal.trb_type(), TRB_TYPE_NORMAL);
        assert_ne!(normal.control & TRB_INTERRUPT_ON_COMPLETION, 0);
    }

    #[test]
    fn reserves_transfer_ring_without_splitting_a_td() {
        let mut cursor = RingCursor::new();
        cursor.enqueue_index = COMMAND_RING_USABLE_TRBS - 3;
        let reservation = cursor.reserve(3).unwrap();
        assert_eq!(reservation.start_index, COMMAND_RING_USABLE_TRBS - 3);
        assert!(reservation.reached_link);
        assert_eq!(cursor.enqueue_index, 0);
        assert!(!cursor.cycle);

        cursor.enqueue_index = COMMAND_RING_USABLE_TRBS - 2;
        assert_eq!(cursor.reserve(3), Err(XhciInitError::RingFull));
        assert_eq!(cursor.enqueue_index, COMMAND_RING_USABLE_TRBS - 2);
    }

    #[test]
    fn parses_device_and_hid_boot_keyboard_descriptors() {
        let device = [
            18, 1, 0x00, 0x02, 0, 0, 0, 64, 0x27, 0x06, 0x01, 0x00, 0x00, 0x01, 1, 2, 3, 1,
        ];
        let parsed = parse_device_descriptor(&device, 3).unwrap();
        assert_eq!(parsed.usb_version, 0x0200);
        assert_eq!(parsed.max_packet_size_zero, 64);
        assert_eq!(parsed.vendor_id, 0x0627);
        assert_eq!(parsed.product_id, 0x0001);

        let configuration = [
            9, 2, 34, 0, 1, 1, 0, 0xA0, 50, // Configuration
            9, 4, 0, 0, 1, 3, 1, 1, 0, // HID boot keyboard interface
            9, 0x21, 0x11, 0x01, 0, 1, 0x22, 63, 0, // HID descriptor
            7, 5, 0x81, 3, 8, 0, 10, // Interrupt IN endpoint
        ];
        assert_eq!(
            parse_hid_keyboard_configuration(&configuration),
            Ok(HidKeyboardDescriptor {
                configuration_value: 1,
                interface_number: 0,
                endpoint_address: 0x81,
                max_packet_size: 8,
                interval: 10,
            })
        );
    }

    #[test]
    fn rejects_malformed_descriptor_walks() {
        let mut configuration = [
            9, 2, 25, 0, 1, 1, 0, 0x80, 50, 9, 4, 0, 0, 1, 3, 1, 1, 0, 7, 5, 0x81, 3, 8, 0, 10,
        ];
        configuration[9] = 0;
        assert_eq!(
            parse_hid_keyboard_configuration(&configuration),
            Err(XhciInitError::InvalidDescriptor)
        );
        configuration[9] = 1;
        assert_eq!(
            parse_hid_keyboard_configuration(&configuration),
            Err(XhciInitError::InvalidDescriptor)
        );
        configuration[9] = 9;
        configuration[2] = 26;
        assert_eq!(
            parse_hid_keyboard_configuration(&configuration),
            Err(XhciInitError::InvalidDescriptor)
        );
    }

    #[test]
    fn calculates_endpoint_ids_and_intervals() {
        assert_eq!(endpoint_id(0), Some(1));
        assert_eq!(endpoint_id(0x01), Some(2));
        assert_eq!(endpoint_id(0x81), Some(3));
        assert_eq!(endpoint_id(0x8F), Some(31));
        assert_eq!(interrupt_interval(1, 10), Some(6));
        assert_eq!(interrupt_interval(2, 1), Some(3));
        assert_eq!(interrupt_interval(3, 10), Some(9));
        assert_eq!(interrupt_interval(3, 0), None);
    }

    #[test]
    fn dispatches_interleaved_events_to_their_owner() {
        let mut dispatch = EventDispatchState::new();
        dispatch.command = Some(PendingCommand {
            trb_pointer: 0x1000,
            expected_slot: Some(2),
            completion: None,
        });
        dispatch.transfer = Some(PendingTransfer {
            owner: TransferOwner::Hid,
            trb_pointer: 0x2000,
            short_packet_pointer: None,
            slot_id: 2,
            endpoint_id: 3,
            requested_length: 8,
            completion: None,
        });
        dispatch.dispatch(Trb {
            parameter: 5 << 24,
            status: 0,
            control: TRB_TYPE_PORT_STATUS_CHANGE_EVENT << TRB_TYPE_SHIFT,
        });
        dispatch.dispatch(Trb {
            parameter: 0x2000,
            status: u32::from(COMPLETION_CODE_SUCCESS) << 24,
            control: (TRB_TYPE_TRANSFER_EVENT << TRB_TYPE_SHIFT) | (3 << 16) | (2 << 24),
        });
        dispatch.dispatch(Trb {
            parameter: 0x1000,
            status: u32::from(COMPLETION_CODE_SUCCESS) << 24,
            control: (TRB_TYPE_COMMAND_COMPLETION_EVENT << TRB_TYPE_SHIFT) | (2 << 24),
        });

        assert_eq!(dispatch.changed_ports, 1 << 4);
        assert!(dispatch.command.unwrap().completion.is_some());
        assert!(dispatch.transfer.unwrap().completion.is_some());
        assert_eq!(dispatch.unexpected_events, 0);
    }

    #[test]
    fn rejects_completion_identity_mismatches() {
        let mut dispatch = EventDispatchState::new();
        dispatch.transfer = Some(PendingTransfer {
            owner: TransferOwner::Control,
            trb_pointer: 0x2000,
            short_packet_pointer: Some(0x1FF0),
            slot_id: 2,
            endpoint_id: 1,
            requested_length: 18,
            completion: None,
        });
        dispatch.dispatch(Trb {
            parameter: 0x2010,
            status: u32::from(COMPLETION_CODE_SUCCESS) << 24,
            control: (TRB_TYPE_TRANSFER_EVENT << TRB_TYPE_SHIFT) | (1 << 16) | (2 << 24),
        });
        assert!(dispatch.transfer.unwrap().completion.is_none());
        assert_eq!(dispatch.unexpected_events, 1);

        dispatch.dispatch(Trb {
            parameter: 0x1FF0,
            status: (u32::from(COMPLETION_CODE_SHORT_PACKET) << 24) | 2,
            control: (TRB_TYPE_TRANSFER_EVENT << TRB_TYPE_SHIFT) | (1 << 16) | (2 << 24),
        });
        let completion = dispatch.transfer.unwrap().completion.unwrap();
        assert_eq!(completion.completion_code(), COMPLETION_CODE_SHORT_PACKET);
        assert_eq!(completion.transfer_length(), 2);
    }

    #[test]
    fn decodes_only_new_hid_boot_key_presses() {
        let mut decoder = HidKeyboardDecoder::new();
        let mut report = [0u8; 8];
        report[2] = 0x04;
        assert_eq!(
            decoder.decode(report).iter().collect::<std::vec::Vec<_>>(),
            ['a']
        );
        assert_eq!(decoder.decode(report).len, 0);

        report[0] = 0x02;
        report[2] = 0x05;
        report[3] = 0x28;
        assert_eq!(
            decoder.decode(report).iter().collect::<std::vec::Vec<_>>(),
            ['B', '\n']
        );
        let previous = decoder.previous;
        report[2] = 1;
        assert_eq!(decoder.decode(report).len, 0);
        assert_eq!(decoder.previous, previous);
    }
}

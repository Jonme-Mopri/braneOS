//! Read-only USB Mass Storage Bulk-Only Transport and SCSI block backend.
//!
//! The implementation intentionally supports one xHCI device, interface and
//! LUN 0. Commands are serialized by a device mutex and use a one-page bounce
//! buffer, matching the synchronous polling model used by the Phase 13 xHCI
//! baseline.

#![allow(dead_code)]

use spin::Mutex;
#[cfg(target_os = "none")]
use spin::Once;

use crate::block::{BlockDevice, BlockError};
#[cfg(target_os = "none")]
use crate::xhci;
use crate::xhci::XhciInitError;

const LOGICAL_BLOCK_SIZE: usize = 512;
const BOUNCE_BUFFER_BYTES: usize = 4096;
const CBW_BYTES: usize = 31;
const CSW_BYTES: usize = 13;
const INQUIRY_BYTES: usize = 36;
const REQUEST_SENSE_BYTES: usize = 18;
const READ_CAPACITY_BYTES: usize = 8;
const MAX_READY_ATTEMPTS: usize = 3;

const CBW_SIGNATURE: u32 = 0x4342_5355;
const CSW_SIGNATURE: u32 = 0x5342_5355;
const CBW_DIRECTION_IN: u8 = 0x80;

const SCSI_TEST_UNIT_READY: u8 = 0x00;
const SCSI_REQUEST_SENSE: u8 = 0x03;
const SCSI_INQUIRY: u8 = 0x12;
const SCSI_READ_CAPACITY_10: u8 = 0x25;
const SCSI_READ_10: u8 = 0x28;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbStorageError {
    AlreadyInitialized,
    NotFound,
    Transport(XhciInitError),
    InvalidCommand,
    InvalidCsw,
    ShortTransfer,
    CommandFailed,
    PhaseError,
    UnsupportedDevice,
    UnsupportedGeometry,
    NotReady,
    Unavailable,
}

impl From<XhciInitError> for UsbStorageError {
    fn from(error: XhciInitError) -> Self {
        Self::Transport(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DataDirection {
    None,
    In,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScsiCommand {
    bytes: [u8; 16],
    len: u8,
}

impl ScsiCommand {
    const fn test_unit_ready() -> Self {
        let mut bytes = [0u8; 16];
        bytes[0] = SCSI_TEST_UNIT_READY;
        Self { bytes, len: 6 }
    }

    const fn inquiry() -> Self {
        let mut bytes = [0u8; 16];
        bytes[0] = SCSI_INQUIRY;
        bytes[4] = INQUIRY_BYTES as u8;
        Self { bytes, len: 6 }
    }

    const fn request_sense() -> Self {
        let mut bytes = [0u8; 16];
        bytes[0] = SCSI_REQUEST_SENSE;
        bytes[4] = REQUEST_SENSE_BYTES as u8;
        Self { bytes, len: 6 }
    }

    const fn read_capacity_10() -> Self {
        let mut bytes = [0u8; 16];
        bytes[0] = SCSI_READ_CAPACITY_10;
        Self { bytes, len: 10 }
    }

    fn read_10(lba: u32, blocks: u16) -> Result<Self, UsbStorageError> {
        if blocks == 0 {
            return Err(UsbStorageError::InvalidCommand);
        }
        let mut bytes = [0u8; 16];
        bytes[0] = SCSI_READ_10;
        bytes[2..6].copy_from_slice(&lba.to_be_bytes());
        bytes[7..9].copy_from_slice(&blocks.to_be_bytes());
        Ok(Self { bytes, len: 10 })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandBlockWrapper {
    tag: u32,
    transfer_length: u32,
    flags: u8,
    lun: u8,
    command: ScsiCommand,
}

impl CommandBlockWrapper {
    fn new(
        tag: u32,
        transfer_length: usize,
        direction: DataDirection,
        command: ScsiCommand,
    ) -> Result<Self, UsbStorageError> {
        if !(1..=16).contains(&command.len) || usize::from(command.len) > command.bytes.len() {
            return Err(UsbStorageError::InvalidCommand);
        }
        let transfer_length =
            u32::try_from(transfer_length).map_err(|_| UsbStorageError::InvalidCommand)?;
        Ok(Self {
            tag,
            transfer_length,
            flags: if direction == DataDirection::In {
                CBW_DIRECTION_IN
            } else {
                0
            },
            lun: 0,
            command,
        })
    }

    fn to_bytes(self) -> [u8; CBW_BYTES] {
        let mut bytes = [0u8; CBW_BYTES];
        bytes[0..4].copy_from_slice(&CBW_SIGNATURE.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.tag.to_le_bytes());
        bytes[8..12].copy_from_slice(&self.transfer_length.to_le_bytes());
        bytes[12] = self.flags;
        bytes[13] = self.lun;
        bytes[14] = self.command.len;
        bytes[15..31].copy_from_slice(&self.command.bytes);
        bytes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommandStatus {
    Passed,
    Failed,
    PhaseError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandStatusWrapper {
    residue: u32,
    status: CommandStatus,
}

impl CommandStatusWrapper {
    fn parse(
        bytes: &[u8],
        expected_tag: u32,
        transfer_length: usize,
    ) -> Result<Self, UsbStorageError> {
        if bytes.len() != CSW_BYTES {
            return Err(UsbStorageError::InvalidCsw);
        }
        let signature = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let tag = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let residue = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if signature != CSW_SIGNATURE
            || tag != expected_tag
            || u64::from(residue) > transfer_length as u64
        {
            return Err(UsbStorageError::InvalidCsw);
        }
        let status = match bytes[12] {
            0 => CommandStatus::Passed,
            1 => CommandStatus::Failed,
            2 => CommandStatus::PhaseError,
            _ => return Err(UsbStorageError::InvalidCsw),
        };
        Ok(Self { residue, status })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InquiryData {
    removable: bool,
}

impl InquiryData {
    fn parse(bytes: &[u8]) -> Result<Self, UsbStorageError> {
        if bytes.len() < INQUIRY_BYTES
            || bytes[0] >> 5 != 0
            || bytes[0] & 0x1F != 0
            || bytes[3] & 0x0F < 2
            || bytes[4] < (INQUIRY_BYTES - 5) as u8
        {
            return Err(UsbStorageError::UnsupportedDevice);
        }
        Ok(Self {
            removable: bytes[1] & 0x80 != 0,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SenseData {
    response_code: u8,
    key: u8,
    asc: u8,
    ascq: u8,
}

impl SenseData {
    fn parse(bytes: &[u8]) -> Result<Self, UsbStorageError> {
        if bytes.len() < 14 || !matches!(bytes[0] & 0x7F, 0x70 | 0x71) {
            return Err(UsbStorageError::UnsupportedDevice);
        }
        Ok(Self {
            response_code: bytes[0] & 0x7F,
            key: bytes[2] & 0x0F,
            asc: bytes[12],
            ascq: bytes[13],
        })
    }

    const fn retryable(self) -> bool {
        matches!(self.key, 0x02 | 0x06)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Capacity {
    block_count: u64,
    block_size: u32,
}

impl Capacity {
    fn parse(bytes: &[u8]) -> Result<Self, UsbStorageError> {
        if bytes.len() != READ_CAPACITY_BYTES {
            return Err(UsbStorageError::UnsupportedGeometry);
        }
        let last_lba = u32::from_be_bytes(bytes[0..4].try_into().unwrap());
        let block_size = u32::from_be_bytes(bytes[4..8].try_into().unwrap());
        if last_lba == u32::MAX || block_size != LOGICAL_BLOCK_SIZE as u32 {
            return Err(UsbStorageError::UnsupportedGeometry);
        }
        Ok(Self {
            block_count: u64::from(last_lba) + 1,
            block_size,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryAction {
    BulkOnlyReset,
    ClearHaltIn,
    ClearHaltOut,
    SynchronizeIn,
    SynchronizeOut,
}

const fn reset_recovery_plan() -> [RecoveryAction; 5] {
    [
        RecoveryAction::BulkOnlyReset,
        RecoveryAction::ClearHaltIn,
        RecoveryAction::ClearHaltOut,
        RecoveryAction::SynchronizeIn,
        RecoveryAction::SynchronizeOut,
    ]
}

trait BulkOnlyTransport {
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, UsbStorageError>;
    fn bulk_in(&mut self, output: &mut [u8]) -> Result<usize, UsbStorageError>;
    fn bulk_in_csw(&mut self, output: &mut [u8]) -> Result<usize, UsbStorageError>;
    fn reset_recovery(&mut self, plan: &[RecoveryAction]) -> Result<(), UsbStorageError>;
}

#[cfg(target_os = "none")]
struct XhciBulkOnlyTransport;

#[cfg(target_os = "none")]
impl BulkOnlyTransport for XhciBulkOnlyTransport {
    fn bulk_out(&mut self, data: &[u8]) -> Result<usize, UsbStorageError> {
        xhci::mass_storage_bulk_out(data)
            .map(usize::from)
            .map_err(UsbStorageError::Transport)
    }

    fn bulk_in(&mut self, output: &mut [u8]) -> Result<usize, UsbStorageError> {
        xhci::mass_storage_bulk_in(output)
            .map(usize::from)
            .map_err(UsbStorageError::Transport)
    }

    fn bulk_in_csw(&mut self, output: &mut [u8]) -> Result<usize, UsbStorageError> {
        xhci::mass_storage_bulk_in_csw(output)
            .map(usize::from)
            .map_err(UsbStorageError::Transport)
    }

    fn reset_recovery(&mut self, plan: &[RecoveryAction]) -> Result<(), UsbStorageError> {
        if plan != reset_recovery_plan() {
            return Err(UsbStorageError::InvalidCommand);
        }
        xhci::reset_mass_storage_transport().map_err(UsbStorageError::Transport)
    }
}

struct UsbMassStorageState {
    next_tag: u32,
    bounce: [u8; BOUNCE_BUFFER_BYTES],
    available: bool,
}

impl UsbMassStorageState {
    const fn new() -> Self {
        Self {
            next_tag: 1,
            bounce: [0; BOUNCE_BUFFER_BYTES],
            available: true,
        }
    }

    fn allocate_tag(&mut self) -> u32 {
        let tag = self.next_tag;
        self.next_tag = self.next_tag.wrapping_add(1);
        tag
    }

    fn recover(&mut self, transport: &mut impl BulkOnlyTransport) {
        let plan = reset_recovery_plan();
        if transport.reset_recovery(&plan).is_err() {
            self.available = false;
        }
    }

    fn execute_with(
        &mut self,
        transport: &mut impl BulkOnlyTransport,
        command: ScsiCommand,
        direction: DataDirection,
        transfer_length: usize,
    ) -> Result<usize, UsbStorageError> {
        if !self.available {
            return Err(UsbStorageError::Unavailable);
        }
        if transfer_length > self.bounce.len()
            || (direction == DataDirection::None && transfer_length != 0)
        {
            return Err(UsbStorageError::InvalidCommand);
        }
        let tag = self.allocate_tag();
        let cbw = CommandBlockWrapper::new(tag, transfer_length, direction, command)?.to_bytes();
        let cbw_length = match transport.bulk_out(&cbw) {
            Ok(length) => length,
            Err(error) => {
                self.recover(transport);
                return Err(error);
            }
        };
        if cbw_length != cbw.len() {
            self.recover(transport);
            return Err(UsbStorageError::ShortTransfer);
        }

        let actual_data_length = if direction == DataDirection::In && transfer_length != 0 {
            match transport.bulk_in(&mut self.bounce[..transfer_length]) {
                Ok(length) => length,
                Err(error) => {
                    self.recover(transport);
                    return Err(error);
                }
            }
        } else {
            0
        };

        let mut csw_bytes = [0u8; CSW_BYTES];
        let csw_length = match transport.bulk_in_csw(&mut csw_bytes) {
            Ok(length) => length,
            Err(error) => {
                self.recover(transport);
                return Err(error);
            }
        };
        if csw_length != CSW_BYTES {
            self.recover(transport);
            return Err(UsbStorageError::InvalidCsw);
        }
        let csw = match CommandStatusWrapper::parse(&csw_bytes, tag, transfer_length) {
            Ok(csw) => csw,
            Err(error) => {
                self.recover(transport);
                return Err(error);
            }
        };
        if actual_data_length
            .checked_add(csw.residue as usize)
            .is_none_or(|total| total != transfer_length)
        {
            self.recover(transport);
            return Err(UsbStorageError::InvalidCsw);
        }
        match csw.status {
            CommandStatus::Passed => Ok(actual_data_length),
            CommandStatus::Failed => Err(UsbStorageError::CommandFailed),
            CommandStatus::PhaseError => {
                self.recover(transport);
                Err(UsbStorageError::PhaseError)
            }
        }
    }

    #[cfg(target_os = "none")]
    fn execute(
        &mut self,
        command: ScsiCommand,
        direction: DataDirection,
        transfer_length: usize,
    ) -> Result<usize, UsbStorageError> {
        self.execute_with(
            &mut XhciBulkOnlyTransport,
            command,
            direction,
            transfer_length,
        )
    }

    #[cfg(target_os = "none")]
    fn request_sense(&mut self) -> Result<SenseData, UsbStorageError> {
        let actual = self.execute(
            ScsiCommand::request_sense(),
            DataDirection::In,
            REQUEST_SENSE_BYTES,
        )?;
        SenseData::parse(&self.bounce[..actual])
    }

    #[cfg(target_os = "none")]
    fn read_10(&mut self, lba: u32, blocks: u16) -> Result<usize, UsbStorageError> {
        let bytes = usize::from(blocks)
            .checked_mul(LOGICAL_BLOCK_SIZE)
            .ok_or(UsbStorageError::InvalidCommand)?;
        let command = ScsiCommand::read_10(lba, blocks)?;
        match self.execute(command, DataDirection::In, bytes) {
            Ok(actual) if actual == bytes => Ok(actual),
            Ok(_) => Err(UsbStorageError::ShortTransfer),
            Err(UsbStorageError::CommandFailed) => {
                let _ = self.request_sense();
                Err(UsbStorageError::CommandFailed)
            }
            Err(error) => Err(error),
        }
    }
}

/// Registered read-only USB LUN 0.
pub struct UsbMassStorageDevice {
    state: Mutex<UsbMassStorageState>,
    block_count: u64,
}

impl UsbMassStorageDevice {
    pub const fn capacity_blocks(&self) -> u64 {
        self.block_count
    }

    fn validate_read(&self, lba: u64, bytes: usize) -> Result<(), BlockError> {
        if bytes == 0 || !bytes.is_multiple_of(LOGICAL_BLOCK_SIZE) {
            return Err(BlockError::InvalidBuffer);
        }
        let blocks = (bytes / LOGICAL_BLOCK_SIZE) as u64;
        if lba
            .checked_add(blocks)
            .is_none_or(|end| end > self.block_count)
            || lba > u64::from(u32::MAX)
        {
            return Err(BlockError::OutOfRange);
        }
        Ok(())
    }
}

fn map_block_error(error: UsbStorageError) -> BlockError {
    match error {
        UsbStorageError::Transport(XhciInitError::Busy) => BlockError::Busy,
        UsbStorageError::InvalidCommand | UsbStorageError::UnsupportedGeometry => {
            BlockError::Unsupported
        }
        UsbStorageError::Unavailable
        | UsbStorageError::NotFound
        | UsbStorageError::Transport(_)
        | UsbStorageError::InvalidCsw
        | UsbStorageError::ShortTransfer
        | UsbStorageError::CommandFailed
        | UsbStorageError::PhaseError
        | UsbStorageError::UnsupportedDevice
        | UsbStorageError::NotReady
        | UsbStorageError::AlreadyInitialized => BlockError::Io,
    }
}

#[cfg(test)]
pub(crate) fn fuzz_untrusted_storage_bytes(bytes: &[u8]) {
    let _ = CommandStatusWrapper::parse(bytes, 1, BOUNCE_BUFFER_BYTES);
    let _ = InquiryData::parse(bytes);
    let _ = SenseData::parse(bytes);
    let _ = Capacity::parse(bytes);
}

impl BlockDevice for UsbMassStorageDevice {
    fn name(&self) -> &str {
        "usb-storage0"
    }

    fn block_size(&self) -> u32 {
        LOGICAL_BLOCK_SIZE as u32
    }

    fn block_count(&self) -> u64 {
        self.block_count
    }

    fn read_only(&self) -> bool {
        true
    }

    fn read_blocks(&self, lba: u64, output: &mut [u8]) -> Result<(), BlockError> {
        self.validate_read(lba, output.len())?;
        #[cfg(not(target_os = "none"))]
        {
            let _ = output;
            return Err(BlockError::Unsupported);
        }
        #[cfg(target_os = "none")]
        {
            let mut state = self.state.lock();
            let max_blocks = BOUNCE_BUFFER_BYTES / LOGICAL_BLOCK_SIZE;
            let mut completed_blocks = 0usize;
            for chunk in output.chunks_mut(BOUNCE_BUFFER_BYTES) {
                let blocks = chunk.len() / LOGICAL_BLOCK_SIZE;
                debug_assert!(blocks <= max_blocks);
                let chunk_lba = lba
                    .checked_add(completed_blocks as u64)
                    .and_then(|candidate| u32::try_from(candidate).ok())
                    .ok_or(BlockError::OutOfRange)?;
                let actual = state
                    .read_10(chunk_lba, blocks as u16)
                    .map_err(map_block_error)?;
                chunk.copy_from_slice(&state.bounce[..actual]);
                completed_blocks += blocks;
            }
            Ok(())
        }
    }

    fn write_blocks(&self, _lba: u64, _data: &[u8]) -> Result<(), BlockError> {
        Err(BlockError::ReadOnly)
    }

    fn flush(&self) -> Result<(), BlockError> {
        Err(BlockError::ReadOnly)
    }
}

#[cfg(target_os = "none")]
static USB_STORAGE: Once<UsbMassStorageDevice> = Once::new();

/// Probe the configured xHCI Mass Storage transport and publish LUN 0.
#[cfg(target_os = "none")]
pub fn init() -> Result<&'static UsbMassStorageDevice, UsbStorageError> {
    if USB_STORAGE.get().is_some() {
        return Err(UsbStorageError::AlreadyInitialized);
    }
    xhci::mass_storage_transport_info().ok_or(UsbStorageError::NotFound)?;
    let mut state = UsbMassStorageState::new();

    let inquiry_length = state.execute(ScsiCommand::inquiry(), DataDirection::In, INQUIRY_BYTES)?;
    InquiryData::parse(&state.bounce[..inquiry_length])?;

    let mut ready = false;
    for _ in 0..MAX_READY_ATTEMPTS {
        match state.execute(ScsiCommand::test_unit_ready(), DataDirection::None, 0) {
            Ok(0) => {
                ready = true;
                break;
            }
            Err(UsbStorageError::CommandFailed) => {
                let sense = state.request_sense()?;
                if !sense.retryable() {
                    return Err(UsbStorageError::NotReady);
                }
            }
            Ok(_) => return Err(UsbStorageError::InvalidCsw),
            Err(error) => return Err(error),
        }
    }
    if !ready {
        return Err(UsbStorageError::NotReady);
    }

    let capacity_length = state.execute(
        ScsiCommand::read_capacity_10(),
        DataDirection::In,
        READ_CAPACITY_BYTES,
    )?;
    let capacity = Capacity::parse(&state.bounce[..capacity_length])?;
    let probe_length = state.read_10(0, 1)?;
    if probe_length != LOGICAL_BLOCK_SIZE {
        return Err(UsbStorageError::ShortTransfer);
    }

    Ok(USB_STORAGE.call_once(|| UsbMassStorageDevice {
        state: Mutex::new(state),
        block_count: capacity.block_count,
    }))
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use alloc::vec;
    use alloc::vec::Vec;

    use super::*;

    struct MockTransport {
        inbound: Vec<Result<Vec<u8>, UsbStorageError>>,
        out_calls: usize,
        out_error: Option<UsbStorageError>,
        recovery: Vec<RecoveryAction>,
        recovery_result: Result<(), UsbStorageError>,
    }

    impl MockTransport {
        fn new(inbound: Vec<Result<Vec<u8>, UsbStorageError>>) -> Self {
            Self {
                inbound,
                out_calls: 0,
                out_error: None,
                recovery: Vec::new(),
                recovery_result: Ok(()),
            }
        }
    }

    impl BulkOnlyTransport for MockTransport {
        fn bulk_out(&mut self, data: &[u8]) -> Result<usize, UsbStorageError> {
            self.out_calls += 1;
            if let Some(error) = self.out_error.take() {
                return Err(error);
            }
            Ok(data.len())
        }

        fn bulk_in(&mut self, output: &mut [u8]) -> Result<usize, UsbStorageError> {
            if self.inbound.is_empty() {
                return Err(UsbStorageError::ShortTransfer);
            }
            let response = self.inbound.remove(0)?;
            if response.len() > output.len() {
                return Err(UsbStorageError::ShortTransfer);
            }
            output[..response.len()].copy_from_slice(&response);
            Ok(response.len())
        }

        fn bulk_in_csw(&mut self, output: &mut [u8]) -> Result<usize, UsbStorageError> {
            self.bulk_in(output)
        }

        fn reset_recovery(&mut self, plan: &[RecoveryAction]) -> Result<(), UsbStorageError> {
            self.recovery.extend_from_slice(plan);
            self.recovery_result
        }
    }

    fn csw(tag: u32, residue: u32, status: u8) -> [u8; CSW_BYTES] {
        let mut bytes = [0u8; CSW_BYTES];
        bytes[0..4].copy_from_slice(&CSW_SIGNATURE.to_le_bytes());
        bytes[4..8].copy_from_slice(&tag.to_le_bytes());
        bytes[8..12].copy_from_slice(&residue.to_le_bytes());
        bytes[12] = status;
        bytes
    }

    #[test]
    fn serializes_exact_bot_command_block_wrapper() {
        let bytes = CommandBlockWrapper::new(
            0x1234_5678,
            INQUIRY_BYTES,
            DataDirection::In,
            ScsiCommand::inquiry(),
        )
        .unwrap()
        .to_bytes();
        assert_eq!(bytes.len(), 31);
        assert_eq!(&bytes[0..4], &CBW_SIGNATURE.to_le_bytes());
        assert_eq!(&bytes[4..8], &0x1234_5678u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &(INQUIRY_BYTES as u32).to_le_bytes());
        assert_eq!(bytes[12], 0x80);
        assert_eq!(bytes[13], 0);
        assert_eq!(bytes[14], 6);
        assert_eq!(bytes[15], SCSI_INQUIRY);
        assert_eq!(bytes[19], INQUIRY_BYTES as u8);
        assert!(bytes[21..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn validates_command_status_wrapper_identity_and_status() {
        assert_eq!(
            CommandStatusWrapper::parse(&csw(7, 4, 0), 7, 36),
            Ok(CommandStatusWrapper {
                residue: 4,
                status: CommandStatus::Passed,
            })
        );
        let mut invalid_signature = csw(7, 0, 0);
        invalid_signature[0] ^= 1;
        assert_eq!(
            CommandStatusWrapper::parse(&invalid_signature, 7, 36),
            Err(UsbStorageError::InvalidCsw)
        );
        assert_eq!(
            CommandStatusWrapper::parse(&csw(8, 0, 0), 7, 36),
            Err(UsbStorageError::InvalidCsw)
        );
        assert_eq!(
            CommandStatusWrapper::parse(&csw(7, 37, 0), 7, 36),
            Err(UsbStorageError::InvalidCsw)
        );
        assert_eq!(
            CommandStatusWrapper::parse(&csw(7, 0, 3), 7, 36),
            Err(UsbStorageError::InvalidCsw)
        );
        assert_eq!(
            CommandStatusWrapper::parse(&csw(7, 0, 0)[..12], 7, 36),
            Err(UsbStorageError::InvalidCsw)
        );
    }

    #[test]
    fn builds_minimum_scsi_cdbs_with_scsi_endianness() {
        assert_eq!(
            ScsiCommand::test_unit_ready().bytes[0],
            SCSI_TEST_UNIT_READY
        );
        assert_eq!(ScsiCommand::request_sense().bytes[4], 18);
        assert_eq!(ScsiCommand::inquiry().bytes[4], 36);
        assert_eq!(ScsiCommand::read_capacity_10().len, 10);
        let read = ScsiCommand::read_10(0x1234_5678, 0x2345).unwrap();
        assert_eq!(&read.bytes[2..6], &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(&read.bytes[7..9], &[0x23, 0x45]);
        assert_eq!(
            ScsiCommand::read_10(0, 0),
            Err(UsbStorageError::InvalidCommand)
        );
    }

    #[test]
    fn parses_inquiry_capacity_and_fixed_sense_defensively() {
        let mut inquiry = [0u8; INQUIRY_BYTES];
        inquiry[1] = 0x80;
        inquiry[3] = 2;
        inquiry[4] = 31;
        assert_eq!(
            InquiryData::parse(&inquiry),
            Ok(InquiryData { removable: true })
        );
        inquiry[0] = 5;
        assert_eq!(
            InquiryData::parse(&inquiry),
            Err(UsbStorageError::UnsupportedDevice)
        );

        let mut capacity = [0u8; 8];
        capacity[0..4].copy_from_slice(&0x0001_FFFFu32.to_be_bytes());
        capacity[4..8].copy_from_slice(&512u32.to_be_bytes());
        assert_eq!(
            Capacity::parse(&capacity),
            Ok(Capacity {
                block_count: 131_072,
                block_size: 512,
            })
        );
        capacity[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            Capacity::parse(&capacity),
            Err(UsbStorageError::UnsupportedGeometry)
        );

        let mut sense = [0u8; REQUEST_SENSE_BYTES];
        sense[0] = 0x70;
        sense[2] = 0x06;
        sense[12] = 0x28;
        assert!(SenseData::parse(&sense).unwrap().retryable());
        assert_eq!(
            SenseData::parse(&sense[..13]),
            Err(UsbStorageError::UnsupportedDevice)
        );
    }

    #[test]
    fn reset_recovery_order_is_complete_and_stable() {
        assert_eq!(
            reset_recovery_plan(),
            [
                RecoveryAction::BulkOnlyReset,
                RecoveryAction::ClearHaltIn,
                RecoveryAction::ClearHaltOut,
                RecoveryAction::SynchronizeIn,
                RecoveryAction::SynchronizeOut,
            ]
        );
    }

    #[test]
    fn phase_error_executes_recovery_once_without_reissuing_cbw() {
        let mut state = UsbMassStorageState::new();
        let mut transport = MockTransport::new(vec![Ok(csw(1, 0, 2).to_vec())]);
        assert_eq!(
            state.execute_with(
                &mut transport,
                ScsiCommand::test_unit_ready(),
                DataDirection::None,
                0,
            ),
            Err(UsbStorageError::PhaseError)
        );
        assert_eq!(transport.out_calls, 1);
        assert_eq!(transport.recovery, reset_recovery_plan());
        assert!(state.available);

        let stall = UsbStorageError::Transport(XhciInitError::TransferFailed(6));
        let mut stalled_state = UsbMassStorageState::new();
        let mut stalled_transport = MockTransport::new(vec![]);
        stalled_transport.out_error = Some(stall);
        assert_eq!(
            stalled_state.execute_with(
                &mut stalled_transport,
                ScsiCommand::test_unit_ready(),
                DataDirection::None,
                0,
            ),
            Err(stall)
        );
        assert_eq!(stalled_transport.recovery, reset_recovery_plan());
    }

    #[test]
    fn invalid_csw_identity_forces_recovery() {
        let mut state = UsbMassStorageState::new();
        let mut transport = MockTransport::new(vec![Ok(csw(99, 0, 0).to_vec())]);
        assert_eq!(
            state.execute_with(
                &mut transport,
                ScsiCommand::test_unit_ready(),
                DataDirection::None,
                0,
            ),
            Err(UsbStorageError::InvalidCsw)
        );
        assert_eq!(transport.recovery, reset_recovery_plan());

        let mut residue_state = UsbMassStorageState::new();
        let mut residue_transport =
            MockTransport::new(vec![Ok(vec![0; 4]), Ok(csw(1, 3, 0).to_vec())]);
        assert_eq!(
            residue_state.execute_with(
                &mut residue_transport,
                ScsiCommand::read_capacity_10(),
                DataDirection::In,
                READ_CAPACITY_BYTES,
            ),
            Err(UsbStorageError::InvalidCsw)
        );
        assert_eq!(residue_transport.recovery, reset_recovery_plan());
    }

    #[test]
    fn timeout_and_failed_recovery_quarantine_transport() {
        let timeout = UsbStorageError::Transport(XhciInitError::TransferTimeout);
        let mut state = UsbMassStorageState::new();
        let mut transport = MockTransport::new(vec![Err(timeout)]);
        transport.recovery_result = Err(UsbStorageError::Transport(XhciInitError::Busy));
        assert_eq!(
            state.execute_with(
                &mut transport,
                ScsiCommand::read_capacity_10(),
                DataDirection::In,
                READ_CAPACITY_BYTES,
            ),
            Err(timeout)
        );
        assert!(!state.available);
        assert_eq!(transport.out_calls, 1);
        assert_eq!(
            state.execute_with(
                &mut transport,
                ScsiCommand::test_unit_ready(),
                DataDirection::None,
                0,
            ),
            Err(UsbStorageError::Unavailable)
        );
        assert_eq!(transport.out_calls, 1);
    }

    #[test]
    fn command_failed_is_not_confused_with_transport_failure() {
        let mut state = UsbMassStorageState::new();
        let mut sense = vec![0; REQUEST_SENSE_BYTES];
        sense[0] = 0x70;
        sense[2] = 0x06;
        sense[12] = 0x28;
        let mut transport = MockTransport::new(vec![
            Ok(csw(1, 0, 1).to_vec()),
            Ok(sense),
            Ok(csw(2, 0, 0).to_vec()),
        ]);
        assert_eq!(
            state.execute_with(
                &mut transport,
                ScsiCommand::test_unit_ready(),
                DataDirection::None,
                0,
            ),
            Err(UsbStorageError::CommandFailed)
        );
        assert!(transport.recovery.is_empty());
        assert!(state.available);
        let actual = state
            .execute_with(
                &mut transport,
                ScsiCommand::request_sense(),
                DataDirection::In,
                REQUEST_SENSE_BYTES,
            )
            .unwrap();
        assert!(SenseData::parse(&state.bounce[..actual])
            .unwrap()
            .retryable());
        assert_eq!(transport.out_calls, 2);
        assert!(transport.recovery.is_empty());
    }

    #[test]
    fn validates_read_geometry_and_rejects_mutation() {
        let device = UsbMassStorageDevice {
            state: Mutex::new(UsbMassStorageState::new()),
            block_count: 32,
        };
        assert_eq!(device.validate_read(0, 512), Ok(()));
        assert_eq!(device.validate_read(31, 1024), Err(BlockError::OutOfRange));
        assert_eq!(device.validate_read(0, 513), Err(BlockError::InvalidBuffer));
        assert_eq!(device.write_blocks(0, &[0; 512]), Err(BlockError::ReadOnly));
        assert_eq!(device.flush(), Err(BlockError::ReadOnly));
    }

    #[test]
    fn assigns_wrapping_non_reused_tags() {
        let mut state = UsbMassStorageState::new();
        state.next_tag = u32::MAX;
        assert_eq!(state.allocate_tag(), u32::MAX);
        assert_eq!(state.allocate_tag(), 0);
        assert_eq!(state.allocate_tag(), 1);
    }

    #[test]
    fn parser_fuzz_inputs_fail_without_panicking() {
        for len in 0..64 {
            let bytes = vec![0xA5; len];
            let _ = CommandStatusWrapper::parse(&bytes, 1, BOUNCE_BUFFER_BYTES);
            let _ = InquiryData::parse(&bytes);
            let _ = SenseData::parse(&bytes);
            let _ = Capacity::parse(&bytes);
        }
    }
}

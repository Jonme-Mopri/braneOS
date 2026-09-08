//! Device-independent block I/O layer.
//!
//! Drivers expose sector-oriented storage through [`BlockDevice`]. The fixed
//! registry assigns stable kernel IDs, validates every transfer before it
//! reaches a driver, and contains no heap allocation in its core path.

#![allow(dead_code)]

use spin::Mutex;

pub const MAX_BLOCK_DEVICES: usize = 16;
pub const MAX_BLOCK_DEVICE_NAME: usize = 24;
pub const MIN_BLOCK_SIZE: u32 = 512;
pub const MAX_BLOCK_SIZE: u32 = 64 * 1024;

pub type BlockDeviceId = u16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockError {
    RegistryFull,
    DuplicateName,
    InvalidDevice,
    InvalidGeometry,
    InvalidBuffer,
    OutOfRange,
    ReadOnly,
    Busy,
    Unsupported,
    Io,
}

/// Copyable metadata published by the block registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockDeviceInfo {
    pub id: BlockDeviceId,
    name: [u8; MAX_BLOCK_DEVICE_NAME],
    name_len: usize,
    pub block_size: u32,
    pub block_count: u64,
    pub read_only: bool,
}

impl BlockDeviceInfo {
    const fn empty() -> Self {
        Self {
            id: 0,
            name: [0; MAX_BLOCK_DEVICE_NAME],
            name_len: 0,
            block_size: 0,
            block_count: 0,
            read_only: false,
        }
    }

    fn from_device(id: BlockDeviceId, device: &dyn BlockDevice) -> Self {
        let mut info = Self::empty();
        info.id = id;
        let name = device.name().as_bytes();
        info.name_len = name.len().min(MAX_BLOCK_DEVICE_NAME);
        info.name[..info.name_len].copy_from_slice(&name[..info.name_len]);
        info.block_size = device.block_size();
        info.block_count = device.block_count();
        info.read_only = device.read_only();
        info
    }

    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("invalid")
    }

    pub const fn capacity_bytes(&self) -> Option<u64> {
        self.block_count.checked_mul(self.block_size as u64)
    }
}

/// Interface implemented by storage drivers.
///
/// Implementations must synchronize their controller state internally because
/// one device may be called by tasks on different CPUs.
pub trait BlockDevice: Send + Sync {
    fn name(&self) -> &str;
    fn block_size(&self) -> u32;
    fn block_count(&self) -> u64;

    fn read_only(&self) -> bool {
        false
    }

    fn read_blocks(&self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError>;

    fn write_blocks(&self, _lba: u64, _data: &[u8]) -> Result<(), BlockError> {
        Err(BlockError::ReadOnly)
    }

    fn flush(&self) -> Result<(), BlockError> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct RegisteredBlockDevice {
    info: BlockDeviceInfo,
    device: &'static dyn BlockDevice,
}

/// Copyable, validated reference to a registered block device.
///
/// Filesystems keep this handle after discovery so normal reads do not need
/// to hold the global registry lock while a controller performs I/O.
#[derive(Clone, Copy)]
pub struct BlockDeviceHandle {
    info: BlockDeviceInfo,
    device: &'static dyn BlockDevice,
}

impl BlockDeviceHandle {
    pub const fn info(&self) -> BlockDeviceInfo {
        self.info
    }

    pub fn read(&self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        validate_transfer(self.info, lba, buffer.len())?;
        self.device.read_blocks(lba, buffer)
    }

    pub fn write(&self, lba: u64, data: &[u8]) -> Result<(), BlockError> {
        if self.info.read_only {
            return Err(BlockError::ReadOnly);
        }
        validate_transfer(self.info, lba, data.len())?;
        self.device.write_blocks(lba, data)
    }

    pub fn flush(&self) -> Result<(), BlockError> {
        self.device.flush()
    }
}

/// Fixed-capacity registry and checked dispatch surface.
pub struct BlockRegistry {
    devices: [Option<RegisteredBlockDevice>; MAX_BLOCK_DEVICES],
    count: usize,
    next_id: BlockDeviceId,
}

impl BlockRegistry {
    pub const fn new() -> Self {
        Self {
            devices: [None; MAX_BLOCK_DEVICES],
            count: 0,
            next_id: 1,
        }
    }

    pub const fn len(&self) -> usize {
        self.count
    }

    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn register(
        &mut self,
        device: &'static dyn BlockDevice,
    ) -> Result<BlockDeviceId, BlockError> {
        if self.count == MAX_BLOCK_DEVICES {
            return Err(BlockError::RegistryFull);
        }
        let block_size = device.block_size();
        if device.name().is_empty()
            || !(MIN_BLOCK_SIZE..=MAX_BLOCK_SIZE).contains(&block_size)
            || !block_size.is_power_of_two()
            || device.block_count() == 0
        {
            return Err(BlockError::InvalidGeometry);
        }

        let candidate = BlockDeviceInfo::from_device(self.next_id, device);
        if self
            .devices
            .iter()
            .flatten()
            .any(|registered| registered.info.name() == candidate.name())
        {
            return Err(BlockError::DuplicateName);
        }

        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(BlockError::RegistryFull)?;
        self.devices[self.count] = Some(RegisteredBlockDevice {
            info: candidate,
            device,
        });
        self.count += 1;
        Ok(id)
    }

    pub fn info(&self, id: BlockDeviceId) -> Option<BlockDeviceInfo> {
        self.find(id).map(|registered| registered.info)
    }

    /// Obtain a stable device handle without extending the registry lock over
    /// controller I/O. Registered drivers have static kernel lifetime.
    pub fn handle(&self, id: BlockDeviceId) -> Option<BlockDeviceHandle> {
        self.find(id).map(|registered| BlockDeviceHandle {
            info: registered.info,
            device: registered.device,
        })
    }

    pub fn snapshot(&self) -> [Option<BlockDeviceInfo>; MAX_BLOCK_DEVICES] {
        let mut snapshot = [None; MAX_BLOCK_DEVICES];
        for (index, registered) in self.devices[..self.count].iter().flatten().enumerate() {
            snapshot[index] = Some(registered.info);
        }
        snapshot
    }

    pub fn read(&self, id: BlockDeviceId, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
        self.handle(id)
            .ok_or(BlockError::InvalidDevice)?
            .read(lba, buffer)
    }

    pub fn write(&self, id: BlockDeviceId, lba: u64, data: &[u8]) -> Result<(), BlockError> {
        self.handle(id)
            .ok_or(BlockError::InvalidDevice)?
            .write(lba, data)
    }

    pub fn flush(&self, id: BlockDeviceId) -> Result<(), BlockError> {
        self.handle(id).ok_or(BlockError::InvalidDevice)?.flush()
    }

    fn find(&self, id: BlockDeviceId) -> Option<&RegisteredBlockDevice> {
        self.devices[..self.count]
            .iter()
            .flatten()
            .find(|registered| registered.info.id == id)
    }
}

impl Default for BlockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_transfer(info: BlockDeviceInfo, lba: u64, buffer_len: usize) -> Result<(), BlockError> {
    let block_size = info.block_size as usize;
    if buffer_len == 0 || !buffer_len.is_multiple_of(block_size) {
        return Err(BlockError::InvalidBuffer);
    }
    let transfer_blocks = (buffer_len / block_size) as u64;
    let end = lba
        .checked_add(transfer_blocks)
        .ok_or(BlockError::OutOfRange)?;
    if end > info.block_count {
        return Err(BlockError::OutOfRange);
    }
    Ok(())
}

pub static BLOCK_REGISTRY: Mutex<BlockRegistry> = Mutex::new(BlockRegistry::new());

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;

    struct MemoryBlockDevice {
        name: &'static str,
        storage: Mutex<Vec<u8>>,
        read_only: bool,
    }

    impl MemoryBlockDevice {
        fn new(name: &'static str, blocks: usize, read_only: bool) -> Self {
            Self {
                name,
                storage: Mutex::new(vec![0; blocks * MIN_BLOCK_SIZE as usize]),
                read_only,
            }
        }
    }

    impl BlockDevice for MemoryBlockDevice {
        fn name(&self) -> &str {
            self.name
        }

        fn block_size(&self) -> u32 {
            MIN_BLOCK_SIZE
        }

        fn block_count(&self) -> u64 {
            (self.storage.lock().len() / MIN_BLOCK_SIZE as usize) as u64
        }

        fn read_only(&self) -> bool {
            self.read_only
        }

        fn read_blocks(&self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
            let start = lba as usize * MIN_BLOCK_SIZE as usize;
            let storage = self.storage.lock();
            buffer.copy_from_slice(&storage[start..start + buffer.len()]);
            Ok(())
        }

        fn write_blocks(&self, lba: u64, data: &[u8]) -> Result<(), BlockError> {
            if self.read_only {
                return Err(BlockError::ReadOnly);
            }
            let start = lba as usize * MIN_BLOCK_SIZE as usize;
            let mut storage = self.storage.lock();
            storage[start..start + data.len()].copy_from_slice(data);
            Ok(())
        }
    }

    #[test]
    fn registers_lists_and_routes_block_io() {
        let device = Box::leak(Box::new(MemoryBlockDevice::new("mem0", 4, false)));
        let mut registry = BlockRegistry::new();
        let id = registry.register(device).unwrap();
        let data = [0xA5; MIN_BLOCK_SIZE as usize];
        let mut output = [0; MIN_BLOCK_SIZE as usize];

        registry.write(id, 2, &data).unwrap();
        registry.read(id, 2, &mut output).unwrap();

        assert_eq!(data, output);
        assert_eq!(registry.len(), 1);
        assert_eq!(registry.info(id).unwrap().name(), "mem0");
        assert_eq!(registry.info(id).unwrap().capacity_bytes(), Some(2048));
        assert_eq!(registry.snapshot()[0].unwrap().id, id);
    }

    #[test]
    fn rejects_unaligned_and_out_of_range_transfers() {
        let device = Box::leak(Box::new(MemoryBlockDevice::new("mem1", 2, false)));
        let mut registry = BlockRegistry::new();
        let id = registry.register(device).unwrap();
        let mut short = [0; 511];
        let mut sector = [0; MIN_BLOCK_SIZE as usize];

        assert_eq!(
            registry.read(id, 0, &mut short),
            Err(BlockError::InvalidBuffer)
        );
        assert_eq!(
            registry.read(id, 2, &mut sector),
            Err(BlockError::OutOfRange)
        );
        assert_eq!(
            registry.read(99, 0, &mut sector),
            Err(BlockError::InvalidDevice)
        );
    }

    #[test]
    fn enforces_read_only_and_unique_names() {
        let first = Box::leak(Box::new(MemoryBlockDevice::new("disk0", 1, true)));
        let duplicate = Box::leak(Box::new(MemoryBlockDevice::new("disk0", 1, false)));
        let mut registry = BlockRegistry::new();
        let id = registry.register(first).unwrap();
        let data = [0; MIN_BLOCK_SIZE as usize];

        assert_eq!(registry.write(id, 0, &data), Err(BlockError::ReadOnly));
        assert_eq!(registry.register(duplicate), Err(BlockError::DuplicateName));
    }
}

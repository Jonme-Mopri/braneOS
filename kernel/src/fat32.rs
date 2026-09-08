//! Read-only FAT32 filesystem backed by the kernel block layer.
//!
//! The implementation accepts both partitioned MBR disks and FAT32
//! superfloppies. It validates the BIOS parameter block before exposing the
//! volume, follows FAT chains, resolves case-insensitive 8.3 paths and serves
//! directory/file operations through the VFS.

#![allow(dead_code)]

use crate::block::{BlockDeviceHandle, BlockDeviceId, BLOCK_REGISTRY, MIN_BLOCK_SIZE};
use crate::vfs::{DirEntry, FileSystem, NodeInfo, NodeType, VfsError, MAX_NAME};

/// FAT32 logical sectors are currently required to match the 512-byte block
/// size exposed by the first storage backends.
pub const SECTOR_SIZE: usize = 512;

const MBR_PARTITION_TABLE_OFFSET: usize = 446;
const MBR_PARTITION_ENTRY_SIZE: usize = 16;
const FAT32_MIN_CLUSTERS: u32 = 65_525;
const FAT32_ENTRY_MASK: u32 = 0x0FFF_FFFF;
const FAT32_BAD_CLUSTER: u32 = 0x0FFF_FFF7;
const FAT32_END_OF_CHAIN: u32 = 0x0FFF_FFF8;

/// Failures that can occur while discovering or traversing a FAT32 volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fat32Error {
    DeviceNotFound,
    Io,
    NoFat32Volume,
    UnsupportedGeometry,
    CorruptFilesystem,
}

/// A parsed Master Boot Record (MBR) partition entry.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct PartitionEntry {
    pub status: u8,
    pub start_chs: [u8; 3],
    pub partition_type: u8,
    pub end_chs: [u8; 3],
    pub start_lba: u32,
    pub sector_count: u32,
}

impl PartitionEntry {
    /// Parse a partition entry from a 16-byte slice of the MBR.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < MBR_PARTITION_ENTRY_SIZE || data[4] == 0x00 {
            return None;
        }
        Some(Self {
            status: data[0],
            start_chs: [data[1], data[2], data[3]],
            partition_type: data[4],
            end_chs: [data[5], data[6], data[7]],
            start_lba: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            sector_count: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
        })
    }

    const fn is_fat32(self) -> bool {
        matches!(self.partition_type, 0x0B | 0x0C | 0x1B | 0x1C)
    }
}

/// A parsed FAT32 BIOS parameter block and extended boot record.
#[derive(Debug, Clone)]
pub struct Fat32BootSector {
    pub bytes_per_sector: u16,
    pub sectors_per_cluster: u8,
    pub reserved_sectors: u16,
    pub fat_count: u8,
    pub root_dir_entries: u16,
    pub total_sectors_16: u16,
    pub media_descriptor: u8,
    pub sectors_per_fat_16: u16,
    pub sectors_per_track: u16,
    pub heads: u16,
    pub hidden_sectors: u32,
    pub total_sectors_32: u32,
    pub sectors_per_fat_32: u32,
    pub ext_flags: u16,
    pub fs_version: u16,
    pub root_cluster: u32,
    pub fs_info_sector: u16,
    pub backup_boot_sector: u16,
    pub drive_number: u8,
    pub boot_signature: u8,
    pub volume_id: u32,
    pub volume_label: [u8; 11],
    pub fs_type_label: [u8; 8],
}

impl Fat32BootSector {
    /// Parse a FAT32 boot sector from a 512-byte sector slice.
    ///
    /// Structural FAT32 validation is intentionally performed by `mount` so
    /// this parser remains total over arbitrary mutation-fuzz input.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < SECTOR_SIZE || data[510] != 0x55 || data[511] != 0xAA {
            return None;
        }

        let mut volume_label = [0u8; 11];
        volume_label.copy_from_slice(&data[71..82]);
        let mut fs_type_label = [0u8; 8];
        fs_type_label.copy_from_slice(&data[82..90]);

        Some(Self {
            bytes_per_sector: u16::from_le_bytes([data[11], data[12]]),
            sectors_per_cluster: data[13],
            reserved_sectors: u16::from_le_bytes([data[14], data[15]]),
            fat_count: data[16],
            root_dir_entries: u16::from_le_bytes([data[17], data[18]]),
            total_sectors_16: u16::from_le_bytes([data[19], data[20]]),
            media_descriptor: data[21],
            sectors_per_fat_16: u16::from_le_bytes([data[22], data[23]]),
            sectors_per_track: u16::from_le_bytes([data[24], data[25]]),
            heads: u16::from_le_bytes([data[26], data[27]]),
            hidden_sectors: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
            total_sectors_32: u32::from_le_bytes([data[32], data[33], data[34], data[35]]),
            sectors_per_fat_32: u32::from_le_bytes([data[36], data[37], data[38], data[39]]),
            ext_flags: u16::from_le_bytes([data[40], data[41]]),
            fs_version: u16::from_le_bytes([data[42], data[43]]),
            root_cluster: u32::from_le_bytes([data[44], data[45], data[46], data[47]]),
            fs_info_sector: u16::from_le_bytes([data[48], data[49]]),
            backup_boot_sector: u16::from_le_bytes([data[50], data[51]]),
            drive_number: data[64],
            boot_signature: data[66],
            volume_id: u32::from_le_bytes([data[67], data[68], data[69], data[70]]),
            volume_label,
            fs_type_label,
        })
    }

    const fn total_sectors(&self) -> u32 {
        if self.total_sectors_16 != 0 {
            self.total_sectors_16 as u32
        } else {
            self.total_sectors_32
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Fat32Geometry {
    partition_lba: u64,
    fat_lba: u64,
    data_lba: u64,
    sectors_per_cluster: u32,
    cluster_count: u32,
    root_cluster: u32,
}

impl Fat32Geometry {
    fn parse(
        boot: &Fat32BootSector,
        partition_lba: u64,
        partition_sectors: Option<u64>,
        device_blocks: u64,
    ) -> Result<Self, Fat32Error> {
        if boot.bytes_per_sector != MIN_BLOCK_SIZE as u16
            || boot.sectors_per_cluster == 0
            || !boot.sectors_per_cluster.is_power_of_two()
            || boot.sectors_per_cluster > 128
            || boot.reserved_sectors == 0
            || boot.fat_count == 0
            || boot.root_dir_entries != 0
            || boot.sectors_per_fat_16 != 0
            || boot.sectors_per_fat_32 == 0
            || boot.fs_version != 0
            || boot.root_cluster < 2
        {
            return Err(Fat32Error::UnsupportedGeometry);
        }

        let total_sectors = u64::from(boot.total_sectors());
        let fat_sectors = u64::from(boot.fat_count)
            .checked_mul(u64::from(boot.sectors_per_fat_32))
            .ok_or(Fat32Error::CorruptFilesystem)?;
        let first_data_sector = u64::from(boot.reserved_sectors)
            .checked_add(fat_sectors)
            .ok_or(Fat32Error::CorruptFilesystem)?;
        if total_sectors <= first_data_sector
            || partition_sectors.is_some_and(|sectors| total_sectors > sectors)
            || partition_lba
                .checked_add(total_sectors)
                .is_none_or(|end| end > device_blocks)
        {
            return Err(Fat32Error::CorruptFilesystem);
        }

        let cluster_count =
            ((total_sectors - first_data_sector) / u64::from(boot.sectors_per_cluster)) as u32;
        if cluster_count < FAT32_MIN_CLUSTERS || boot.root_cluster > cluster_count.saturating_add(1)
        {
            return Err(Fat32Error::UnsupportedGeometry);
        }

        let fat_entries = u64::from(boot.sectors_per_fat_32) * (SECTOR_SIZE as u64 / 4);
        if fat_entries < u64::from(cluster_count) + 2 {
            return Err(Fat32Error::CorruptFilesystem);
        }

        Ok(Self {
            partition_lba,
            fat_lba: partition_lba + u64::from(boot.reserved_sectors),
            data_lba: partition_lba + first_data_sector,
            sectors_per_cluster: u32::from(boot.sectors_per_cluster),
            cluster_count,
            root_cluster: boot.root_cluster,
        })
    }

    fn cluster_lba(self, cluster: u32) -> Result<u64, Fat32Error> {
        if cluster < 2 || cluster > self.cluster_count.saturating_add(1) {
            return Err(Fat32Error::CorruptFilesystem);
        }
        Ok(self.data_lba + u64::from(cluster - 2) * u64::from(self.sectors_per_cluster))
    }
}

#[derive(Debug, Clone, Copy)]
struct DirectoryRecord {
    name: [u8; MAX_NAME],
    name_len: usize,
    node_type: NodeType,
    first_cluster: u32,
    size: u32,
}

impl DirectoryRecord {
    fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 32 || matches!(data[0], 0x00 | 0xE5) {
            return None;
        }
        let attributes = data[11];
        if attributes & 0x0F == 0x0F || attributes & 0x08 != 0 {
            return None;
        }

        let mut name = [0u8; MAX_NAME];
        let mut name_len = copy_short_name(&data[..11], &mut name)?;
        if attributes & 0x10 == 0 && data[8..11].iter().any(|byte| *byte != b' ') {
            name[name_len] = b'.';
            name_len += 1;
            for byte in data[8..11].iter().copied().take_while(|byte| *byte != b' ') {
                name[name_len] = sanitise_name_byte(byte);
                name_len += 1;
            }
        }

        let cluster_high = u16::from_le_bytes([data[20], data[21]]) as u32;
        let cluster_low = u16::from_le_bytes([data[26], data[27]]) as u32;
        Some(Self {
            name,
            name_len,
            node_type: if attributes & 0x10 != 0 {
                NodeType::Directory
            } else {
                NodeType::File
            },
            first_cluster: (cluster_high << 16) | cluster_low,
            size: u32::from_le_bytes([data[28], data[29], data[30], data[31]]),
        })
    }

    fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }
}

fn sanitise_name_byte(byte: u8) -> u8 {
    if byte.is_ascii() && !byte.is_ascii_control() {
        byte
    } else {
        b'_'
    }
}

fn copy_short_name(source: &[u8], destination: &mut [u8; MAX_NAME]) -> Option<usize> {
    let base = source.get(..8)?;
    let base_len = base.iter().position(|byte| *byte == b' ').unwrap_or(8);
    if base_len == 0 {
        return None;
    }
    for (target, byte) in destination[..base_len].iter_mut().zip(base.iter().copied()) {
        *target = sanitise_name_byte(if byte == 0x05 { 0xE5 } else { byte });
    }
    Some(base_len)
}

fn names_equal(left: &str, right: &str) -> bool {
    left.len() == right.len()
        && left
            .bytes()
            .zip(right.bytes())
            .all(|(a, b)| a.eq_ignore_ascii_case(&b))
}

/// Mounted, read-only FAT32 filesystem.
pub struct Fat32Fs {
    device: BlockDeviceHandle,
    boot_sector: Fat32BootSector,
    geometry: Fat32Geometry,
}

impl Fat32Fs {
    /// Discover and mount a FAT32 volume from a registered block device.
    pub fn mount(device_id: BlockDeviceId) -> Result<Self, Fat32Error> {
        let device = BLOCK_REGISTRY
            .lock()
            .handle(device_id)
            .ok_or(Fat32Error::DeviceNotFound)?;
        Self::from_device(device)
    }

    fn from_device(device: BlockDeviceHandle) -> Result<Self, Fat32Error> {
        let info = device.info();
        if info.block_size != SECTOR_SIZE as u32 {
            return Err(Fat32Error::UnsupportedGeometry);
        }

        let mut sector = [0u8; SECTOR_SIZE];
        device.read(0, &mut sector).map_err(|_| Fat32Error::Io)?;

        if let Some(boot_sector) = Fat32BootSector::parse(&sector) {
            if let Ok(geometry) = Fat32Geometry::parse(&boot_sector, 0, None, info.block_count) {
                return Ok(Self {
                    device,
                    boot_sector,
                    geometry,
                });
            }
        }

        for index in 0..4 {
            let offset = MBR_PARTITION_TABLE_OFFSET + index * MBR_PARTITION_ENTRY_SIZE;
            let Some(partition) = PartitionEntry::parse(&sector[offset..offset + 16]) else {
                continue;
            };
            if !partition.is_fat32() || partition.start_lba == 0 || partition.sector_count == 0 {
                continue;
            }
            let start_lba = u64::from(partition.start_lba);
            if start_lba >= info.block_count {
                continue;
            }
            let mut boot_data = [0u8; SECTOR_SIZE];
            device
                .read(start_lba, &mut boot_data)
                .map_err(|_| Fat32Error::Io)?;
            let Some(boot_sector) = Fat32BootSector::parse(&boot_data) else {
                continue;
            };
            let Ok(geometry) = Fat32Geometry::parse(
                &boot_sector,
                start_lba,
                Some(u64::from(partition.sector_count)),
                info.block_count,
            ) else {
                continue;
            };
            return Ok(Self {
                device,
                boot_sector,
                geometry,
            });
        }

        Err(Fat32Error::NoFat32Volume)
    }

    pub fn device_id(&self) -> BlockDeviceId {
        self.device.info().id
    }

    pub fn partition_lba(&self) -> u64 {
        self.geometry.partition_lba
    }

    pub fn volume_label(&self) -> &str {
        core::str::from_utf8(&self.boot_sector.volume_label)
            .unwrap_or("NO NAME")
            .trim()
    }

    pub fn root_cluster(&self) -> u32 {
        self.geometry.root_cluster
    }

    fn read_sector(&self, lba: u64, sector: &mut [u8; SECTOR_SIZE]) -> Result<(), Fat32Error> {
        self.device.read(lba, sector).map_err(|_| Fat32Error::Io)
    }

    fn next_cluster(&self, cluster: u32) -> Result<Option<u32>, Fat32Error> {
        if cluster < 2 || cluster > self.geometry.cluster_count.saturating_add(1) {
            return Err(Fat32Error::CorruptFilesystem);
        }
        let fat_offset = u64::from(cluster) * 4;
        let fat_sector = self.geometry.fat_lba + fat_offset / SECTOR_SIZE as u64;
        let entry_offset = (fat_offset % SECTOR_SIZE as u64) as usize;
        let mut sector = [0u8; SECTOR_SIZE];
        self.read_sector(fat_sector, &mut sector)?;
        let value = u32::from_le_bytes([
            sector[entry_offset],
            sector[entry_offset + 1],
            sector[entry_offset + 2],
            sector[entry_offset + 3],
        ]) & FAT32_ENTRY_MASK;

        if value >= FAT32_END_OF_CHAIN {
            Ok(None)
        } else if value < 2
            || value == FAT32_BAD_CLUSTER
            || value > self.geometry.cluster_count.saturating_add(1)
        {
            Err(Fat32Error::CorruptFilesystem)
        } else {
            Ok(Some(value))
        }
    }

    fn scan_directory<F>(&self, start_cluster: u32, mut visitor: F) -> Result<(), Fat32Error>
    where
        F: FnMut(DirectoryRecord) -> bool,
    {
        let mut cluster = start_cluster;
        let mut traversed = 0u32;
        loop {
            let first_lba = self.geometry.cluster_lba(cluster)?;
            for sector_index in 0..self.geometry.sectors_per_cluster {
                let mut sector = [0u8; SECTOR_SIZE];
                self.read_sector(first_lba + u64::from(sector_index), &mut sector)?;
                for raw in sector.chunks_exact(32) {
                    if raw[0] == 0x00 {
                        return Ok(());
                    }
                    if let Some(record) = DirectoryRecord::parse(raw) {
                        if !visitor(record) {
                            return Ok(());
                        }
                    }
                }
            }

            traversed += 1;
            if traversed > self.geometry.cluster_count {
                return Err(Fat32Error::CorruptFilesystem);
            }
            let Some(next) = self.next_cluster(cluster)? else {
                return Ok(());
            };
            cluster = next;
        }
    }

    fn find_entry(
        &self,
        directory_cluster: u32,
        component: &str,
    ) -> Result<Option<DirectoryRecord>, Fat32Error> {
        let mut result = None;
        self.scan_directory(directory_cluster, |record| {
            if names_equal(record.name(), component) {
                result = Some(record);
                false
            } else {
                true
            }
        })?;
        Ok(result)
    }

    fn resolve_path(&self, path: &str) -> Result<Option<DirectoryRecord>, VfsError> {
        if path.is_empty() || path == "/" {
            return Ok(None);
        }
        if !path.starts_with('/') {
            return Err(VfsError::InvalidPath);
        }

        let mut directory_cluster = self.geometry.root_cluster;
        let mut current = None;
        let mut components = path
            .split('/')
            .filter(|component| !component.is_empty())
            .peekable();
        while let Some(component) = components.next() {
            if component == "." || component == ".." || component.len() > 12 {
                return Err(VfsError::InvalidPath);
            }
            let record = self
                .find_entry(directory_cluster, component)
                .map_err(|_| VfsError::IoError)?
                .ok_or(VfsError::NotFound)?;
            if components.peek().is_some() {
                if record.node_type != NodeType::Directory {
                    return Err(VfsError::NotADirectory);
                }
                directory_cluster = record.first_cluster;
            }
            current = Some(record);
        }
        Ok(current)
    }

    fn node_info(record: DirectoryRecord) -> NodeInfo {
        NodeInfo {
            name: record.name,
            name_len: record.name_len,
            node_type: record.node_type,
            size: record.size as usize,
            inode: u64::from(record.first_cluster),
        }
    }

    fn read_file(
        &self,
        record: DirectoryRecord,
        offset: usize,
        buffer: &mut [u8],
    ) -> Result<usize, VfsError> {
        if record.node_type != NodeType::File {
            return Err(VfsError::NotAFile);
        }
        let file_size = record.size as usize;
        if offset >= file_size || buffer.is_empty() {
            return Ok(0);
        }
        if record.first_cluster < 2 {
            return Err(VfsError::IoError);
        }

        let cluster_size = self.geometry.sectors_per_cluster as usize * SECTOR_SIZE;
        let mut cluster = record.first_cluster;
        let mut clusters_to_skip = offset / cluster_size;
        let mut traversed = 0u32;
        while clusters_to_skip > 0 {
            cluster = self
                .next_cluster(cluster)
                .map_err(|_| VfsError::IoError)?
                .ok_or(VfsError::IoError)?;
            clusters_to_skip -= 1;
            traversed += 1;
            if traversed > self.geometry.cluster_count {
                return Err(VfsError::IoError);
            }
        }

        let wanted = buffer.len().min(file_size - offset);
        let mut written = 0usize;
        let mut cluster_offset = offset % cluster_size;
        while written < wanted {
            let first_lba = self
                .geometry
                .cluster_lba(cluster)
                .map_err(|_| VfsError::IoError)?;
            let first_sector = cluster_offset / SECTOR_SIZE;
            let mut sector_offset = cluster_offset % SECTOR_SIZE;
            for sector_index in first_sector..self.geometry.sectors_per_cluster as usize {
                let mut sector = [0u8; SECTOR_SIZE];
                self.read_sector(first_lba + sector_index as u64, &mut sector)
                    .map_err(|_| VfsError::IoError)?;
                let available = SECTOR_SIZE - sector_offset;
                let count = available.min(wanted - written);
                buffer[written..written + count]
                    .copy_from_slice(&sector[sector_offset..sector_offset + count]);
                written += count;
                sector_offset = 0;
                if written == wanted {
                    return Ok(written);
                }
            }

            traversed += 1;
            if traversed > self.geometry.cluster_count {
                return Err(VfsError::IoError);
            }
            cluster = self
                .next_cluster(cluster)
                .map_err(|_| VfsError::IoError)?
                .ok_or(VfsError::IoError)?;
            cluster_offset = 0;
        }
        Ok(written)
    }
}

impl FileSystem for Fat32Fs {
    fn stat(&self, path: &str) -> Result<NodeInfo, VfsError> {
        if let Some(record) = self.resolve_path(path)? {
            return Ok(Self::node_info(record));
        }
        let mut name = [0u8; MAX_NAME];
        name[0] = b'/';
        Ok(NodeInfo {
            name,
            name_len: 1,
            node_type: NodeType::Directory,
            size: 0,
            inode: u64::from(self.geometry.root_cluster),
        })
    }

    fn read(&self, path: &str, offset: usize, buffer: &mut [u8]) -> Result<usize, VfsError> {
        let record = self.resolve_path(path)?.ok_or(VfsError::NotAFile)?;
        self.read_file(record, offset, buffer)
    }

    fn write(&mut self, _path: &str, _offset: usize, _data: &[u8]) -> Result<usize, VfsError> {
        Err(VfsError::IoError)
    }

    fn create(&mut self, _path: &str, _node_type: NodeType) -> Result<(), VfsError> {
        Err(VfsError::IoError)
    }

    fn readdir(&self, path: &str, entries: &mut [DirEntry]) -> Result<usize, VfsError> {
        let cluster = match self.resolve_path(path)? {
            Some(record) if record.node_type == NodeType::Directory => record.first_cluster,
            Some(_) => return Err(VfsError::NotADirectory),
            None => self.geometry.root_cluster,
        };
        let mut count = 0;
        self.scan_directory(cluster, |record| {
            if count == entries.len() {
                return false;
            }
            entries[count] = DirEntry {
                name: record.name,
                name_len: record.name_len,
                node_type: record.node_type,
            };
            count += 1;
            true
        })
        .map_err(|_| VfsError::IoError)?;
        Ok(count)
    }

    fn remove(&mut self, _path: &str) -> Result<(), VfsError> {
        Err(VfsError::IoError)
    }

    fn fs_name(&self) -> &str {
        "fat32"
    }
}

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use crate::block::{BlockDevice, BlockError, BlockRegistry};
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::vec::Vec;
    use spin::Mutex;

    const VOLUME_SECTORS: u32 = 131_072;
    const RESERVED_SECTORS: u32 = 32;
    const FAT_SECTORS: u32 = 1_024;
    const DATA_SECTOR: u32 = RESERVED_SECTORS + FAT_SECTORS;
    const README_CONTENT: &[u8] = b"Brane OS FAT32 block path ready.\n";
    const HELLO_CONTENT: &[u8] = b"hello from a nested FAT32 directory\n";

    struct SparseBlockDevice {
        sectors: Mutex<BTreeMap<u64, [u8; SECTOR_SIZE]>>,
        block_count: u64,
    }

    impl SparseBlockDevice {
        fn volume(partition_lba: u32) -> Self {
            let device = Self {
                sectors: Mutex::new(BTreeMap::new()),
                block_count: u64::from(partition_lba) + u64::from(VOLUME_SECTORS),
            };
            if partition_lba != 0 {
                let mut mbr = [0u8; SECTOR_SIZE];
                mbr[510] = 0x55;
                mbr[511] = 0xAA;
                let entry = &mut mbr[446..462];
                entry[0] = 0x80;
                entry[4] = 0x0C;
                entry[8..12].copy_from_slice(&partition_lba.to_le_bytes());
                entry[12..16].copy_from_slice(&VOLUME_SECTORS.to_le_bytes());
                device.insert(0, mbr);
            }

            let mut boot = [0u8; SECTOR_SIZE];
            boot[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
            boot[3..11].copy_from_slice(b"BRANEOS ");
            boot[11..13].copy_from_slice(&(SECTOR_SIZE as u16).to_le_bytes());
            boot[13] = 1;
            boot[14..16].copy_from_slice(&(RESERVED_SECTORS as u16).to_le_bytes());
            boot[16] = 1;
            boot[21] = 0xF8;
            boot[32..36].copy_from_slice(&VOLUME_SECTORS.to_le_bytes());
            boot[36..40].copy_from_slice(&FAT_SECTORS.to_le_bytes());
            boot[44..48].copy_from_slice(&2u32.to_le_bytes());
            boot[48..50].copy_from_slice(&1u16.to_le_bytes());
            boot[50..52].copy_from_slice(&6u16.to_le_bytes());
            boot[64] = 0x80;
            boot[66] = 0x29;
            boot[67..71].copy_from_slice(&0xB4A9_3201u32.to_le_bytes());
            boot[71..82].copy_from_slice(b"BRANEOS    ");
            boot[82..90].copy_from_slice(b"FAT32   ");
            boot[510] = 0x55;
            boot[511] = 0xAA;
            device.insert(u64::from(partition_lba), boot);

            let mut fat = [0u8; SECTOR_SIZE];
            write_fat_entry(&mut fat, 0, 0x0FFF_FFF8);
            write_fat_entry(&mut fat, 1, 0x0FFF_FFFF);
            write_fat_entry(&mut fat, 2, 0x0FFF_FFFF);
            write_fat_entry(&mut fat, 3, 4);
            write_fat_entry(&mut fat, 4, 0x0FFF_FFFF);
            write_fat_entry(&mut fat, 5, 0x0FFF_FFFF);
            write_fat_entry(&mut fat, 6, 0x0FFF_FFFF);
            write_fat_entry(&mut fat, 7, 8);
            write_fat_entry(&mut fat, 8, 0x0FFF_FFFF);
            device.insert(u64::from(partition_lba + RESERVED_SECTORS), fat);

            let mut root = [0u8; SECTOR_SIZE];
            write_directory_entry(
                &mut root,
                0,
                b"README  TXT",
                0x20,
                3,
                README_CONTENT.len() as u32,
            );
            write_directory_entry(&mut root, 1, b"DOCS       ", 0x10, 5, 0);
            write_directory_entry(&mut root, 2, b"CHAIN   BIN", 0x20, 7, 700);
            device.insert(u64::from(partition_lba + DATA_SECTOR), root);

            let mut readme = [0u8; SECTOR_SIZE];
            readme[..README_CONTENT.len()].copy_from_slice(README_CONTENT);
            device.insert(u64::from(partition_lba + DATA_SECTOR + 1), readme);

            let mut docs = [0u8; SECTOR_SIZE];
            write_directory_entry(
                &mut docs,
                0,
                b"HELLO   TXT",
                0x20,
                6,
                HELLO_CONTENT.len() as u32,
            );
            device.insert(u64::from(partition_lba + DATA_SECTOR + 3), docs);

            let mut hello = [0u8; SECTOR_SIZE];
            hello[..HELLO_CONTENT.len()].copy_from_slice(HELLO_CONTENT);
            device.insert(u64::from(partition_lba + DATA_SECTOR + 4), hello);

            device.insert(
                u64::from(partition_lba + DATA_SECTOR + 5),
                [b'A'; SECTOR_SIZE],
            );
            let mut chain_tail = [0u8; SECTOR_SIZE];
            chain_tail[..188].fill(b'B');
            device.insert(u64::from(partition_lba + DATA_SECTOR + 6), chain_tail);
            device
        }

        fn insert(&self, lba: u64, sector: [u8; SECTOR_SIZE]) {
            self.sectors.lock().insert(lba, sector);
        }
    }

    impl BlockDevice for SparseBlockDevice {
        fn name(&self) -> &str {
            "fat-test"
        }

        fn block_size(&self) -> u32 {
            SECTOR_SIZE as u32
        }

        fn block_count(&self) -> u64 {
            self.block_count
        }

        fn read_only(&self) -> bool {
            true
        }

        fn read_blocks(&self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError> {
            for (index, destination) in buffer.chunks_exact_mut(SECTOR_SIZE).enumerate() {
                let sector_lba = lba + index as u64;
                if let Some(source) = self.sectors.lock().get(&sector_lba) {
                    destination.copy_from_slice(source);
                } else {
                    destination.fill(0);
                }
            }
            Ok(())
        }
    }

    fn write_fat_entry(sector: &mut [u8; SECTOR_SIZE], cluster: usize, value: u32) {
        sector[cluster * 4..cluster * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_directory_entry(
        sector: &mut [u8; SECTOR_SIZE],
        index: usize,
        name: &[u8; 11],
        attributes: u8,
        cluster: u32,
        size: u32,
    ) {
        let entry = &mut sector[index * 32..index * 32 + 32];
        entry[..11].copy_from_slice(name);
        entry[11] = attributes;
        entry[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        entry[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        entry[28..32].copy_from_slice(&size.to_le_bytes());
    }

    fn mounted_volume(partition_lba: u32) -> Fat32Fs {
        let device = Box::leak(Box::new(SparseBlockDevice::volume(partition_lba)));
        let mut registry = BlockRegistry::new();
        let id = registry.register(device).unwrap();
        Fat32Fs::from_device(registry.handle(id).unwrap()).unwrap()
    }

    #[test]
    fn mounts_superfloppy_and_lists_root_directory() {
        let fs = mounted_volume(0);
        assert_eq!(fs.partition_lba(), 0);
        assert_eq!(fs.volume_label(), "BRANEOS");

        let mut entries = Vec::from(
            [DirEntry {
                name: [0; MAX_NAME],
                name_len: 0,
                node_type: NodeType::File,
            }; 4],
        );
        let count = fs.readdir("/", &mut entries).unwrap();
        assert_eq!(count, 3);
        assert_eq!(entries[0].name_str(), "README.TXT");
        assert_eq!(entries[1].name_str(), "DOCS");
        assert_eq!(entries[2].name_str(), "CHAIN.BIN");
    }

    #[test]
    fn mounts_mbr_partition_and_reads_nested_file_case_insensitively() {
        let fs = mounted_volume(2_048);
        assert_eq!(fs.partition_lba(), 2_048);
        let info = fs.stat("/docs/hello.txt").unwrap();
        assert_eq!(info.node_type, NodeType::File);
        assert_eq!(info.size, HELLO_CONTENT.len());

        let mut output = [0u8; 64];
        let count = fs.read("/DoCs/HeLLo.TxT", 0, &mut output).unwrap();
        assert_eq!(&output[..count], HELLO_CONTENT);
    }

    #[test]
    fn reads_file_offsets_and_reports_vfs_type_errors() {
        let fs = mounted_volume(0);
        let mut output = [0u8; 12];
        let count = fs.read("/README.TXT", 9, &mut output).unwrap();
        assert_eq!(&output[..count], &README_CONTENT[9..21]);

        let mut chained = [0u8; 32];
        assert_eq!(fs.read("/CHAIN.BIN", 500, &mut chained), Ok(32));
        assert_eq!(&chained[..12], &[b'A'; 12]);
        assert_eq!(&chained[12..], &[b'B'; 20]);
        assert_eq!(fs.read("/DOCS", 0, &mut output), Err(VfsError::NotAFile));
        assert_eq!(
            fs.readdir("/README.TXT", &mut []),
            Err(VfsError::NotADirectory)
        );
        assert!(matches!(fs.stat("/MISSING.TXT"), Err(VfsError::NotFound)));
    }

    #[test]
    fn rejects_non_fat32_geometry() {
        let device = Box::leak(Box::new(SparseBlockDevice {
            sectors: Mutex::new(BTreeMap::new()),
            block_count: 16,
        }));
        let mut registry = BlockRegistry::new();
        let id = registry.register(device).unwrap();
        assert!(matches!(
            Fat32Fs::from_device(registry.handle(id).unwrap()),
            Err(Fat32Error::NoFat32Volume)
        ));
    }
}

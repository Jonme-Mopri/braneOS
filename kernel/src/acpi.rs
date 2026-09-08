// ============================================================
// Brane OS Kernel — ACPI Power Management
// ============================================================
//
// Parses the RSDP, XSDT/RSDT, FADT, FACS and DSDT sleep packages.
// Provides ACPI shutdown, reboot and S3 suspend/resume on legacy IA-PC
// platforms. S3 resume uses the FACS FirmwareWakingVector and a reserved
// low-memory trampoline that restores long mode and the saved kernel stack.
// ============================================================

use core::sync::atomic::{AtomicBool, Ordering};

use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::structures::paging::OffsetPageTable;
#[cfg(target_os = "none")]
use x86_64::structures::paging::{Page, PageTableFlags, PhysFrame, Translate};
#[cfg(target_os = "none")]
use x86_64::{PhysAddr, VirtAddr};

use crate::memory::frame_allocator::BitmapFrameAllocator;
#[cfg(target_os = "none")]
use crate::memory::paging;

const DESCRIPTION_HEADER_LEN: usize = core::mem::size_of::<DescriptionHeader>();
const FADT_LEGACY_PM1_CNT_END: usize = 72;
const FADT_RESET_VALUE_END: usize = 129;
const FADT_X_DSDT_END: usize = 148;
const FADT_X_PM1A_EVT_END: usize = 160;
const FADT_X_PM1B_EVT_END: usize = 172;
const FADT_X_PM1A_CNT_END: usize = 184;
const FADT_X_PM1B_CNT_END: usize = 196;

const FACS_MIN_LEN: usize = 24;
const MADT_MAX_LEN: usize = 4096;
const MCFG_MIN_LEN: usize = 44;
const MCFG_MAX_LEN: usize = 4096;
const ACPI_MAX_TABLE_LEN: usize = 1024 * 1024;
#[cfg(target_os = "none")]
const FACS_FIRMWARE_WAKING_VECTOR_OFFSET: usize = 12;
#[cfg(target_os = "none")]
const FACS_X_FIRMWARE_WAKING_VECTOR_OFFSET: usize = 24;
#[cfg(target_os = "none")]
const FACS_OSPM_FLAGS_OFFSET: usize = 36;

#[cfg(target_os = "none")]
const LEGACY_WAKE_LIMIT: u64 = 0x10_0000;
#[cfg(target_os = "none")]
const PAGE_SIZE: usize = 4096;
#[cfg(target_os = "none")]
const PM1_WAK_STS: u16 = 1 << 15;
#[cfg(target_os = "none")]
const PM1_SLP_TYP_MASK: u16 = 0b111 << 10;
const PM1_SLP_EN: u16 = 1 << 13;

/// Generic Address Structure (GAS) as defined by ACPI.
#[derive(Debug, Clone, Copy)]
#[repr(C, packed)]
pub struct GenericAddressStructure {
    pub address_space: u8,
    pub bit_width: u8,
    pub bit_offset: u8,
    pub access_size: u8,
    pub address: u64,
}

#[repr(C, packed)]
struct RsdpHeader {
    signature: [u8; 8],
    checksum: u8,
    oem_id: [u8; 6],
    revision: u8,
    rsdt_address: u32,
    length: u32,
    xsdt_address: u64,
    extended_checksum: u8,
    reserved: [u8; 3],
}

#[repr(C, packed)]
struct DescriptionHeader {
    signature: [u8; 4],
    length: u32,
    revision: u8,
    checksum: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    creator_id: [u8; 4],
    creator_revision: u32,
}

#[repr(C, packed)]
struct Fadt {
    header: DescriptionHeader,
    firmware_ctrl: u32,
    dsdt: u32,
    reserved: u8,
    preferred_pm_profile: u8,
    sci_int: u16,
    smi_cmd: u32,
    acpi_enable: u8,
    acpi_disable: u8,
    s4bios_req: u8,
    pstate_cnt: u8,
    pm1a_evt_blk: u32,
    pm1b_evt_blk: u32,
    pm1a_cnt_blk: u32,
    pm1b_cnt_blk: u32,
    pm2_cnt_blk: u32,
    pm_tmr_blk: u32,
    gpe0_blk: u32,
    gpe1_blk: u32,
    pm1_evt_len: u8,
    pm1_cnt_len: u8,
    pm2_cnt_len: u8,
    pm_tmr_len: u8,
    gpe0_len: u8,
    gpe1_len: u8,
    gpe1_base: u8,
    cst_cnt: u8,
    p_lvl2_lat: u16,
    p_lvl3_lat: u16,
    flush_size: u16,
    flush_stride: u16,
    duty_offset: u8,
    duty_width: u8,
    day_alrm: u8,
    mon_alrm: u8,
    century: u8,
    iapc_boot_arch: u16,
    reserved2: u8,
    flags: u32,
    reset_reg: GenericAddressStructure,
    reset_value: u8,
    arm_boot_arch: u16,
    fadt_minor_version: u8,
    x_firmware_ctrl: u64,
    x_dsdt: u64,
    x_pm1a_evt_blk: GenericAddressStructure,
    x_pm1b_evt_blk: GenericAddressStructure,
    x_pm1a_cnt_blk: GenericAddressStructure,
    x_pm1b_cnt_blk: GenericAddressStructure,
    x_pm2_cnt_blk: GenericAddressStructure,
    x_pm_tmr_blk: GenericAddressStructure,
    x_gpe0_blk: GenericAddressStructure,
    x_gpe1_blk: GenericAddressStructure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SleepType {
    typa: u8,
    typb: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AcpiInfo {
    pub initialized: bool,
    pub apic: Option<crate::apic::ApicTopology>,
    pub smp: Option<crate::smp::CpuBootPlan>,
    pub ecam: crate::pci::EcamRegions,
    pub s1_supported: bool,
    pub s3_supported: bool,
    pub s4_supported: bool,
    pub s5_supported: bool,
    pub wake_trampoline_ready: bool,
    pub wake_trampoline_phys: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendError {
    AcpiUnavailable,
    S3Unsupported,
    WakeVectorUnavailable,
    LowMemoryUnavailable,
    WakePageConflict,
    WakeTrampolineTooLarge,
    UnsupportedRegisterSpace,
}

#[derive(Debug, Default)]
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
struct AcpiState {
    initialized: bool,
    apic: Option<crate::apic::ApicTopology>,
    smp: Option<crate::smp::CpuBootPlan>,
    ecam: crate::pci::EcamRegions,
    pm1a_evt_blk: Option<u64>,
    pm1b_evt_blk: Option<u64>,
    pm1a_cnt_blk: Option<u64>,
    pm1b_cnt_blk: Option<u64>,
    evt_is_io: bool,
    cnt_is_io: bool,
    sleep_types: [Option<SleepType>; 6],
    reset_reg: Option<GenericAddressStructure>,
    reset_value: Option<u8>,
    facs_virt: Option<u64>,
    facs_len: usize,
    phys_mem_offset: Option<u64>,
    wake_trampoline_phys: Option<u64>,
    wake_context_virt: Option<u64>,
}

static ACPI_STATE: Mutex<AcpiState> = Mutex::new(AcpiState {
    initialized: false,
    apic: None,
    smp: None,
    ecam: crate::pci::EcamRegions::new(),
    pm1a_evt_blk: None,
    pm1b_evt_blk: None,
    pm1a_cnt_blk: None,
    pm1b_cnt_blk: None,
    evt_is_io: true,
    cnt_is_io: true,
    sleep_types: [None; 6],
    reset_reg: None,
    reset_value: None,
    facs_virt: None,
    facs_len: 0,
    phys_mem_offset: None,
    wake_trampoline_phys: None,
    wake_context_virt: None,
});

static RESUME_PENDING: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "none")]
#[repr(C)]
struct WakeContext {
    cr0: u64,
    cr3: u64,
    cr4: u64,
    efer: u64,
    rsp: u64,
    resume_rip: u64,
}

#[cfg(target_os = "none")]
const WAKE_CR0: usize = core::mem::offset_of!(WakeContext, cr0);
#[cfg(target_os = "none")]
const WAKE_CR3: usize = core::mem::offset_of!(WakeContext, cr3);
#[cfg(target_os = "none")]
const WAKE_CR4: usize = core::mem::offset_of!(WakeContext, cr4);
#[cfg(target_os = "none")]
const WAKE_EFER: usize = core::mem::offset_of!(WakeContext, efer);
#[cfg(target_os = "none")]
const WAKE_RSP: usize = core::mem::offset_of!(WakeContext, rsp);
#[cfg(target_os = "none")]
const WAKE_RESUME_RIP: usize = core::mem::offset_of!(WakeContext, resume_rip);

fn verify_checksum(addr: *const u8, len: usize) -> bool {
    let mut sum: u8 = 0;
    for i in 0..len {
        sum = sum.wrapping_add(unsafe { *addr.add(i) });
    }
    sum == 0
}

pub fn init(rsdp_phys_addr: u64, physical_memory_offset: u64) {
    if rsdp_phys_addr == 0 {
        crate::serial_println!("[acpi] RSDP not provided; ACPI disabled.");
        return;
    }

    let rsdp_virt = physical_memory_offset + rsdp_phys_addr;
    let rsdp = unsafe { &*(rsdp_virt as *const RsdpHeader) };
    if &rsdp.signature != b"RSD PTR " {
        crate::serial_println!("[acpi] Error: Invalid RSDP signature.");
        return;
    }
    if !verify_checksum(rsdp_virt as *const u8, 20) {
        crate::serial_println!("[acpi] Error: RSDP base checksum mismatch.");
        return;
    }
    if rsdp.revision >= 2 {
        let rsdp_len = rsdp.length as usize;
        if rsdp_len < core::mem::size_of::<RsdpHeader>()
            || !verify_checksum(rsdp_virt as *const u8, rsdp_len)
        {
            crate::serial_println!("[acpi] Error: RSDP extended checksum mismatch.");
            return;
        }
    }

    let mut fadt = None;
    if rsdp.revision >= 2 && rsdp.xsdt_address != 0 {
        fadt = find_fadt(
            physical_memory_offset + rsdp.xsdt_address,
            physical_memory_offset,
            8,
            b"XSDT",
        );
    }
    if fadt.is_none() && rsdp.rsdt_address != 0 {
        fadt = find_fadt(
            physical_memory_offset + rsdp.rsdt_address as u64,
            physical_memory_offset,
            4,
            b"RSDT",
        );
    }
    let fadt = match fadt {
        Some(value) => value,
        None => {
            crate::serial_println!("[acpi] Error: FADT (FACP) table not found.");
            return;
        }
    };
    let madt_addr = if rsdp.revision >= 2 && rsdp.xsdt_address != 0 {
        find_table_address(
            physical_memory_offset + rsdp.xsdt_address,
            physical_memory_offset,
            8,
            b"XSDT",
            b"APIC",
        )
    } else {
        None
    }
    .or_else(|| {
        (rsdp.rsdt_address != 0).then(|| {
            find_table_address(
                physical_memory_offset + rsdp.rsdt_address as u64,
                physical_memory_offset,
                4,
                b"RSDT",
                b"APIC",
            )
        })?
    });
    let mut apic_topology = None;
    let mut smp_plan = None;
    if let Some(madt_addr) = madt_addr {
        if let Some(madt_virt) = physical_memory_offset.checked_add(madt_addr) {
            let madt_len =
                unsafe { core::ptr::read_unaligned((madt_virt + 4) as *const u32) } as usize;
            if !(44..=MADT_MAX_LEN).contains(&madt_len) {
                crate::serial_println!("[acpi] MADT invalid: length outside supported bounds");
            } else {
                let madt_bytes =
                    unsafe { core::slice::from_raw_parts(madt_virt as *const u8, madt_len) };
                match crate::madt::parse(madt_bytes) {
                    Ok(info) => {
                        apic_topology = Some(crate::apic::ApicTopology::from_madt(&info));
                        match crate::smp::CpuBootPlan::from_madt(&info) {
                            Ok(plan) => {
                                crate::serial_println!(
                                    "[smp] CPU boot plan ready: {} enabled CPU(s)",
                                    plan.enabled_cpu_count
                                );
                                smp_plan = Some(plan);
                            }
                            Err(error) => {
                                crate::serial_println!(
                                    "[smp] CPU boot plan unavailable: {:?}; AP startup disabled",
                                    error
                                );
                            }
                        }
                        crate::serial_println!(
                            "[acpi] MADT: {} enabled CPU(s), {} I/O APIC(s), LAPIC=0x{:X}",
                            info.enabled_cpu_count(),
                            info.io_apic_count,
                            info.local_apic_address
                        );
                    }
                    Err(error) => {
                        crate::serial_println!("[acpi] MADT invalid: {:?}", error);
                    }
                }
            }
        } else {
            crate::serial_println!("[acpi] MADT address overflows physical mapping");
        }
    } else {
        crate::serial_println!("[acpi] MADT (APIC) table not found; SMP disabled.");
    }

    let mcfg_addr = if rsdp.revision >= 2 && rsdp.xsdt_address != 0 {
        find_table_address(
            physical_memory_offset + rsdp.xsdt_address,
            physical_memory_offset,
            8,
            b"XSDT",
            b"MCFG",
        )
    } else {
        None
    }
    .or_else(|| {
        (rsdp.rsdt_address != 0).then(|| {
            find_table_address(
                physical_memory_offset + rsdp.rsdt_address as u64,
                physical_memory_offset,
                4,
                b"RSDT",
                b"MCFG",
            )
        })?
    });
    let ecam = mcfg_addr
        .and_then(|address| parse_mcfg(address, physical_memory_offset))
        .unwrap_or_default();
    if ecam.is_empty() {
        crate::serial_println!("[acpi] MCFG unavailable; PCI will use legacy config access.");
    } else {
        crate::serial_println!("[acpi] MCFG: {} ECAM region(s) validated.", ecam.len());
    }
    let fadt_len = fadt.header.length as usize;

    let (pm1a_evt, pm1b_evt, evt_is_io) = event_registers(fadt, fadt_len);
    let (pm1a_cnt, pm1b_cnt, cnt_is_io) = control_registers(fadt, fadt_len);
    let sleep_types = parse_sleep_types(fadt, fadt_len, physical_memory_offset);
    let (facs_virt, facs_len) = parse_facs(fadt, fadt_len, physical_memory_offset);
    let (reset_reg, reset_value) = if fadt_len >= FADT_RESET_VALUE_END
        && (fadt.flags & (1 << 10)) != 0
        && fadt.reset_reg.address != 0
    {
        (Some(fadt.reset_reg), Some(fadt.reset_value))
    } else {
        (None, None)
    };

    let mut state = ACPI_STATE.lock();
    *state = AcpiState {
        initialized: true,
        apic: apic_topology,
        smp: smp_plan,
        ecam,
        pm1a_evt_blk: nonzero(pm1a_evt),
        pm1b_evt_blk: nonzero(pm1b_evt),
        pm1a_cnt_blk: nonzero(pm1a_cnt),
        pm1b_cnt_blk: nonzero(pm1b_cnt),
        evt_is_io,
        cnt_is_io,
        sleep_types,
        reset_reg,
        reset_value,
        facs_virt,
        facs_len,
        phys_mem_offset: Some(physical_memory_offset),
        wake_trampoline_phys: None,
        wake_context_virt: None,
    };

    crate::serial_println!(
        "[acpi] ACPI subsystem initialized. PM1a_CNT: 0x{:X} (I/O: {}), DSDT S3: {:?}, S5: {:?}, FACS: {}",
        pm1a_cnt,
        cnt_is_io,
        sleep_types[3],
        sleep_types[5],
        facs_virt.is_some()
    );
}

fn nonzero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

fn parse_mcfg(table_phys: u64, physical_memory_offset: u64) -> Option<crate::pci::EcamRegions> {
    let table_virt = physical_memory_offset.checked_add(table_phys)?;
    let length = unsafe { core::ptr::read_unaligned((table_virt + 4) as *const u32) } as usize;
    if !(MCFG_MIN_LEN..=MCFG_MAX_LEN).contains(&length) {
        crate::serial_println!("[acpi] MCFG invalid: length outside supported bounds.");
        return None;
    }
    let table = unsafe { core::slice::from_raw_parts(table_virt as *const u8, length) };
    match crate::pci::EcamRegions::parse_mcfg(table) {
        Ok(regions) => Some(regions),
        Err(error) => {
            crate::serial_println!("[acpi] MCFG invalid: {:?}.", error);
            None
        }
    }
}

fn find_fadt(
    root_virt: u64,
    physical_memory_offset: u64,
    entry_size: usize,
    signature: &[u8; 4],
) -> Option<&'static Fadt> {
    let root = unsafe { &*(root_virt as *const DescriptionHeader) };
    let root_len = root.length as usize;
    if &root.signature != signature
        || root_len < DESCRIPTION_HEADER_LEN
        || !verify_checksum(root_virt as *const u8, root_len)
    {
        return None;
    }
    let entry_count = (root_len - DESCRIPTION_HEADER_LEN) / entry_size;
    let entries = (root_virt + DESCRIPTION_HEADER_LEN as u64) as *const u8;
    for index in 0..entry_count {
        let entry_phys = unsafe {
            if entry_size == 8 {
                core::ptr::read_unaligned(entries.add(index * 8) as *const u64)
            } else {
                core::ptr::read_unaligned(entries.add(index * 4) as *const u32) as u64
            }
        };
        if entry_phys == 0 {
            continue;
        }
        let entry_virt = physical_memory_offset + entry_phys;
        let header = unsafe { &*(entry_virt as *const DescriptionHeader) };
        let header_len = header.length as usize;
        if &header.signature == b"FACP"
            && header_len >= FADT_LEGACY_PM1_CNT_END
            && verify_checksum(entry_virt as *const u8, header_len)
        {
            return Some(unsafe { &*(entry_virt as *const Fadt) });
        }
    }
    None
}

fn find_table_address(
    root_virt: u64,
    physical_memory_offset: u64,
    entry_size: usize,
    signature: &[u8; 4],
    wanted: &[u8; 4],
) -> Option<u64> {
    let root = unsafe { &*(root_virt as *const DescriptionHeader) };
    let root_len = root.length as usize;
    if &root.signature != signature
        || root_len < DESCRIPTION_HEADER_LEN
        || !verify_checksum(root_virt as *const u8, root_len)
    {
        return None;
    }
    let entry_count = (root_len - DESCRIPTION_HEADER_LEN) / entry_size;
    let entries = (root_virt + DESCRIPTION_HEADER_LEN as u64) as *const u8;
    for index in 0..entry_count {
        let entry_phys = unsafe {
            if entry_size == 8 {
                core::ptr::read_unaligned(entries.add(index * 8) as *const u64)
            } else {
                core::ptr::read_unaligned(entries.add(index * 4) as *const u32) as u64
            }
        };
        if entry_phys == 0 {
            continue;
        }
        let entry_virt = physical_memory_offset + entry_phys;
        let header = unsafe { &*(entry_virt as *const DescriptionHeader) };
        let header_len = header.length as usize;
        if &header.signature == wanted
            && (DESCRIPTION_HEADER_LEN..=ACPI_MAX_TABLE_LEN).contains(&header_len)
            && verify_checksum(entry_virt as *const u8, header_len)
        {
            return Some(entry_phys);
        }
    }
    None
}

fn event_registers(fadt: &Fadt, fadt_len: usize) -> (u64, u64, bool) {
    let mut pm1a = fadt.pm1a_evt_blk as u64;
    let mut pm1b = fadt.pm1b_evt_blk as u64;
    let mut is_io = true;
    if fadt_len >= FADT_X_PM1A_EVT_END && fadt.x_pm1a_evt_blk.address != 0 {
        pm1a = fadt.x_pm1a_evt_blk.address;
        is_io = fadt.x_pm1a_evt_blk.address_space == 1;
    }
    if fadt_len >= FADT_X_PM1B_EVT_END && fadt.x_pm1b_evt_blk.address != 0 {
        pm1b = fadt.x_pm1b_evt_blk.address;
    }
    (pm1a, pm1b, is_io)
}

fn control_registers(fadt: &Fadt, fadt_len: usize) -> (u64, u64, bool) {
    let mut pm1a = fadt.pm1a_cnt_blk as u64;
    let mut pm1b = fadt.pm1b_cnt_blk as u64;
    let mut is_io = true;
    if fadt_len >= FADT_X_PM1A_CNT_END && fadt.x_pm1a_cnt_blk.address != 0 {
        pm1a = fadt.x_pm1a_cnt_blk.address;
        is_io = fadt.x_pm1a_cnt_blk.address_space == 1;
    }
    if fadt_len >= FADT_X_PM1B_CNT_END && fadt.x_pm1b_cnt_blk.address != 0 {
        pm1b = fadt.x_pm1b_cnt_blk.address;
    }
    (pm1a, pm1b, is_io)
}

fn parse_sleep_types(
    fadt: &Fadt,
    fadt_len: usize,
    physical_memory_offset: u64,
) -> [Option<SleepType>; 6] {
    let mut result = [None; 6];
    let dsdt_phys = if fadt_len >= FADT_X_DSDT_END && fadt.x_dsdt != 0 {
        fadt.x_dsdt
    } else {
        fadt.dsdt as u64
    };
    if dsdt_phys == 0 {
        return result;
    }
    let dsdt_virt = physical_memory_offset + dsdt_phys;
    let dsdt = unsafe { &*(dsdt_virt as *const DescriptionHeader) };
    let dsdt_len = dsdt.length as usize;
    if &dsdt.signature != b"DSDT"
        || dsdt_len < DESCRIPTION_HEADER_LEN
        || !verify_checksum(dsdt_virt as *const u8, dsdt_len)
    {
        return result;
    }
    let aml = unsafe {
        core::slice::from_raw_parts(
            (dsdt_virt + DESCRIPTION_HEADER_LEN as u64) as *const u8,
            dsdt_len - DESCRIPTION_HEADER_LEN,
        )
    };
    for state in [1usize, 3, 4, 5] {
        result[state] =
            scan_sleep_values(aml, state as u8).map(|(typa, typb)| SleepType { typa, typb });
    }
    result
}

fn parse_facs(fadt: &Fadt, fadt_len: usize, physical_memory_offset: u64) -> (Option<u64>, usize) {
    let facs_phys = if fadt_len >= FADT_X_DSDT_END && fadt.x_firmware_ctrl != 0 {
        fadt.x_firmware_ctrl
    } else {
        fadt.firmware_ctrl as u64
    };
    if facs_phys == 0 {
        return (None, 0);
    }
    let facs_virt = physical_memory_offset + facs_phys;
    let signature = unsafe { core::slice::from_raw_parts(facs_virt as *const u8, 4) };
    let length = unsafe { core::ptr::read_unaligned((facs_virt + 4) as *const u32) as usize };
    if signature == b"FACS" && length >= FACS_MIN_LEN {
        (Some(facs_virt), length)
    } else {
        (None, 0)
    }
}

fn parse_pkg_length(bytes: &[u8], cursor: &mut usize) -> Option<usize> {
    let lead = *bytes.get(*cursor)?;
    let start = *cursor;
    *cursor += 1;
    let following = (lead >> 6) as usize;
    let mut length = if following == 0 {
        (lead & 0x3f) as usize
    } else {
        (lead & 0x0f) as usize
    };
    for index in 0..following {
        let value = *bytes.get(*cursor)? as usize;
        *cursor += 1;
        length |= value << (4 + index * 8);
    }
    start.checked_add(length)
}

fn parse_aml_integer(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    let opcode = *bytes.get(*cursor)?;
    *cursor += 1;
    match opcode {
        0x00 => Some(0),
        0x01 => Some(1),
        0xff => Some(u64::MAX),
        0x0a => {
            let value = *bytes.get(*cursor)?;
            *cursor += 1;
            Some(value as u64)
        }
        0x0b => {
            let value = u16::from_le_bytes(bytes.get(*cursor..*cursor + 2)?.try_into().ok()?);
            *cursor += 2;
            Some(value as u64)
        }
        0x0c => {
            let value = u32::from_le_bytes(bytes.get(*cursor..*cursor + 4)?.try_into().ok()?);
            *cursor += 4;
            Some(value as u64)
        }
        0x0e => {
            let value = u64::from_le_bytes(bytes.get(*cursor..*cursor + 8)?.try_into().ok()?);
            *cursor += 8;
            Some(value)
        }
        _ => None,
    }
}

fn scan_sleep_values(aml: &[u8], state: u8) -> Option<(u8, u8)> {
    let name = [b'_', b'S', b'0' + state, b'_'];
    for index in 0..aml.len().saturating_sub(3) {
        if aml.get(index..index + 4) != Some(name.as_slice()) {
            continue;
        }
        let mut cursor = index + 4;
        if aml.get(cursor) != Some(&0x12) {
            continue;
        }
        cursor += 1;
        let package_end = parse_pkg_length(aml, &mut cursor)?;
        if package_end > aml.len() || cursor >= package_end {
            continue;
        }
        let elements = aml[cursor];
        cursor += 1;
        if elements < 2 {
            continue;
        }
        let typa = parse_aml_integer(aml, &mut cursor)?;
        let typb = parse_aml_integer(aml, &mut cursor)?;
        if cursor <= package_end && typa <= 7 && typb <= 7 {
            return Some((typa as u8, typb as u8));
        }
    }
    None
}

#[cfg(target_os = "none")]
pub fn configure_wake_trampoline(
    mapper: &mut OffsetPageTable<'static>,
    frame_allocator: &mut BitmapFrameAllocator,
    physical_memory_offset: u64,
) -> Result<u64, SuspendError> {
    let (facs_virt, facs_len, s3_supported) = {
        let state = ACPI_STATE.lock();
        (
            state.facs_virt,
            state.facs_len,
            state.sleep_types[3].is_some(),
        )
    };
    let facs_virt = facs_virt.ok_or(SuspendError::WakeVectorUnavailable)?;
    if !s3_supported {
        return Err(SuspendError::S3Unsupported);
    }

    let mut wake_phys = frame_allocator
        .allocate_below(LEGACY_WAKE_LIMIT)
        .ok_or(SuspendError::LowMemoryUnavailable)?;
    if wake_phys == 0 {
        wake_phys = frame_allocator
            .allocate_below(LEGACY_WAKE_LIMIT)
            .ok_or(SuspendError::LowMemoryUnavailable)?;
    }
    let page = Page::containing_address(VirtAddr::new(wake_phys));
    let frame = PhysFrame::containing_address(PhysAddr::new(wake_phys));
    match mapper.translate_addr(page.start_address()) {
        Some(mapped) if mapped == frame.start_address() => {}
        Some(_) => return Err(SuspendError::WakePageConflict),
        None => paging::map_page(
            mapper,
            page,
            frame,
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
            frame_allocator,
        )
        .map_err(|_| SuspendError::WakePageConflict)?,
    }

    unsafe {
        install_wake_template(wake_phys, physical_memory_offset)?;
    }

    let context_offset = wake_symbol_offset(addr_of_wake_context());
    let wake_context_virt = physical_memory_offset + wake_phys + context_offset as u64;
    unsafe { program_wake_vector(facs_virt, facs_len, wake_phys) };
    let mut state = ACPI_STATE.lock();
    state.wake_trampoline_phys = Some(wake_phys);
    state.wake_context_virt = Some(wake_context_virt);
    Ok(wake_phys)
}

#[cfg(target_os = "none")]
unsafe fn program_wake_vector(facs_virt: u64, facs_len: usize, wake_phys: u64) {
    core::ptr::write_volatile(
        (facs_virt + FACS_FIRMWARE_WAKING_VECTOR_OFFSET as u64) as *mut u32,
        wake_phys as u32,
    );
    if facs_len >= FACS_X_FIRMWARE_WAKING_VECTOR_OFFSET + 8 {
        core::ptr::write_volatile(
            (facs_virt + FACS_X_FIRMWARE_WAKING_VECTOR_OFFSET as u64) as *mut u64,
            0,
        );
    }
    if facs_len >= FACS_OSPM_FLAGS_OFFSET + 4 {
        core::ptr::write_volatile((facs_virt + FACS_OSPM_FLAGS_OFFSET as u64) as *mut u32, 0);
    }
}

#[cfg(not(target_os = "none"))]
pub fn configure_wake_trampoline(
    _mapper: &mut OffsetPageTable<'static>,
    _frame_allocator: &mut BitmapFrameAllocator,
    _physical_memory_offset: u64,
) -> Result<u64, SuspendError> {
    Err(SuspendError::WakeVectorUnavailable)
}

#[cfg(target_os = "none")]
unsafe fn install_wake_template(
    wake_phys: u64,
    physical_memory_offset: u64,
) -> Result<(), SuspendError> {
    unsafe extern "C" {
        static acpi_wake_trampoline_start: u8;
        static acpi_wake_trampoline_end: u8;
        static acpi_wake_gdt: u8;
        static acpi_wake_gdt_base: u8;
        static acpi_wake_long_mode_target: u8;
        static acpi_wake_far_target: u8;
    }
    let source = core::ptr::addr_of!(acpi_wake_trampoline_start);
    let end = core::ptr::addr_of!(acpi_wake_trampoline_end);
    let len = end as usize - source as usize;
    if len > PAGE_SIZE {
        return Err(SuspendError::WakeTrampolineTooLarge);
    }
    let destination = (physical_memory_offset + wake_phys) as *mut u8;
    core::ptr::write_bytes(destination, 0, PAGE_SIZE);
    core::ptr::copy_nonoverlapping(source, destination, len);

    let gdt_offset = wake_symbol_offset(core::ptr::addr_of!(acpi_wake_gdt));
    let gdt_base_offset = wake_symbol_offset(core::ptr::addr_of!(acpi_wake_gdt_base));
    let long_mode_offset = wake_symbol_offset(core::ptr::addr_of!(acpi_wake_long_mode_target));
    let far_target_offset = wake_symbol_offset(core::ptr::addr_of!(acpi_wake_far_target));
    core::ptr::write_unaligned(
        destination.add(gdt_base_offset) as *mut u32,
        (wake_phys + gdt_offset as u64) as u32,
    );
    core::ptr::write_unaligned(
        destination.add(far_target_offset) as *mut u32,
        (wake_phys + long_mode_offset as u64) as u32,
    );
    Ok(())
}

#[cfg(target_os = "none")]
fn addr_of_wake_context() -> *const u8 {
    unsafe extern "C" {
        static acpi_wake_context: u8;
    }
    core::ptr::addr_of!(acpi_wake_context)
}

#[cfg(target_os = "none")]
fn wake_symbol_offset(symbol: *const u8) -> usize {
    unsafe extern "C" {
        static acpi_wake_trampoline_start: u8;
    }
    symbol as usize - core::ptr::addr_of!(acpi_wake_trampoline_start) as usize
}

#[cfg(target_os = "none")]
pub fn suspend_s3() -> Result<(), SuspendError> {
    let (pm1a_evt, pm1b_evt, pm1a_cnt, pm1b_cnt, sleep, evt_is_io, cnt_is_io, wake) = {
        let state = ACPI_STATE.lock();
        if !state.initialized {
            return Err(SuspendError::AcpiUnavailable);
        }
        (
            state.pm1a_evt_blk,
            state.pm1b_evt_blk,
            state.pm1a_cnt_blk,
            state.pm1b_cnt_blk,
            state.sleep_types[3],
            state.evt_is_io,
            state.cnt_is_io,
            state.wake_context_virt,
        )
    };
    let sleep = sleep.ok_or(SuspendError::S3Unsupported)?;
    let pm1a_cnt = pm1a_cnt.ok_or(SuspendError::AcpiUnavailable)?;
    let wake_context = wake.ok_or(SuspendError::WakeVectorUnavailable)?;
    if !evt_is_io || !cnt_is_io {
        return Err(SuspendError::UnsupportedRegisterSpace);
    }

    clear_wake_status(pm1a_evt, pm1b_evt);
    refresh_wake_vector()?;
    let current_a = read_control(pm1a_cnt)?;
    let value_a =
        (current_a & !(PM1_SLP_TYP_MASK | PM1_SLP_EN)) | ((sleep.typa as u16) << 10) | PM1_SLP_EN;
    let value_b = ((sleep.typb as u16) << 10) | PM1_SLP_EN;
    let port_a = u16::try_from(pm1a_cnt).map_err(|_| SuspendError::UnsupportedRegisterSpace)?;
    let port_b = match pm1b_cnt {
        Some(port) => u16::try_from(port).map_err(|_| SuspendError::UnsupportedRegisterSpace)?,
        None => 0,
    };

    RESUME_PENDING.store(true, Ordering::Release);
    unsafe {
        enter_s3(
            port_a,
            value_a,
            port_b,
            value_b,
            wake_context as *mut WakeContext,
        );
    }
    Ok(())
}

#[cfg(target_os = "none")]
fn refresh_wake_vector() -> Result<(), SuspendError> {
    let state = ACPI_STATE.lock();
    let facs = state.facs_virt.ok_or(SuspendError::WakeVectorUnavailable)?;
    let wake = state
        .wake_trampoline_phys
        .ok_or(SuspendError::WakeVectorUnavailable)?;
    unsafe { program_wake_vector(facs, state.facs_len, wake) };
    Ok(())
}

#[cfg(not(target_os = "none"))]
pub fn suspend_s3() -> Result<(), SuspendError> {
    Err(SuspendError::AcpiUnavailable)
}

#[cfg(target_os = "none")]
fn clear_wake_status(pm1a: Option<u64>, pm1b: Option<u64>) {
    for address in [pm1a, pm1b].into_iter().flatten() {
        if let Ok(port_address) = u16::try_from(address) {
            unsafe { Port::<u16>::new(port_address).write(PM1_WAK_STS) };
        }
    }
}

#[cfg(target_os = "none")]
fn read_control(address: u64) -> Result<u16, SuspendError> {
    let address = u16::try_from(address).map_err(|_| SuspendError::UnsupportedRegisterSpace)?;
    Ok(unsafe { Port::<u16>::new(address).read() })
}

pub fn take_resume_pending() -> bool {
    RESUME_PENDING.swap(false, Ordering::AcqRel)
}

pub fn info() -> AcpiInfo {
    let state = ACPI_STATE.lock();
    AcpiInfo {
        initialized: state.initialized,
        apic: state.apic,
        smp: state.smp,
        ecam: state.ecam,
        s1_supported: state.sleep_types[1].is_some(),
        s3_supported: state.sleep_types[3].is_some(),
        s4_supported: state.sleep_types[4].is_some(),
        s5_supported: state.sleep_types[5].is_some(),
        wake_trampoline_ready: state.wake_context_virt.is_some(),
        wake_trampoline_phys: state.wake_trampoline_phys,
    }
}

/// Physical-to-virtual direct-map offset captured during ACPI discovery.
/// APIC resume uses the same offset after firmware returns from S3.
pub fn physical_memory_offset() -> u64 {
    ACPI_STATE.lock().phys_mem_offset.unwrap_or(0)
}

/// Record the bootstrap processor selected by the Local APIC hand-off.
pub fn assign_bsp(apic_id: u32) -> Result<usize, crate::smp::CpuPlanError> {
    ACPI_STATE
        .lock()
        .smp
        .as_mut()
        .ok_or(crate::smp::CpuPlanError::NoEnabledCpu)?
        .assign_bsp(apic_id)
}

/// Store AP lifecycle updates made by the SMP bootstrap sequence.
pub fn set_smp_plan(plan: crate::smp::CpuBootPlan) {
    ACPI_STATE.lock().smp = Some(plan);
}

pub fn shutdown() -> ! {
    let state = ACPI_STATE.lock();
    if let (Some(pm1a), Some(sleep)) = (state.pm1a_cnt_blk, state.sleep_types[5]) {
        let val_a = ((sleep.typa as u16) << 10) | PM1_SLP_EN;
        if state.cnt_is_io {
            if let Ok(address) = u16::try_from(pm1a) {
                unsafe { Port::<u16>::new(address).write(val_a) };
            }
            if let Some(pm1b) = state.pm1b_cnt_blk {
                if let Ok(address) = u16::try_from(pm1b) {
                    let val_b = ((sleep.typb as u16) << 10) | PM1_SLP_EN;
                    unsafe { Port::<u16>::new(address).write(val_b) };
                }
            }
        } else if let Some(offset) = state.phys_mem_offset {
            unsafe { core::ptr::write_volatile((offset + pm1a) as *mut u16, val_a) };
        }
    }
    drop(state);

    for address in [0x604u16, 0xb004u16] {
        let mut port = Port::<u16>::new(address);
        unsafe {
            port.write(PM1_SLP_EN);
            port.write(PM1_SLP_EN | (5 << 10));
        }
    }
    unsafe { Port::<u32>::new(0x501).write(0) };
    loop {
        x86_64::instructions::hlt();
    }
}

pub fn reboot() -> bool {
    let state = ACPI_STATE.lock();
    if let (Some(reg), Some(value)) = (state.reset_reg, state.reset_value) {
        if reg.address_space == 1 {
            if let Ok(address) = u16::try_from(reg.address) {
                unsafe { Port::<u8>::new(address).write(value) };
                return true;
            }
        } else if reg.address_space == 0 {
            if let Some(offset) = state.phys_mem_offset {
                unsafe { core::ptr::write_volatile((offset + reg.address) as *mut u8, value) };
                return true;
            }
        }
    }
    false
}

#[cfg(target_os = "none")]
#[unsafe(naked)]
unsafe extern "C" fn enter_s3(
    _pm1a_port: u16,
    _pm1a_value: u16,
    _pm1b_port: u16,
    _pm1b_value: u16,
    _wake_context: *mut WakeContext,
) {
    core::arch::naked_asm!(
        "push rbx",
        "push rbp",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "cli",
        "mov r9d, edx",
        "mov r10d, ecx",
        "mov [r8 + {wake_rsp}], rsp",
        "lea rax, [rip + 2f]",
        "mov [r8 + {wake_resume_rip}], rax",
        "mov rax, cr0",
        "mov [r8 + {wake_cr0}], rax",
        "mov rax, cr3",
        "mov [r8 + {wake_cr3}], rax",
        "mov rax, cr4",
        "mov [r8 + {wake_cr4}], rax",
        "mov ecx, 0xc0000080",
        "rdmsr",
        "mov [r8 + {wake_efer}], eax",
        "mov [r8 + {wake_efer} + 4], edx",
        "wbinvd",
        "mov dx, di",
        "mov ax, si",
        "out dx, ax",
        "test r9w, r9w",
        "jz 1f",
        "mov dx, r9w",
        "mov ax, r10w",
        "out dx, ax",
        "1:",
        "hlt",
        "jmp 1b",
        "2:",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbp",
        "pop rbx",
        "ret",
        wake_cr0 = const WAKE_CR0,
        wake_cr3 = const WAKE_CR3,
        wake_cr4 = const WAKE_CR4,
        wake_efer = const WAKE_EFER,
        wake_rsp = const WAKE_RSP,
        wake_resume_rip = const WAKE_RESUME_RIP,
    );
}

#[cfg(target_os = "none")]
core::arch::global_asm!(
    r#"
    .pushsection .text.acpi_wake, "ax", @progbits
    .balign 16
    .global acpi_wake_trampoline_start
    .global acpi_wake_trampoline_end
    .global acpi_wake_long_mode_target
    .global acpi_wake_far_target
    .global acpi_wake_gdt
    .global acpi_wake_gdt_base
    .global acpi_wake_context

    .code16
acpi_wake_trampoline_start:
    cli
    cld
    mov ax, cs
    mov ds, ax
    lgdt cs:[ACPI_WAKE_GDT_DESCRIPTOR_OFFSET]

    mov eax, dword ptr cs:[ACPI_WAKE_CONTEXT_OFFSET + 16]
    mov cr4, eax
    mov eax, dword ptr cs:[ACPI_WAKE_CONTEXT_OFFSET + 8]
    mov cr3, eax
    mov ecx, 0xc0000080
    mov eax, dword ptr cs:[ACPI_WAKE_CONTEXT_OFFSET + 24]
    mov edx, dword ptr cs:[ACPI_WAKE_CONTEXT_OFFSET + 28]
    wrmsr
    mov eax, dword ptr cs:[ACPI_WAKE_CONTEXT_OFFSET]
    mov cr0, eax

    .byte 0x66, 0xea
acpi_wake_far_target:
    .long 0
    .word 0x08

    .code64
acpi_wake_long_mode_target:
    mov ax, 0x10
    mov ds, ax
    mov es, ax
    mov ss, ax
    mov rsp, qword ptr [rip + acpi_wake_context + 32]
    mov rax, qword ptr [rip + acpi_wake_context + 40]
    jmp rax

    .balign 8
acpi_wake_gdt:
    .quad 0x0000000000000000
    .quad 0x00af9a000000ffff
    .quad 0x00cf92000000ffff
acpi_wake_gdt_end:
acpi_wake_gdt_descriptor:
    .word acpi_wake_gdt_end - acpi_wake_gdt - 1
acpi_wake_gdt_base:
    .long 0

    .balign 8
acpi_wake_context:
    .zero 48
acpi_wake_trampoline_end:
    .set ACPI_WAKE_GDT_DESCRIPTOR_OFFSET, acpi_wake_gdt_descriptor - acpi_wake_trampoline_start
    .set ACPI_WAKE_CONTEXT_OFFSET, acpi_wake_context - acpi_wake_trampoline_start
    .code64
    .popsection
"#
);

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;
    use alloc::vec;

    #[test]
    fn scans_qemu_style_sleep_packages() {
        let aml = vec![
            0x08, b'_', b'S', b'3', b'_', 0x12, 0x06, 0x04, 0x01, 0x01, 0x00, 0x00, 0x08, b'_',
            b'S', b'5', b'_', 0x12, 0x08, 0x04, 0x0a, 0x05, 0x0a, 0x05, 0x00, 0x00,
        ];
        assert_eq!(scan_sleep_values(&aml, 3), Some((1, 1)));
        assert_eq!(scan_sleep_values(&aml, 5), Some((5, 5)));
        assert_eq!(scan_sleep_values(&aml, 1), None);
    }

    #[test]
    fn rejects_invalid_sleep_types_and_truncated_packages() {
        let invalid = vec![
            0x08, b'_', b'S', b'3', b'_', 0x12, 0x06, 0x02, 0x0a, 0x08, 0x01,
        ];
        let truncated = vec![0x08, b'_', b'S', b'3', b'_', 0x12, 0x08, 0x02, 0x01];
        assert_eq!(scan_sleep_values(&invalid, 3), None);
        assert_eq!(scan_sleep_values(&truncated, 3), None);
    }

    #[test]
    fn parses_all_aml_integer_widths() {
        let bytes = [
            0x00, 0x01, 0x0a, 0x7f, 0x0b, 0x34, 0x12, 0x0c, 0x78, 0x56, 0x34, 0x12,
        ];
        let mut cursor = 0;
        assert_eq!(parse_aml_integer(&bytes, &mut cursor), Some(0));
        assert_eq!(parse_aml_integer(&bytes, &mut cursor), Some(1));
        assert_eq!(parse_aml_integer(&bytes, &mut cursor), Some(0x7f));
        assert_eq!(parse_aml_integer(&bytes, &mut cursor), Some(0x1234));
        assert_eq!(parse_aml_integer(&bytes, &mut cursor), Some(0x1234_5678));
    }
}

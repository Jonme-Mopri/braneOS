//! APIC register definitions, MMIO windows and deterministic redirection
//! helpers.
//!
//! The register wrappers deliberately keep volatile access behind `unsafe`
//! methods.  Discovery and address arithmetic remain safe and are exercised
//! by host-side tests; only the bare-metal bring-up maps the MMIO pages.

#[cfg(target_os = "none")]
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(target_os = "none")]
use spin::Mutex;
#[cfg(target_os = "none")]
use x86_64::structures::paging::{Mapper, Page, PageTableFlags, PhysFrame, Size2MiB, Translate};
#[cfg(target_os = "none")]
use x86_64::{PhysAddr, VirtAddr};

pub const LOCAL_APIC_DEFAULT_BASE: u64 = 0xFEE0_0000;
pub const LOCAL_APIC_ID: u32 = 0x020;
pub const LOCAL_APIC_VERSION: u32 = 0x030;
pub const LOCAL_APIC_SIVR: u32 = 0x0F0;
pub const LOCAL_APIC_EOI: u32 = 0x0B0;
pub const LOCAL_APIC_ICR_LOW: u32 = 0x300;
pub const LOCAL_APIC_ICR_HIGH: u32 = 0x310;
pub const LOCAL_APIC_SIVR_ENABLE: u32 = 1 << 8;
pub const LOCAL_APIC_SPURIOUS_VECTOR: u8 = 0xFF;
pub const IO_APIC_REGSEL: u32 = 0x00;
pub const IO_APIC_WINDOW: u32 = 0x10;
pub const IO_APIC_ID: u32 = 0x00;
pub const IO_APIC_VERSION: u32 = 0x01;
pub const IO_APIC_ARBITRATION: u32 = 0x02;
pub const IO_APIC_REDIRECTION_BASE: u32 = 0x10;

#[cfg(target_os = "none")]
const IA32_APIC_BASE_LAPIC_ENABLE: u64 = 1 << 11;
#[cfg(target_os = "none")]
const IA32_APIC_BASE_X2APIC_ENABLE: u64 = 1 << 10;

/// Memory-mapped Local APIC window. The caller must provide the physical
/// base reported by MADT and the kernel's direct-map offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalApic {
    physical_base: u64,
    virtual_base: u64,
}

impl LocalApic {
    pub fn new(physical_base: u64, physical_memory_offset: u64) -> Option<Self> {
        if !is_valid_mmio_base(physical_base) {
            return None;
        }
        Some(Self {
            physical_base,
            virtual_base: physical_base.checked_add(physical_memory_offset)?,
        })
    }

    pub const fn physical_base(self) -> u64 {
        self.physical_base
    }

    pub const fn virtual_base(self) -> u64 {
        self.virtual_base
    }

    /// Returns the direct-map address for a register offset.
    pub const fn register_address(self, register: u32) -> Option<u64> {
        self.virtual_base.checked_add(register as u64)
    }

    /// Read one 32-bit APIC register through the direct map.
    ///
    /// # Safety
    /// The direct-map address must be mapped and point to a Local APIC page.
    pub unsafe fn read(self, register: u32) -> u32 {
        let address = self
            .register_address(register)
            .expect("Local APIC register address overflow");
        core::ptr::read_volatile(address as *const u32)
    }

    /// Write one 32-bit APIC register through the direct map.
    ///
    /// # Safety
    /// The direct-map address must be mapped and point to a writable Local
    /// APIC page; the register must be valid for the target APIC.
    pub unsafe fn write(self, register: u32, value: u32) {
        let address = self
            .register_address(register)
            .expect("Local APIC register address overflow");
        core::ptr::write_volatile(address as *mut u32, value);
    }

    /// Enable the Local APIC with the supplied spurious-interrupt vector.
    ///
    /// # Safety
    /// The Local APIC MMIO page must be mapped and owned by this CPU.
    pub unsafe fn enable(self, spurious_vector: u8) {
        let value = self.read(LOCAL_APIC_SIVR);
        self.write(
            LOCAL_APIC_SIVR,
            (value & !0xff) | spurious_vector as u32 | LOCAL_APIC_SIVR_ENABLE,
        );
    }

    /// Signal end-of-interrupt to the Local APIC.
    ///
    /// # Safety
    /// The Local APIC MMIO page must be mapped.
    pub unsafe fn end_of_interrupt(self) {
        self.write(LOCAL_APIC_EOI, 0);
    }

    #[cfg(target_os = "none")]
    unsafe fn wait_for_ipi_delivery(self) -> bool {
        const DELIVERY_STATUS: u32 = 1 << 12;
        const WAIT_SPINS: usize = 1_000_000;

        for _ in 0..WAIT_SPINS {
            if self.read(LOCAL_APIC_ICR_LOW) & DELIVERY_STATUS == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    #[cfg(target_os = "none")]
    unsafe fn send_ipi(self, destination: u8, low: u32) -> bool {
        if !self.wait_for_ipi_delivery() {
            return false;
        }
        self.write(LOCAL_APIC_ICR_HIGH, (destination as u32) << 24);
        self.write(LOCAL_APIC_ICR_LOW, low);
        self.wait_for_ipi_delivery()
    }

    /// Send a fixed-delivery IPI to one xAPIC destination.
    ///
    /// # Safety
    /// The Local APIC MMIO page must belong to the calling CPU and the target
    /// must have installed an IDT entry for `vector`.
    #[cfg(target_os = "none")]
    pub unsafe fn send_fixed_ipi(self, destination: u8, vector: u8) -> bool {
        if vector < 32 {
            return false;
        }
        self.send_ipi(destination, vector as u32)
    }

    /// Send the architected INIT + double-SIPI sequence to one xAPIC target.
    /// The bounded delivery-status waits prevent a wedged APIC from hanging
    /// the bootstrap CPU indefinitely. The caller remains responsible for
    /// checking the target's startup acknowledgement.
    ///
    /// # Safety
    /// The Local APIC MMIO page must be mapped and the target must be a valid
    /// xAPIC destination ID. The supplied SIPI vector must identify a mapped
    /// 4 KiB real-mode trampoline below 1 MiB.
    #[cfg(target_os = "none")]
    pub unsafe fn send_init_sipi(self, destination: u8, vector: u8) -> bool {
        const LEVEL_ASSERT: u32 = 1 << 14;
        const TRIGGER_LEVEL: u32 = 1 << 15;
        const DELIVERY_INIT: u32 = 5 << 8;
        const DELIVERY_STARTUP: u32 = 6 << 8;

        if !self.send_ipi(destination, DELIVERY_INIT | LEVEL_ASSERT | TRIGGER_LEVEL) {
            return false;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
        if !self.send_ipi(destination, DELIVERY_INIT | TRIGGER_LEVEL) {
            return false;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
        if !self.send_ipi(destination, DELIVERY_STARTUP | vector as u32) {
            return false;
        }
        for _ in 0..10_000 {
            core::hint::spin_loop();
        }
        self.send_ipi(destination, DELIVERY_STARTUP | vector as u32)
    }
}

/// Memory-mapped I/O APIC window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoApic {
    physical_base: u64,
    virtual_base: u64,
}

impl IoApic {
    pub fn new(physical_base: u64, physical_memory_offset: u64) -> Option<Self> {
        if !is_valid_mmio_base(physical_base) {
            return None;
        }
        Some(Self {
            physical_base,
            virtual_base: physical_base.checked_add(physical_memory_offset)?,
        })
    }

    pub const fn physical_base(self) -> u64 {
        self.physical_base
    }

    pub const fn virtual_base(self) -> u64 {
        self.virtual_base
    }

    pub const fn register_address(self, register: u32) -> Option<u64> {
        self.virtual_base.checked_add(register as u64)
    }

    /// Read one I/O APIC register through the selector/window pair.
    ///
    /// # Safety
    /// The direct-map address must be mapped to an I/O APIC page. Calls must
    /// be serialized when multiple CPUs can access the same controller.
    pub unsafe fn read_register(self, register: u8) -> u32 {
        let selector = self
            .register_address(IO_APIC_REGSEL)
            .expect("I/O APIC selector address overflow") as *mut u32;
        let window = self
            .register_address(IO_APIC_WINDOW)
            .expect("I/O APIC window address overflow") as *const u32;
        core::ptr::write_volatile(selector, register as u32);
        core::ptr::read_volatile(window)
    }

    /// Write one I/O APIC register through the selector/window pair.
    ///
    /// # Safety
    /// The direct-map address must be mapped to a writable I/O APIC page.
    /// Calls must be serialized when multiple CPUs can access the controller.
    pub unsafe fn write_register(self, register: u8, value: u32) {
        let selector = self
            .register_address(IO_APIC_REGSEL)
            .expect("I/O APIC selector address overflow") as *mut u32;
        let window = self
            .register_address(IO_APIC_WINDOW)
            .expect("I/O APIC window address overflow") as *mut u32;
        core::ptr::write_volatile(selector, register as u32);
        core::ptr::write_volatile(window, value);
    }

    /// Return the number of redirection entries advertised by a VERSION
    /// register value. The encoded field is the highest valid index.
    pub const fn redirection_count(version: u32) -> u16 {
        ((version >> 16) & 0xff) as u16 + 1
    }

    const fn redirection_registers(index: u8) -> Option<(u8, u8)> {
        let low = IO_APIC_REDIRECTION_BASE as u16 + index as u16 * 2;
        let high = low + 1;
        if high > u8::MAX as u16 {
            None
        } else {
            Some((low as u8, high as u8))
        }
    }

    /// Read one redirection-table entry.
    ///
    /// Returns `None` when the register pair cannot be represented by the
    /// controller's 8-bit selector (indices above the architectural range).
    ///
    /// # Safety
    /// The I/O APIC MMIO page must be mapped and this controller must not be
    /// accessed concurrently.
    pub unsafe fn read_redirection(self, index: u8) -> Option<RedirectionEntry> {
        let (low, high) = Self::redirection_registers(index)?;
        let value = self.read_register(low) as u64 | (self.read_register(high) as u64) << 32;
        Some(RedirectionEntry::from_raw(value))
    }

    /// Write one redirection-table entry.
    ///
    /// # Safety
    /// The I/O APIC MMIO page must be mapped and this controller must not be
    /// accessed concurrently.
    pub unsafe fn write_redirection(self, index: u8, entry: RedirectionEntry) -> bool {
        let Some((low, high)) = Self::redirection_registers(index) else {
            return false;
        };
        let value = entry.to_raw();
        self.write_register(low, value as u32);
        self.write_register(high, (value >> 32) as u32);
        true
    }
}

/// Summary of the APIC topology discovered from MADT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApicTopology {
    pub local_apic_address: u64,
    pub enabled_cpu_count: usize,
    pub io_apic_count: usize,
    pub first_io_apic_address: Option<u64>,
    pub first_io_apic_global_irq_base: Option<u32>,
    pub timer_route: Option<crate::madt::IsaIrqRoute>,
    pub keyboard_route: Option<crate::madt::IsaIrqRoute>,
}

impl ApicTopology {
    pub fn from_madt(info: &crate::madt::MadtInfo) -> Self {
        Self {
            local_apic_address: info.local_apic_address,
            enabled_cpu_count: info.enabled_cpu_count(),
            io_apic_count: info.io_apic_count,
            first_io_apic_address: (info.io_apic_count > 0)
                .then(|| info.io_apics[0])
                .flatten()
                .map(|entry| entry.address as u64),
            first_io_apic_global_irq_base: (info.io_apic_count > 0)
                .then(|| info.io_apics[0])
                .flatten()
                .map(|entry| entry.global_irq_base),
            timer_route: info.isa_irq_route(0).ok(),
            keyboard_route: info.isa_irq_route(1).ok(),
        }
    }
}

/// Map one 4 KiB APIC MMIO page into the kernel's direct-map address space.
///
/// The bootloader normally maps this range already. If it does not, the page
/// is installed with uncached, writable permissions before a register wrapper
/// is constructed; an existing mapping to the same frame is left intact. This
/// function is only meaningful on bare metal.
#[cfg(target_os = "none")]
pub fn map_mmio_page(
    mapper: &mut x86_64::structures::paging::OffsetPageTable<'static>,
    frame_allocator: &mut crate::memory::frame_allocator::BitmapFrameAllocator,
    physical_memory_offset: u64,
    physical_base: u64,
) -> Result<(), &'static str> {
    if !is_valid_mmio_base(physical_base) {
        return Err("APIC MMIO base is not page aligned");
    }
    let virtual_base = physical_base
        .checked_add(physical_memory_offset)
        .ok_or("APIC MMIO virtual address overflow")?;
    let page = Page::containing_address(VirtAddr::new(virtual_base));
    let frame = PhysFrame::containing_address(PhysAddr::new(physical_base));
    let flags = PageTableFlags::PRESENT
        | PageTableFlags::WRITABLE
        | PageTableFlags::NO_EXECUTE
        | PageTableFlags::NO_CACHE;
    match mapper.translate_addr(page.start_address()) {
        Some(mapped) if mapped == frame.start_address() => {
            // The bootloader's direct map is cacheable by default. APIC
            // registers are side-effectful, so retain the mapping but switch
            // this page to uncached attributes before exposing it.
            unsafe {
                if let Ok(flush) = mapper.update_flags(page, flags) {
                    flush.flush();
                } else {
                    // bootloader 0.11 commonly uses 2 MiB direct-map pages;
                    // update the containing large page when a 4 KiB update
                    // is rejected because its parent entry is huge.
                    let large_page =
                        Page::<Size2MiB>::containing_address(VirtAddr::new(virtual_base));
                    let large_frame =
                        PhysFrame::<Size2MiB>::containing_address(PhysAddr::new(physical_base));
                    if mapper.translate_addr(large_page.start_address())
                        != Some(large_frame.start_address())
                    {
                        return Err("APIC MMIO page is mapped to an unexpected frame");
                    }
                    mapper
                        .update_flags(large_page, flags)
                        .map_err(|_| "failed to update APIC MMIO page flags")?
                        .flush();
                }
            }
            Ok(())
        }
        Some(_) => Err("APIC MMIO page is mapped to a different frame"),
        None => crate::memory::paging::map_page(mapper, page, frame, flags, frame_allocator),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedirectionEntry {
    pub vector: u8,
    pub delivery_mode: u8,
    pub active_low: bool,
    pub level_triggered: bool,
    pub masked: bool,
    pub destination: u8,
}

impl RedirectionEntry {
    pub const fn new(vector: u8, destination: u8) -> Self {
        Self {
            vector,
            delivery_mode: 0,
            active_low: false,
            level_triggered: false,
            masked: true,
            destination,
        }
    }

    pub const fn unmasked(mut self) -> Self {
        self.masked = false;
        self
    }

    pub const fn with_delivery_mode(mut self, delivery_mode: u8) -> Self {
        self.delivery_mode = delivery_mode & 7;
        self
    }

    pub const fn with_polarity(mut self, active_low: bool) -> Self {
        self.active_low = active_low;
        self
    }

    pub const fn with_trigger_mode(mut self, level_triggered: bool) -> Self {
        self.level_triggered = level_triggered;
        self
    }

    pub const fn from_raw(raw: u64) -> Self {
        Self {
            vector: raw as u8,
            delivery_mode: ((raw >> 8) & 7) as u8,
            active_low: raw & (1 << 13) != 0,
            level_triggered: raw & (1 << 15) != 0,
            masked: raw & (1 << 16) != 0,
            destination: (raw >> 56) as u8,
        }
    }

    pub const fn to_raw(self) -> u64 {
        let mut value = self.vector as u64 | ((self.delivery_mode as u64 & 7) << 8);
        if self.active_low {
            value |= 1 << 13;
        }
        if self.level_triggered {
            value |= 1 << 15;
        }
        if self.masked {
            value |= 1 << 16;
        }
        value | ((self.destination as u64) << 56)
    }
}

pub const fn is_valid_mmio_base(address: u64) -> bool {
    address != 0 && address & 0xFFF == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivationError {
    MissingIoApic,
    MissingIrqRoute,
    InvalidIoApicBase,
    GsiOutsideIoApic,
    RedirectionIndexUnavailable,
    LocalApicUnavailable,
    X2ApicMode,
    AlreadyActive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApicActivation {
    pub local_apic_id: u8,
    pub io_apic_redirection_count: u16,
    pub timer_global_irq: u32,
    pub keyboard_global_irq: u32,
}

#[cfg(target_os = "none")]
#[derive(Debug, Clone, Copy)]
struct ApicRuntime {
    local: LocalApic,
    io: IoApic,
    timer_index: u8,
    keyboard_index: u8,
    timer_entry: RedirectionEntry,
    keyboard_entry: RedirectionEntry,
}

#[cfg(target_os = "none")]
static APIC_ACTIVE: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "none")]
static APIC_RUNTIME: Mutex<Option<ApicRuntime>> = Mutex::new(None);
/// Cached EOI MMIO address used by interrupt wrappers.  The top half must not
/// take `APIC_RUNTIME`'s spinlock: an interrupt can preempt code that holds it.
#[cfg(target_os = "none")]
static APIC_EOI_ADDRESS: AtomicU64 = AtomicU64::new(0);

/// Whether hardware IRQ acknowledgements should be sent to the Local APIC.
pub fn is_active() -> bool {
    #[cfg(target_os = "none")]
    {
        APIC_ACTIVE.load(Ordering::Acquire)
    }
    #[cfg(not(target_os = "none"))]
    {
        false
    }
}

/// Stop APIC acknowledgements while the legacy PIC is being restored.
#[cfg(target_os = "none")]
pub fn deactivate() {
    APIC_EOI_ADDRESS.store(0, Ordering::Release);
    APIC_ACTIVE.store(false, Ordering::Release);
}

#[cfg(not(target_os = "none"))]
pub fn deactivate() {}

/// Acknowledge an interrupt through the active Local APIC.
///
/// Returns `false` when APIC routing is not active, allowing the caller to
/// acknowledge the same vector through the legacy PIC.
#[cfg(target_os = "none")]
pub fn end_of_interrupt() -> bool {
    let address = APIC_EOI_ADDRESS.load(Ordering::Acquire);
    if !APIC_ACTIVE.load(Ordering::Acquire) || address == 0 {
        return false;
    }
    // This is the only operation required for an EOI and is safe to do
    // directly from the interrupt wrapper.  The cached address is published
    // before APIC_ACTIVE during activation.
    unsafe { core::ptr::write_volatile(address as *mut u32, 0) };
    true
}

#[cfg(not(target_os = "none"))]
pub fn end_of_interrupt() -> bool {
    false
}

/// Return a copy of the BSP's Local APIC handle after IRQ routing is active.
#[cfg(target_os = "none")]
pub fn local_apic_handle() -> Option<LocalApic> {
    APIC_RUNTIME.lock().map(|runtime| runtime.local)
}

/// Enable the xAPIC MMIO interface and software spurious vector on the
/// calling processor. This is used by APs after loading their IDT.
#[cfg(target_os = "none")]
pub fn enable_current_local_apic() -> bool {
    let Some(local) = local_apic_handle() else {
        return false;
    };
    let (current_frame, current_raw) = x86_64::registers::model_specific::ApicBase::read_raw();
    if current_raw & IA32_APIC_BASE_X2APIC_ENABLE != 0 {
        return false;
    }
    let desired_frame = PhysFrame::containing_address(PhysAddr::new(local.physical_base()));
    let mut flags =
        x86_64::registers::model_specific::ApicBaseFlags::from_bits_truncate(current_raw);
    flags.remove(x86_64::registers::model_specific::ApicBaseFlags::X2APIC_ENABLE);
    flags.insert(x86_64::registers::model_specific::ApicBaseFlags::LAPIC_ENABLE);
    if current_frame.start_address() != desired_frame.start_address()
        || current_raw & IA32_APIC_BASE_LAPIC_ENABLE == 0
    {
        unsafe {
            x86_64::registers::model_specific::ApicBase::write(desired_frame, flags);
        }
    }
    unsafe { local.enable(LOCAL_APIC_SPURIOUS_VECTOR) };
    true
}

/// Read the xAPIC ID of the processor executing this function.
#[cfg(target_os = "none")]
pub fn current_local_apic_id() -> Option<u8> {
    let local = local_apic_handle()?;
    Some((unsafe { local.read(LOCAL_APIC_ID) } >> 24) as u8)
}

#[cfg(target_os = "none")]
fn redirection_index(
    global_irq: u32,
    global_irq_base: u32,
    redirection_count: u16,
) -> Result<u8, ActivationError> {
    let index = global_irq
        .checked_sub(global_irq_base)
        .ok_or(ActivationError::GsiOutsideIoApic)?;
    if index >= redirection_count as u32 || index > 119 {
        return Err(ActivationError::RedirectionIndexUnavailable);
    }
    Ok(index as u8)
}

/// Enable the Local APIC and atomically move ISA timer/keyboard IRQs from the
/// 8259 PIC to the first MADT I/O APIC. The supplied closure masks the PIC at
/// the point where all APIC entries are still masked and CPU interrupts are
/// disabled, so a failed validation leaves the legacy path untouched.
#[cfg(target_os = "none")]
pub fn activate_legacy_irqs<F>(
    topology: ApicTopology,
    physical_memory_offset: u64,
    mask_pic: F,
) -> Result<ApicActivation, ActivationError>
where
    F: FnOnce(),
{
    let mut result = Err(ActivationError::LocalApicUnavailable);
    x86_64::instructions::interrupts::without_interrupts(|| {
        if APIC_ACTIVE.load(Ordering::Acquire) {
            result = Err(ActivationError::AlreadyActive);
            return;
        }
        let Some(io_apic_address) = topology.first_io_apic_address else {
            result = Err(ActivationError::MissingIoApic);
            return;
        };
        let Some(global_irq_base) = topology.first_io_apic_global_irq_base else {
            result = Err(ActivationError::InvalidIoApicBase);
            return;
        };
        let Some(timer_route) = topology.timer_route else {
            result = Err(ActivationError::MissingIrqRoute);
            return;
        };
        let Some(keyboard_route) = topology.keyboard_route else {
            result = Err(ActivationError::MissingIrqRoute);
            return;
        };
        let Some(local) = LocalApic::new(topology.local_apic_address, physical_memory_offset)
        else {
            result = Err(ActivationError::LocalApicUnavailable);
            return;
        };
        let Some(io) = IoApic::new(io_apic_address, physical_memory_offset) else {
            result = Err(ActivationError::InvalidIoApicBase);
            return;
        };

        let (current_frame, current_raw) = x86_64::registers::model_specific::ApicBase::read_raw();
        if current_raw & IA32_APIC_BASE_X2APIC_ENABLE != 0 {
            result = Err(ActivationError::X2ApicMode);
            return;
        }
        let desired_frame =
            PhysFrame::containing_address(PhysAddr::new(topology.local_apic_address));
        let mut flags =
            x86_64::registers::model_specific::ApicBaseFlags::from_bits_truncate(current_raw);
        flags.remove(x86_64::registers::model_specific::ApicBaseFlags::X2APIC_ENABLE);
        flags.insert(x86_64::registers::model_specific::ApicBaseFlags::LAPIC_ENABLE);
        if current_frame.start_address() != desired_frame.start_address()
            || current_raw & IA32_APIC_BASE_LAPIC_ENABLE == 0
        {
            unsafe {
                x86_64::registers::model_specific::ApicBase::write(desired_frame, flags);
            }
        }

        let local_apic_id = (unsafe { local.read(LOCAL_APIC_ID) } >> 24) as u8;
        let redirection_count =
            IoApic::redirection_count(unsafe { io.read_register(IO_APIC_VERSION as u8) });
        let Ok(timer_index) =
            redirection_index(timer_route.global_irq, global_irq_base, redirection_count)
        else {
            result = Err(ActivationError::GsiOutsideIoApic);
            return;
        };
        let Ok(keyboard_index) = redirection_index(
            keyboard_route.global_irq,
            global_irq_base,
            redirection_count,
        ) else {
            result = Err(ActivationError::GsiOutsideIoApic);
            return;
        };
        if timer_index == keyboard_index {
            result = Err(ActivationError::RedirectionIndexUnavailable);
            return;
        }
        let timer_entry = RedirectionEntry::new(32, local_apic_id)
            .with_polarity(timer_route.active_low)
            .with_trigger_mode(timer_route.level_triggered);
        let keyboard_entry = RedirectionEntry::new(33, local_apic_id)
            .with_polarity(keyboard_route.active_low)
            .with_trigger_mode(keyboard_route.level_triggered);
        if !unsafe { io.write_redirection(timer_index, timer_entry) }
            || !unsafe { io.write_redirection(keyboard_index, keyboard_entry) }
        {
            result = Err(ActivationError::RedirectionIndexUnavailable);
            return;
        }
        unsafe { local.enable(LOCAL_APIC_SPURIOUS_VECTOR) };

        *APIC_RUNTIME.lock() = Some(ApicRuntime {
            local,
            io,
            timer_index,
            keyboard_index,
            timer_entry,
            keyboard_entry,
        });
        APIC_EOI_ADDRESS.store(
            local.register_address(LOCAL_APIC_EOI).unwrap_or(0),
            Ordering::Release,
        );
        mask_pic();
        APIC_ACTIVE.store(true, Ordering::Release);
        if let Some(runtime) = *APIC_RUNTIME.lock() {
            unsafe {
                runtime
                    .io
                    .write_redirection(runtime.timer_index, runtime.timer_entry.unmasked());
                runtime
                    .io
                    .write_redirection(runtime.keyboard_index, runtime.keyboard_entry.unmasked());
            }
        }
        result = Ok(ApicActivation {
            local_apic_id,
            io_apic_redirection_count: redirection_count,
            timer_global_irq: timer_route.global_irq,
            keyboard_global_irq: keyboard_route.global_irq,
        });
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_masked_redirection_entry() {
        let raw = RedirectionEntry::new(0x31, 2).to_raw();
        assert_eq!(raw & 0xFF, 0x31);
        assert_ne!(raw & (1 << 16), 0);
        assert_eq!(raw >> 56, 2);
    }

    #[test]
    fn unmask_clears_mask_bit() {
        assert_eq!(
            RedirectionEntry::new(32, 0).unmasked().to_raw() & (1 << 16),
            0
        );
    }

    #[test]
    fn validates_page_aligned_mmio_addresses() {
        assert!(is_valid_mmio_base(LOCAL_APIC_DEFAULT_BASE));
        assert!(!is_valid_mmio_base(0));
        assert!(!is_valid_mmio_base(0xFEE0_0001));
    }

    #[test]
    fn constructs_direct_map_window() {
        let apic = LocalApic::new(LOCAL_APIC_DEFAULT_BASE, 0x1000_0000).unwrap();
        assert_eq!(apic.physical_base(), LOCAL_APIC_DEFAULT_BASE);
        assert_eq!(apic.virtual_base(), 0x10EE_0000_0);
        assert!(LocalApic::new(0xFEE0_0001, 0).is_none());
    }

    #[test]
    fn rejects_virtual_address_overflow() {
        assert!(LocalApic::new(u64::MAX - 0xfff, 0x1000).is_none());
        assert!(IoApic::new(u64::MAX - 0xfff, 0x1000).is_none());
    }

    #[test]
    fn decodes_and_encodes_redirection_entries() {
        let entry = RedirectionEntry::new(0x45, 3)
            .with_delivery_mode(4)
            .with_polarity(true)
            .with_trigger_mode(true)
            .unmasked();
        let raw = entry.to_raw();
        assert_ne!(raw & (1 << 13), 0);
        assert_ne!(raw & (1 << 15), 0);
        assert_eq!(RedirectionEntry::from_raw(raw), entry);
    }

    #[test]
    fn computes_io_apic_redirection_count() {
        assert_eq!(IoApic::redirection_count(0x0017_0011), 0x18);
        assert_eq!(IoApic::redirection_count(0), 1);
    }

    #[test]
    fn validates_io_apic_register_pair_range() {
        assert!(IoApic::new(0xFEC0_0000, 0).is_some());
        assert!(IoApic::new(0xFEC0_0001, 0).is_none());
        assert!(IoApic::new(0xFEC0_0000, 0)
            .unwrap()
            .register_address(IO_APIC_WINDOW)
            .is_some());
    }

    #[test]
    fn topology_carries_first_io_apic() {
        let info = crate::madt::MadtInfo {
            local_apic_address: LOCAL_APIC_DEFAULT_BASE,
            cpus: [None; 32],
            cpu_count: 0,
            io_apics: [Some(crate::madt::IoApicEntry {
                id: 1,
                address: 0xFEC0_0000,
                global_irq_base: 0,
            }); 8],
            io_apic_count: 1,
            interrupt_overrides: [None; 16],
            interrupt_override_count: 0,
        };
        let topology = ApicTopology::from_madt(&info);
        assert_eq!(topology.local_apic_address, LOCAL_APIC_DEFAULT_BASE);
        assert_eq!(topology.io_apic_count, 1);
        assert_eq!(topology.first_io_apic_address, Some(0xFEC0_0000));
        assert_eq!(topology.first_io_apic_global_irq_base, Some(0));
        assert_eq!(topology.timer_route.unwrap().global_irq, 0);
    }
}

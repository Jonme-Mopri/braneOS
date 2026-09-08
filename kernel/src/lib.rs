// ============================================================
// Brane OS Kernel — Library Root
// ============================================================
//
// Re-exports and module declarations for the kernel crate.
// All kernel subsystems are declared here so they can be
// used by both the binary target (main.rs) and unit tests.
//
// Spec reference: PROJECT_MASTER_SPEC.md §9.2 (Kernel Core)
// ============================================================

#![no_std]

#[cfg(test)]
extern crate std;

pub mod serial;

pub mod acpi;
pub mod ai;
pub mod apic;
pub mod audit;
pub mod block;
pub mod brane;
pub mod brane_discovery;
pub mod brane_session;
pub mod context;
pub mod crypto;
pub mod dma;
pub mod dns;
pub mod fat32;
pub mod framebuffer;
pub mod gdt;
pub mod ipc;
pub mod madt;
pub mod memory;
pub mod module_loader;
pub mod net;
pub mod pci;
pub mod process;
pub mod ramfs;
pub mod sched;
pub mod security;
pub mod shell;
pub mod signal;
pub mod smp;
pub mod socket;
pub mod syscall;
pub mod tty;
pub mod usermode;
pub mod vfs;
pub mod virtio;
pub mod virtio_block;
pub mod xhci;

#[cfg(test)]
mod tests;

// -----------------------------------------------------------------------
// Kernel Time
// -----------------------------------------------------------------------

/// Returns a monotonic millisecond timestamp.
///
/// In bare-metal mode, this derives from the scheduler tick count
/// (each PIT tick ≈ 55 ms at 18.2 Hz).
/// In test mode (host), returns 0 since there is no hardware timer.
pub fn get_time_millis() -> u64 {
    #[cfg(test)]
    {
        0
    }
    #[cfg(not(test))]
    {
        // Use scheduler tick count × ~55 ms per tick (PIT at 18.2 Hz)
        let ticks = sched::SCHEDULER.lock().total_ticks();
        ticks * 55
    }
}

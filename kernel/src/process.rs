#![allow(dead_code)]
// ============================================================
// Brane OS Kernel — User Space Init
// ============================================================
//
// Provides the foundation for user-space processes.
// In the current phase, this module defines the process
// control block (PCB), process states, and a process table.
//
// Actual ring 3 transitions require page table setup and
// a return-to-user-mode trampoline (future work).
//
// Spec reference: ARCHITECTURE.md §8 (Capa 4 — User Space)
// ============================================================

use spin::Mutex;

use crate::sched::TaskId;
use crate::security::CapabilityId;

// -----------------------------------------------------------------------
// Types
// -----------------------------------------------------------------------

pub type Pid = u64;

/// Process state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    /// Process is being created.
    Creating,
    /// Process is ready to run.
    Ready,
    /// Process is currently executing.
    Running,
    /// Process is blocked on I/O or IPC.
    Blocked,
    /// Process is sleeping (timed wait).
    Sleeping,
    /// Process has exited and is awaiting cleanup.
    Zombie,
    /// Process has been terminated.
    Terminated,
}

/// Memory map of a user-space process.
#[derive(Debug, Clone, Copy)]
pub struct ProcessMemory {
    /// Start of the code segment (virtual addr).
    pub code_start: u64,
    /// Size of the code segment.
    pub code_size: u64,
    /// Start of the stack (virtual addr, grows downward).
    pub stack_top: u64,
    /// Stack size.
    pub stack_size: u64,
    /// Start of the heap (virtual addr).
    pub heap_start: u64,
    /// Current heap end (grows upward via brk/sbrk).
    pub heap_end: u64,
}

/// Stable generation and reader pins for one process address space.
///
/// The page-table mapper remains owned by the memory subsystem. This state is
/// the process-table side of the protocol: a mapping update cannot commit
/// while a user-copy pin is active, and each pin records the generation it
/// validated against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressSpaceState {
    generation: u64,
    pins: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub struct AddressSpacePin {
    pid: Pid,
    generation: u64,
}

impl AddressSpacePin {
    pub const fn pid(&self) -> Pid {
        self.pid
    }

    pub const fn generation(&self) -> u64 {
        self.generation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressSpaceError {
    NotFound,
    Busy,
    InvalidPin,
    PinCountExhausted,
    GenerationExhausted,
}

impl AddressSpaceState {
    pub const fn new() -> Self {
        Self {
            generation: 1,
            pins: 0,
        }
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }

    pub const fn pin_count(self) -> u32 {
        self.pins
    }

    fn pin(&mut self, pid: Pid) -> Result<AddressSpacePin, AddressSpaceError> {
        self.pins = self
            .pins
            .checked_add(1)
            .ok_or(AddressSpaceError::PinCountExhausted)?;
        Ok(AddressSpacePin {
            pid,
            generation: self.generation,
        })
    }

    fn release(&mut self, pin: AddressSpacePin) -> Result<(), AddressSpaceError> {
        if pin.generation != self.generation || self.pins == 0 {
            return Err(AddressSpaceError::InvalidPin);
        }
        self.pins -= 1;
        Ok(())
    }

    fn bump_generation(&mut self) -> Result<u64, AddressSpaceError> {
        if self.pins != 0 {
            return Err(AddressSpaceError::Busy);
        }
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or(AddressSpaceError::GenerationExhausted)?;
        Ok(self.generation)
    }
}

impl Default for AddressSpaceState {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessMemory {
    /// Return whether a complete user range belongs to one declared process
    /// region.  This is a metadata guard only: callers must still validate
    /// every page against the process page table before copying.
    pub fn contains_user_range(&self, start: u64, len: usize) -> bool {
        if len == 0 {
            // Empty ranges do not touch memory.  `UserRange` separately
            // enforces canonicality and the configured user address limit.
            return true;
        }

        let end = match start.checked_add(len as u64) {
            Some(end) => end,
            None => return false,
        };

        let fits = |region_start: u64, region_size: u64| {
            let region_end = match region_start.checked_add(region_size) {
                Some(end) => end,
                None => return false,
            };
            start >= region_start && end <= region_end
        };

        let stack_start = match self.stack_top.checked_sub(self.stack_size) {
            Some(start) => start,
            None => return false,
        };

        fits(self.code_start, self.code_size)
            || fits(stack_start, self.stack_size)
            || fits(
                self.heap_start,
                self.heap_end.saturating_sub(self.heap_start),
            )
    }
}

/// Process Control Block — one per user-space process.
#[derive(Debug, Clone)]
pub struct Process {
    pub pid: Pid,
    pub parent_pid: Option<Pid>,
    pub name: [u8; 32],
    pub name_len: usize,
    pub state: ProcessState,
    pub scheduler_task: TaskId,
    pub capabilities: [Option<CapabilityId>; 8],
    pub memory: ProcessMemory,
    pub address_space: AddressSpaceState,
    pub exit_code: i32,
    /// CPU ticks consumed by this process.
    pub cpu_ticks: u64,
    /// Number of syscalls issued.
    pub syscall_count: u64,
    pub signal_pending: u64,
    pub signal_blocked: u64,
    pub saved_context: Option<crate::usermode::UserContext>,
}

impl Process {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("???")
    }
}

// -----------------------------------------------------------------------
// Process Table
// -----------------------------------------------------------------------

const MAX_PROCESSES: usize = 128;

pub struct ProcessTable {
    processes: [Option<Process>; MAX_PROCESSES],
    next_pid: Pid,
}

impl Default for ProcessTable {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTable {
    pub const fn new() -> Self {
        const NONE: Option<Process> = None;
        Self {
            processes: [NONE; MAX_PROCESSES],
            next_pid: 1,
        }
    }

    /// Create a new process (returns its PID).
    pub fn create(
        &mut self,
        name: &str,
        parent: Option<Pid>,
        scheduler_task: TaskId,
    ) -> Option<Pid> {
        for slot in self.processes.iter_mut() {
            if slot.is_none() {
                let pid = self.next_pid;
                self.next_pid += 1;

                let mut name_buf = [0u8; 32];
                let len = name.len().min(32);
                name_buf[..len].copy_from_slice(&name.as_bytes()[..len]);

                *slot = Some(Process {
                    pid,
                    parent_pid: parent,
                    name: name_buf,
                    name_len: len,
                    state: ProcessState::Creating,
                    scheduler_task,
                    capabilities: [None; 8],
                    memory: ProcessMemory {
                        code_start: 0x0040_0000,
                        code_size: 0,
                        stack_top: 0x0080_0000,
                        stack_size: 4096 * 16, // 64 KiB
                        heap_start: 0x0100_0000,
                        heap_end: 0x0100_0000,
                    },
                    address_space: AddressSpaceState::new(),
                    exit_code: 0,
                    cpu_ticks: 0,
                    syscall_count: 0,
                    signal_pending: 0,
                    signal_blocked: 0,
                    saved_context: None,
                });

                crate::serial_println!("[proc] Created process '{}' (pid={})", name, pid);
                return Some(pid);
            }
        }
        None // Table full
    }

    /// Transition a process to Ready state.
    pub fn start(&mut self, pid: Pid) -> bool {
        if let Some(proc) = self.get_mut(pid) {
            proc.state = ProcessState::Ready;
            crate::serial_println!(
                "[proc] Process '{}' (pid={}) -> Ready",
                proc.name_str(),
                pid
            );
            return true;
        }
        false
    }

    /// Terminate a process.
    pub fn terminate(&mut self, pid: Pid, exit_code: i32) -> bool {
        if let Some(proc) = self.get_mut(pid) {
            proc.state = ProcessState::Terminated;
            proc.exit_code = exit_code;
            crate::serial_println!(
                "[proc] Process '{}' (pid={}) terminated (code={})",
                proc.name_str(),
                pid,
                exit_code
            );
            return true;
        }
        false
    }

    /// Get a process by PID.
    pub fn get(&self, pid: Pid) -> Option<&Process> {
        self.processes.iter().flatten().find(|p| p.pid == pid)
    }

    /// Get a mutable reference to a process by PID.
    pub fn get_mut(&mut self, pid: Pid) -> Option<&mut Process> {
        self.processes.iter_mut().flatten().find(|p| p.pid == pid)
    }

    /// Resolve a kernel-owned process PID to its scheduler task identity.
    pub fn get_task_id_by_pid(&self, pid: Pid) -> Option<TaskId> {
        self.get(pid).map(|process| process.scheduler_task)
    }

    /// Check the relationship used by `SelfOrChild` syscall rules.
    pub fn is_self_or_child(&self, caller_pid: Pid, target_pid: Pid) -> bool {
        target_pid == caller_pid
            || self.get(target_pid).and_then(|process| process.parent_pid) == Some(caller_pid)
    }

    /// Pin the process address-space generation while a caller validates and
    /// copies user memory. The process-table lock is released by the caller
    /// after this method returns; the pin remains represented in the PCB.
    pub fn pin_address_space(&mut self, pid: Pid) -> Result<AddressSpacePin, AddressSpaceError> {
        let process = self.get_mut(pid).ok_or(AddressSpaceError::NotFound)?;
        process.address_space.pin(pid)
    }

    /// Release a previously acquired address-space pin.
    pub fn release_address_space_pin(
        &mut self,
        pin: AddressSpacePin,
    ) -> Result<(), AddressSpaceError> {
        let process = self.get_mut(pin.pid).ok_or(AddressSpaceError::NotFound)?;
        process.address_space.release(pin)
    }

    /// Commit a mapping change. Writers must wait until all user-copy pins are
    /// released, which makes a generation change observable to later callers.
    pub fn bump_address_space_generation(&mut self, pid: Pid) -> Result<u64, AddressSpaceError> {
        let process = self.get_mut(pid).ok_or(AddressSpaceError::NotFound)?;
        process.address_space.bump_generation()
    }

    /// Find slot index for a PID.
    pub fn slot_of(&self, pid: Pid) -> Option<usize> {
        self.processes.iter().enumerate().find_map(|(idx, opt_p)| {
            if let Some(p) = opt_p {
                if p.pid == pid {
                    return Some(idx);
                }
            }
            None
        })
    }

    /// Get PID of process associated with scheduler TaskId.
    pub fn get_pid_by_task_id(&self, task_id: TaskId) -> Option<Pid> {
        let mut matches = self
            .processes
            .iter()
            .flatten()
            .filter(|p| {
                p.scheduler_task == task_id
                    && p.state != ProcessState::Terminated
                    && p.state != ProcessState::Zombie
            })
            .map(|p| p.pid);
        let pid = matches.next()?;
        // A scheduler task must belong to exactly one live process. Returning
        // None on duplicate ownership prevents GetPid from selecting an
        // arbitrary process after a stale/reused task mapping.
        if matches.next().is_some() {
            None
        } else {
            Some(pid)
        }
    }

    /// List all non-terminated processes.
    pub fn active_processes(&self) -> impl Iterator<Item = &Process> {
        self.processes
            .iter()
            .flatten()
            .filter(|p| p.state != ProcessState::Terminated && p.state != ProcessState::Zombie)
    }

    /// Number of active processes.
    pub fn active_count(&self) -> usize {
        self.active_processes().count()
    }

    /// Total processes created since boot.
    pub fn total_created(&self) -> u64 {
        self.next_pid - 1
    }
}

/// Global process table.
pub static PROCESS_TABLE: Mutex<ProcessTable> = Mutex::new(ProcessTable::new());

#[cfg(test)]
mod memory_tests {
    use super::{AddressSpaceError, ProcessMemory, ProcessTable};

    fn memory() -> ProcessMemory {
        ProcessMemory {
            code_start: 0x0040_0000,
            code_size: 0x2000,
            stack_top: 0x0080_0000,
            stack_size: 0x4000,
            heap_start: 0x0100_0000,
            heap_end: 0x0100_3000,
        }
    }

    #[test]
    fn metadata_accepts_only_ranges_fully_inside_one_region() {
        let memory = memory();
        assert!(memory.contains_user_range(0x0040_0100, 0x100));
        assert!(memory.contains_user_range(0x007f_f000, 0x100));
        assert!(memory.contains_user_range(0x0100_1000, 0x100));
        assert!(!memory.contains_user_range(0x0040_1ff0, 0x20));
        assert!(!memory.contains_user_range(0x0040_1ff0, 0x100));
    }

    #[test]
    fn metadata_rejects_overflow_and_empty_ranges_are_noops() {
        let memory = memory();
        assert!(!memory.contains_user_range(u64::MAX - 1, 4));
        assert!(memory.contains_user_range(0, 0));
    }

    #[test]
    fn address_space_pin_blocks_generation_changes_until_release() {
        let mut table = ProcessTable::new();
        let pid = table.create("mapped", None, 17).unwrap();
        let pin = table.pin_address_space(pid).unwrap();
        assert_eq!(table.get(pid).unwrap().address_space.generation(), 1);
        assert_eq!(table.get(pid).unwrap().address_space.pin_count(), 1);
        assert_eq!(
            table.bump_address_space_generation(pid),
            Err(AddressSpaceError::Busy)
        );

        table.release_address_space_pin(pin).unwrap();
        assert_eq!(table.get(pid).unwrap().address_space.pin_count(), 0);
        assert_eq!(table.bump_address_space_generation(pid), Ok(2));
        assert_eq!(table.get(pid).unwrap().address_space.generation(), 2);
    }

    #[test]
    fn address_space_pins_are_counted_independently() {
        let mut table = ProcessTable::new();
        let pid = table.create("mapped", None, 18).unwrap();
        let first = table.pin_address_space(pid).unwrap();
        let second = table.pin_address_space(pid).unwrap();
        assert_eq!(table.get(pid).unwrap().address_space.pin_count(), 2);

        table.release_address_space_pin(first).unwrap();
        assert_eq!(table.get(pid).unwrap().address_space.pin_count(), 1);
        assert_eq!(
            table.bump_address_space_generation(pid),
            Err(AddressSpaceError::Busy)
        );
        table.release_address_space_pin(second).unwrap();
        assert_eq!(table.bump_address_space_generation(pid), Ok(2));
    }

    #[test]
    fn pinning_unknown_process_fails_closed() {
        let mut table = ProcessTable::new();
        assert_eq!(
            table.pin_address_space(999),
            Err(AddressSpaceError::NotFound)
        );
        assert_eq!(
            table.bump_address_space_generation(999),
            Err(AddressSpaceError::NotFound)
        );
    }
}

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
    use super::ProcessMemory;

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
}

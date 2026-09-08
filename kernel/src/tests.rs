// ============================================================
// Brane OS Kernel — Unit Tests
// ============================================================
//
// Tests for core kernel subsystems. Run with:
//   cargo test --lib
//
// These tests run in the host environment (not bare-metal),
// so they test logic only — not hardware interactions.
// ============================================================

/// Small deterministic generator used by stress and mutation-fuzz tests.
///
/// Keeping the generator dependency-free makes the exact input stream stable
/// across local runs and CI. A failing seed can therefore be reproduced by
/// rerunning the same test.
struct DeterministicRng(u64);

impl DeterministicRng {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut value = self.0;
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        self.0 = value;
        value
    }

    fn next_usize(&mut self, upper_bound: usize) -> usize {
        (self.next_u64() as usize) % upper_bound
    }

    fn fill(&mut self, output: &mut [u8]) {
        for byte in output {
            *byte = self.next_u64() as u8;
        }
    }
}

#[cfg(test)]
mod frame_allocator_tests {
    use crate::memory::frame_allocator::BitmapFrameAllocator;
    use spin::{Mutex, MutexGuard};

    pub(super) static FRAME_ALLOCATOR_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn lock_tests() -> MutexGuard<'static, ()> {
        FRAME_ALLOCATOR_TEST_LOCK.lock()
    }

    #[test]
    fn new_allocator_has_no_free_frames() {
        let _guard = lock_tests();
        let alloc = BitmapFrameAllocator::new();
        assert_eq!(alloc.free_count(), 0);
    }

    #[test]
    fn mark_region_free_increases_count() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        // Mark 1 MiB as free (256 frames)
        alloc.mark_region_free(0, 1024 * 1024);
        assert_eq!(alloc.free_count(), 256);
    }

    #[test]
    fn mark_region_free_uses_end_address() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        alloc.mark_region_free(4096 * 10, 4096 * 15);
        assert_eq!(alloc.free_count(), 5);
        assert_eq!(alloc.allocate(), Some(4096 * 10));
    }

    #[test]
    fn allocate_returns_frame_and_decreases_count() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        alloc.mark_region_free(0, 4096 * 10); // 10 frames
        assert_eq!(alloc.free_count(), 10);

        let frame = alloc.allocate();
        assert!(frame.is_some());
        assert_eq!(alloc.free_count(), 9);
    }

    #[test]
    fn allocate_returns_none_when_empty() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        assert_eq!(alloc.allocate(), None);
    }

    #[test]
    fn allocate_below_respects_physical_limit() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        alloc.mark_region_free(0x9_0000, 0x9_2000);
        alloc.mark_region_free(0x10_0000, 0x10_2000);

        assert_eq!(alloc.allocate_below(0x10_0000), Some(0x9_0000));
        assert_eq!(alloc.allocate_below(0x9_0000), None);
        assert_eq!(alloc.allocate(), Some(0x9_1000));
    }

    #[test]
    fn contiguous_allocation_honors_gaps_alignment_and_limit() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        alloc.mark_region_free(4096, 4096 * 12);
        alloc.mark_region_used(4096 * 4, 4096 * 5);

        assert_eq!(
            alloc.allocate_contiguous_below(3, 4096 * 12, 2),
            Some(4096 * 6)
        );
        assert_eq!(alloc.free_count(), 7);
        assert_eq!(alloc.allocate_contiguous_below(4, 4096 * 6, 1), None);
        assert_eq!(alloc.allocate_contiguous_below(0, 4096 * 12, 1), None);
        assert_eq!(alloc.allocate_contiguous_below(1, 4096 * 12, 3), None);
    }

    #[test]
    fn deallocate_returns_frame() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        alloc.mark_region_free(0, 4096);
        let addr = alloc.allocate().unwrap();
        assert_eq!(alloc.free_count(), 0);

        alloc.deallocate(addr);
        assert_eq!(alloc.free_count(), 1);
    }

    #[test]
    fn mark_region_used_reduces_count() {
        let _guard = lock_tests();
        let mut alloc = BitmapFrameAllocator::new();
        alloc.mark_region_free(0, 4096 * 10);
        assert_eq!(alloc.free_count(), 10);

        alloc.mark_region_used(0, 4096 * 3);
        assert_eq!(alloc.free_count(), 7);
    }
}

#[cfg(test)]
mod scheduler_tests {
    use crate::sched::{Priority, Scheduler};

    #[test]
    fn new_scheduler_has_no_tasks() {
        let sched = Scheduler::new();
        assert_eq!(sched.active_count(), 0);
    }

    #[test]
    fn add_task_returns_id() {
        let mut sched = Scheduler::new();
        let id = sched.add_task("test_task", Priority::Normal);
        assert!(id.is_some());
        assert_eq!(sched.active_count(), 1);
    }

    #[test]
    fn remove_task_succeeds() {
        let mut sched = Scheduler::new();
        let id = sched.add_task("temp_task", Priority::Low).unwrap();
        assert!(sched.remove_task(id));
        assert_eq!(sched.active_count(), 0);
    }

    #[test]
    fn remove_nonexistent_task_returns_false() {
        let mut sched = Scheduler::new();
        assert!(!sched.remove_task(9999));
    }

    #[test]
    fn tick_advances_round_robin() {
        let mut sched = Scheduler::new();
        sched.add_task("task_a", Priority::Normal);
        sched.add_task("task_b", Priority::Normal);

        sched.tick();
        assert_eq!(sched.total_ticks(), 1);

        sched.tick();
        assert_eq!(sched.total_ticks(), 2);
    }
}

#[cfg(test)]
mod syscall_tests {
    use crate::syscall::{SyscallError, SyscallNumber, SyscallResult};

    #[test]
    fn syscall_number_from_valid_raw() {
        assert_eq!(SyscallNumber::from_raw(0), Some(SyscallNumber::Exit));
        assert_eq!(SyscallNumber::from_raw(2), Some(SyscallNumber::GetPid));
        assert_eq!(
            SyscallNumber::from_raw(60),
            Some(SyscallNumber::BraneDiscover)
        );
    }

    #[test]
    fn syscall_number_from_invalid_raw() {
        assert_eq!(SyscallNumber::from_raw(999), None);
        assert_eq!(SyscallNumber::from_raw(100), None);
    }

    #[test]
    fn syscall_result_to_raw() {
        let ok = SyscallResult::Ok(42);
        assert_eq!(ok.to_raw(), 42);

        let err = SyscallResult::Err(SyscallError::PermissionDenied);
        assert_eq!(err.to_raw(), -3);
    }
}

#[cfg(test)]
mod capability_tests {
    use crate::security::{CapError, CapPermissions, CapScope, CapabilityManager, RiskLevel};

    #[test]
    fn grant_and_check_capability() {
        let mut mgr = CapabilityManager::new();
        mgr.grant(
            1,
            CapScope::System,
            CapPermissions::READ,
            RiskLevel::Low,
            true,
        )
        .unwrap();

        let result = mgr.check(1, CapPermissions::READ, CapScope::System);
        assert!(result.is_ok());
    }

    #[test]
    fn check_missing_capability_fails() {
        let mgr = CapabilityManager::new();
        let result = mgr.check(1, CapPermissions::WRITE, CapScope::System);
        assert_eq!(result, Err(CapError::PermissionDenied));
    }

    #[test]
    fn revoke_capability() {
        let mut mgr = CapabilityManager::new();
        let id = mgr
            .grant(
                1,
                CapScope::System,
                CapPermissions::READ,
                RiskLevel::Low,
                true,
            )
            .unwrap();
        assert!(mgr.revoke(id).is_ok());
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn revoke_non_revocable_fails() {
        let mut mgr = CapabilityManager::new();
        let id = mgr
            .grant(
                1,
                CapScope::System,
                CapPermissions::READ,
                RiskLevel::Low,
                false,
            )
            .unwrap();
        assert_eq!(mgr.revoke(id), Err(CapError::PermissionDenied));
    }

    #[test]
    fn permission_union_and_has() {
        let perm = CapPermissions::READ.union(CapPermissions::WRITE);
        assert!(perm.has(CapPermissions::READ));
        assert!(perm.has(CapPermissions::WRITE));
        assert!(!perm.has(CapPermissions::EXECUTE));
    }
}

#[cfg(test)]
mod ipc_tests {
    use crate::ipc::{IpcMessage, MessageType};

    #[test]
    fn create_message() {
        let msg = IpcMessage::new(1, 2, MessageType::Notification, b"hello");
        assert!(msg.is_ok());
        let msg = msg.unwrap();
        assert_eq!(msg.sender, 1);
        assert_eq!(msg.receiver, 2);
        assert_eq!(msg.data(), b"hello");
    }

    #[test]
    fn message_too_large_fails() {
        let big_data = [0u8; 5000]; // > MAX_PAYLOAD (4096)
        let msg = IpcMessage::new(1, 2, MessageType::Request, &big_data);
        assert!(msg.is_err());
    }

    #[test]
    fn message_type_from_raw() {
        assert_eq!(MessageType::from_raw(0), Some(MessageType::Request));
        assert_eq!(MessageType::from_raw(3), Some(MessageType::BraneRelay));
        assert_eq!(MessageType::from_raw(99), None);
    }
}

#[cfg(test)]
mod module_loader_tests {
    use crate::module_loader::{ModuleError, ModuleLoader, ModuleStatus};

    #[test]
    fn load_module() {
        let mut loader = ModuleLoader::new();
        let id = loader.load("test_mod", (1, 0, 0), &[]);
        assert!(id.is_ok());
        assert_eq!(loader.loaded_count(), 1);
    }

    #[test]
    fn duplicate_module_fails() {
        let mut loader = ModuleLoader::new();
        loader.load("test_mod", (1, 0, 0), &[]).unwrap();
        let result = loader.load("test_mod", (2, 0, 0), &[]);
        assert_eq!(result, Err(ModuleError::AlreadyLoaded));
    }

    #[test]
    fn unload_module() {
        let mut loader = ModuleLoader::new();
        let id = loader.load("temp_mod", (1, 0, 0), &[]).unwrap();
        assert!(loader.unload(id).is_ok());
        assert_eq!(loader.loaded_count(), 0);
    }

    #[test]
    fn unload_with_dependents_fails() {
        let mut loader = ModuleLoader::new();
        let base = loader.load("base", (1, 0, 0), &[]).unwrap();
        loader.load("child", (1, 0, 0), &[base]).unwrap();
        assert_eq!(loader.unload(base), Err(ModuleError::HasDependents));
    }

    #[test]
    fn start_and_suspend_module() {
        let mut loader = ModuleLoader::new();
        let id = loader.load("svc", (1, 0, 0), &[]).unwrap();
        assert!(loader.start(id).is_ok());
        assert_eq!(loader.info(id).unwrap().status, ModuleStatus::Running);
        assert!(loader.suspend(id).is_ok());
        assert_eq!(loader.info(id).unwrap().status, ModuleStatus::Suspended);
    }
}

#[cfg(test)]
mod brane_tests {
    use crate::brane::{
        BraneError, BraneManager, BraneMessage, BraneMessageType, BraneType, Transport,
    };

    #[test]
    fn discover_brane() {
        let mut mgr = BraneManager::new();
        let id = mgr.register_discovered(
            "test-phone",
            BraneType::Companion,
            Transport::Bluetooth,
            0x07,
            90,
        );
        assert!(id.is_ok());
        assert_eq!(mgr.discovered_count(), 1);
    }

    #[test]
    fn connect_to_brane() {
        let mut mgr = BraneManager::new();
        mgr.set_local_id(1);
        let brane_id = mgr
            .register_discovered("srv", BraneType::Peer, Transport::TcpIp, 0xFF, 100)
            .unwrap();
        let session = mgr.connect(brane_id, 0);
        assert!(session.is_ok());
        assert_eq!(mgr.active_session_count(), 1);
    }

    #[test]
    fn double_connect_fails() {
        let mut mgr = BraneManager::new();
        mgr.set_local_id(1);
        let id = mgr
            .register_discovered("dev", BraneType::IoT, Transport::Ble, 0x01, 50)
            .unwrap();
        mgr.connect(id, 0).unwrap();
        assert_eq!(mgr.connect(id, 0), Err(BraneError::AlreadyConnected));
    }

    #[test]
    fn create_brane_message() {
        let msg = BraneMessage::new(BraneMessageType::Data, 1, 2, 1, b"payload");
        assert!(msg.is_ok());
        assert_eq!(msg.unwrap().data(), b"payload");
    }
}

#[cfg(test)]
mod process_tests {
    use crate::process::ProcessTable;

    #[test]
    fn create_process() {
        let mut table = ProcessTable::new();
        let pid = table.create("init", None, 0);
        assert!(pid.is_some());
        assert_eq!(table.active_count(), 1);
    }

    #[test]
    fn start_process() {
        let mut table = ProcessTable::new();
        let pid = table.create("svc", None, 0).unwrap();
        assert!(table.start(pid));
    }

    #[test]
    fn terminate_process() {
        let mut table = ProcessTable::new();
        let pid = table.create("temp", None, 0).unwrap();
        table.start(pid);
        assert!(table.terminate(pid, 0));
        assert_eq!(table.active_count(), 0);
    }
}

#[cfg(test)]
mod ai_tests {
    use crate::ai::{AiCategory, AiEngine, AiMode, AiSeverity};

    #[test]
    fn default_mode_is_observe_only() {
        let engine = AiEngine::new();
        assert_eq!(engine.mode(), AiMode::ObserveOnly);
    }

    #[test]
    fn disabled_mode_ignores_observations() {
        let mut engine = AiEngine::new();
        engine.set_mode(AiMode::Disabled);
        let id = engine.observe(AiCategory::Resource, AiSeverity::Info, "test", None);
        assert_eq!(id, 0);
    }

    #[test]
    fn observe_returns_incrementing_ids() {
        let mut engine = AiEngine::new();
        let id1 = engine.observe(AiCategory::Security, AiSeverity::Low, "evt1", None);
        let id2 = engine.observe(AiCategory::Security, AiSeverity::Low, "evt2", None);
        assert_eq!(id2, id1 + 1);
    }
}

// -----------------------------------------------------------------------
// Context switching tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod context_tests {
    use crate::context::{KernelStack, TaskContext};

    #[test]
    fn empty_context_is_all_zeros() {
        let ctx = TaskContext::empty();
        assert_eq!(ctx.rbx, 0);
        assert_eq!(ctx.r12, 0);
        assert_eq!(ctx.r13, 0);
        assert_eq!(ctx.r14, 0);
        assert_eq!(ctx.r15, 0);
        assert_eq!(ctx.rbp, 0);
        assert_eq!(ctx.rsp, 0);
        assert_eq!(ctx.rip, 0);
    }

    #[test]
    fn new_task_context_sets_rip() {
        let fake_entry: u64 = 0xDEAD_BEEF_0000_1234;
        let stack_top: u64 = 0xFFFF_8000_0001_0000;
        let ctx = TaskContext::new_task(stack_top, fake_entry);
        assert_eq!(ctx.rip, fake_entry);
    }

    #[test]
    fn new_task_context_rsp_below_stack_top() {
        let stack_top: u64 = 0xFFFF_8000_0001_0000;
        let ctx = TaskContext::new_task(stack_top, 0x1000);
        assert_eq!(ctx.rsp, stack_top - 16);
        assert_eq!(
            (ctx.rsp + 8) % 16,
            8,
            "restored entry must satisfy the System V stack convention"
        );
    }

    #[test]
    fn new_task_context_rbp_equals_stack_top() {
        let stack_top: u64 = 0xFFFF_8000_0001_0000;
        let ctx = TaskContext::new_task(stack_top, 0x1000);
        assert_eq!(ctx.rbp, stack_top);
    }

    #[test]
    fn new_task_callee_regs_zero() {
        let ctx = TaskContext::new_task(0xFFFF_0000, 0x1000);
        assert_eq!(ctx.rbx, 0);
        assert_eq!(ctx.r12, 0);
        assert_eq!(ctx.r13, 0);
        assert_eq!(ctx.r14, 0);
        assert_eq!(ctx.r15, 0);
    }

    #[test]
    fn context_is_copy() {
        let ctx = TaskContext::new_task(0x1_0000, 0x2000);
        let ctx2 = ctx;
        assert_eq!(ctx.rip, ctx2.rip);
        assert_eq!(ctx.rsp, ctx2.rsp);
    }

    #[test]
    fn kernel_stack_size_is_16_kib() {
        assert_eq!(KernelStack::SIZE, 16 * 1024);
    }

    #[test]
    fn kernel_stack_top_above_base() {
        let stack = KernelStack::new();
        let base = stack.base_ptr();
        assert!(stack.top() > base);
    }

    #[test]
    fn kernel_stack_top_within_bounds() {
        let stack = KernelStack::new();
        let base = stack.base_ptr();
        let end = base + KernelStack::SIZE as u64;
        assert!(stack.top() <= end);
    }

    #[test]
    fn kernel_stack_top_is_16_byte_aligned() {
        let stack = KernelStack::new();
        assert_eq!(stack.top() % 16, 0, "stack top must be 16-byte aligned");
    }
}

// -----------------------------------------------------------------------
// Scheduler context-switch integration tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod scheduler_context_tests {
    use crate::sched::{Priority, Scheduler, TaskState};

    #[test]
    fn add_boot_task_has_zero_rsp() {
        let mut sched = Scheduler::new();
        let id = sched.add_task("boot", Priority::System).unwrap();
        let snap = sched.snapshot();
        let t = snap.iter().flatten().find(|t| t.id == id).unwrap();
        assert_eq!(t.rsp, 0, "boot task has no real RSP until first switch");
        assert_eq!(t.rip, 0, "boot task RIP is zero until first switch");
    }

    #[test]
    fn add_task_with_entry_has_nonzero_rip() {
        fn fake_task() -> ! {
            loop {}
        }
        let mut sched = Scheduler::new();
        let id = sched
            .add_task_with_entry("worker", Priority::Normal, fake_task)
            .unwrap();
        let snap = sched.snapshot();
        let t = snap.iter().flatten().find(|t| t.id == id).unwrap();
        assert_ne!(t.rip, 0, "entry task should have a valid RIP");
        assert_ne!(t.rsp, 0, "entry task should have an allocated RSP");
    }

    #[test]
    fn prepare_switch_returns_none_with_one_task() {
        let mut sched = Scheduler::new();
        sched.add_task("solo", Priority::System);
        assert!(sched.prepare_switch().is_none());
    }

    #[test]
    fn prepare_switch_advances_current_task() {
        let mut sched = Scheduler::new();
        sched.add_task("task_a", Priority::Normal);
        sched.add_task("task_b", Priority::Normal);
        sched.tick();
        let before = sched.current_task_id();
        let pair = sched.prepare_switch();
        assert!(pair.is_some());
        let after = sched.current_task_id();
        assert_ne!(before, after, "current task should have changed");
    }

    #[test]
    fn blocked_task_not_selected_for_switch() {
        let mut sched = Scheduler::new();
        sched.add_task("task_a", Priority::Normal).unwrap();
        let id_b = sched.add_task("task_b", Priority::Normal).unwrap();
        sched.add_task("task_c", Priority::Normal).unwrap();
        sched.tick();
        sched.block_task(id_b);
        for _ in 0..6 {
            let _ = sched.prepare_switch();
            if let Some(cur) = sched.current_task_id() {
                assert_ne!(cur, id_b, "blocked task must not be scheduled");
            }
        }
    }

    #[test]
    fn unblock_task_makes_it_ready() {
        let mut sched = Scheduler::new();
        sched.add_task("task_a", Priority::Normal).unwrap();
        let id_b = sched.add_task("task_b", Priority::Normal).unwrap();
        sched.block_task(id_b);
        assert!(sched.unblock_task(id_b));
        let snap = sched.snapshot();
        let t = snap.iter().flatten().find(|t| t.id == id_b).unwrap();
        assert_eq!(t.state, TaskState::Ready);
    }

    #[test]
    fn snapshot_reflects_all_tasks() {
        let mut sched = Scheduler::new();
        sched.add_task("a", Priority::Low);
        sched.add_task("b", Priority::Normal);
        sched.add_task("c", Priority::High);
        let snap = sched.snapshot();
        assert_eq!(snap.iter().flatten().count(), 3);
    }

    #[test]
    fn remove_task_decreases_count() {
        let mut sched = Scheduler::new();
        let id = sched.add_task("tmp", Priority::Low).unwrap();
        assert_eq!(sched.active_count(), 1);
        sched.remove_task(id);
        assert_eq!(sched.active_count(), 0);
    }
}
// -----------------------------------------------------------------------
// FAT32 parser tests
// -----------------------------------------------------------------------

#[cfg(test)]
mod fat32_tests {
    use crate::fat32::{Fat32BootSector, PartitionEntry};

    #[test]
    fn parse_mbr_valid_partition() {
        let mut data = [0u8; 16];
        data[0] = 0x80; // active
        data[4] = 0x0B; // FAT32 (CHS)
        data[8..12].copy_from_slice(&2048u32.to_le_bytes()); // start LBA
        data[12..16].copy_from_slice(&102400u32.to_le_bytes()); // sectors

        let entry = PartitionEntry::parse(&data).expect("should parse valid partition");
        assert_eq!(entry.status, 0x80);
        assert_eq!(entry.partition_type, 0x0B);
        assert_eq!(entry.start_lba, 2048);
        assert_eq!(entry.sector_count, 102400);
    }

    #[test]
    fn parse_mbr_empty_partition() {
        let data = [0u8; 16];
        assert!(
            PartitionEntry::parse(&data).is_none(),
            "zeroed partition should be None"
        );
    }

    #[test]
    fn parse_boot_sector_invalid_signature() {
        let data = [0u8; 512];
        assert!(
            Fat32BootSector::parse(&data).is_none(),
            "missing 0x55AA signature"
        );
    }

    #[test]
    fn parse_boot_sector_valid() {
        let mut data = [0u8; 512];
        data[510] = 0x55;
        data[511] = 0xAA;

        // Bytes per sector
        data[11..13].copy_from_slice(&512u16.to_le_bytes());
        // Sectors per cluster
        data[13] = 8;
        // Reserved sectors
        data[14..16].copy_from_slice(&32u16.to_le_bytes());
        // FAT count
        data[16] = 2;
        // Total sectors 32
        data[32..36].copy_from_slice(&200000u32.to_le_bytes());
        // Sectors per FAT 32
        data[36..40].copy_from_slice(&1000u32.to_le_bytes());

        // Volume label "BRANE_OS   "
        let label = b"BRANE_OS   ";
        data[71..82].copy_from_slice(label);

        // FS Type "FAT32   "
        let fstype = b"FAT32   ";
        data[82..90].copy_from_slice(fstype);

        let bs = Fat32BootSector::parse(&data).expect("should parse valid boot sector");
        assert_eq!(bs.bytes_per_sector, 512);
        assert_eq!(bs.sectors_per_cluster, 8);
        assert_eq!(bs.reserved_sectors, 32);
        assert_eq!(bs.fat_count, 2);
        assert_eq!(bs.total_sectors_32, 200000);
        assert_eq!(bs.sectors_per_fat_32, 1000);
        assert_eq!(&bs.volume_label, label);
        assert_eq!(&bs.fs_type_label, fstype);
    }
}
// -----------------------------------------------------------------------
// Integration Tests & Security Tests
// -----------------------------------------------------------------------

static INTEGRATION_TEST_LOCK: spin::Mutex<()> = spin::Mutex::new(());

#[cfg(test)]
mod integration_syscall_tests {
    use super::INTEGRATION_TEST_LOCK;
    use crate::syscall::{dispatch, SyscallContext, SyscallError, SyscallNumber, SyscallResult};

    #[repr(C)]
    struct TestStackFrame {
        ctx: SyscallContext,
        user_ctx: crate::usermode::UserContext,
    }

    fn setup_test_process(name: &str) -> (crate::process::Pid, crate::sched::TaskId) {
        let mut sched = crate::sched::SCHEDULER.lock();
        let mut p_table = crate::process::PROCESS_TABLE.lock();

        // Reset state for isolation
        *sched = crate::sched::Scheduler::new();
        *p_table = crate::process::ProcessTable::new();
        *crate::signal::SIGNAL_MANAGER.lock() = crate::signal::SignalManager::new();

        let task_id = sched
            .add_task(name, crate::sched::Priority::Normal)
            .unwrap();
        sched.tick(); // transition task to Running

        let pid = p_table.create(name, None, task_id).unwrap();
        p_table.start(pid);

        (pid, task_id)
    }

    #[test]
    fn syscall_dispatch_write_returns_len() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let (_pid, _task_id) = setup_test_process("test_write");
        let ctx = SyscallContext {
            number: SyscallNumber::Write as u64,
            arg1: 1, // fd = stdout
            arg2: 0x1000,
            arg3: 42,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx);
        match res {
            SyscallResult::Ok(len) => assert_eq!(len, 42),
            _ => panic!("Expected Ok(42)"),
        }
    }

    #[test]
    fn syscall_yield_triggers_scheduler_tick() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let (_pid, _task_id) = setup_test_process("test_yield");
        let before_ticks = crate::sched::SCHEDULER.lock().total_ticks();
        let ctx = SyscallContext {
            number: SyscallNumber::Yield as u64,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx);
        match res {
            SyscallResult::Ok(_) => {
                let after_ticks = crate::sched::SCHEDULER.lock().total_ticks();
                assert_eq!(after_ticks, before_ticks + 1);
            }
            _ => panic!("Expected yield success"),
        }
    }

    #[test]
    fn syscall_unknown_returns_invalid() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let (_pid, _task_id) = setup_test_process("test_unknown");
        let ctx = SyscallContext {
            number: 999,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx);
        match res {
            SyscallResult::Err(e) => assert_eq!(e, SyscallError::InvalidSyscall),
            _ => panic!("Expected InvalidSyscall"),
        }
    }

    #[test]
    fn process_create_then_exit_syscall() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let (_pid, _task_id) = setup_test_process("test_exit");
        let ctx = SyscallContext {
            number: SyscallNumber::Exit as u64,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx);
        assert!(matches!(res, SyscallResult::Ok(0)));
    }

    #[test]
    fn getpid_returns_current_task() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let (_pid, task_id) = setup_test_process("test_getpid");
        let ctx = SyscallContext {
            number: SyscallNumber::GetPid as u64,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx);
        match res {
            SyscallResult::Ok(id) => assert_eq!(id, task_id),
            _ => panic!("Expected Ok(task_id)"),
        }
    }

    #[test]
    fn sigaction_and_kill_flow() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let (pid, _task_id) = setup_test_process("test_sigaction");

        // Install handler
        let ctx_action = SyscallContext {
            number: SyscallNumber::SigAction as u64,
            arg1: crate::signal::Signal::Usr1 as u64,
            arg2: 0x5555_0000,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx_action);
        assert!(matches!(res, SyscallResult::Ok(0)));

        // Send signal directly to process (avoiding dispatch self-delivery on a non-frame context)
        crate::signal::SIGNAL_MANAGER
            .lock()
            .send(pid, crate::signal::Signal::Usr1)
            .unwrap();

        // Trigger delivery via gettime (neutral syscall)
        let ctx_time = SyscallContext {
            number: SyscallNumber::GetTime as u64,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let mut frame = TestStackFrame {
            ctx: ctx_time,
            user_ctx: crate::usermode::UserContext::empty(),
        };
        frame.user_ctx.rax = 99; // set standard register value

        let res = dispatch(&frame.ctx);
        // Dispatch redirects execution, resulting in Ok(0)
        assert!(matches!(res, SyscallResult::Ok(0)));

        // RIP (rcx) should now point to handler, RDI should have the signal number
        assert_eq!(frame.user_ctx.rcx, 0x5555_0000);
        assert_eq!(frame.user_ctx.rdi, crate::signal::Signal::Usr1 as u64);

        // SigReturn restores the original UserContext (including rax = original syscall return value)
        let ctx_return = SyscallContext {
            number: SyscallNumber::SigReturn as u64,
            arg1: 0,
            arg2: 0,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let frame_return = TestStackFrame {
            ctx: ctx_return,
            user_ctx: crate::usermode::UserContext::empty(),
        };

        let res_return = dispatch(&frame_return.ctx);
        // It should restore the original RAX (since the gettime syscall ticks count is returned)
        let ticks = crate::sched::SCHEDULER.lock().total_ticks();
        assert_eq!(res_return.to_raw(), ticks as i64);
    }

    #[test]
    fn sigprocmask_flow() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let (pid, _task_id) = setup_test_process("test_sigprocmask");

        // Set up SIG_BLOCK (1) for SIGUSR1 (10)
        let mask = 1 << (crate::signal::Signal::Usr1 as u8);
        let ctx_block = SyscallContext {
            number: SyscallNumber::SigProcMask as u64,
            arg1: 1, // SIG_BLOCK
            arg2: mask,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx_block);
        assert_eq!(res.to_raw(), 0); // old mask was 0

        // Check that signal_blocked is updated
        {
            let p_table = crate::process::PROCESS_TABLE.lock();
            let proc = p_table.get(pid).unwrap();
            assert_eq!(proc.signal_blocked, mask);
        }

        // Try to block SIGKILL (9) - should be ignored/filtered out
        let kill_mask = 1 << (crate::signal::Signal::Kill as u8);
        let ctx_block_kill = SyscallContext {
            number: SyscallNumber::SigProcMask as u64,
            arg1: 1, // SIG_BLOCK
            arg2: kill_mask,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx_block_kill);
        assert_eq!(res.to_raw(), mask as i64); // old mask was Usr1

        // Check that SIGKILL is NOT blocked
        {
            let p_table = crate::process::PROCESS_TABLE.lock();
            let proc = p_table.get(pid).unwrap();
            assert_eq!(proc.signal_blocked, mask); // still only Usr1 blocked
        }

        // Set mask directly using SIG_SETMASK (3) to Usr2 (12)
        let mask_usr2 = 1 << (crate::signal::Signal::Usr2 as u8);
        let ctx_set = SyscallContext {
            number: SyscallNumber::SigProcMask as u64,
            arg1: 3, // SIG_SETMASK
            arg2: mask_usr2,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx_set);
        assert_eq!(res.to_raw(), mask as i64); // old mask was Usr1

        // Check that mask is now Usr2
        {
            let p_table = crate::process::PROCESS_TABLE.lock();
            let proc = p_table.get(pid).unwrap();
            assert_eq!(proc.signal_blocked, mask_usr2);
        }

        // Unblock SIGUSR2 using SIG_UNBLOCK (2)
        let ctx_unblock = SyscallContext {
            number: SyscallNumber::SigProcMask as u64,
            arg1: 2, // SIG_UNBLOCK
            arg2: mask_usr2,
            arg3: 0,
            arg4: 0,
            arg5: 0,
        };
        let res = dispatch(&ctx_unblock);
        assert_eq!(res.to_raw(), mask_usr2 as i64); // old mask was Usr2

        // Check that mask is now 0
        {
            let p_table = crate::process::PROCESS_TABLE.lock();
            let proc = p_table.get(pid).unwrap();
            assert_eq!(proc.signal_blocked, 0);
        }
    }
}

#[cfg(test)]
mod integration_ipc_tests {
    use super::INTEGRATION_TEST_LOCK;

    #[test]
    fn ipc_send_recv_roundtrip() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let mut ipc_mgr = crate::ipc::IPC.lock();

        // Safely clear queues
        while ipc_mgr.recv(1).is_ok() {}
        while ipc_mgr.recv(2).is_ok() {}

        let msg =
            crate::ipc::IpcMessage::new(1, 2, crate::ipc::MessageType::Request, b"Hello World!")
                .unwrap();

        let res = ipc_mgr.send(msg);
        assert!(matches!(res, crate::syscall::SyscallResult::Ok(0)));
        assert_eq!(ipc_mgr.pending_count(2), 1);

        let received = ipc_mgr.recv(2).unwrap();
        assert_eq!(received.sender, 1);
        assert_eq!(received.receiver, 2);
        assert_eq!(received.msg_type, crate::ipc::MessageType::Request);
        assert_eq!(received.data(), b"Hello World!");
        assert_eq!(ipc_mgr.pending_count(2), 0);
    }
}

#[cfg(test)]
mod security_capability_tests {
    use super::INTEGRATION_TEST_LOCK;
    use crate::security::{CapError, CapPermissions, CapScope, CapabilityManager, RiskLevel};

    #[test]
    fn deny_ipc_send_without_cap() {
        let mgr = CapabilityManager::new();
        let res = mgr.check(1, CapPermissions::IPC_SEND, CapScope::Process(2));
        assert_eq!(res, Err(CapError::PermissionDenied));
    }

    #[test]
    fn deny_brane_connect_without_cap() {
        let mgr = CapabilityManager::new();
        let res = mgr.check(1, CapPermissions::BRANE_CONNECT, CapScope::Brane(42));
        assert_eq!(res, Err(CapError::PermissionDenied));
    }

    #[test]
    fn revoked_cap_is_denied() {
        let mut mgr = CapabilityManager::new();
        let cap_id = mgr
            .grant(
                1,
                CapScope::System,
                CapPermissions::WRITE,
                RiskLevel::Medium,
                true,
            )
            .unwrap();
        assert!(mgr
            .check(1, CapPermissions::WRITE, CapScope::System)
            .is_ok());

        mgr.revoke(cap_id).unwrap();
        assert_eq!(
            mgr.check(1, CapPermissions::WRITE, CapScope::System),
            Err(CapError::PermissionDenied)
        );
    }

    #[test]
    fn non_revocable_cap_cannot_be_revoked() {
        let mut mgr = CapabilityManager::new();
        let cap_id = mgr
            .grant(
                1,
                CapScope::System,
                CapPermissions::EXECUTE,
                RiskLevel::High,
                false,
            )
            .unwrap();

        let res = mgr.revoke(cap_id);
        assert_eq!(res, Err(CapError::PermissionDenied));
    }

    #[test]
    fn audit_records_capability_grant() {
        let _guard = INTEGRATION_TEST_LOCK.lock();
        let mut mgr = CapabilityManager::new();
        let before_events = crate::audit::AUDIT.lock().total_events();

        let _cap_id = mgr
            .grant(
                1,
                CapScope::System,
                CapPermissions::GRANT,
                RiskLevel::Critical,
                true,
            )
            .unwrap();

        let after_events = crate::audit::AUDIT.lock().total_events();
        assert_eq!(after_events, before_events + 1);

        // Inspect the last event
        let audit_log = crate::audit::AUDIT.lock();
        let mut events = audit_log.last_n(1);
        let event = events.next().expect("Expected at least one event");
        assert!(matches!(
            event.action,
            crate::audit::AuditAction::CapabilityGranted(_)
        ));
        assert!(matches!(event.result, crate::audit::AuditResult::Success));
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::DeterministicRng;
    use crate::brane_discovery::{DiscoveryPacket, PacketType};
    use crate::brane_session::{SessionPacket, SessionPacketType};
    use crate::fat32::{Fat32BootSector, PartitionEntry, SECTOR_SIZE};
    use std::string::String;

    const FUZZ_CASES: usize = 25_000;
    const MAX_INPUT: usize = 768;

    #[test]
    fn fuzz_untrusted_parsers_are_total() {
        let mut rng = DeterministicRng::new(0xB4A9_E5E5_5EED_2026);
        let mut storage = [0u8; MAX_INPUT];

        for case in 0..FUZZ_CASES {
            let len = rng.next_usize(MAX_INPUT + 1);
            rng.fill(&mut storage[..len]);

            // Periodically retain the FAT signature so the deeper boot-sector
            // field parser is exercised, rather than only its early reject.
            if case % 4 == 0 && len >= SECTOR_SIZE {
                storage[510] = 0x55;
                storage[511] = 0xAA;
            }

            let input = &storage[..len];
            let _ = PartitionEntry::parse(input);
            let _ = Fat32BootSector::parse(input);
            let _ = DiscoveryPacket::parse(input);
            let parsed = SessionPacket::parse(input);

            if let Some((packet, consumed)) = parsed {
                assert!((4..=input.len()).contains(&consumed));
                assert_eq!(packet.payload.len(), consumed - 4);
            }
        }
    }

    #[test]
    fn fuzz_valid_protocol_packets_roundtrip() {
        let mut rng = DeterministicRng::new(0x5E55_10A5_CAFE_BABE);

        for case in 0..10_000 {
            let payload_len = rng.next_usize(1025);
            let mut payload = std::vec![0; payload_len];
            rng.fill(&mut payload);

            let ptype = match case % 6 {
                0 => SessionPacketType::HandshakeInit,
                1 => SessionPacketType::HandshakeResponse,
                2 => SessionPacketType::CapabilityExchange,
                3 => SessionPacketType::EncryptedData,
                4 => SessionPacketType::Alert,
                _ => SessionPacketType::Disconnect,
            };
            let encoded = SessionPacket {
                ptype,
                payload: payload.clone(),
            }
            .to_bytes();
            let (decoded, consumed) = SessionPacket::parse(&encoded).expect("valid packet");
            assert_eq!(consumed, encoded.len());
            assert_eq!(decoded.ptype, ptype);
            assert_eq!(decoded.payload, payload);

            let discovery = DiscoveryPacket {
                ptype: if case % 2 == 0 {
                    PacketType::Announce
                } else {
                    PacketType::Discover
                },
                node_id: String::from("00112233445566778899aabbccddeeff"),
                name: String::from("brane-node"),
                capabilities: String::from("IPC_SEND,BRANE_CONNECT"),
            };
            let discovery_bytes = discovery.to_bytes();
            let reparsed = DiscoveryPacket::parse(&discovery_bytes).expect("valid discovery");
            assert_eq!(reparsed.ptype, discovery.ptype);
            assert_eq!(reparsed.node_id, discovery.node_id);
            assert_eq!(reparsed.name, discovery.name);
            assert_eq!(reparsed.capabilities, discovery.capabilities);
        }
    }
}

#[cfg(test)]
mod stress_tests {
    use super::frame_allocator_tests::FRAME_ALLOCATOR_TEST_LOCK;
    use super::DeterministicRng;
    use crate::ipc::{IpcManager, IpcMessage, MessageType};
    use crate::memory::frame_allocator::{BitmapFrameAllocator, FRAME_SIZE};
    use crate::sched::{DispatchResult, MultiCoreScheduler, TaskId};
    use crate::syscall::{SyscallError, SyscallResult};
    use std::format;
    use std::string::String;
    use std::sync::{Arc, Barrier, Mutex};
    use std::vec::Vec;

    const MODEL_FRAMES: usize = 1024;
    const ALLOCATOR_OPERATIONS: usize = 50_000;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum FrameState {
        Reserved,
        Free,
        Allocated,
    }

    #[test]
    fn stress_frame_allocator_matches_reference_model() {
        let _guard = FRAME_ALLOCATOR_TEST_LOCK.lock();
        let mut rng = DeterministicRng::new(0xA110_CA70_5EED_0001);
        let mut allocator = BitmapFrameAllocator::new();
        let mut model = [FrameState::Reserved; MODEL_FRAMES];
        let mut allocated = Vec::new();

        for _ in 0..ALLOCATOR_OPERATIONS {
            match rng.next_usize(5) {
                0 => {
                    let start = rng.next_usize(MODEL_FRAMES);
                    let count = rng.next_usize(32) + 1;
                    let end = (start + count).min(MODEL_FRAMES);
                    allocator
                        .mark_region_free((start * FRAME_SIZE) as u64, (end * FRAME_SIZE) as u64);
                    model[start..end].fill(FrameState::Free);
                    allocated.retain(|frame| !(*frame >= start && *frame < end));
                }
                1 => {
                    let start = rng.next_usize(MODEL_FRAMES);
                    let count = rng.next_usize(32) + 1;
                    let end = (start + count).min(MODEL_FRAMES);
                    allocator
                        .mark_region_used((start * FRAME_SIZE) as u64, (end * FRAME_SIZE) as u64);
                    model[start..end].fill(FrameState::Reserved);
                    allocated.retain(|frame| !(*frame >= start && *frame < end));
                }
                2 => {
                    let expected = model.iter().position(|state| *state == FrameState::Free);
                    let actual = allocator.allocate().map(|addr| addr as usize / FRAME_SIZE);
                    assert_eq!(actual, expected);
                    if let Some(frame) = actual {
                        model[frame] = FrameState::Allocated;
                        allocated.push(frame);
                    }
                }
                3 => {
                    let limit = rng.next_usize(MODEL_FRAMES + 1);
                    let expected = model[..limit]
                        .iter()
                        .position(|state| *state == FrameState::Free);
                    let actual = allocator
                        .allocate_below((limit * FRAME_SIZE) as u64)
                        .map(|addr| addr as usize / FRAME_SIZE);
                    assert_eq!(actual, expected);
                    if let Some(frame) = actual {
                        model[frame] = FrameState::Allocated;
                        allocated.push(frame);
                    }
                }
                _ if !allocated.is_empty() => {
                    let index = rng.next_usize(allocated.len());
                    let frame = allocated.swap_remove(index);
                    allocator.deallocate((frame * FRAME_SIZE) as u64);
                    model[frame] = FrameState::Free;
                }
                _ => {}
            }

            let expected_free = model
                .iter()
                .filter(|state| **state == FrameState::Free)
                .count();
            assert_eq!(allocator.free_count(), expected_free);
        }
    }

    #[test]
    fn stress_ipc_queue_wraparound_and_backpressure() {
        std::thread::Builder::new()
            .name(String::from("ipc-stress"))
            // Construction of the fixed 64 × 16 × 4 KiB queue table may
            // temporarily require two copies before optimization.
            .stack_size(32 * 1024 * 1024)
            .spawn(run_ipc_queue_stress)
            .expect("spawn IPC stress thread")
            .join()
            .expect("IPC stress thread panicked");
    }

    fn run_ipc_queue_stress() {
        const QUEUE_CAPACITY: usize = 16;
        const CYCLES: usize = 256;
        let mut ipc = IpcManager::new();

        for cycle in 0..CYCLES {
            for sequence in 0..QUEUE_CAPACITY {
                let payload = [cycle as u8, sequence as u8];
                let message = IpcMessage::new(1, 7, MessageType::Notification, &payload)
                    .expect("bounded payload");
                assert!(matches!(ipc.send(message), SyscallResult::Ok(0)));
            }

            let overflow = IpcMessage::new(1, 7, MessageType::Notification, b"overflow")
                .expect("bounded payload");
            assert!(matches!(
                ipc.send(overflow),
                SyscallResult::Err(SyscallError::WouldBlock)
            ));
            assert_eq!(ipc.pending_count(7), QUEUE_CAPACITY);

            for sequence in 0..QUEUE_CAPACITY {
                let message = ipc.recv(7).expect("queued message");
                assert_eq!(message.data(), &[cycle as u8, sequence as u8]);
            }
            assert!(matches!(ipc.recv(7), Err(SyscallError::NoMessage)));
        }

        let expected_delivered = (CYCLES * QUEUE_CAPACITY) as u64;
        assert_eq!(
            ipc.stats(),
            (
                expected_delivered + CYCLES as u64,
                expected_delivered,
                CYCLES as u64
            )
        );
    }

    #[test]
    fn stress_multicore_dispatch_preserves_task_ownership() {
        const CPU_COUNT: usize = 4;
        const TASK_COUNT: TaskId = 32;
        const ROUNDS: usize = 2_000;

        let scheduler = Arc::new(Mutex::new(MultiCoreScheduler::new()));
        {
            let mut scheduler = scheduler.lock().expect("scheduler setup lock");
            assert_eq!(scheduler.configure(CPU_COUNT), CPU_COUNT);
            for task_id in 1..=TASK_COUNT {
                scheduler
                    .enqueue_on_cpu(0, task_id)
                    .expect("affinity enqueue");
            }
        }

        let barrier = Arc::new(Barrier::new(CPU_COUNT));
        let mut workers = Vec::new();
        for cpu in 0..CPU_COUNT {
            let scheduler = Arc::clone(&scheduler);
            let barrier = Arc::clone(&barrier);
            workers.push(
                std::thread::Builder::new()
                    .name(format!("smp-dispatch-{cpu}"))
                    .stack_size(256 * 1024)
                    .spawn(move || {
                        barrier.wait();
                        for _ in 0..ROUNDS {
                            let mut scheduler = scheduler.lock().expect("scheduler worker lock");
                            match scheduler.dispatch(cpu) {
                                DispatchResult::Dispatched(_) | DispatchResult::Stole(_) => {
                                    scheduler.complete(cpu).expect("complete quantum");
                                }
                                DispatchResult::Idle => {}
                                result => panic!("unexpected dispatch result: {result:?}"),
                            }

                            let runtime = scheduler.runtime_snapshots();
                            let owned = runtime
                                .iter()
                                .take(CPU_COUNT)
                                .filter(|cpu| cpu.current.is_some())
                                .count();
                            let queued: usize = scheduler.loads().iter().take(CPU_COUNT).sum();
                            assert_eq!(queued + owned, TASK_COUNT as usize);
                        }
                    })
                    .expect("spawn SMP dispatch worker"),
            );
        }
        for worker in workers {
            worker.join().expect("SMP dispatch worker panicked");
        }

        let scheduler = scheduler.lock().expect("scheduler final lock");
        let runtime = scheduler.runtime_snapshots();
        assert!(runtime
            .iter()
            .take(CPU_COUNT)
            .all(|cpu| cpu.current.is_none()));
        let total_dispatches: u64 = runtime
            .iter()
            .take(CPU_COUNT)
            .map(|cpu| cpu.dispatches)
            .sum();
        let total_steals: u64 = runtime.iter().take(CPU_COUNT).map(|cpu| cpu.steals).sum();
        assert_eq!(total_dispatches, (CPU_COUNT * ROUNDS) as u64);
        assert!(total_steals > 0, "idle CPUs should steal queued work");
        assert_eq!(
            scheduler.loads()[..CPU_COUNT].iter().sum::<usize>(),
            TASK_COUNT as usize
        );
    }
}

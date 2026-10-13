#![allow(dead_code)]
// ============================================================
// Brane OS Kernel — Syscall Dispatcher
// ============================================================
//
// Handles system calls from user space via the `syscall`/`sysret`
// instruction pair (or int 0x80 as fallback).
//
// All syscalls pass through this dispatcher, which:
//   1. Reads the syscall number from RAX.
//   2. Validates the capability (future: via capability_manager).
//   3. Dispatches to the appropriate handler.
//   4. Returns the result in RAX.
//
// Spec reference: ARCHITECTURE.md §5.2.4 (Syscall Dispatcher)
// ============================================================

// -----------------------------------------------------------------------
// Syscall Numbers
// -----------------------------------------------------------------------

/// System call identifiers.
///
/// Convention: grouped by subsystem in ranges of 10.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum SyscallNumber {
    // --- Process (0–9) ---
    Exit = 0,
    Yield = 1,
    GetPid = 2,
    Fork = 3,
    Exec = 4,
    WaitPid = 5,

    // --- Memory (10–19) ---
    Mmap = 10,
    Munmap = 11,

    // --- I/O (20–29) ---
    Write = 20,
    Read = 21,
    Open = 22,
    Close = 23,

    // --- IPC (30–39) ---
    Send = 30,
    Recv = 31,
    SendRecv = 32,

    // --- Capabilities (40–49) ---
    RequestCap = 40,
    ReleaseCap = 41,
    CheckCap = 42,

    // --- System (50–59) ---
    GetTime = 50,
    GetSystemInfo = 51,

    // --- Brane (60–69) ---
    BraneDiscover = 60,
    BraneConnect = 61,
    BraneSend = 62,
    BraneRecv = 63,

    // --- Signals (70–79) ---
    Kill = 70,
    SigAction = 71,
    SigReturn = 72,
    SigProcMask = 73,
}

impl SyscallNumber {
    /// Every ABI number, in numeric/contract order.
    ///
    /// Keeping this list beside the enum makes the metadata table auditable:
    /// a new syscall cannot be added without also updating the exhaustive
    /// table and its host-side coverage test.
    pub const ALL: [Self; 28] = [
        Self::Exit,
        Self::Yield,
        Self::GetPid,
        Self::Fork,
        Self::Exec,
        Self::WaitPid,
        Self::Mmap,
        Self::Munmap,
        Self::Write,
        Self::Read,
        Self::Open,
        Self::Close,
        Self::Send,
        Self::Recv,
        Self::SendRecv,
        Self::RequestCap,
        Self::ReleaseCap,
        Self::CheckCap,
        Self::GetTime,
        Self::GetSystemInfo,
        Self::BraneDiscover,
        Self::BraneConnect,
        Self::BraneSend,
        Self::BraneRecv,
        Self::Kill,
        Self::SigAction,
        Self::SigReturn,
        Self::SigProcMask,
    ];

    /// Convert a raw u64 to a SyscallNumber, if valid.
    pub fn from_raw(n: u64) -> Option<Self> {
        match n {
            0 => Some(Self::Exit),
            1 => Some(Self::Yield),
            2 => Some(Self::GetPid),
            3 => Some(Self::Fork),
            4 => Some(Self::Exec),
            5 => Some(Self::WaitPid),
            10 => Some(Self::Mmap),
            11 => Some(Self::Munmap),
            20 => Some(Self::Write),
            21 => Some(Self::Read),
            22 => Some(Self::Open),
            23 => Some(Self::Close),
            30 => Some(Self::Send),
            31 => Some(Self::Recv),
            32 => Some(Self::SendRecv),
            40 => Some(Self::RequestCap),
            41 => Some(Self::ReleaseCap),
            42 => Some(Self::CheckCap),
            50 => Some(Self::GetTime),
            51 => Some(Self::GetSystemInfo),
            60 => Some(Self::BraneDiscover),
            61 => Some(Self::BraneConnect),
            62 => Some(Self::BraneSend),
            63 => Some(Self::BraneRecv),
            70 => Some(Self::Kill),
            71 => Some(Self::SigAction),
            72 => Some(Self::SigReturn),
            73 => Some(Self::SigProcMask),
            _ => None,
        }
    }
}

// -----------------------------------------------------------------------
// Static mediation metadata
// -----------------------------------------------------------------------

/// Whether a syscall has a complete implementation behind the mediation
/// boundary.  `Unimplemented` is rejected before a handler or user memory is
/// touched; it is deliberately not an alias for a permissive default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerState {
    Implemented,
    Partial,
    Unimplemented,
}

/// Stable audit grouping for the syscall result event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditClass {
    Process,
    Memory,
    Io,
    Ipc,
    Capability,
    System,
    Brane,
    Signal,
}

/// Capability permission named by a syscall policy.
///
/// The two process/memory permissions are policy vocabulary for the next
/// increment. They are intentionally separate from the current wire bitfield
/// in `security.rs` and do not change the ABI in this cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityPermission {
    Read,
    Write,
    Execute,
    Grant,
    Revoke,
    IpcSend,
    IpcRecv,
    BraneConnect,
    BraneDiscover,
    BraneSend,
    BraneRecv,
    ProcessCreate,
    ProcessSignal,
    MemoryMap,
}

/// Scope to derive from kernel-owned objects when capability checks are wired
/// into the dispatcher.  A caller never supplies this scope as an argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityScope {
    System,
    SelfProcess,
    TargetProcess,
    DestinationProcess,
    VfsService,
    BraneTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapabilityRequirement {
    pub permission: CapabilityPermission,
    pub scope: CapabilityScope,
}

/// Authorization rule attached to one syscall number.
///
/// `Deny` is available for a future reservation, but is not used as an
/// implicit table fallback. A rule for an unimplemented call still documents
/// the intended policy; `HandlerState::Unimplemented` makes the current
/// dispatcher fail closed before that policy can be reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationRule {
    Deny,
    Public,
    SelfOnly,
    SelfOrChild,
    HandleOwner,
    Capability(CapabilityRequirement),
    SelfOrCapability(CapabilityRequirement),
    SelfOrChildOrCapability(CapabilityRequirement),
    DualCapability(CapabilityRequirement, CapabilityRequirement),
}

/// Static policy and memory budget for one ABI number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyscallSpec {
    pub number: SyscallNumber,
    pub state: HandlerState,
    pub authorization: AuthorizationRule,
    pub audit_class: AuditClass,
    pub max_copy_bytes: usize,
}

const MAX_COPY_NONE: usize = 0;
const MAX_COPY_PATH: usize = 4 * 1024;
const MAX_COPY_IPC: usize = 4 * 1024;
const MAX_COPY_BUFFER: usize = 64 * 1024;

const fn cap(permission: CapabilityPermission, scope: CapabilityScope) -> AuthorizationRule {
    AuthorizationRule::Capability(CapabilityRequirement { permission, scope })
}

/// Exhaustive metadata table for all 28 reserved ABI numbers.
///
/// Do not replace this with a wildcard/default. The table is intentionally
/// explicit so a missing policy is caught by the unit test and by review.
pub const SYSCALL_SPECS: [SyscallSpec; 28] = [
    SyscallSpec {
        number: SyscallNumber::Exit,
        state: HandlerState::Partial,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Process,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::Yield,
        state: HandlerState::Implemented,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Process,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::GetPid,
        state: HandlerState::Implemented,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Process,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::Fork,
        state: HandlerState::Unimplemented,
        authorization: cap(CapabilityPermission::ProcessCreate, CapabilityScope::System),
        audit_class: AuditClass::Process,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::Exec,
        state: HandlerState::Unimplemented,
        authorization: cap(CapabilityPermission::Execute, CapabilityScope::VfsService),
        audit_class: AuditClass::Process,
        max_copy_bytes: MAX_COPY_PATH,
    },
    SyscallSpec {
        number: SyscallNumber::WaitPid,
        state: HandlerState::Unimplemented,
        authorization: AuthorizationRule::SelfOrChild,
        audit_class: AuditClass::Process,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::Mmap,
        state: HandlerState::Unimplemented,
        authorization: cap(
            CapabilityPermission::MemoryMap,
            CapabilityScope::SelfProcess,
        ),
        audit_class: AuditClass::Memory,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::Munmap,
        state: HandlerState::Unimplemented,
        authorization: cap(
            CapabilityPermission::MemoryMap,
            CapabilityScope::SelfProcess,
        ),
        audit_class: AuditClass::Memory,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::Write,
        state: HandlerState::Partial,
        authorization: cap(CapabilityPermission::Write, CapabilityScope::VfsService),
        audit_class: AuditClass::Io,
        max_copy_bytes: MAX_COPY_BUFFER,
    },
    SyscallSpec {
        number: SyscallNumber::Read,
        state: HandlerState::Unimplemented,
        authorization: cap(CapabilityPermission::Read, CapabilityScope::VfsService),
        audit_class: AuditClass::Io,
        max_copy_bytes: MAX_COPY_BUFFER,
    },
    SyscallSpec {
        number: SyscallNumber::Open,
        state: HandlerState::Unimplemented,
        authorization: cap(CapabilityPermission::Read, CapabilityScope::VfsService),
        audit_class: AuditClass::Io,
        max_copy_bytes: MAX_COPY_PATH,
    },
    SyscallSpec {
        number: SyscallNumber::Close,
        state: HandlerState::Unimplemented,
        authorization: AuthorizationRule::HandleOwner,
        audit_class: AuditClass::Io,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::Send,
        state: HandlerState::Partial,
        authorization: cap(
            CapabilityPermission::IpcSend,
            CapabilityScope::DestinationProcess,
        ),
        audit_class: AuditClass::Ipc,
        max_copy_bytes: MAX_COPY_IPC,
    },
    SyscallSpec {
        number: SyscallNumber::Recv,
        state: HandlerState::Partial,
        authorization: cap(CapabilityPermission::IpcRecv, CapabilityScope::SelfProcess),
        audit_class: AuditClass::Ipc,
        max_copy_bytes: MAX_COPY_IPC,
    },
    SyscallSpec {
        number: SyscallNumber::SendRecv,
        state: HandlerState::Unimplemented,
        authorization: AuthorizationRule::DualCapability(
            CapabilityRequirement {
                permission: CapabilityPermission::IpcSend,
                scope: CapabilityScope::DestinationProcess,
            },
            CapabilityRequirement {
                permission: CapabilityPermission::IpcRecv,
                scope: CapabilityScope::SelfProcess,
            },
        ),
        audit_class: AuditClass::Ipc,
        max_copy_bytes: MAX_COPY_IPC,
    },
    SyscallSpec {
        number: SyscallNumber::RequestCap,
        state: HandlerState::Unimplemented,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Capability,
        max_copy_bytes: MAX_COPY_IPC,
    },
    SyscallSpec {
        number: SyscallNumber::ReleaseCap,
        state: HandlerState::Unimplemented,
        authorization: AuthorizationRule::HandleOwner,
        audit_class: AuditClass::Capability,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::CheckCap,
        state: HandlerState::Unimplemented,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Capability,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::GetTime,
        state: HandlerState::Implemented,
        authorization: AuthorizationRule::Public,
        audit_class: AuditClass::System,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::GetSystemInfo,
        state: HandlerState::Implemented,
        authorization: AuthorizationRule::Public,
        audit_class: AuditClass::System,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::BraneDiscover,
        state: HandlerState::Unimplemented,
        authorization: cap(CapabilityPermission::BraneDiscover, CapabilityScope::System),
        audit_class: AuditClass::Brane,
        max_copy_bytes: MAX_COPY_BUFFER,
    },
    SyscallSpec {
        number: SyscallNumber::BraneConnect,
        state: HandlerState::Unimplemented,
        authorization: cap(
            CapabilityPermission::BraneConnect,
            CapabilityScope::BraneTarget,
        ),
        audit_class: AuditClass::Brane,
        max_copy_bytes: MAX_COPY_PATH,
    },
    SyscallSpec {
        number: SyscallNumber::BraneSend,
        state: HandlerState::Unimplemented,
        authorization: cap(
            CapabilityPermission::BraneSend,
            CapabilityScope::BraneTarget,
        ),
        audit_class: AuditClass::Brane,
        max_copy_bytes: MAX_COPY_BUFFER,
    },
    SyscallSpec {
        number: SyscallNumber::BraneRecv,
        state: HandlerState::Unimplemented,
        authorization: cap(
            CapabilityPermission::BraneRecv,
            CapabilityScope::BraneTarget,
        ),
        audit_class: AuditClass::Brane,
        max_copy_bytes: MAX_COPY_BUFFER,
    },
    SyscallSpec {
        number: SyscallNumber::Kill,
        state: HandlerState::Partial,
        authorization: AuthorizationRule::SelfOrChildOrCapability(CapabilityRequirement {
            permission: CapabilityPermission::ProcessSignal,
            scope: CapabilityScope::TargetProcess,
        }),
        audit_class: AuditClass::Signal,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::SigAction,
        state: HandlerState::Partial,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Signal,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::SigReturn,
        state: HandlerState::Partial,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Signal,
        max_copy_bytes: MAX_COPY_NONE,
    },
    SyscallSpec {
        number: SyscallNumber::SigProcMask,
        state: HandlerState::Implemented,
        authorization: AuthorizationRule::SelfOnly,
        audit_class: AuditClass::Signal,
        max_copy_bytes: MAX_COPY_NONE,
    },
];

/// Look up mediation metadata without a permissive fallback.
pub fn syscall_spec(number: SyscallNumber) -> Option<&'static SyscallSpec> {
    SYSCALL_SPECS.iter().find(|spec| spec.number == number)
}

// -----------------------------------------------------------------------
// Pure user-range validation
// -----------------------------------------------------------------------

pub const USER_CANONICAL_BITS: u8 = 48;
pub const USER_ADDRESS_MAX: u64 = (1u64 << (USER_CANONICAL_BITS - 1)) - 1;
pub const USER_PAGE_SIZE: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserAccess {
    Read,
    Write,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserRangeError {
    NullAddress,
    LengthExceedsMaximum,
    NonCanonicalStart,
    NonCanonicalEnd,
    AddressOverflow,
    OutsideUserSpace,
    InvalidUserLimit,
}

/// A validated user range. This type contains no page-table references: page
/// presence, `USER_ACCESSIBLE`, and write permission are checked by the later
/// copy/pin layer. Keeping this constructor pure makes hostile pointer cases
/// testable on the host without dereferencing the address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserRange {
    pub start: u64,
    pub len: usize,
    pub access: UserAccess,
}

impl UserRange {
    /// Validate against the canonical x86-64 low-half user range.
    pub fn new(
        start: u64,
        len: usize,
        access: UserAccess,
        max_copy_bytes: usize,
    ) -> Result<Self, UserRangeError> {
        Self::new_with_user_limit(start, len, access, max_copy_bytes, USER_ADDRESS_MAX)
    }

    /// Variant used by an address space with a lower configured user limit.
    pub fn new_with_user_limit(
        start: u64,
        len: usize,
        access: UserAccess,
        max_copy_bytes: usize,
        user_address_max: u64,
    ) -> Result<Self, UserRangeError> {
        if user_address_max > USER_ADDRESS_MAX {
            return Err(UserRangeError::InvalidUserLimit);
        }
        if start == 0 && len != 0 {
            return Err(UserRangeError::NullAddress);
        }
        if len > max_copy_bytes {
            return Err(UserRangeError::LengthExceedsMaximum);
        }
        if !is_canonical_user_address(start) {
            return Err(UserRangeError::NonCanonicalStart);
        }

        let end = start
            .checked_add(len as u64)
            .ok_or(UserRangeError::AddressOverflow)?;
        if len != 0 {
            let last = end - 1;
            if !is_canonical_user_address(last) {
                return Err(UserRangeError::NonCanonicalEnd);
            }
            let user_end = user_address_max
                .checked_add(1)
                .ok_or(UserRangeError::InvalidUserLimit)?;
            if end > user_end {
                return Err(UserRangeError::OutsideUserSpace);
            }
        } else if start > user_address_max {
            return Err(UserRangeError::OutsideUserSpace);
        }

        Ok(Self { start, len, access })
    }

    pub const fn is_empty(self) -> bool {
        self.len == 0
    }

    pub fn end_exclusive(self) -> Result<u64, UserRangeError> {
        self.start
            .checked_add(self.len as u64)
            .ok_or(UserRangeError::AddressOverflow)
    }

    /// Number of 4 KiB pages touched by the range. Empty ranges touch none.
    pub fn page_count(self) -> u64 {
        if self.len == 0 {
            return 0;
        }
        let last = self.start + self.len as u64 - 1;
        last / USER_PAGE_SIZE - self.start / USER_PAGE_SIZE + 1
    }

    pub fn crosses_page_boundary(self) -> bool {
        self.page_count() > 1
    }
}

pub fn is_canonical_user_address(address: u64) -> bool {
    // A valid user address is in the canonical low half. The explicit mask
    // keeps this helper independent of `x86_64::VirtAddr` and safe on host.
    address <= USER_ADDRESS_MAX
}

// -----------------------------------------------------------------------
// Syscall Result
// -----------------------------------------------------------------------

/// Result of a syscall execution.
#[derive(Debug, Clone, Copy)]
pub enum SyscallResult {
    /// Syscall completed successfully. Value is the return data.
    Ok(u64),
    /// Syscall failed with an error code.
    Err(SyscallError),
}

/// Error codes returned by failed syscalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum SyscallError {
    /// Unknown or invalid syscall number.
    InvalidSyscall = -1,
    /// Invalid argument provided.
    InvalidArgument = -2,
    /// Permission denied (capability check failed).
    PermissionDenied = -3,
    /// The requested resource was not found.
    NotFound = -4,
    /// Out of memory.
    OutOfMemory = -5,
    /// The operation would block and non-blocking was requested.
    WouldBlock = -6,
    /// IPC: no message available.
    NoMessage = -7,
    /// IPC: destination task not found.
    InvalidDestination = -8,
    /// Brane: not connected.
    BraneNotConnected = -9,
    /// Generic / internal error.
    Internal = -100,
}

impl SyscallResult {
    /// Convert to a raw i64 for returning via RAX.
    pub fn to_raw(self) -> i64 {
        match self {
            SyscallResult::Ok(val) => val as i64,
            SyscallResult::Err(e) => e as i64,
        }
    }
}

// -----------------------------------------------------------------------
// Dispatcher
// -----------------------------------------------------------------------

/// Syscall context — the register state at the time of the syscall.
///
/// On x86_64:
///   rax = syscall number
///   rdi = arg1, rsi = arg2, rdx = arg3, r10 = arg4, r8 = arg5
#[derive(Debug, Clone, Copy)]
pub struct SyscallContext {
    pub number: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
}

/// Main dispatcher: routes a syscall to the appropriate handler.
///
/// This is called from the low-level syscall entry point (assembly
/// stub or int 0x80 handler). Returns a result to be placed in RAX.
pub fn dispatch(ctx: &SyscallContext) -> SyscallResult {
    let syscall = match SyscallNumber::from_raw(ctx.number) {
        Some(s) => s,
        None => {
            crate::serial_println!("[syscall] UNKNOWN syscall number: {}", ctx.number);
            return SyscallResult::Err(SyscallError::InvalidSyscall);
        }
    };

    // Metadata lookup is a mandatory gate. An absent policy is an internal
    // error and every reserved-but-unimplemented number fails before a
    // handler, capability manager, or user-memory path can run.
    let spec = match syscall_spec(syscall) {
        Some(spec) => spec,
        None => {
            crate::serial_println!("[syscall] {:?} has no mediation metadata", syscall);
            return SyscallResult::Err(SyscallError::InvalidSyscall);
        }
    };
    if spec.state == HandlerState::Unimplemented {
        crate::serial_println!("[syscall] {:?} is not implemented", syscall);
        return SyscallResult::Err(SyscallError::InvalidSyscall);
    }

    crate::serial_println!(
        "[syscall] {:?}(0x{:x}, 0x{:x}, 0x{:x})",
        syscall,
        ctx.arg1,
        ctx.arg2,
        ctx.arg3
    );

    let mut result = match syscall {
        // --- Process ---
        SyscallNumber::Exit => handle_exit(ctx),
        SyscallNumber::Yield => handle_yield(ctx),
        SyscallNumber::GetPid => handle_getpid(ctx),

        // --- I/O ---
        SyscallNumber::Write => handle_write(ctx),

        // --- IPC ---
        SyscallNumber::Send => handle_ipc_send(ctx),
        SyscallNumber::Recv => handle_ipc_recv(ctx),

        // --- System ---
        SyscallNumber::GetTime => handle_get_time(ctx),
        SyscallNumber::GetSystemInfo => handle_get_system_info(ctx),

        // --- Signals ---
        SyscallNumber::Kill => handle_kill(ctx),
        SyscallNumber::SigAction => handle_sigaction(ctx),
        SyscallNumber::SigReturn => handle_sigreturn(ctx),
        SyscallNumber::SigProcMask => handle_sigprocmask(ctx),

        // --- Unimplemented ---
        _ => {
            crate::serial_println!("[syscall] {:?} not yet implemented", syscall);
            SyscallResult::Err(SyscallError::InvalidSyscall)
        }
    };

    // Deliver pending signals if any (unless Exit or SigReturn)
    if syscall != SyscallNumber::Exit && syscall != SyscallNumber::SigReturn {
        if let Some(pid) = get_current_pid() {
            let mut sig_mgr = crate::signal::SIGNAL_MANAGER.lock();
            if let Some((sig, handler_addr)) = sig_mgr.deliver_pending(pid) {
                let user_context_ptr =
                    (ctx as *const SyscallContext as u64 + 48) as *mut crate::usermode::UserContext;
                unsafe {
                    let mut p_table = crate::process::PROCESS_TABLE.lock();
                    if let Some(proc) = p_table.get_mut(pid) {
                        proc.saved_context = Some(*user_context_ptr);
                    }

                    if let Some(ref mut saved) =
                        p_table.get_mut(pid).and_then(|p| p.saved_context.as_mut())
                    {
                        saved.rax = result.to_raw() as u64;
                    }

                    let user_ctx = &mut *user_context_ptr;
                    user_ctx.rcx = handler_addr; // redirect userspace RIP
                    user_ctx.rdi = sig as u64; // signal number as first arg
                    result = SyscallResult::Ok(0);
                }
            }
        }
    }

    result
}

// -----------------------------------------------------------------------
// Syscall Handlers (stubs — to be fully implemented)
// -----------------------------------------------------------------------

fn handle_exit(_ctx: &SyscallContext) -> SyscallResult {
    crate::serial_println!("[syscall] exit() — task termination requested");
    // Future: remove task from scheduler
    SyscallResult::Ok(0)
}

fn handle_yield(_ctx: &SyscallContext) -> SyscallResult {
    // Yield the current time slice to the next task
    crate::sched::SCHEDULER.lock().tick();
    SyscallResult::Ok(0)
}

fn handle_getpid(_ctx: &SyscallContext) -> SyscallResult {
    let scheduler = crate::sched::SCHEDULER.lock();
    match scheduler.current_task() {
        Some(task) => SyscallResult::Ok(task.id),
        None => SyscallResult::Err(SyscallError::Internal),
    }
}

fn handle_write(ctx: &SyscallContext) -> SyscallResult {
    // arg1 = fd (1 = stdout/serial), arg2 = buffer ptr, arg3 = length
    let fd = ctx.arg1;
    let _buf_ptr = ctx.arg2;
    let len = ctx.arg3;

    if fd != 1 {
        return SyscallResult::Err(SyscallError::InvalidArgument);
    }

    // In kernel mode, we can't safely dereference user pointers yet.
    // For now, just acknowledge the write.
    crate::serial_println!("[syscall] write(fd={}, len={}) — stub", fd, len);
    SyscallResult::Ok(len)
}

fn handle_ipc_send(ctx: &SyscallContext) -> SyscallResult {
    // Delegate to the IPC subsystem
    let dest = ctx.arg1;
    crate::serial_println!("[syscall] ipc_send(dest={}) — routing to IPC core", dest);
    // Future: crate::ipc::send(...)
    SyscallResult::Ok(0)
}

fn handle_ipc_recv(_ctx: &SyscallContext) -> SyscallResult {
    crate::serial_println!("[syscall] ipc_recv() — routing to IPC core");
    // Future: crate::ipc::recv(...)
    SyscallResult::Err(SyscallError::NoMessage)
}

fn handle_get_time(_ctx: &SyscallContext) -> SyscallResult {
    let ticks = crate::sched::SCHEDULER.lock().total_ticks();
    SyscallResult::Ok(ticks)
}

fn handle_get_system_info(_ctx: &SyscallContext) -> SyscallResult {
    let scheduler = crate::sched::SCHEDULER.lock();
    let active = scheduler.active_count() as u64;
    SyscallResult::Ok(active)
}

/// Helper to get current running process PID.
pub fn get_current_pid() -> Option<u64> {
    let task_id = crate::sched::SCHEDULER.lock().current_task()?.id;
    let p_table = crate::process::PROCESS_TABLE.lock();
    p_table.get_pid_by_task_id(task_id)
}

fn handle_kill(ctx: &SyscallContext) -> SyscallResult {
    let pid = ctx.arg1;
    let sig_val = ctx.arg2 as u8;
    match crate::signal::Signal::try_from(sig_val) {
        Ok(sig) => match crate::signal::SIGNAL_MANAGER.lock().send(pid, sig) {
            Ok(_) => SyscallResult::Ok(0),
            Err(_) => SyscallResult::Err(SyscallError::NotFound),
        },
        Err(_) => SyscallResult::Err(SyscallError::InvalidArgument),
    }
}

fn handle_sigaction(ctx: &SyscallContext) -> SyscallResult {
    let sig_val = ctx.arg1 as u8;
    let handler_addr = ctx.arg2;

    let current_pid = match get_current_pid() {
        Some(pid) => pid,
        None => return SyscallResult::Err(SyscallError::Internal),
    };

    match crate::signal::Signal::try_from(sig_val) {
        Ok(sig) => {
            let action = if handler_addr == 0 {
                crate::signal::SignalAction::Default
            } else if handler_addr == 1 {
                crate::signal::SignalAction::Ignore
            } else {
                crate::signal::SignalAction::Handler(handler_addr)
            };

            match crate::signal::SIGNAL_MANAGER
                .lock()
                .set_action(current_pid, sig, action)
            {
                Ok(_) => SyscallResult::Ok(0),
                Err(_) => SyscallResult::Err(SyscallError::InvalidArgument),
            }
        }
        Err(_) => SyscallResult::Err(SyscallError::InvalidArgument),
    }
}

fn handle_sigreturn(ctx: &SyscallContext) -> SyscallResult {
    let current_pid = match get_current_pid() {
        Some(pid) => pid,
        None => return SyscallResult::Err(SyscallError::Internal),
    };

    let mut p_table = crate::process::PROCESS_TABLE.lock();
    if let Some(proc) = p_table.get_mut(current_pid) {
        if let Some(saved) = proc.saved_context.take() {
            unsafe {
                let user_context_ptr =
                    (ctx as *const SyscallContext as u64 + 48) as *mut crate::usermode::UserContext;
                *user_context_ptr = saved;
                SyscallResult::Ok(saved.rax)
            }
        } else {
            SyscallResult::Err(SyscallError::InvalidArgument)
        }
    } else {
        SyscallResult::Err(SyscallError::NotFound)
    }
}

fn handle_sigprocmask(ctx: &SyscallContext) -> SyscallResult {
    let how = ctx.arg1;
    let set = ctx.arg2; // bitmask of signals

    let current_pid = match get_current_pid() {
        Some(pid) => pid,
        None => return SyscallResult::Err(SyscallError::Internal),
    };

    let mut p_table = crate::process::PROCESS_TABLE.lock();
    if let Some(proc) = p_table.get_mut(current_pid) {
        let old_mask = proc.signal_blocked;
        match how {
            1 => {
                // SIG_BLOCK
                proc.signal_blocked |= set;
            }
            2 => {
                // SIG_UNBLOCK
                proc.signal_blocked &= !set;
            }
            3 => {
                // SIG_SETMASK
                proc.signal_blocked = set;
            }
            _ => return SyscallResult::Err(SyscallError::InvalidArgument),
        }
        // SIGKILL and SIGSTOP cannot be blocked
        let cannot_block =
            (1 << (crate::signal::Signal::Kill as u8)) | (1 << (crate::signal::Signal::Stop as u8));
        proc.signal_blocked &= !cannot_block;

        SyscallResult::Ok(old_mask)
    } else {
        SyscallResult::Err(SyscallError::NotFound)
    }
}

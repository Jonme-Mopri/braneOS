#![allow(dead_code)]
// ============================================================
// Brane OS Kernel — Capability Manager
// ============================================================
//
// Validates and manages capability tokens at runtime.
// Every privileged action must be authorized by a capability.
//
// Flow: task → syscall → capability_manager.check() → allow/deny
//
// Spec reference: ARCHITECTURE.md §5.2.6
// ============================================================

use spin::Mutex;

use crate::sched::TaskId;

// -----------------------------------------------------------------------
// Types
// -----------------------------------------------------------------------

pub type CapabilityId = u64;

/// Risk level associated with a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RiskLevel {
    Low = 0,
    Medium = 1,
    High = 2,
    Critical = 3,
}

/// Scope of a capability — what it applies to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapScope {
    /// Applies to a specific task.
    Process(TaskId),
    /// Applies to a named service.
    Service(u64),
    /// System-wide capability.
    System,
    /// Capability over a remote brane.
    Brane(u64),
}

/// Permissions encoded as a bitfield.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapPermissions(pub u32);

impl CapPermissions {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const GRANT: Self = Self(1 << 3);
    pub const REVOKE: Self = Self(1 << 4);
    pub const IPC_SEND: Self = Self(1 << 5);
    pub const IPC_RECV: Self = Self(1 << 6);
    pub const BRANE_CONNECT: Self = Self(1 << 7);
    pub const BRANE_DISCOVER: Self = Self(1 << 8);
    /// Internal policy bits used by syscall mediation; these are not ABI
    /// values and are never accepted from a user-supplied bitfield.
    pub const PROCESS_CREATE: Self = Self(1 << 9);
    pub const PROCESS_SIGNAL: Self = Self(1 << 10);
    pub const MEMORY_MAP: Self = Self(1 << 11);
    pub const BRANE_SEND: Self = Self(1 << 12);
    pub const BRANE_RECV: Self = Self(1 << 13);

    pub fn has(self, perm: Self) -> bool {
        self.0 & perm.0 == perm.0
    }

    pub fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// A capability token.
#[derive(Debug, Clone)]
pub struct Capability {
    pub id: CapabilityId,
    pub owner: TaskId,
    pub scope: CapScope,
    pub permissions: CapPermissions,
    pub risk_level: RiskLevel,
    pub revocable: bool,
}

// -----------------------------------------------------------------------
// Errors
// -----------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    NotFound,
    PermissionDenied,
    AlreadyRevoked,
    TableFull,
    InvalidScope,
}

// -----------------------------------------------------------------------
// Capability Manager
// -----------------------------------------------------------------------

const MAX_CAPABILITIES: usize = 256;

pub struct CapabilityManager {
    capabilities: [Option<Capability>; MAX_CAPABILITIES],
    next_id: CapabilityId,
}

impl Default for CapabilityManager {
    fn default() -> Self {
        Self::new()
    }
}

impl CapabilityManager {
    pub const fn new() -> Self {
        const NONE: Option<Capability> = None;
        Self {
            capabilities: [NONE; MAX_CAPABILITIES],
            next_id: 1,
        }
    }

    /// Grant a new capability to a task.
    pub fn grant(
        &mut self,
        owner: TaskId,
        scope: CapScope,
        permissions: CapPermissions,
        risk_level: RiskLevel,
        revocable: bool,
    ) -> Result<CapabilityId, CapError> {
        for slot in self.capabilities.iter_mut() {
            if slot.is_none() {
                let id = self.next_id;
                self.next_id += 1;
                *slot = Some(Capability {
                    id,
                    owner,
                    scope,
                    permissions,
                    risk_level,
                    revocable,
                });
                crate::serial_println!(
                    "[cap]  Granted cap #{} to task {} ({:?}, risk={:?})",
                    id,
                    owner,
                    scope,
                    risk_level
                );
                return Ok(id);
            }
        }
        Err(CapError::TableFull)
    }

    /// Check if a task holds a capability with the required permissions.
    pub fn check(
        &self,
        task: TaskId,
        required: CapPermissions,
        scope: CapScope,
    ) -> Result<CapabilityId, CapError> {
        for cap in self.capabilities.iter().flatten() {
            if cap.owner == task && cap.scope == scope && cap.permissions.has(required) {
                return Ok(cap.id);
            }
        }
        Err(CapError::PermissionDenied)
    }

    /// Revoke a capability by its ID.
    pub fn revoke(&mut self, cap_id: CapabilityId) -> Result<(), CapError> {
        for slot in self.capabilities.iter_mut() {
            if let Some(cap) = slot {
                if cap.id == cap_id {
                    if !cap.revocable {
                        return Err(CapError::PermissionDenied);
                    }
                    crate::serial_println!("[cap]  Revoked cap #{}", cap_id);
                    *slot = None;
                    return Ok(());
                }
            }
        }
        Err(CapError::NotFound)
    }

    /// List all capabilities held by a task.
    pub fn list_for_task(&self, task: TaskId) -> impl Iterator<Item = &Capability> {
        self.capabilities
            .iter()
            .filter_map(|s| s.as_ref())
            .filter(move |c| c.owner == task)
    }

    /// Total active capabilities.
    pub fn active_count(&self) -> usize {
        self.capabilities.iter().filter(|s| s.is_some()).count()
    }
}

/// Global capability manager.
pub static CAP_MANAGER: Mutex<CapabilityManager> = Mutex::new(CapabilityManager::new());

/// Grant through the global service boundary. The capability mutation is
/// committed before its audit event is appended, so CAP_MANAGER is never held
/// while the audit path snapshots scheduler time.
pub fn grant_capability(
    owner: TaskId,
    scope: CapScope,
    permissions: CapPermissions,
    risk_level: RiskLevel,
    revocable: bool,
) -> Result<CapabilityId, CapError> {
    let result = CAP_MANAGER
        .lock()
        .grant(owner, scope, permissions, risk_level, revocable);
    if let Ok(cap_id) = result {
        crate::audit::record(
            owner,
            crate::audit::AuditAction::CapabilityGranted(cap_id),
            None,
            crate::audit::AuditResult::Success,
        );
    }
    result
}

/// Revoke through the global service boundary, preserving an audit event for
/// both successful and denied revocations without holding CAP_MANAGER.
pub fn revoke_capability(cap_id: CapabilityId) -> Result<(), CapError> {
    let (owner, result) = {
        let mut manager = CAP_MANAGER.lock();
        let owner = manager
            .capabilities
            .iter()
            .flatten()
            .find(|cap| cap.id == cap_id)
            .map(|cap| cap.owner);
        let result = manager.revoke(cap_id);
        (owner, result)
    };
    let audit_result = match result {
        Ok(()) => crate::audit::AuditResult::Success,
        Err(CapError::PermissionDenied) => crate::audit::AuditResult::Denied,
        Err(error) => crate::audit::AuditResult::Error(cap_error_code(error)),
    };
    if let Some(owner) = owner {
        crate::audit::record(
            owner,
            crate::audit::AuditAction::CapabilityRevoked(cap_id),
            None,
            audit_result,
        );
    } else {
        crate::audit::record_anonymous(
            crate::audit::AuditAction::CapabilityRevoked(cap_id),
            audit_result,
        );
    }
    result
}

const fn cap_error_code(error: CapError) -> i64 {
    match error {
        CapError::NotFound => -4,
        CapError::PermissionDenied => -3,
        CapError::AlreadyRevoked => -10,
        CapError::TableFull => -5,
        CapError::InvalidScope => -2,
    }
}

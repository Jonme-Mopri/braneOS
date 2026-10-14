#![allow(dead_code)]
// ============================================================
// Brane OS Kernel — Audit Hooks
// ============================================================
//
// Audit recording is deliberately split into two operations. Callers take
// their identity/tick snapshot before taking the audit lock, then record_at()
// only mutates the ring. This keeps the audit lock from acquiring scheduler
// (or any other subsystem) locks and gives every event a stable origin.
// ============================================================

use spin::Mutex;

use crate::process::Pid;
use crate::sched::TaskId;
use crate::security::CapabilityId;

// -----------------------------------------------------------------------
// Audit Event Types
// -----------------------------------------------------------------------

/// Identity captured at the syscall/event boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditOrigin {
    pub task_id: Option<TaskId>,
    pub pid: Option<Pid>,
    pub cpu_id: u16,
}

impl AuditOrigin {
    pub const fn anonymous(cpu_id: u16) -> Self {
        Self {
            task_id: None,
            pid: None,
            cpu_id,
        }
    }
}

/// What kind of action was audited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAction {
    /// Legacy entry event retained for existing domain producers.
    SyscallInvoked(u64),
    /// Terminal syscall result, including its static mediation class.
    SyscallCompleted {
        number: u64,
        class: Option<crate::syscall::AuditClass>,
    },
    CapabilityChecked(CapabilityId),
    CapabilityGranted(CapabilityId),
    CapabilityRevoked(CapabilityId),
    IpcMessageSent {
        to: TaskId,
    },
    IpcMessageReceived {
        from: TaskId,
    },
    BraneConnected(u64),
    BraneDisconnected(u64),
    AiActionRequested(u64),
    AiActionAuthorized(u64),
    AiActionDenied(u64),
    PolicyEvaluated(u64),
    TaskCreated(TaskId),
    TaskTerminated(TaskId),
}

/// Outcome of the audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditResult {
    Success,
    Denied,
    Error(i64),
    Escalated,
}

/// A single audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditEvent {
    /// Monotonic event sequence number.
    pub seq: u64,
    /// Timer ticks captured before taking the audit lock.
    pub tick: u64,
    /// Authenticated task, when one was available.
    pub source: Option<TaskId>,
    /// Authenticated process, when one was available.
    pub pid: Option<Pid>,
    /// Logical CPU that captured the origin.
    pub cpu_id: u16,
    /// What happened.
    pub action: AuditAction,
    /// Capability used (if any).
    pub capability_used: Option<CapabilityId>,
    /// Result of the action.
    pub result: AuditResult,
}

// -----------------------------------------------------------------------
// Audit Log (Ring Buffer)
// -----------------------------------------------------------------------

pub const AUDIT_LOG_SIZE: usize = 512;

pub struct AuditLog {
    events: [Option<AuditEvent>; AUDIT_LOG_SIZE],
    head: usize,
    count: usize,
    next_seq: u64,
    total_events: u64,
    overwritten: u64,
}

impl AuditLog {
    pub const fn new() -> Self {
        const NONE: Option<AuditEvent> = None;
        Self {
            events: [NONE; AUDIT_LOG_SIZE],
            head: 0,
            count: 0,
            next_seq: 1,
            total_events: 0,
            overwritten: 0,
        }
    }

    /// Record an event using an already captured tick and identity.
    ///
    /// This method must remain free of scheduler, CPU, and other subsystem
    /// lock acquisition. The global wrapper below performs that snapshot.
    pub fn record_at(
        &mut self,
        tick: u64,
        origin: AuditOrigin,
        action: AuditAction,
        capability_used: Option<CapabilityId>,
        result: AuditResult,
    ) {
        let event = AuditEvent {
            seq: self.next_seq,
            tick,
            source: origin.task_id,
            pid: origin.pid,
            cpu_id: origin.cpu_id,
            action,
            capability_used,
            result,
        };

        let idx = (self.head + self.count) % AUDIT_LOG_SIZE;
        if self.count < AUDIT_LOG_SIZE {
            self.count += 1;
        } else {
            self.head = (self.head + 1) % AUDIT_LOG_SIZE;
            self.overwritten += 1;
        }

        self.events[idx] = Some(event);
        self.next_seq += 1;
        self.total_events += 1;
    }

    /// Get up to `n` events, newest first.
    pub fn last_n(&self, n: usize) -> impl Iterator<Item = &AuditEvent> {
        let take = n.min(self.count);
        (0..take).filter_map(move |i| {
            let idx = (self.head + self.count - 1 - i) % AUDIT_LOG_SIZE;
            self.events[idx].as_ref()
        })
    }

    /// Total events recorded since boot (including overwritten ones).
    pub fn total_events(&self) -> u64 {
        self.total_events
    }

    /// Number of events currently in the buffer.
    pub fn buffered_count(&self) -> usize {
        self.count
    }

    /// Number of events overwritten after the ring reached capacity.
    pub fn overwritten_count(&self) -> u64 {
        self.overwritten
    }
}

impl Default for AuditLog {
    fn default() -> Self {
        Self::new()
    }
}

/// Global audit log.
pub static AUDIT: Mutex<AuditLog> = Mutex::new(AuditLog::new());

// -----------------------------------------------------------------------
// Convenience functions
// -----------------------------------------------------------------------

fn current_origin(source: Option<TaskId>) -> AuditOrigin {
    AuditOrigin {
        task_id: source,
        pid: None,
        cpu_id: crate::smp::current_cpu_index() as u16,
    }
}

/// Record a domain event. The tick and CPU are captured before AUDIT is
/// locked, so this wrapper cannot introduce scheduler↔audit lock inversion.
pub fn record(
    source: TaskId,
    action: AuditAction,
    capability_used: Option<CapabilityId>,
    result: AuditResult,
) {
    let tick = crate::sched::SCHEDULER.lock().total_ticks();
    let origin = current_origin(Some(source));
    AUDIT
        .lock()
        .record_at(tick, origin, action, capability_used, result);
}

/// Record a domain event whose caller identity could not be resolved.
pub fn record_anonymous(action: AuditAction, result: AuditResult) {
    let tick = crate::sched::SCHEDULER.lock().total_ticks();
    AUDIT.lock().record_at(
        tick,
        AuditOrigin::anonymous(crate::smp::current_cpu_index() as u16),
        action,
        None,
        result,
    );
}

/// Record the terminal result of one syscall. An absent caller is preserved
/// explicitly in the event instead of manufacturing task zero/capability zero.
pub fn log_syscall(
    caller: Option<&crate::syscall::CallerContext>,
    number: u64,
    class: Option<crate::syscall::AuditClass>,
    result: AuditResult,
) {
    let tick = crate::sched::SCHEDULER.lock().total_ticks();
    let origin = caller
        .map(|caller| AuditOrigin {
            task_id: Some(caller.task_id),
            pid: Some(caller.pid),
            cpu_id: caller.cpu_id,
        })
        .unwrap_or_else(|| current_origin(None));
    AUDIT.lock().record_at(
        tick,
        origin,
        AuditAction::SyscallCompleted { number, class },
        None,
        result,
    );
}

/// Record a capability check event.
pub fn log_cap_check(source: TaskId, cap_id: CapabilityId, result: AuditResult) {
    record(
        source,
        AuditAction::CapabilityChecked(cap_id),
        Some(cap_id),
        result,
    );
}

/// Record an IPC send event.
pub fn log_ipc_send(source: TaskId, dest: TaskId, result: AuditResult) {
    record(
        source,
        AuditAction::IpcMessageSent { to: dest },
        None,
        result,
    );
}

/// Record a brane connection event.
pub fn log_brane_connect(source: TaskId, brane_id: u64, result: AuditResult) {
    record(source, AuditAction::BraneConnected(brane_id), None, result);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(task_id: Option<TaskId>) -> AuditOrigin {
        AuditOrigin {
            task_id,
            pid: task_id,
            cpu_id: 7,
        }
    }

    #[test]
    fn ring_wrap_tracks_overwrites_and_sequences() {
        let mut log = AuditLog::new();
        for i in 0..(AUDIT_LOG_SIZE as u64 + 3) {
            log.record_at(
                i,
                origin(Some(i)),
                AuditAction::SyscallInvoked(i),
                None,
                AuditResult::Success,
            );
        }

        assert_eq!(log.buffered_count(), AUDIT_LOG_SIZE);
        assert_eq!(log.total_events(), AUDIT_LOG_SIZE as u64 + 3);
        assert_eq!(log.overwritten_count(), 3);
        let events: std::vec::Vec<_> = log.last_n(3).collect();
        assert_eq!(events[0].seq, AUDIT_LOG_SIZE as u64 + 3);
        assert_eq!(events[1].seq, AUDIT_LOG_SIZE as u64 + 2);
        assert_eq!(events[2].seq, AUDIT_LOG_SIZE as u64 + 1);
        let retained: std::vec::Vec<_> = log.last_n(usize::MAX).collect();
        assert_eq!(retained.len(), AUDIT_LOG_SIZE);
        assert_eq!(retained.last().unwrap().seq, 4);
        assert!(retained
            .windows(2)
            .all(|pair| pair[0].seq == pair[1].seq + 1));
    }

    #[test]
    fn last_n_is_newest_first_without_wrap() {
        let mut log = AuditLog::new();
        for i in 1..=3 {
            log.record_at(
                i,
                origin(Some(i)),
                AuditAction::TaskCreated(i),
                None,
                AuditResult::Success,
            );
        }
        let events: std::vec::Vec<_> = log.last_n(2).collect();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].seq, 3);
        assert_eq!(events[1].seq, 2);
        assert_eq!(log.last_n(0).count(), 0);
        assert_eq!(log.last_n(usize::MAX).count(), 3);
    }

    #[test]
    fn origin_can_be_explicitly_absent() {
        let mut log = AuditLog::new();
        log.record_at(
            11,
            AuditOrigin::anonymous(2),
            AuditAction::SyscallCompleted {
                number: 99,
                class: None,
            },
            None,
            AuditResult::Denied,
        );
        let event = log.last_n(1).next().unwrap();
        assert_eq!(event.source, None);
        assert_eq!(event.pid, None);
        assert_eq!(event.cpu_id, 2);
        assert_eq!(event.tick, 11);
        assert_eq!(event.capability_used, None);
        assert_eq!(event.result, AuditResult::Denied);
        assert_eq!(log.total_events(), 1);
    }
}

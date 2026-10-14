#![allow(dead_code)]
// ============================================================
// Brane OS Kernel — AI Subsystem
// ============================================================
//
// The AI subsystem provides a native, security-controlled layer
// for intelligent observation and limited actuation.
//
// Architecture:
//   - Observer: reads telemetry, audit logs, resource metrics
//   - Analyzer: detects anomalies, suggests optimizations
//   - Actuator: executes sanctioned actions (requires capability)
//
// All AI actions are:
//   1. Mediated by the capability manager
//   2. Recorded in the audit log
//   3. Rate-limited and sandboxed
//
// Spec reference: docs/AI_SUBSYSTEM.md
// ============================================================

use spin::Mutex;

use crate::audit;
use crate::sched::TaskId;
use crate::security::CapabilityId;

// -----------------------------------------------------------------------
// Types
// -----------------------------------------------------------------------

/// AI subsystem operational mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiMode {
    /// AI is disabled entirely.
    Disabled,
    /// AI can observe but not act.
    ObserveOnly,
    /// AI can observe and suggest actions (logged but not executed).
    Suggest,
    /// AI can observe, suggest, and execute sanctioned actions.
    ActRestricted,
}

/// Category of an AI observation or suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiCategory {
    /// Resource usage (CPU, memory, etc.)
    Resource,
    /// Security events (failed auth, anomalous syscalls)
    Security,
    /// Performance optimization opportunities
    Performance,
    /// Brane connection health
    BraneHealth,
    /// Scheduler tuning
    Scheduling,
    /// Anomaly detection
    Anomaly,
}

/// Severity of an AI finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AiSeverity {
    Info,
    Low,
    Medium,
    High,
    Critical,
}

/// An observation or suggestion from the AI subsystem.
#[derive(Debug, Clone)]
pub struct AiInsight {
    pub id: u64,
    pub category: AiCategory,
    pub severity: AiSeverity,
    pub message: [u8; 128],
    pub message_len: usize,
    pub suggested_action: Option<AiAction>,
    pub tick: u64,
}

impl AiInsight {
    pub fn message_str(&self) -> &str {
        core::str::from_utf8(&self.message[..self.message_len]).unwrap_or("???")
    }
}

/// An action the AI proposes (or executes if in ActRestricted mode).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAction {
    /// Suggest adjusting scheduler priority for a task.
    AdjustPriority(TaskId, u8),
    /// Suggest suspending a misbehaving task.
    SuspendTask(TaskId),
    /// Suggest freeing memory from a task.
    ReclaimMemory(TaskId, u64),
    /// Suggest disconnecting a brane.
    DisconnectBrane(u64),
    /// Alert the user / admin.
    AlertUser,
    /// No action, purely informational.
    None,
}

// -----------------------------------------------------------------------
// AI Engine
// -----------------------------------------------------------------------

const MAX_INSIGHTS: usize = 64;

pub struct AiEngine {
    mode: AiMode,
    insights: [Option<AiInsight>; MAX_INSIGHTS],
    next_id: u64,
    total_observations: u64,
    total_suggestions: u64,
    total_actions_executed: u64,
    total_actions_denied: u64,
    /// Capability required for AI actuation.
    actuation_cap: Option<CapabilityId>,
}

impl Default for AiEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AiEngine {
    pub const fn new() -> Self {
        const NONE: Option<AiInsight> = None;
        Self {
            mode: AiMode::ObserveOnly,
            insights: [NONE; MAX_INSIGHTS],
            next_id: 1,
            total_observations: 0,
            total_suggestions: 0,
            total_actions_executed: 0,
            total_actions_denied: 0,
            actuation_cap: None,
        }
    }

    /// Set the AI operational mode.
    pub fn set_mode(&mut self, mode: AiMode) {
        crate::serial_println!("[ai]   Mode changed: {:?} -> {:?}", self.mode, mode);
        self.mode = mode;
    }

    /// Get current mode.
    pub fn mode(&self) -> AiMode {
        self.mode
    }

    /// Bind an actuation capability (required for ActRestricted mode).
    pub fn set_actuation_cap(&mut self, cap: CapabilityId) {
        self.actuation_cap = Some(cap);
    }

    /// Record an observation from system telemetry.
    pub fn observe(
        &mut self,
        category: AiCategory,
        severity: AiSeverity,
        message: &str,
        action: Option<AiAction>,
    ) -> u64 {
        if self.mode == AiMode::Disabled {
            return 0;
        }

        self.total_observations += 1;

        let id = self.next_id;
        self.next_id += 1;

        let mut msg_buf = [0u8; 128];
        let len = message.len().min(128);
        msg_buf[..len].copy_from_slice(&message.as_bytes()[..len]);

        let tick = crate::sched::SCHEDULER.lock().total_ticks();

        let has_action = action.is_some();
        let insight = AiInsight {
            id,
            category,
            severity,
            message: msg_buf,
            message_len: len,
            suggested_action: action,
            tick,
        };

        // Store in ring-buffer style
        let idx = (id as usize - 1) % MAX_INSIGHTS;
        self.insights[idx] = Some(insight);

        if has_action {
            self.total_suggestions += 1;
        }

        crate::serial_println!("[ai]   [{:?}] {:?}: {}", category, severity, message);

        // Auto-execute if in ActRestricted mode and action is present
        if self.mode == AiMode::ActRestricted {
            if let Some(act) = action {
                self.try_execute(act);
            }
        }

        id
    }

    /// Attempt to execute an AI-suggested action.
    fn try_execute(&mut self, action: AiAction) {
        // In a real system, this would check the actuation capability
        // and potentially interact with the scheduler, memory manager, etc.
        match action {
            AiAction::AlertUser => {
                crate::serial_println!("[ai]   ACTION: Alerting user");
                self.total_actions_executed += 1;
                audit::record(
                    0,
                    audit::AuditAction::AiActionAuthorized(0),
                    self.actuation_cap,
                    audit::AuditResult::Success,
                );
            }
            AiAction::None => {}
            _ => {
                crate::serial_println!("[ai]   ACTION DENIED: {:?} (restricted)", action);
                self.total_actions_denied += 1;
                audit::record(
                    0,
                    audit::AuditAction::AiActionDenied(0),
                    self.actuation_cap,
                    audit::AuditResult::Denied,
                );
            }
        }
    }

    /// Get the last N insights.
    pub fn last_insights(&self, n: usize) -> impl Iterator<Item = &AiInsight> {
        let start = if self.next_id as usize > n {
            (self.next_id as usize - 1 - n) % MAX_INSIGHTS
        } else {
            0
        };
        let take = n.min(self.total_observations as usize);

        (0..take).filter_map(move |i| {
            let idx = (start + i) % MAX_INSIGHTS;
            self.insights[idx].as_ref()
        })
    }

    /// Statistics.
    pub fn stats(&self) -> AiStats {
        AiStats {
            mode: self.mode,
            total_observations: self.total_observations,
            total_suggestions: self.total_suggestions,
            total_actions_executed: self.total_actions_executed,
            total_actions_denied: self.total_actions_denied,
        }
    }
}

/// AI subsystem statistics.
#[derive(Debug, Clone, Copy)]
pub struct AiStats {
    pub mode: AiMode,
    pub total_observations: u64,
    pub total_suggestions: u64,
    pub total_actions_executed: u64,
    pub total_actions_denied: u64,
}

/// Global AI engine.
pub static AI_ENGINE: Mutex<AiEngine> = Mutex::new(AiEngine::new());

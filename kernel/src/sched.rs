#![allow(dead_code)]
// ============================================================
// Brane OS Kernel — Scheduler (Round-Robin + Context Switch)
// ============================================================
//
// A cooperative round-robin scheduler that manages kernel tasks, plus a
// fixed-size per-CPU run-queue coordinator for the SMP bring-up path.
// Each task has a unique ID, a priority, a state, and a saved
// CPU context (`TaskContext`) so that it can be suspended and
// resumed at will.
//
// Context switching (Phase 10):
//   - Each task owns a `KernelStack` and a `TaskContext`.
//   - `yield_current()` triggers a cooperative switch to the
//     next ready task by calling `context::switch_context`.
//   - The timer tick marks tasks as Ready and can also trigger
//     a switch from an interrupt handler.
//
// Spec reference: ARCHITECTURE.md §5.2.3 (Scheduler)
//                 ROADMAP.md Fase 10 (Context switching real)
// ============================================================

use crate::context::{self, KernelStack, TaskContext};
use spin::Mutex;

/// Run a scheduler critical section with local interrupts masked on bare
/// metal. Timer/IPI handlers touch the same locks, so allowing an interrupt
/// to preempt the owner would deadlock trying to reacquire a spinlock.
#[inline]
pub fn with_interrupts_disabled<R>(f: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "none")]
    {
        x86_64::instructions::interrupts::without_interrupts(f)
    }
    #[cfg(not(target_os = "none"))]
    {
        f()
    }
}

/// Maximum number of tasks the scheduler can manage.
const MAX_TASKS: usize = 64;

/// Global scheduler instance.
pub static SCHEDULER: Mutex<Scheduler> = Mutex::new(Scheduler::new());

/// Unique task identifier.
pub type TaskId = u64;

/// Maximum number of entries in one per-CPU run queue.
const MAX_RUN_QUEUE_TASKS: usize = MAX_TASKS;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunQueueError {
    InvalidCpu,
    InvalidTask,
    Full,
    Duplicate,
}

/// Result of asking one logical CPU to select work from its run queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchResult {
    /// The CPU selected a task that was already assigned to its queue.
    Dispatched(TaskId),
    /// The CPU had no local work and successfully stole a task.
    Stole(TaskId),
    /// No runnable task was available for this CPU.
    Idle,
    /// The CPU is outside the configured topology.
    InvalidCpu,
    /// The CPU already owns a running task and must yield it first.
    Busy(TaskId),
}

/// Runtime accounting for one logical CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuRuntimeSnapshot {
    pub current: Option<TaskId>,
    pub dispatches: u64,
    pub steals: u64,
    pub idle_dispatches: u64,
}

impl CpuRuntimeSnapshot {
    const fn new() -> Self {
        Self {
            current: None,
            dispatches: 0,
            steals: 0,
            idle_dispatches: 0,
        }
    }
}

/// CPU-local scheduler state kept separate from the global BSP scheduler.
///
/// The saved context is metadata until AP register switching is enabled, but
/// it is intentionally per CPU now so an AP can never overwrite the BSP's
/// current task or saved context while the transition is in progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSchedulerSnapshot {
    pub initialized: bool,
    pub current_task: Option<TaskId>,
    pub saved_context: TaskContext,
    pub context_switches: u64,
    pub timer_ticks: u64,
}

impl CpuSchedulerSnapshot {
    const fn new() -> Self {
        Self {
            initialized: false,
            current_task: None,
            saved_context: TaskContext::empty(),
            context_switches: 0,
            timer_ticks: 0,
        }
    }
}

/// Stable CPU-local scheduler slots. Each slot is protected independently so
/// an AP's bookkeeping does not share the BSP's global task cursor.
pub static PER_CPU_SCHEDULER: [Mutex<CpuSchedulerSnapshot>; crate::smp::MAX_CPUS] =
    [const { Mutex::new(CpuSchedulerSnapshot::new()) }; crate::smp::MAX_CPUS];

/// Fixed-size FIFO/round-robin queue owned by one logical CPU.
#[derive(Clone, Copy)]
pub struct CpuRunQueue {
    tasks: [TaskId; MAX_RUN_QUEUE_TASKS],
    len: usize,
    cursor: usize,
}

impl CpuRunQueue {
    pub const fn new() -> Self {
        Self {
            tasks: [0; MAX_RUN_QUEUE_TASKS],
            len: 0,
            cursor: 0,
        }
    }

    pub const fn len(&self) -> usize {
        self.len
    }

    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn contains(&self, task_id: TaskId) -> bool {
        self.tasks[..self.len].contains(&task_id)
    }

    fn enqueue(&mut self, task_id: TaskId) -> Result<(), RunQueueError> {
        if task_id == 0 {
            return Err(RunQueueError::InvalidTask);
        }
        if self.contains(task_id) {
            return Err(RunQueueError::Duplicate);
        }
        if self.len == MAX_RUN_QUEUE_TASKS {
            return Err(RunQueueError::Full);
        }
        self.tasks[self.len] = task_id;
        self.len += 1;
        Ok(())
    }

    fn remove(&mut self, task_id: TaskId) -> bool {
        let Some(index) = self.tasks[..self.len].iter().position(|id| *id == task_id) else {
            return false;
        };
        self.tasks.copy_within(index + 1..self.len, index);
        self.len -= 1;
        self.tasks[self.len] = 0;
        if self.len == 0 {
            self.cursor = 0;
        } else {
            self.cursor %= self.len;
        }
        true
    }

    fn next(&mut self) -> Option<TaskId> {
        if self.len == 0 {
            return None;
        }
        let task_id = self.tasks[self.cursor % self.len];
        self.cursor = (self.cursor + 1) % self.len;
        Some(task_id)
    }

    /// Remove and return the next task. A running task is therefore never
    /// available for another CPU to steal until it yields back to its queue.
    fn pop_next(&mut self) -> Option<TaskId> {
        let task_id = self.next()?;
        self.remove(task_id);
        Some(task_id)
    }

    /// Remove the next task except `excluded`.
    ///
    /// A yielding CPU puts its current task back on the local queue before
    /// selecting a successor. Keeping the current ID out of this selection
    /// guarantees that a real handoff is made when another runnable task is
    /// available, while preserving the queue cursor for the next quantum.
    fn pop_next_excluding(&mut self, excluded: Option<TaskId>) -> Option<TaskId> {
        let attempts = self.len;
        for _ in 0..attempts {
            let task_id = self.next()?;
            if Some(task_id) != excluded {
                self.remove(task_id);
                return Some(task_id);
            }
        }
        None
    }
}

impl Default for CpuRunQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-CPU run-queue coordinator.
///
/// Task ownership is assigned to the least-loaded queue, with a rotating
/// tie-breaker. An idle CPU can steal the oldest task from the busiest peer;
/// actual register/context switching remains in `Scheduler` until the AP
/// interrupt path is connected to it.
pub struct MultiCoreScheduler {
    queues: [CpuRunQueue; crate::smp::MAX_CPUS],
    runtime: [CpuRuntimeSnapshot; crate::smp::MAX_CPUS],
    cpu_count: usize,
    next_assignment: usize,
}

impl MultiCoreScheduler {
    pub const fn new() -> Self {
        Self {
            queues: [const { CpuRunQueue::new() }; crate::smp::MAX_CPUS],
            runtime: [const { CpuRuntimeSnapshot::new() }; crate::smp::MAX_CPUS],
            cpu_count: 1,
            next_assignment: 0,
        }
    }

    /// Reset queues and select the number of CPUs participating in dispatch.
    /// At least the BSP is always present; excess CPUs are capped at the
    /// static topology limit.
    pub fn configure(&mut self, cpu_count: usize) -> usize {
        self.cpu_count = cpu_count.clamp(1, crate::smp::MAX_CPUS);
        self.queues = [const { CpuRunQueue::new() }; crate::smp::MAX_CPUS];
        self.runtime = [const { CpuRuntimeSnapshot::new() }; crate::smp::MAX_CPUS];
        self.next_assignment = 0;
        self.cpu_count
    }

    pub const fn cpu_count(&self) -> usize {
        self.cpu_count
    }

    /// Assign a task to the least-loaded active CPU.
    pub fn enqueue(&mut self, task_id: TaskId) -> Result<usize, RunQueueError> {
        if task_id == 0 {
            return Err(RunQueueError::InvalidTask);
        }
        if self.queues[..self.cpu_count]
            .iter()
            .any(|queue| queue.contains(task_id))
            || self.runtime[..self.cpu_count]
                .iter()
                .any(|runtime| runtime.current == Some(task_id))
        {
            return Err(RunQueueError::Duplicate);
        }
        let mut selected = self.next_assignment % self.cpu_count;
        let mut selected_load = self.queues[selected].len();
        for offset in 1..self.cpu_count {
            let candidate = (self.next_assignment + offset) % self.cpu_count;
            let candidate_load = self.queues[candidate].len();
            if candidate_load < selected_load {
                selected = candidate;
                selected_load = candidate_load;
            }
        }
        self.queues[selected].enqueue(task_id)?;
        self.next_assignment = (selected + 1) % self.cpu_count;
        Ok(selected)
    }

    /// Enqueue a task on a specific active CPU.
    ///
    /// This is used for affinity-aware startup and deterministic stress
    /// tests. Ownership checks are identical to [`Self::enqueue`].
    pub fn enqueue_on_cpu(&mut self, cpu: usize, task_id: TaskId) -> Result<(), RunQueueError> {
        if cpu >= self.cpu_count {
            return Err(RunQueueError::InvalidCpu);
        }
        if task_id == 0 {
            return Err(RunQueueError::InvalidTask);
        }
        if self.queues[..self.cpu_count]
            .iter()
            .any(|queue| queue.contains(task_id))
            || self.runtime[..self.cpu_count]
                .iter()
                .any(|runtime| runtime.current == Some(task_id))
        {
            return Err(RunQueueError::Duplicate);
        }
        self.queues[cpu].enqueue(task_id)
    }

    /// Remove a task from a CPU's queue, typically when it blocks or exits.
    pub fn dequeue(&mut self, cpu: usize, task_id: TaskId) -> Result<bool, RunQueueError> {
        if cpu >= self.cpu_count {
            return Err(RunQueueError::InvalidCpu);
        }
        if self.runtime[cpu].current == Some(task_id) {
            // A blocked or exited task must relinquish its runtime slot
            // without being re-enqueued by `complete`.
            self.runtime[cpu].current = None;
            return Ok(true);
        }
        Ok(self.queues[cpu].remove(task_id))
    }

    /// Pick the next task on a CPU without removing it from the queue.
    pub fn pick_next(&mut self, cpu: usize) -> Result<Option<TaskId>, RunQueueError> {
        if cpu >= self.cpu_count {
            return Err(RunQueueError::InvalidCpu);
        }
        Ok(self.queues[cpu].next())
    }

    /// Steal one task from the busiest peer queue.
    pub fn steal(&mut self, thief_cpu: usize) -> Result<Option<TaskId>, RunQueueError> {
        if thief_cpu >= self.cpu_count {
            return Err(RunQueueError::InvalidCpu);
        }
        if self.queues[thief_cpu].len() == MAX_RUN_QUEUE_TASKS {
            return Err(RunQueueError::Full);
        }
        let Some((victim, _)) = self
            .queues
            .iter()
            .take(self.cpu_count)
            .enumerate()
            .filter(|(cpu, queue)| *cpu != thief_cpu && !queue.is_empty())
            .max_by_key(|(cpu, queue)| (queue.len(), usize::MAX - *cpu))
        else {
            return Ok(None);
        };
        let task_id = self.queues[victim].tasks[0];
        self.queues[victim].remove(task_id);
        self.queues[thief_cpu]
            .enqueue(task_id)
            .expect("empty thief queue has capacity");
        Ok(Some(task_id))
    }

    /// Dispatch one task on a CPU. The selected task is removed from its
    /// queue while running, preventing a concurrent steal of the same task.
    pub fn dispatch(&mut self, cpu: usize) -> DispatchResult {
        self.dispatch_excluding_mode(cpu, None, true)
    }

    fn dispatch_excluding_mode(
        &mut self,
        cpu: usize,
        excluded: Option<TaskId>,
        allow_steal: bool,
    ) -> DispatchResult {
        if cpu >= self.cpu_count {
            return DispatchResult::InvalidCpu;
        }
        if let Some(task_id) = self.runtime[cpu].current {
            return DispatchResult::Busy(task_id);
        }

        if let Some(task_id) = self.queues[cpu].pop_next_excluding(excluded) {
            self.runtime[cpu].current = Some(task_id);
            self.runtime[cpu].dispatches += 1;
            return DispatchResult::Dispatched(task_id);
        }

        if allow_steal {
            if let Some(task_id) = self.steal(cpu).ok().flatten() {
                // `steal` moved the task onto the thief's queue; remove it
                // again so the runtime state is the single source of
                // ownership.
                let selected = self.queues[cpu]
                    .pop_next_excluding(excluded)
                    .unwrap_or(task_id);
                self.runtime[cpu].current = Some(selected);
                self.runtime[cpu].dispatches += 1;
                self.runtime[cpu].steals += 1;
                return DispatchResult::Stole(selected);
            }
        }

        self.runtime[cpu].idle_dispatches += 1;
        DispatchResult::Idle
    }

    /// Prepare one bounded dispatcher handoff.
    ///
    /// An idle CPU may enter exactly one task. A running task always returns
    /// to the CPU-local idle continuation when it yields, even if peers are
    /// ready on the same queue. This keeps an IPI kick bounded to one quantum
    /// and lets the AP loop consume subsequent interrupts before selecting
    /// more work.
    fn bounded_handoff(
        &mut self,
        cpu: usize,
        allow_steal: bool,
    ) -> (Option<TaskId>, DispatchResult) {
        if cpu >= self.cpu_count {
            return (None, DispatchResult::InvalidCpu);
        }
        let previous = self.runtime[cpu].current;
        if previous.is_some() {
            // `complete` cannot fail here: dispatch owns one slot and the
            // queue has capacity for the task that just left it.
            let _ = self.complete(cpu);
            return (previous, DispatchResult::Idle);
        }
        (None, self.dispatch_excluding_mode(cpu, None, allow_steal))
    }

    /// Return the currently running task to its owning CPU queue.
    pub fn complete(&mut self, cpu: usize) -> Result<Option<TaskId>, RunQueueError> {
        if cpu >= self.cpu_count {
            return Err(RunQueueError::InvalidCpu);
        }
        let Some(task_id) = self.runtime[cpu].current.take() else {
            return Ok(None);
        };
        self.queues[cpu].enqueue(task_id)?;
        Ok(Some(task_id))
    }

    pub fn runtime(&self, cpu: usize) -> Option<CpuRuntimeSnapshot> {
        (cpu < self.cpu_count).then(|| self.runtime[cpu])
    }

    pub fn runtime_snapshots(&self) -> [CpuRuntimeSnapshot; crate::smp::MAX_CPUS] {
        self.runtime
    }

    pub fn queue_load(&self, cpu: usize) -> Option<usize> {
        (cpu < self.cpu_count).then(|| self.queues[cpu].len())
    }

    pub fn loads(&self) -> [usize; crate::smp::MAX_CPUS] {
        let mut loads = [0; crate::smp::MAX_CPUS];
        for (index, queue) in self.queues.iter().take(self.cpu_count).enumerate() {
            loads[index] = queue.len();
        }
        loads
    }
}

impl Default for MultiCoreScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Global run-queue coordinator used once AP dispatch is enabled.
pub static MULTICORE_SCHEDULER: Mutex<MultiCoreScheduler> = Mutex::new(MultiCoreScheduler::new());

/// Configure per-CPU queues from the number of processors that completed SMP
/// bring-up. This is safe to call during early boot before tasks are enqueued.
pub fn configure_multicore(cpu_count: usize) -> usize {
    with_interrupts_disabled(|| {
        let configured = MULTICORE_SCHEDULER.lock().configure(cpu_count);
        for cpu in 0..configured {
            init_cpu_scheduler(cpu);
        }
        configured
    })
}

/// Add a task to the active run queues while keeping timer/IPI handlers from
/// re-entering the queue lock on the same CPU.
pub fn enqueue_task(task_id: TaskId) -> Result<usize, RunQueueError> {
    with_interrupts_disabled(|| MULTICORE_SCHEDULER.lock().enqueue(task_id))
}

pub fn enqueue_task_on_cpu(cpu: usize, task_id: TaskId) -> Result<(), RunQueueError> {
    with_interrupts_disabled(|| MULTICORE_SCHEDULER.lock().enqueue_on_cpu(cpu, task_id))
}

/// Initialize one CPU-local scheduler slot. APs call this after loading their
/// private GDT/TSS; the BSP slot is initialized when run queues are configured.
pub fn init_cpu_scheduler(cpu: usize) -> bool {
    with_interrupts_disabled(|| {
        let Some(slot) = PER_CPU_SCHEDULER.get(cpu) else {
            return false;
        };
        let mut state = slot.lock();
        *state = CpuSchedulerSnapshot::new();
        state.initialized = true;
        true
    })
}

pub fn cpu_scheduler_snapshot(cpu: usize) -> Option<CpuSchedulerSnapshot> {
    with_interrupts_disabled(|| PER_CPU_SCHEDULER.get(cpu).map(|slot| *slot.lock()))
}

/// Whether a logical CPU owns a fully initialized scheduler slot.
pub fn cpu_scheduler_is_initialized(cpu: usize) -> bool {
    with_interrupts_disabled(|| {
        PER_CPU_SCHEDULER
            .get(cpu)
            .is_some_and(|slot| slot.lock().initialized)
    })
}

/// Select the next task for a logical CPU, falling back to a steal from the
/// busiest peer when its own queue is empty. The returned ID is metadata only;
/// callers must still coordinate with `Scheduler::prepare_switch` before
/// changing register state.
pub fn next_task_for_cpu(cpu: usize) -> Option<TaskId> {
    with_interrupts_disabled(|| {
        let mut run_queues = MULTICORE_SCHEDULER.lock();
        run_queues
            .pick_next(cpu)
            .ok()
            .flatten()
            .or_else(|| run_queues.steal(cpu).ok().flatten())
    })
}

/// Select the next task for the current processor using the APIC-to-slot map.
pub fn next_task_for_current_cpu() -> Option<TaskId> {
    next_task_for_cpu(crate::smp::current_cpu_index())
}

/// Dispatch work for a logical CPU and update per-CPU runtime accounting.
pub fn dispatch_for_cpu(cpu: usize) -> DispatchResult {
    with_interrupts_disabled(|| {
        let result = MULTICORE_SCHEDULER.lock().dispatch(cpu);
        if let DispatchResult::Dispatched(task_id) | DispatchResult::Stole(task_id) = result {
            if let Some(slot) = PER_CPU_SCHEDULER.get(cpu) {
                let mut state = slot.lock();
                state.current_task = Some(task_id);
                state.context_switches += 1;
            }
        }
        result
    })
}

/// Complete the current dispatch and put the task back on its CPU queue.
pub fn complete_for_cpu(cpu: usize) -> Option<TaskId> {
    with_interrupts_disabled(|| {
        let completed = MULTICORE_SCHEDULER.lock().complete(cpu).ok().flatten();
        if completed.is_some() {
            if let Some(slot) = PER_CPU_SCHEDULER.get(cpu) {
                slot.lock().current_task = None;
            }
        }
        completed
    })
}

/// Dispatch one quantum for the processor executing this code.
pub fn dispatch_current_cpu() -> DispatchResult {
    dispatch_for_cpu(crate::smp::current_cpu_index())
}

/// Complete the current quantum on the processor executing this code.
pub fn complete_current_cpu() -> Option<TaskId> {
    complete_for_cpu(crate::smp::current_cpu_index())
}

/// Prepare a real register-context handoff for one CPU.
///
/// The run-queue lock and task-table lock are held only while selecting task
/// IDs and taking stable pointers. They are released before assembly runs.
/// When the CPU has no successor, its saved per-CPU idle context is selected;
/// this lets an AP return to its scheduler loop instead of spinning forever
/// inside a task that is the only runnable entity.
pub fn prepare_context_switch_for_cpu(
    cpu: usize,
) -> Option<(*mut TaskContext, *const TaskContext)> {
    with_interrupts_disabled(|| {
        let slot = PER_CPU_SCHEDULER.get(cpu)?;
        if !slot.lock().initialized {
            return None;
        }

        let (previous, result) = {
            let mut run_queues = MULTICORE_SCHEDULER.lock();
            // Keep a task pinned to its AP while saving its register frame.
            // Migration/stealing is still exercised by the metadata stress
            // path, but a live context cannot be stolen until a later
            // migration-safe handoff is added.
            let (previous, result) = run_queues.bounded_handoff(cpu, false);
            (previous, result)
        };
        let next = match result {
            DispatchResult::Dispatched(task_id) | DispatchResult::Stole(task_id) => Some(task_id),
            DispatchResult::Idle => None,
            DispatchResult::Busy(_) | DispatchResult::InvalidCpu => return None,
        };
        if previous.is_none() && next.is_none() {
            return None;
        }

        let mut scheduler = SCHEDULER.lock();
        let mut state = slot.lock();
        let idle_context = &mut state.saved_context as *mut TaskContext;

        // The first AP handoff saves the normal AP scheduler loop into the
        // idle context. Subsequent handoffs reuse the slot as the return
        // continuation after a task yields.
        let old_ptr = previous
            .and_then(|task_id| scheduler.task_context_ptr(task_id))
            .unwrap_or(idle_context);

        let new_ptr = if let Some(next_id) = next {
            let Some(task_ptr) = scheduler.task_context_ptr(next_id) else {
                // Do not leave a selected ID marked as running when a task
                // was removed concurrently between queue and table access.
                drop(state);
                drop(scheduler);
                let _ = MULTICORE_SCHEDULER.lock().complete(cpu);
                return None;
            };
            state.current_task = Some(next_id);
            state.context_switches += 1;
            task_ptr as *const TaskContext
        } else {
            state.current_task = None;
            if previous.is_some() {
                state.context_switches += 1;
            }
            idle_context as *const TaskContext
        };

        Some((old_ptr, new_ptr))
    })
}

/// Execute one real context handoff on the current processor.
///
/// The AP scheduler loop and AP tasks use this entry point. Scheduler locks
/// are released before `switch_context` jumps, so the resumed task can acquire
/// them again without self-deadlocking.
pub fn switch_current_cpu_context() -> bool {
    let Some((old_ptr, new_ptr)) = prepare_context_switch_for_cpu(crate::smp::current_cpu_index())
    else {
        return false;
    };
    // SAFETY: pointers refer to static scheduler task slots or the static
    // per-CPU idle context; both remain allocated for the kernel lifetime.
    // Keep interrupts masked across the save/restore window so an IPI cannot
    // observe a half-written task context or interrupt the assembly handoff.
    // The matching `sti` runs only when the saved continuation returns to the
    // AP loop, so a newly entered task starts with a deterministic IF=0 state.
    #[cfg(target_os = "none")]
    x86_64::instructions::interrupts::disable();
    unsafe { context::switch_context(old_ptr, new_ptr) };
    #[cfg(target_os = "none")]
    x86_64::instructions::interrupts::enable();
    true
}

/// Account one hardware timer quantum for a CPU and run a bounded dispatcher
/// pass. The actual register switch remains explicit and is not performed
/// from an interrupt frame yet.
pub fn timer_tick_for_cpu(cpu: usize) -> DispatchResult {
    with_interrupts_disabled(|| {
        let Some(slot) = PER_CPU_SCHEDULER.get(cpu) else {
            return DispatchResult::InvalidCpu;
        };
        slot.lock().timer_ticks += 1;

        // The legacy BSP scheduler still owns its cooperative task cursor.
        // APs use their per-CPU run queues and private saved contexts. The BSP
        // keeps the legacy cooperative cursor until both paths are unified.
        if cpu == 0 {
            // Interrupt handlers must never spin on a lock held by the
            // interrupted context. If the BSP is already updating a task
            // table, this tick is safely deferred to the next interrupt.
            if let Some(mut scheduler) = SCHEDULER.try_lock() {
                scheduler.tick();
            }
        }

        let result = dispatch_for_cpu(cpu);
        if matches!(
            result,
            DispatchResult::Dispatched(_) | DispatchResult::Stole(_)
        ) {
            let _ = complete_for_cpu(cpu);
        }
        result
    })
}

pub fn timer_tick_current_cpu() -> DispatchResult {
    timer_tick_for_cpu(crate::smp::current_cpu_index())
}

pub fn multicore_runtime_snapshots() -> [CpuRuntimeSnapshot; crate::smp::MAX_CPUS] {
    with_interrupts_disabled(|| MULTICORE_SCHEDULER.lock().runtime_snapshots())
}

/// Task priority levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Priority {
    Idle = 0,
    Low = 1,
    Normal = 2,
    High = 3,
    Realtime = 4,
    System = 5,
}

/// Task execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Ready,
    Running,
    Blocked,
    Finished,
}

// -----------------------------------------------------------------------
// Task descriptor
// -----------------------------------------------------------------------

/// Represents a scheduled kernel task.
///
/// Each task owns its execution context and kernel stack.
/// The stack is `Box`-allocated at task creation and lives
/// for the full lifetime of the task.
pub struct Task {
    pub id: TaskId,
    pub name: [u8; 32],
    pub name_len: usize,
    pub priority: Priority,
    pub state: TaskState,
    pub ticks: u64,

    /// Saved CPU state. Updated every time the task is preempted.
    pub ctx: TaskContext,

    /// Owning pointer to this task's kernel stack.
    ///
    /// `None` for tasks that were created without a real entry
    /// point (e.g. the initial boot task that reuses the bootloader
    /// stack).
    pub stack: Option<StackBox>,
}

/// Heap-allocated `KernelStack` wrapped in a raw pointer so that
/// `Task` can be stored in a fixed-size array without `Box<T>` in
/// `no_std`. We manage lifetimes manually.
///
/// Safety invariant: the pointer is always valid while the `Task`
/// exists.  When the task is dropped (slot → None), `StackBox::drop`
/// deallocates through `Box`.
pub struct StackBox(*mut KernelStack);

// SAFETY: tasks are only ever accessed behind the `Mutex<Scheduler>`
// so there is no concurrent access without synchronization.
unsafe impl Send for StackBox {}
unsafe impl Sync for StackBox {}

impl Drop for StackBox {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // Re-create the Box so Rust drops (and deallocates) it.
            let _ = unsafe { alloc::boxed::Box::from_raw(self.0) };
        }
    }
}

extern crate alloc;
use alloc::boxed::Box;

impl Task {
    /// Create a metadata-only task (no stack, no entry point).
    ///
    /// Used for the initial boot task which already has a stack
    /// provided by the bootloader.
    pub fn new_boot(id: TaskId, name: &str, priority: Priority) -> Self {
        let mut name_buf = [0u8; 32];
        let len = name.len().min(32);
        name_buf[..len].copy_from_slice(&name.as_bytes()[..len]);
        Self {
            id,
            name: name_buf,
            name_len: len,
            priority,
            state: TaskState::Ready,
            ticks: 0,
            ctx: TaskContext::empty(),
            stack: None,
        }
    }

    /// Create a task with its own kernel stack, ready to start at `entry`.
    ///
    /// The entry function must have the signature:
    /// ```rust
    /// extern "C" fn my_task() -> ! { ... }
    /// ```
    /// It must never return; call `exit_task()` or `loop {}` at the end.
    pub fn new_with_stack(id: TaskId, name: &str, priority: Priority, entry: fn() -> !) -> Self {
        let mut name_buf = [0u8; 32];
        let len = name.len().min(32);
        name_buf[..len].copy_from_slice(&name.as_bytes()[..len]);

        // Allocate the stack on the kernel heap.
        let stack_box = Box::new(KernelStack::new());
        let stack_top = stack_box.top();
        let stack_ptr = Box::into_raw(stack_box);

        // Build the initial context pointing to entry.
        let ctx = TaskContext::new_task(stack_top, entry as usize as u64);

        Self {
            id,
            name: name_buf,
            name_len: len,
            priority,
            state: TaskState::Ready,
            ticks: 0,
            ctx,
            stack: Some(StackBox(stack_ptr)),
        }
    }

    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("???")
    }
}

// -----------------------------------------------------------------------
// Scheduler
// -----------------------------------------------------------------------

/// Round-robin scheduler with cooperative context switching.
pub struct Scheduler {
    tasks: [Option<Task>; MAX_TASKS],
    current: usize,
    next_id: TaskId,
    tick_count: u64,
}

// We need `const fn new()` so the static can be initialized at compile time.
// `Option<Task>` is not Copy because `Task` contains a `StackBox`, so we
// use a helper to build the array.
impl Scheduler {
    pub const fn new() -> Self {
        // SAFETY: `Option<Task>` is a valid all-zero value (None).
        // We cannot use `[None; MAX_TASKS]` directly because `Task`
        // is not Copy, so we use MaybeUninit to build the array.
        const NONE: Option<Task> = None;
        Self {
            tasks: [NONE; MAX_TASKS],
            current: 0,
            next_id: 1,
            tick_count: 0,
        }
    }

    // -----------------------------------------------------------------------
    // Task management
    // -----------------------------------------------------------------------

    /// Add a boot task (no stack allocation).
    ///
    /// Use this for the initial kernel context that is already
    /// running on the bootloader stack.
    pub fn add_task(&mut self, name: &str, priority: Priority) -> Option<TaskId> {
        for slot in self.tasks.iter_mut() {
            if slot.is_none() {
                let id = self.next_id;
                self.next_id += 1;
                *slot = Some(Task::new_boot(id, name, priority));
                return Some(id);
            }
        }
        None
    }

    /// Add a task that starts execution at `entry` on its own stack.
    ///
    /// Returns the TaskId, or `None` if the task table is full.
    pub fn add_task_with_entry(
        &mut self,
        name: &str,
        priority: Priority,
        entry: fn() -> !,
    ) -> Option<TaskId> {
        for slot in self.tasks.iter_mut() {
            if slot.is_none() {
                let id = self.next_id;
                self.next_id += 1;
                *slot = Some(Task::new_with_stack(id, name, priority, entry));
                return Some(id);
            }
        }
        None
    }

    /// Remove a task by ID. Returns `true` if found and removed.
    pub fn remove_task(&mut self, id: TaskId) -> bool {
        for slot in self.tasks.iter_mut() {
            if let Some(task) = slot {
                if task.id == id {
                    *slot = None; // Drop releases the StackBox
                    return true;
                }
            }
        }
        false
    }

    /// Block a task (move it to Blocked state).
    pub fn block_task(&mut self, id: TaskId) -> bool {
        for task in self.tasks.iter_mut().flatten() {
            if task.id == id {
                task.state = TaskState::Blocked;
                return true;
            }
        }
        false
    }

    /// Unblock a task (move it back to Ready).
    pub fn unblock_task(&mut self, id: TaskId) -> bool {
        for task in self.tasks.iter_mut().flatten() {
            if task.id == id && task.state == TaskState::Blocked {
                task.state = TaskState::Ready;
                return true;
            }
        }
        false
    }

    // -----------------------------------------------------------------------
    // Scheduling
    // -----------------------------------------------------------------------

    /// Called on every timer tick. Advances round-robin state.
    ///
    /// This updates task states but does NOT perform the context
    /// switch itself; the actual `switch_context` call happens in
    /// `yield_current()` to avoid holding the scheduler lock across
    /// the assembly switch.
    pub fn tick(&mut self) {
        self.tick_count += 1;

        // Mark current as Ready if it was Running
        if let Some(ref mut task) = self.tasks[self.current] {
            if task.state == TaskState::Running {
                task.state = TaskState::Ready;
            }
        }

        // Find next Ready task (round-robin)
        let start = self.current;
        loop {
            self.current = (self.current + 1) % MAX_TASKS;
            if let Some(ref mut task) = self.tasks[self.current] {
                if task.state == TaskState::Ready {
                    task.state = TaskState::Running;
                    task.ticks += 1;
                    return;
                }
            }
            if self.current == start {
                break; // Wrapped around — no ready tasks
            }
        }
    }

    /// Pick the next ready task index without advancing `self.current`.
    ///
    /// Returns `None` if there is no ready task other than the current one.
    fn next_ready(&self) -> Option<usize> {
        let start = self.current;
        let mut idx = (start + 1) % MAX_TASKS;
        loop {
            if let Some(ref task) = self.tasks[idx] {
                if task.state == TaskState::Ready {
                    return Some(idx);
                }
            }
            idx = (idx + 1) % MAX_TASKS;
            if idx == start {
                break;
            }
        }
        None
    }

    // -----------------------------------------------------------------------
    // Context-switch helpers (called from yield_current)
    // -----------------------------------------------------------------------

    /// Returns raw pointers to the old and new task contexts so that
    /// `yield_current` can perform the switch after releasing the lock.
    ///
    /// Returns `None` if there is no other ready task to switch to.
    ///
    /// SAFETY: The caller must ensure the scheduler lock is released
    /// before actually calling `switch_context` with these pointers.
    pub fn prepare_switch(&mut self) -> Option<(*mut TaskContext, *const TaskContext)> {
        let next_idx = self.next_ready()?;

        // Mark current → Ready, next → Running
        if let Some(ref mut task) = self.tasks[self.current] {
            if task.state == TaskState::Running {
                task.state = TaskState::Ready;
            }
        }
        if let Some(ref mut task) = self.tasks[next_idx] {
            task.state = TaskState::Running;
            task.ticks += 1;
        }

        let old_ptr = self.tasks[self.current]
            .as_mut()
            .map(|t| &mut t.ctx as *mut TaskContext)?;

        let new_ptr = self.tasks[next_idx]
            .as_ref()
            .map(|t| &t.ctx as *const TaskContext)?;

        self.current = next_idx;

        Some((old_ptr, new_ptr))
    }

    /// Return a raw pointer to a task's saved context.
    ///
    /// The scheduler owns every task slot for the lifetime of the kernel;
    /// callers must hold the scheduler lock while obtaining this pointer and
    /// release the lock before invoking the assembly switch primitive.
    fn task_context_ptr(&mut self, id: TaskId) -> Option<*mut TaskContext> {
        self.tasks
            .iter_mut()
            .flatten()
            .find(|task| task.id == id)
            .map(|task| &mut task.ctx as *mut TaskContext)
    }

    // -----------------------------------------------------------------------
    // Queries
    // -----------------------------------------------------------------------

    pub fn current_task(&self) -> Option<&Task> {
        self.tasks[self.current].as_ref()
    }

    pub fn current_task_id(&self) -> Option<TaskId> {
        self.tasks[self.current].as_ref().map(|t| t.id)
    }

    pub fn total_ticks(&self) -> u64 {
        self.tick_count
    }

    pub fn active_count(&self) -> usize {
        self.tasks.iter().filter(|t| t.is_some()).count()
    }

    /// Returns a short summary of all tasks (for the `sched` shell command).
    pub fn snapshot(&self) -> [Option<TaskSnapshot>; MAX_TASKS] {
        const NONE: Option<TaskSnapshot> = None;
        let mut out = [NONE; MAX_TASKS];
        for (i, slot) in self.tasks.iter().enumerate() {
            if let Some(task) = slot {
                out[i] = Some(TaskSnapshot {
                    id: task.id,
                    name: task.name,
                    name_len: task.name_len,
                    priority: task.priority,
                    state: task.state,
                    ticks: task.ticks,
                    rsp: task.ctx.rsp,
                    rip: task.ctx.rip,
                });
            }
        }
        out
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// A lightweight snapshot of a task's state (no heap, Copy-able).
#[derive(Debug, Clone, Copy)]
pub struct TaskSnapshot {
    pub id: TaskId,
    pub name: [u8; 32],
    pub name_len: usize,
    pub priority: Priority,
    pub state: TaskState,
    pub ticks: u64,
    pub rsp: u64,
    pub rip: u64,
}

impl TaskSnapshot {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("???")
    }
}

// -----------------------------------------------------------------------
// Public API: cooperative yield
// -----------------------------------------------------------------------

/// Voluntarily yield the current task and switch to the next ready task.
///
/// This is the primary cooperative scheduling primitive. A task should
/// call this when it is waiting for I/O, sleeping, or has finished its
/// current work quantum.
///
/// # Safety
///
/// Must not be called while holding any spinlock that might be needed
/// by another task (deadlock). The scheduler lock is released before
/// the actual context switch occurs.
pub fn yield_current() {
    // Once the per-CPU coordinator is initialized, every processor (including
    // the BSP) saves its idle continuation in a private slot. This prevents a
    // BSP shell yield from overwriting a task context currently owned by an
    // AP. The legacy cursor remains only as an early-boot fallback.
    let cpu = crate::smp::current_cpu_index();
    if cpu_scheduler_is_initialized(cpu) {
        let _ = switch_current_cpu_context();
        return;
    }

    // Get the raw context pointers while holding the lock, then
    // immediately release it before doing the switch.
    let switch_pair = with_interrupts_disabled(|| SCHEDULER.lock().prepare_switch());

    if let Some((old_ptr, new_ptr)) = switch_pair {
        // SAFETY:
        // - Both pointers come from live task descriptors inside the
        //   scheduler's fixed-size array, so they are valid for the
        //   lifetime of the scheduler (static).
        // - The scheduler lock is NOT held during the switch.
        // - Interrupts may fire during the switch; the IDT handlers
        //   do NOT acquire the scheduler lock, so there's no deadlock.
        unsafe {
            context::switch_context(old_ptr, new_ptr);
        }
    }
    // If there is no other ready task, we simply return and the
    // current task continues.
}

#[cfg(test)]
mod multicore_tests {
    use super::*;

    #[test]
    fn assigns_tasks_to_least_loaded_cpu() {
        let mut scheduler = MultiCoreScheduler::new();
        assert_eq!(scheduler.configure(3), 3);
        for task_id in 1..=6 {
            assert_eq!(scheduler.enqueue(task_id), Ok((task_id as usize - 1) % 3));
        }
        assert_eq!(scheduler.loads()[..3], [2, 2, 2]);
        assert_eq!(scheduler.enqueue(1), Err(RunQueueError::Duplicate));
        assert_eq!(scheduler.enqueue(0), Err(RunQueueError::InvalidTask));
    }

    #[test]
    fn picks_round_robin_and_steals_from_busiest_peer() {
        let mut scheduler = MultiCoreScheduler::new();
        scheduler.configure(3);
        for task_id in 1..=4 {
            scheduler.enqueue(task_id).unwrap();
        }
        assert_eq!(scheduler.pick_next(0), Ok(Some(1)));
        assert_eq!(scheduler.pick_next(0), Ok(Some(4)));
        assert_eq!(scheduler.steal(2), Ok(Some(1)));
        assert_eq!(scheduler.loads()[..3], [1, 1, 2]);
        assert_eq!(scheduler.dequeue(2, 1), Ok(true));
        assert_eq!(scheduler.dequeue(2, 99), Ok(false));
        assert_eq!(scheduler.pick_next(3), Err(RunQueueError::InvalidCpu));
    }

    #[test]
    fn dispatch_removes_running_task_until_completion() {
        let mut scheduler = MultiCoreScheduler::new();
        scheduler.configure(2);
        scheduler.enqueue(1).unwrap();

        assert_eq!(scheduler.dispatch(0), DispatchResult::Dispatched(1));
        assert_eq!(scheduler.dispatch(0), DispatchResult::Busy(1));
        // A running task is not stealable by another CPU.
        assert_eq!(scheduler.dispatch(1), DispatchResult::Idle);
        assert_eq!(scheduler.complete(0), Ok(Some(1)));
        assert_eq!(scheduler.runtime(0).unwrap().current, None);
        assert_eq!(scheduler.queue_load(0), Some(1));

        assert_eq!(scheduler.dispatch(0), DispatchResult::Dispatched(1));
        assert_eq!(scheduler.dequeue(0, 1), Ok(true));
        assert_eq!(scheduler.complete(0), Ok(None));
        assert_eq!(scheduler.queue_load(0), Some(0));
    }

    #[test]
    fn idle_cpu_dispatches_a_stolen_task_and_accounts_it() {
        let mut scheduler = MultiCoreScheduler::new();
        scheduler.configure(2);
        // Force both tasks onto CPU zero so CPU one must steal.
        scheduler.queues[0].enqueue(1).unwrap();
        scheduler.queues[0].enqueue(2).unwrap();

        assert_eq!(scheduler.dispatch(1), DispatchResult::Stole(1));
        let runtime = scheduler.runtime(1).unwrap();
        assert_eq!(runtime.current, Some(1));
        assert_eq!(runtime.dispatches, 1);
        assert_eq!(runtime.steals, 1);
        assert_eq!(scheduler.complete(1), Ok(Some(1)));
        assert_eq!(scheduler.queue_load(1), Some(1));
    }

    #[test]
    fn affinity_enqueue_rejects_invalid_cpu_and_duplicate_ownership() {
        let mut scheduler = MultiCoreScheduler::new();
        scheduler.configure(2);
        assert_eq!(
            scheduler.enqueue_on_cpu(2, 1),
            Err(RunQueueError::InvalidCpu)
        );
        assert_eq!(scheduler.enqueue_on_cpu(0, 1), Ok(()));
        assert_eq!(
            scheduler.enqueue_on_cpu(1, 1),
            Err(RunQueueError::Duplicate)
        );
        assert_eq!(
            scheduler.enqueue_on_cpu(1, 0),
            Err(RunQueueError::InvalidTask)
        );
    }

    #[test]
    fn cpu_local_scheduler_tracks_timer_without_touching_bsp() {
        let cpu = crate::smp::MAX_CPUS - 1;
        assert!(init_cpu_scheduler(cpu));
        assert!(cpu_scheduler_is_initialized(cpu));
        assert_eq!(timer_tick_for_cpu(cpu), DispatchResult::InvalidCpu);
        let snapshot = cpu_scheduler_snapshot(cpu).unwrap();
        assert!(snapshot.initialized);
        assert_eq!(snapshot.timer_ticks, 1);
        assert_eq!(snapshot.current_task, None);
        assert_eq!(snapshot.context_switches, 0);
    }

    #[test]
    fn bounded_handoff_returns_to_idle_between_tasks() {
        let mut scheduler = MultiCoreScheduler::new();
        scheduler.configure(1);
        scheduler.enqueue(1).unwrap();
        scheduler.enqueue(2).unwrap();

        assert_eq!(
            scheduler.bounded_handoff(0, false),
            (None, DispatchResult::Dispatched(1))
        );
        assert_eq!(scheduler.runtime(0).unwrap().current, Some(1));

        assert_eq!(
            scheduler.bounded_handoff(0, false),
            (Some(1), DispatchResult::Idle)
        );
        assert_eq!(scheduler.runtime(0).unwrap().current, None);
        assert_eq!(scheduler.queue_load(0), Some(2));

        assert_eq!(
            scheduler.bounded_handoff(0, false),
            (None, DispatchResult::Dispatched(2))
        );
    }
}

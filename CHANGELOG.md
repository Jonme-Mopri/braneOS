# Changelog

All notable changes to Brane OS are documented here. The project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Trusted syscall caller capture from the BSP/AP scheduler and process table,
  kernel-derived `GetPid`, saturating per-process syscall accounting, and
  terminal `SyscallCompleted` audit events with PID/TaskId/CPU origin. Audit
  snapshots avoid scheduler↔audit lock inversion; capability mutation events
  are appended after releasing the capability manager.
- First Phase 14 syscall mediation cut: exhaustive metadata for all 28 ABI
  numbers, fail-closed rejection of reserved unimplemented calls, and a pure
  `UserRange` validator for null, length, overflow, canonicality, user limits
  and page-boundary accounting. Copy helpers, dynamic capability checks and
  ring-3 return validation remain subsequent gates.
- xHCI ACPI S3 gate with fail-closed quiesce, ring/register retention
  validation, polling fallback and quarantine when firmware resets state;
  `make acpi-xhci-test` provides an opt-in Q35 smoke path.
- Two deterministic PCI vector-registry tests covering invalid ownership
  transitions and metadata preservation across mask/rearm.
- Documentation portal, reconciled current-vs-target status, and ADRs for Brane
  Protocol v2, the syscall ABI, IPC message passing and virtual memory.
- Phase 13 implementation specification for xHCI control transfers, USB
  descriptor validation, HID boot keyboard reports and QEMU acceptance tests.
- Proposed xHCI event and transfer model with a single Event Ring dispatcher,
  bounded polling, explicit DMA ownership and completion correlation.
- Phase 13 follow-up specification for read-only USB Mass Storage using
  Bulk-Only Transport, a minimal SCSI profile and the existing block/FAT32 path.
- Phase 13 MSI/MSI-X specification and ADR covering PCI capability walking,
  vector ownership, transactional programming, deferred work and polling fallback.
- Phase 14 prerequisite specification and ADR for centralized syscall mediation,
  user-memory copying, capability policy, auditing and safe ring-3 return.
- Phase 14 IPC runtime specification and ADR for authenticated endpoint handles,
  service discovery, blocking wait cells, cancellation and RPC correlation.
- Phase 14 security-services specification and ADR for bootstrap roles, ordered
  identity/policy/broker startup, capability commit and auditable failure modes.
- Phase 14 isolated AI runtime specification and ADR for typed telemetry,
  sandboxed inference, per-action gates and single-use execution leases.
- Phase 14 `bpkg` specification and ADR for deterministic signed packages,
  TUF-style repository roles, immutable storage and transactional activation.
- Portable UEFI ISO packaging with BIOS and UEFI disk artifacts.
- Automated ISO boot verification with QEMU, OVMF and TCG.
- Tag-driven GitHub release workflow with SHA-256 verification.
- MADT support for x2APIC CPUs and Local APIC address overrides.
- Safe Local APIC/I/O APIC MMIO windows with initial boot-time page mapping.
- MADT ISA interrupt overrides and controlled IRQ0/IRQ1 hand-off to the APIC,
  including LAPIC EOI, S3 restoration and automatic 8259 PIC fallback.
- Deterministic SMP boot plan with APIC ID validation, BSP assignment,
  INIT/SIPI startup and explicit AP lifecycle tracking.
- Per-CPU scheduler runtime and idle continuations for BSP/APs, bounded
  dispatch/complete quanta, duplicate-run protection and safe stealing.
- Real task stack/register restoration on three APs under QEMU/TCG, verified
  by eight IPI rounds and a CPU execution mask.
- Lost-wakeup-safe AP idle loop and post-S3 BSP context restoration test.
- Shared PCI inventory using Configuration Mechanism #1, with bridge and
  multifunction traversal plus I/O, 32-bit MMIO and 64-bit MMIO BAR decoding.
- Fixed-capacity block-device registry with validated geometry, aligned and
  bounds-checked transfers, read-only enforcement and `pci`/`block` shell
  inspection commands.
- Virtio discovery now consumes the shared PCI inventory and identifies both
  legacy and modern network/block controller candidates.
- Physically contiguous DMA regions below 4 GiB, including aligned allocation
  support in the boot frame allocator.
- Synchronous legacy virtio-blk driver with PCI bus mastering, feature
  negotiation, a polling virtqueue, 512-byte bounce buffer and block-registry
  integration. QEMU boot tests verify a real LBA0 transfer with 1 and 4 vCPUs.
- xHCI host initialization with scratchpads, DCBAA, persistent command/event
  rings, Supported Protocol discovery and root-port reset. The Q35 test attaches
  a USB keyboard and verifies Enable Slot plus Address Device using DMA contexts.
- A single-owner xHCI Event Ring dispatcher, synchronous EP0 control transfers,
  defensive USB descriptor parsing, HID boot protocol setup and a continuously
  rearmed interrupt IN endpoint. QMP tests prove key delivery to the TTY with
  both 1 and 4 vCPUs while retaining the virtio-blk/FAT32 path.
- Read-only USB Mass Storage over xHCI Bulk IN/OUT rings and BOT, including
  defensive `08/06/50` descriptor selection, validated CBW/CSW framing,
  `INQUIRY`, readiness/sense, `READ CAPACITY(10)` and fragmented `READ(10)`.
  The device registers as `usb-storage0`; Q35 mounts a distinct FAT32 image at
  `/usb` with 1 and 4 vCPUs while retaining `virtio-blk0` at `/disk`.
- BOT Reset Recovery with Mass Storage Reset, endpoint halt clearing and xHCI
  endpoint dequeue synchronization. Host-side negative tests cover phase
  errors, invalid CSW identity and timeout quarantine without issuing a second
  CBW after failed recovery.
- PCI conventional capability walker with fail-closed bounds, cycle and
  truncation checks; MSI/MSI-X decoders; and validated xAPIC MSI message
  construction for the reserved Phase 13 vector range.
- Fixed PCI vector ownership registry with duplicate prevention, validated
  state transitions, and failed-setup release semantics.
- Transactional host models for MSI and MSI-X programming, including one-message
  MSI setup, masked MSI-X entry writes, measured-BAR bounds checks, and
  no-mutation failure behavior.
- IDT wrappers for the complete PCI vector range with release/acquire pending
  bits and a constant-time top half that defers device work.
- Deferred xHCI Event Ring service after `hlt`, bounded by the existing runtime
  event budget and followed by the normal HID completion/rearm path.
- Real Q35 MSI-X setup for xHCI: capability discovery from ECAM, BAR0 table
  bounds validation, masked entry-0 programming, vector `0x40`, interrupter
-  enable, and polling fallback. A dedicated 1/4-vCPU QMP test proves deferred
  HID completion delivery.
- Checked MSI-X/MSI programming transactions: measured BAR and PBA validation,
  readback after every config/MMIO write, rollback that retains vector ownership
  on failure, APIC EOI without taking the runtime lock, and xHCI MSI fallback.
  Q35 covers MSI-X IRQ delivery at 1/4 vCPU and `msix=off,msi=on` delivery with
  the bounded watchdog.
- Deferred xHCI interrupt acknowledgement now clears `IMAN.IP` and
  `USBSTS.EINT` after advancing `ERDP`, with a bounded second pass for events
  arriving during acknowledgement. The shell closes the pending-to-`hlt` race
  by checking the vector while interrupts are disabled before `sti; hlt`.
- Q35 PCI interrupt coverage now injects two distinct HID reports and checks a
  fresh `source=irq` dispatch for each MSI-X/MSI event; disabling both
  capabilities verifies the polling fallback. `make test-all` includes this
  interrupt matrix.
- The deterministic parser corpus now exercises the PCI capability walker and
  MSI/MSI-X decoders alongside the existing USB and filesystem parsers.

## 0.1.0 — Foundation

### Added

- Bootable x86_64 kernel with GDT, IDT, PIC, paging and heap allocation.
- Cooperative scheduler, syscalls, IPC, processes and POSIX-style signals.
- Capability security model, audit log and restricted AI observer.
- VFS, RamFS, TTY, `brsh`, networking and Brane Protocol v2.
- ACPI shutdown, reboot and tested S3 suspend/resume.
- Unit, stress, mutation-fuzz, security, integration and E2E suites.

### Known limitations

- Live migration of a context that has already run remains disabled; tasks may
  be balanced before dispatch or pinned by affinity.
- MSI-X/MSI is implemented for xHCI; virtio PCI remains on polling. Hotplug,
  hubs, mice, simultaneous USB slots, IRQ resume after S3 and modern virtio PCI
  transport remain outside the Phase 13 baseline. xHCI supports either one HID
  boot keyboard or one BOT/SCSI Mass Storage device.
- FAT32 is read-only and supports 8.3 names; LFN and write operations remain
  unimplemented.

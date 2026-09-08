# Changelog

All notable changes to Brane OS are documented here. The project follows
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

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
- USB HID transfers, USB mass storage and modern virtio PCI transport remain
  planned for Phase 13; xHCI currently addresses the first device by polling.
- FAT32 is read-only and supports 8.3 names; LFN and write operations remain
  unimplemented.

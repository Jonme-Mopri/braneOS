// ============================================================
// Brane OS Kernel — Entry Point
// ============================================================
//
// This is the bare-metal entry point for the Brane OS kernel.
// It runs on x86_64 with no standard library.
//
// Architecture: hybrid modular kernel
// See: docs/PROJECT_MASTER_SPEC.md §8-§10
//      docs/ARCHITECTURE.md §4-§5
// ============================================================

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

use alloc::boxed::Box;
use bootloader_api::{entry_point, BootInfo, BootloaderConfig};

pub const CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(bootloader_api::config::Mapping::Dynamic);
    config
};

entry_point!(kernel_main, config = &CONFIG);

use core::panic::PanicInfo;
use core::sync::atomic::{AtomicU32, Ordering};

/// APs that have entered a real scheduled task context during boot.
static SMP_WORKER_CPU_MASK: AtomicU32 = AtomicU32::new(0);

// --- Hardware-specific modules (binary-only, not in lib) ---
mod idt;
mod keyboard;
mod pic;

// --- Re-import shared modules from the lib crate ---
use brane_os_kernel::serial_println;
use brane_os_kernel::{
    acpi, ai, apic, audit, block, brane, dns, fat32, framebuffer, gdt, ipc, memory, module_loader,
    net, pci, process, ramfs, sched, security, serial, shell, smp, socket, syscall, tty, usermode,
    vfs, virtio, virtio_block, xhci,
};

// -----------------------------------------------------------------------
// Kernel Init
// -----------------------------------------------------------------------

/// Kernel entry point.
///
/// Called after the bootloader hands control to us.
/// Initializes subsystems in order:
/// 1. Serial output (logging)
/// 2. GDT + TSS (required for IST)
/// 3. IDT (exception & interrupt handlers)
/// 4. PIC (hardware interrupts)
/// 5. Frame allocator (physical memory)
/// 6. Heap allocator (kernel heap)
/// 7. Scheduler (task management)
///
/// After init, the kernel enters a halt loop waiting for interrupts.
fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    // --- Banner ---
    serial::init();
    serial_println!("===========================================");
    serial_println!("  Brane OS v0.1 — Kernel Booting");
    serial_println!("===========================================");
    serial_println!();

    // === Phase 1.5: Framebuffer (if available) ===
    if let Some(fb) = boot_info.framebuffer.as_mut() {
        let info = fb.info();
        let pixel_format = match info.pixel_format {
            bootloader_api::info::PixelFormat::Rgb => framebuffer::PixelFormat::Rgb,
            bootloader_api::info::PixelFormat::Bgr => framebuffer::PixelFormat::Bgr,
            bootloader_api::info::PixelFormat::U8 => framebuffer::PixelFormat::U8,
            _ => framebuffer::PixelFormat::Unknown,
        };

        let buffer = fb.buffer_mut();
        let config = framebuffer::FramebufferConfig {
            buffer_start: buffer.as_mut_ptr() as u64,
            buffer_len: buffer.len(),
            width: info.width,
            height: info.height,
            stride: info.stride,
            bytes_per_pixel: info.bytes_per_pixel,
            pixel_format,
        };
        framebuffer::FB_WRITER.lock().init(config);

        // Write to framebuffer
        use core::fmt::Write;
        let mut fb_writer = framebuffer::FB_WRITER.lock();
        let _ = writeln!(fb_writer, "Brane OS v0.1");
        let _ = writeln!(fb_writer, "Framebuffer: {}x{}", info.width, info.height);
        let _ = writeln!(fb_writer);
    } else {
        serial_println!("[fb]   No framebuffer available (serial only).");
    }

    // === Phase 1: Core Hardware ===
    serial_println!("[boot] Phase 1: Core hardware...");

    gdt::init();
    serial_println!("[gdt]  Global Descriptor Table loaded.");

    idt::init();
    // idt::init() prints its own message

    usermode::init_syscall_msrs();

    let keyboard_ready = keyboard::init();
    serial_println!("[kbd]  PS/2 keyboard initialized: {}.", keyboard_ready);

    pic::init();
    // pic::init() prints its own message

    // === Phase 2: Memory ===
    serial_println!();
    serial_println!("[boot] Phase 2: Memory subsystem...");

    // Initialize the frame allocator with the real bootloader memory map
    let mut frame_alloc = memory::frame_allocator::BitmapFrameAllocator::new();

    let mut usable_bytes: u64 = 0;
    for region in boot_info.memory_regions.iter() {
        use bootloader_api::info::MemoryRegionKind;
        if region.kind == MemoryRegionKind::Usable {
            let start = region.start;
            let size = region.end - region.start;
            frame_alloc.mark_region_free(start, region.end);
            usable_bytes += size;
        }
    }

    serial_println!(
        "[mem]  Frame allocator ready: {} free frames ({} MiB usable)",
        frame_alloc.free_count(),
        usable_bytes / (1024 * 1024)
    );

    // Initialize paging — get the OffsetPageTable from the bootloader's CR3
    let phys_offset = boot_info
        .physical_memory_offset
        .into_option()
        .expect("bootloader must provide physical_memory_offset");

    let mut mapper = unsafe { memory::paging::init(x86_64::VirtAddr::new(phys_offset)) };
    serial_println!(
        "[page] OffsetPageTable initialized (phys_offset=0x{:X})",
        phys_offset
    );

    // Initialize ACPI power management
    if let Some(rsdp_addr) = boot_info.rsdp_addr.into_option() {
        acpi::init(rsdp_addr, phys_offset);
        match acpi::configure_wake_trampoline(&mut mapper, &mut frame_alloc, phys_offset) {
            Ok(address) => {
                serial_println!(
                    "[acpi] S3 wake trampoline ready at physical address 0x{:X}.",
                    address
                );
            }
            Err(error) => {
                serial_println!(
                    "[acpi] S3 suspend unavailable: {:?}; shutdown/reboot remain enabled.",
                    error
                );
            }
        }
    } else {
        serial_println!("[acpi] RSDP unavailable; power management disabled.");
    }

    // Map APIC controller windows discovered through MADT. Register access is
    // deferred until both windows are available; only then is the IRQ hand-off
    // attempted. Any discovery, mapping or hardware validation failure leaves
    // the already-working PIC path in place.
    if let Some(topology) = acpi::info().apic {
        let mut local_mapped = false;
        let mut io_mapped = false;
        let local_result = apic::map_mmio_page(
            &mut mapper,
            &mut frame_alloc,
            phys_offset,
            topology.local_apic_address,
        );
        match local_result {
            Ok(()) => {
                local_mapped = true;
                serial_println!(
                    "[apic] Local APIC MMIO mapped at phys=0x{:X} virt=0x{:X}",
                    topology.local_apic_address,
                    topology.local_apic_address.saturating_add(phys_offset)
                );
            }
            Err(error) => {
                serial_println!("[apic] Local APIC mapping unavailable: {}", error);
            }
        }
        if let Some(io_apic_address) = topology.first_io_apic_address {
            match apic::map_mmio_page(&mut mapper, &mut frame_alloc, phys_offset, io_apic_address) {
                Ok(()) => {
                    io_mapped = true;
                    serial_println!(
                        "[apic] I/O APIC MMIO mapped at phys=0x{:X} virt=0x{:X}",
                        io_apic_address,
                        io_apic_address.saturating_add(phys_offset)
                    );
                }
                Err(error) => {
                    serial_println!("[apic] I/O APIC mapping unavailable: {}", error);
                }
            }
        }
        if local_mapped && io_mapped && acpi::info().smp.is_some() {
            match apic::activate_legacy_irqs(topology, phys_offset, pic::mask_all) {
                Ok(activation) => {
                    match acpi::assign_bsp(activation.local_apic_id as u32) {
                        Ok(slot) => {
                            smp::register_cpu_index(activation.local_apic_id as u32, 0);
                            serial_println!(
                                "[smp] BSP assigned to CPU slot {}; AP startup pending",
                                slot
                            );
                        }
                        Err(error) => {
                            serial_println!("[smp] BSP assignment unavailable: {:?}", error);
                        }
                    }
                    serial_println!(
                        "[apic] IRQ routing active: LAPIC ID={}, IOAPIC redirections={}, timer GSI={}, keyboard GSI={}",
                        activation.local_apic_id,
                        activation.io_apic_redirection_count,
                        activation.timer_global_irq,
                        activation.keyboard_global_irq,
                    );
                    if let Some(mut plan) = acpi::info().smp {
                        if plan.enabled_cpu_count > 1 {
                            match smp::prepare_ap_trampoline(
                                &mut mapper,
                                &mut frame_alloc,
                                phys_offset,
                            ) {
                                Ok(trampoline) => match apic::local_apic_handle() {
                                    Some(local_apic) => {
                                        match smp::start_application_processors(
                                            trampoline,
                                            phys_offset,
                                            local_apic,
                                            &mut plan,
                                        ) {
                                            Ok(report) => {
                                                let interrupt_report =
                                                    smp::verify_application_processors(
                                                        local_apic, &mut plan,
                                                    );
                                                acpi::set_smp_plan(plan);
                                                serial_println!(
                                                    "[smp] AP startup complete: attempted={}, online={}, failed={}",
                                                    report.attempted,
                                                    report.online,
                                                    report.failed,
                                                );
                                                serial_println!(
                                                    "[smp] AP interrupt check: attempted={}, responsive={}, failed={}",
                                                    interrupt_report.attempted,
                                                    interrupt_report.responsive,
                                                    interrupt_report.failed,
                                                );
                                            }
                                            Err(error) => {
                                                serial_println!(
                                                    "[smp] AP startup unavailable ({:?}); APs remain offline.",
                                                    error
                                                );
                                            }
                                        }
                                    }
                                    None => {
                                        serial_println!(
                                            "[smp] AP startup skipped; Local APIC handle unavailable."
                                        );
                                    }
                                },
                                Err(error) => {
                                    serial_println!(
                                        "[smp] AP trampoline unavailable ({:?}); APs remain offline.",
                                        error
                                    );
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    serial_println!(
                        "[apic] IRQ hand-off skipped ({:?}); retaining 8259 PIC fallback.",
                        error
                    );
                }
            }
        } else {
            serial_println!("[apic] IRQ hand-off skipped; required MMIO windows are unavailable.");
        }
    }

    // Initialize kernel heap — map pages and set up the linked-list allocator
    memory::heap::init(&mut mapper, &mut frame_alloc).expect("heap initialization failed");
    serial_println!(
        "[heap] Kernel heap initialized: {} KiB at 0x{:X}",
        memory::heap::HEAP_SIZE / 1024,
        memory::heap::HEAP_START
    );

    // Snapshot frame count for the `mem` command
    memory::frame_allocator::snapshot_free_count(&frame_alloc);

    // === Phase 2: Scheduler ===
    serial_println!();
    serial_println!("[boot] Phase 2: Scheduler (with cooperative context switching)...");
    let online_cpu_count = acpi::info()
        .smp
        .map(|plan| plan.online_cpu_count())
        .unwrap_or(1);
    let queue_cpu_count = sched::configure_multicore(online_cpu_count.max(1));
    let (idle_task, init_task, smp_worker_tasks, active_tasks) =
        sched::with_interrupts_disabled(|| {
            let mut scheduler = sched::SCHEDULER.lock();
            // Give the idle task a dedicated context as APs can now enter it
            // through the real per-CPU dispatcher.
            let idle_task = scheduler.add_task_with_entry(
                "kernel_idle",
                sched::Priority::Idle,
                kernel_idle_task,
            );
            // Register init as a real task with its own 16 KiB kernel stack.
            // In a future phase this will jump to user-space.
            let init_task =
                scheduler.add_task_with_entry("init", sched::Priority::System, kernel_init_task);
            let mut smp_worker_tasks = [None; smp::MAX_CPUS];
            for task in smp_worker_tasks.iter_mut().take(queue_cpu_count).skip(1) {
                *task = scheduler.add_task_with_entry(
                    "smp_worker",
                    sched::Priority::High,
                    smp_worker_task,
                );
            }
            (
                idle_task,
                init_task,
                smp_worker_tasks,
                scheduler.active_count(),
            )
        });
    if let Some(task_id) = idle_task {
        let _ = sched::enqueue_task_on_cpu(0, task_id);
    }
    if let Some(task_id) = init_task {
        let init_cpu = usize::from(queue_cpu_count > 1);
        let _ = sched::enqueue_task_on_cpu(init_cpu, task_id);
    }
    for (cpu, task_id) in smp_worker_tasks
        .iter()
        .enumerate()
        .take(queue_cpu_count)
        .skip(1)
    {
        if let Some(task_id) = task_id {
            let _ = sched::enqueue_task_on_cpu(cpu, *task_id);
        }
    }
    serial_println!(
        "[sched] Scheduler ready: {} tasks, cooperative context switching enabled.",
        active_tasks
    );
    serial_println!(
        "[sched] Multicore run queues ready: {} CPU(s).",
        queue_cpu_count
    );

    // Kick the online APs after their queues contain real tasks. The probe
    // handler wakes the AP loop, which performs one bounded stack/register
    // handoff outside the interrupt frame and then returns to its idle context.
    let dispatch_stress = if let Some(mut plan) = acpi::info().smp {
        if let Some(local_apic) = apic::local_apic_handle() {
            let report = smp::stress_application_processors(local_apic, &mut plan, 8);
            acpi::set_smp_plan(plan);
            report
        } else {
            smp::ApDispatchStressReport::default()
        }
    } else {
        smp::ApDispatchStressReport::default()
    };
    let runtime = sched::multicore_runtime_snapshots();
    let mut dispatches = 0u64;
    let mut steals = 0u64;
    let mut idle_dispatches = 0u64;
    for cpu in runtime.iter().take(queue_cpu_count) {
        dispatches += cpu.dispatches;
        steals += cpu.steals;
        idle_dispatches += cpu.idle_dispatches;
    }
    serial_println!(
        "[sched] Multicore dispatch active: attempted={}, responsive={}, dispatched={}, steals={}, idle={}",
        dispatch_stress.attempted,
        dispatch_stress.responsive,
        dispatches,
        steals,
        idle_dispatches,
    );
    serial_println!(
        "[sched] Multicore dispatch stress: rounds={}, attempted={}, responsive={}, failed={}, dispatched={}",
        dispatch_stress.rounds,
        dispatch_stress.attempted,
        dispatch_stress.responsive,
        dispatch_stress.failed,
        dispatches,
    );
    let expected_ap_mask = if queue_cpu_count == smp::MAX_CPUS {
        !1u32
    } else {
        ((1u32 << queue_cpu_count) - 1) & !1
    };
    let mut observed_ap_mask = SMP_WORKER_CPU_MASK.load(Ordering::Acquire) & expected_ap_mask;
    for _ in 0..2_000_000 {
        if observed_ap_mask == expected_ap_mask {
            break;
        }
        core::hint::spin_loop();
        observed_ap_mask = SMP_WORKER_CPU_MASK.load(Ordering::Acquire) & expected_ap_mask;
    }
    serial_println!(
        "[sched] Multicore task execution: expected={}, observed={}, mask=0x{:08X}",
        expected_ap_mask.count_ones(),
        observed_ap_mask.count_ones(),
        observed_ap_mask,
    );

    // === Phase 3: Syscalls & IPC ===
    serial_println!();
    serial_println!("[boot] Phase 3: Syscall dispatcher & IPC...");

    // Register a test syscall to verify dispatch
    let test_ctx = syscall::SyscallContext {
        number: syscall::SyscallNumber::GetPid as u64,
        arg1: 0,
        arg2: 0,
        arg3: 0,
        arg4: 0,
        arg5: 0,
    };
    let result = syscall::dispatch(&test_ctx);
    serial_println!(
        "[sys]  Syscall dispatcher ready. Test GetPid => {}",
        result.to_raw()
    );

    // Test IPC: send a message between tasks
    {
        let msg = ipc::IpcMessage::new(
            1, // sender: init
            0, // receiver: kernel_idle
            ipc::MessageType::Notification,
            b"boot_complete",
        )
        .unwrap();
        let _ = ipc::IPC.lock().send(msg);
        let pending = ipc::IPC.lock().pending_count(0);
        serial_println!(
            "[ipc]  IPC core ready. Task 0 has {} pending message(s).",
            pending
        );
    }

    // === Phase 4: Security & Adaptability ===
    serial_println!();
    serial_println!("[boot] Phase 4: Security & adaptability...");

    // Grant initial capabilities
    {
        use security::{CapPermissions, CapScope, RiskLevel};
        let mut cap_mgr = security::CAP_MANAGER.lock();
        // kernel_idle gets basic read
        cap_mgr
            .grant(
                0,
                CapScope::System,
                CapPermissions::READ,
                RiskLevel::Low,
                false,
            )
            .ok();
        // init gets full system access
        cap_mgr
            .grant(
                1,
                CapScope::System,
                CapPermissions::READ
                    .union(CapPermissions::WRITE)
                    .union(CapPermissions::EXECUTE)
                    .union(CapPermissions::IPC_SEND)
                    .union(CapPermissions::IPC_RECV),
                RiskLevel::High,
                true,
            )
            .ok();
        serial_println!(
            "[cap]  Capability manager ready: {} active caps.",
            cap_mgr.active_count()
        );
    }

    // Record boot event in audit log
    audit::AUDIT.lock().record(
        0,
        audit::AuditAction::TaskCreated(0),
        None,
        audit::AuditResult::Success,
    );
    audit::AUDIT.lock().record(
        0,
        audit::AuditAction::TaskCreated(1),
        None,
        audit::AuditResult::Success,
    );
    serial_println!(
        "[aud]  Audit log ready: {} events recorded.",
        audit::AUDIT.lock().total_events()
    );

    // Register built-in kernel sub-branes
    {
        let mut loader = module_loader::MODULE_LOADER.lock();
        loader.load("serial_driver", (0, 1, 0), &[]).ok();
        loader.load("keyboard_driver", (0, 1, 0), &[]).ok();
        loader.load("timer_driver", (0, 1, 0), &[]).ok();
        serial_println!(
            "[mod]  Module loader ready: {} modules registered.",
            loader.loaded_count()
        );
    }

    // === Phase 5: Brane Protocol ===
    serial_println!();
    serial_println!("[boot] Phase 5: Brane Protocol...");

    {
        let mut brane_mgr = brane::BRANE.lock();
        // Set our local brane ID (derived from hardware ID in a real system)
        brane_mgr.set_local_id(0xBEA1);

        // Simulate discovering nearby branes
        let phone_id = brane_mgr
            .register_discovered(
                "pixel-9",
                brane::BraneType::Companion,
                brane::Transport::Bluetooth,
                0x07, // advertises read + write + execute
                85,
            )
            .unwrap();

        let _server_id = brane_mgr
            .register_discovered(
                "home-server",
                brane::BraneType::Peer,
                brane::Transport::TcpIp,
                0xFF, // advertises all caps
                100,
            )
            .unwrap();

        brane_mgr
            .register_discovered(
                "temp-sensor-01",
                brane::BraneType::IoT,
                brane::Transport::Ble,
                0x01, // read only
                70,
            )
            .ok();

        serial_println!(
            "[brane] {} branes discovered.",
            brane_mgr.discovered_count()
        );

        // Connect to the companion phone
        let session = brane_mgr.connect(phone_id, 1).unwrap();

        // Send a test telemetry message
        let msg = brane::BraneMessage::new(
            brane::BraneMessageType::Telemetry,
            0xBEA1,
            phone_id,
            session,
            b"{\"status\":\"boot_complete\",\"phase\":5}",
        )
        .unwrap();
        brane_mgr.send(session, &msg).ok();

        serial_println!(
            "[brane] Brane Protocol ready: {} active session(s).",
            brane_mgr.active_session_count()
        );
    }

    // === Phase 6: AI Subsystem ===
    serial_println!();
    serial_println!("[boot] Phase 6: AI subsystem...");
    {
        let mut engine = ai::AI_ENGINE.lock();
        engine.set_mode(ai::AiMode::ObserveOnly);
        engine.observe(
            ai::AiCategory::Resource,
            ai::AiSeverity::Info,
            "Boot complete. All subsystems nominal.",
            None,
        );
        engine.observe(
            ai::AiCategory::Security,
            ai::AiSeverity::Low,
            "2 capabilities granted during boot.",
            None,
        );
        let stats = engine.stats();
        serial_println!(
            "[ai]   AI engine ready (mode={:?}, observations={}).",
            stats.mode,
            stats.total_observations
        );
    }

    // === Phase 7: User Space Init ===
    serial_println!();
    serial_println!("[boot] Phase 7: User space...");
    {
        let mut table = process::PROCESS_TABLE.lock();
        // Create PID 1 — the init process
        let init_pid = table.create("init", None, 1).unwrap();
        table.start(init_pid);

        // Create initial system services
        let _log_pid = table.create("log_service", Some(init_pid), 2);
        let _net_pid = table.create("network_service", Some(init_pid), 3);
        let _brane_pid = table.create("brane_service", Some(init_pid), 4);

        serial_println!(
            "[proc] Process table ready: {} active processes.",
            table.active_count()
        );
    }

    // === Summary ===
    serial_println!();
    serial_println!("===========================================");
    serial_println!("  Brane OS v0.1 — Boot Complete");
    serial_println!("===========================================");
    serial_println!();
    serial_println!("  Phase 1: GDT, IDT, APIC/PIC       ✓");
    serial_println!("  Phase 2: Memory, Scheduler       ✓");
    serial_println!("  Phase 3: Syscalls, IPC           ✓");
    serial_println!("  Phase 4: Caps, Audit, Modules    ✓");
    serial_println!("  Phase 5: Brane Protocol          ✓");
    serial_println!("  Phase 6: AI Subsystem            ✓");
    serial_println!("  Phase 7: User Space              ✓");
    serial_println!();
    serial_println!("  All core subsystems online.");
    serial_println!();

    // === Phase 8: VFS, TTY & Shell ===
    serial_println!("[boot] Phase 8: VFS, TTY & Shell...");

    // Initialize RamFS and mount at /
    ramfs::init();
    {
        let mut vfs_mgr = vfs::VFS.lock();
        let ramfs_ref: &mut dyn vfs::FileSystem = &mut *ramfs::RAMFS.lock();
        let ramfs_ptr: *mut dyn vfs::FileSystem = ramfs_ref;
        unsafe {
            vfs_mgr
                .mount("/", ramfs_ptr)
                .expect("failed to mount ramfs");
        }
    }
    serial_println!("[vfs]  VFS ready. / mounted (RamFS).");
    serial_println!("[tty]  TTY0 ready (serial + framebuffer).");
    serial_println!();

    // === Phase 9: Networking ===
    serial_println!("[boot] Phase 9: Networking...");
    let (pci_functions, pci_buses, pci_overflowed, pci_backend) =
        pci::init(&mut mapper, &mut frame_alloc, acpi::info().ecam);
    serial_println!("[pci]  Config backend: {:?}", pci_backend);
    serial_println!(
        "[pci]  Enumeration complete: {} function(s) across {} bus(es), overflow={}",
        pci_functions,
        pci_buses,
        pci_overflowed,
    );
    match xhci::init(&mut mapper, &mut frame_alloc, phys_offset) {
        Ok(Some(controller)) => {
            serial_println!(
                "[xhci] Controller ready: {:02x}:{:02x}.{}, version={:x}.{:02x}, ports={}, slots={}, interrupters={}, BAR0=0x{:X}/{} KiB",
                controller.pci_device.address.bus,
                controller.pci_device.address.device,
                controller.pci_device.address.function,
                controller.interface_version >> 8,
                controller.interface_version & 0xFF,
                controller.max_ports,
                controller.max_device_slots,
                controller.max_interrupters,
                controller.mmio.physical_base,
                controller.mmio.size / 1024,
            );
            serial_println!(
                "[xhci] Runtime ready: reset=ok, running={}, command_probe={}, page={} bytes, slots={}, scratchpads={}, command_trbs={}, event_trbs={}",
                controller.running,
                if controller.command_probe_completed { "ok" } else { "failed" },
                controller.page_size,
                controller.enabled_slots,
                controller.scratchpad_buffers,
                controller.command_ring_trbs,
                controller.event_ring_trbs,
            );
            serial_println!(
                "[xhci] Ports ready: protocols={}, connected={}, addressed_port={}, addressed_slot={}, speed_id={}",
                controller.supported_protocols,
                controller.connected_ports,
                controller.addressed_port,
                controller.addressed_slot,
                controller.addressed_speed_id,
            );
            if controller.addressed_slot != 0 {
                serial_println!(
                    "[xhci] Device addressed: slot={}, port={}, speed_id={}",
                    controller.addressed_slot,
                    controller.addressed_port,
                    controller.addressed_speed_id,
                );
            }
        }
        Ok(None) => {
            serial_println!("[xhci] No xHCI controller found.");
        }
        Err(error) => {
            serial_println!("[xhci] Controller bootstrap failed: {:?}", error);
        }
    }
    let mut boot_block_id = None;
    if let Some(controller) = virtio::find_virtio_block() {
        match virtio_block::init(controller, &mut frame_alloc, phys_offset) {
            Ok(device) => {
                let registration = {
                    let mut registry = block::BLOCK_REGISTRY.lock();
                    registry.register(device)
                };
                match registration {
                    Ok(id) => {
                        boot_block_id = Some(id);
                        let info = block::BLOCK_REGISTRY.lock().info(id).unwrap();
                        serial_println!(
                            "[block] virtio-blk0 ready: id={}, sectors={}, bytes={}, read_only={}",
                            id,
                            info.block_count,
                            info.capacity_bytes().unwrap_or(0),
                            info.read_only,
                        );
                    }
                    Err(error) => {
                        serial_println!("[block] virtio-blk registration failed: {:?}", error);
                    }
                }
            }
            Err(error) => {
                serial_println!(
                    "[block] virtio-blk initialization failed at PCI {:02x}:{:02x}.{}: {:?}",
                    controller.address.bus,
                    controller.address.device,
                    controller.address.function,
                    error,
                );
            }
        }
    } else {
        serial_println!("[block] No virtio-blk controller found.");
    }
    let registered_blocks = block::BLOCK_REGISTRY.lock().len();
    serial_println!(
        "[block] Block layer ready: {} registered device(s).",
        registered_blocks
    );
    if let Some(id) = boot_block_id {
        let mut sector = [0u8; block::MIN_BLOCK_SIZE as usize];
        match block::BLOCK_REGISTRY.lock().read(id, 0, &mut sector) {
            Ok(()) => {
                serial_println!("[block] LBA0 read probe: ok");
            }
            Err(error) => {
                serial_println!("[block] LBA0 read probe failed: {:?}", error);
            }
        }

        match fat32::Fat32Fs::mount(id) {
            Ok(filesystem) => {
                serial_println!(
                    "[fat32] Volume ready: device={}, label={}, partition_lba={}, root_cluster={}",
                    filesystem.device_id(),
                    filesystem.volume_label(),
                    filesystem.partition_lba(),
                    filesystem.root_cluster(),
                );
                let filesystem = Box::leak(Box::new(filesystem));
                let filesystem_ref: &mut dyn vfs::FileSystem = filesystem;
                let mount_result = unsafe { vfs::VFS.lock().mount("/disk", filesystem_ref) };
                match mount_result {
                    Ok(()) => {
                        let mut probe = [0u8; 64];
                        let prefix_result = vfs::VFS.lock().read("/disk/README.TXT", 0, &mut probe);
                        let prefix_ok = matches!(prefix_result, Ok(bytes) if probe[..bytes]
                            .starts_with(b"Brane OS FAT32 block path ready.\n"));
                        let mut chain_probe = [0u8; 32];
                        let chain_result = vfs::VFS.lock().read(
                            "/disk/README.TXT",
                            fat32::SECTOR_SIZE,
                            &mut chain_probe,
                        );
                        let chain_ok = matches!(chain_result, Ok(bytes) if &chain_probe[..bytes]
                            == b"FAT32-CHAIN-OK\n");
                        if prefix_ok && chain_ok {
                            serial_println!("[fat32] Read probe: /disk/README.TXT ok");
                        } else {
                            serial_println!(
                                "[fat32] Read probe failed: prefix={:?}, chain={:?}",
                                prefix_result,
                                chain_result,
                            );
                        }
                    }
                    Err(error) => {
                        serial_println!("[fat32] Mount at /disk failed: {:?}", error);
                    }
                }
            }
            Err(error) => {
                serial_println!("[fat32] No mountable volume: {:?}", error);
            }
        }
    }
    memory::frame_allocator::snapshot_free_count(&frame_alloc);
    let _net_available = net::init();
    dns::init();
    {
        let _ = socket::SOCKET_TABLE.lock(); // Initialize socket table
        let _ = dns::DNS.lock(); // Initialize DNS resolver

        // Initialize UDP Discovery (BDP)
        use alloc::string::String;
        if let Err(_e) = brane_os_kernel::brane_discovery::DISCOVERY.lock().init(
            String::from("local-kernel-id"),
            String::from("BraneOS-Kernel"),
        ) {
            serial_println!("[bdp] Failed to initialize discovery UDP socket");
        } else {
            serial_println!("[bdp] UDP Discovery Protocol listening on port 9000");
            serial_println!("[bdp] Initial broadcast announce deferred until runtime polling.");
        }

        let dns_resolver = dns::DNS.lock();
        serial_println!(
            "[dns]  DNS resolver ready: {} hosts.",
            dns_resolver.host_count()
        );

        let sock_table = socket::SOCKET_TABLE.lock();
        serial_println!(
            "[sock] Socket subsystem ready ({} slots).",
            sock_table.capacity()
        );
    }
    serial_println!();

    // === Interactive Shell ===
    serial_println!("[boot] Starting brsh (Brane Shell)...");
    serial_println!();
    tty::tty_println("Welcome to Brane OS v0.1");
    tty::tty_println("Type 'help' for available commands.");
    tty::tty_println("");
    shell::prompt();

    // Shell loop: wait for keyboard input, process commands
    loop {
        x86_64::instructions::hlt(); // Wait for interrupt

        // Check if a line is ready
        let mut tty_guard = tty::TTY.lock();
        if tty_guard.has_line() {
            // Copy the line to a local buffer before releasing the lock
            let mut cmd_buf = [0u8; tty::MAX_LINE];
            let line = tty_guard.read_line();
            let len = line.len().min(tty::MAX_LINE);
            cmd_buf[..len].copy_from_slice(&line.as_bytes()[..len]);
            tty_guard.clear_line();
            drop(tty_guard); // Release lock before executing command

            let cmd_str = core::str::from_utf8(&cmd_buf[..len]).unwrap_or("");
            shell::execute(cmd_str);
            if acpi::take_resume_pending() {
                resume_platform();
            }
            shell::prompt();
        }
    }
}

/// Restore platform state that firmware and devices may reset during ACPI S3.
fn resume_platform() {
    x86_64::instructions::interrupts::disable();
    serial::init();
    gdt::init();
    idt::init();
    usermode::init_syscall_msrs();
    let keyboard_ready = keyboard::init();
    // Firmware may reset both controllers during S3. Re-enter through a
    // masked PIC state, then repeat the same APIC hand-off used at boot.
    apic::deactivate();
    pic::init_masked();
    let mut apic_restored = false;
    if let Some(topology) = acpi::info().apic {
        match apic::activate_legacy_irqs(topology, acpi::physical_memory_offset(), pic::mask_all) {
            Ok(activation) => {
                apic_restored = true;
                serial_println!(
                    "[acpi] APIC IRQ routing restored: LAPIC ID={}, timer GSI={}, keyboard GSI={}",
                    activation.local_apic_id,
                    activation.timer_global_irq,
                    activation.keyboard_global_irq,
                );
            }
            Err(error) => {
                serial_println!(
                    "[acpi] APIC restore skipped ({:?}); retaining 8259 PIC fallback.",
                    error
                );
            }
        }
    }
    if apic_restored {
        x86_64::instructions::interrupts::enable();
    } else {
        pic::init();
    }
    let network_ready = net::init();
    serial_println!(
        "[acpi] Resume complete; interrupts restored, keyboard={}, network={}",
        keyboard_ready,
        network_ready,
    );
    tty::tty_println("System resumed from ACPI S3.");
}

// -----------------------------------------------------------------------
// Panic & Halt
// -----------------------------------------------------------------------

/// Panic handler — prints to serial and halts.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    serial_println!();
    serial_println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    serial_println!("[KERNEL PANIC] {}", info);
    serial_println!("!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!");
    halt_loop();
}

/// Halts the CPU in an infinite loop, saving power.
/// Interrupts will still fire and be handled.
pub fn halt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}
// -----------------------------------------------------------------------
// Kernel Init Task
// -----------------------------------------------------------------------

/// The kernel init task runs as a separate scheduled task.
///
/// It performs deferred initialization and then enters an idle loop,
/// periodically yielding back to the scheduler.
fn kernel_init_task() -> ! {
    serial_println!("[init] Kernel init task started.");

    // Future: run deferred init, service startup, etc.

    loop {
        // Cooperatively yield to let other tasks run
        sched::yield_current();
    }
}

/// Dedicated idle task used by both the BSP scheduler and AP run queues.
/// It deliberately yields instead of halting so the saved `TaskContext`
/// remains a valid continuation for a later CPU handoff.
fn kernel_idle_task() -> ! {
    loop {
        sched::yield_current();
    }
}

/// Boot-time worker pinned to one AP. Reaching this entry point proves that
/// the CPU restored a task's private stack/register context, rather than only
/// manipulating run-queue metadata in the IPI handler.
fn smp_worker_task() -> ! {
    let cpu = smp::current_cpu_index();
    if cpu < u32::BITS as usize {
        SMP_WORKER_CPU_MASK.fetch_or(1u32 << cpu, Ordering::Release);
    }
    loop {
        sched::yield_current();
    }
}

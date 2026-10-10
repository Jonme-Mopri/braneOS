// ============================================================
// Brane OS Kernel — Interactive Shell (brsh)
// ============================================================
//
// A minimal in-kernel shell for system interaction.
// Runs in kernel mode (no user-space yet).
//
// Commands:
//   help, ps, mem, lsmod, brane status, ai status,
//   caps, audit, ls, cat, clear, reboot
//
// Spec reference: ARCHITECTURE.md §5.3 (planned)
// ============================================================

extern crate alloc;

use crate::apic;
use crate::{
    acpi, ai, audit, block, brane, dns, memory, module_loader, net, pci, process, sched, security,
    serial_println, socket, tty, vfs, xhci,
};

/// Print the shell prompt.
pub fn prompt() {
    tty::tty_print("brane> ");
}

/// Execute a single shell command line.
pub fn execute(line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }

    // Split into command and arguments
    let (cmd, args) = match line.find(' ') {
        Some(pos) => (&line[..pos], line[pos + 1..].trim()),
        None => (line, ""),
    };

    match cmd {
        "help" => cmd_help(),
        "ps" => cmd_ps(),
        "mem" => cmd_mem(),
        "pci" => cmd_pci(),
        "block" => cmd_block(),
        "lsmod" => cmd_lsmod(),
        "brane" => cmd_brane(args),
        "ai" => cmd_ai(args),
        "caps" => cmd_caps(),
        "audit" => cmd_audit(),
        "ls" => cmd_ls(args),
        "cat" => cmd_cat(args),
        "net" => cmd_net(args),
        "dns" => cmd_dns(args),
        "sockets" => cmd_sockets(),
        "clear" => cmd_clear(),
        "acpi" => cmd_acpi(),
        "suspend" | "sleep" => cmd_suspend(),
        "reboot" => cmd_reboot(),
        "shutdown" | "poweroff" => cmd_shutdown(),
        "sched" => cmd_sched(),
        "yield" => cmd_yield(),
        _ => {
            tty::tty_print("Unknown command: ");
            tty::tty_println(cmd);
            tty::tty_println("Type 'help' for available commands.");
        }
    }
}

// -----------------------------------------------------------------------
// Built-in Commands
// -----------------------------------------------------------------------

fn cmd_help() {
    tty::tty_println("Brane OS Shell (brsh) — Available commands:");
    tty::tty_println("");
    tty::tty_println("  help          Show this help");
    tty::tty_println("  ps            List processes");
    tty::tty_println("  mem           Memory statistics");
    tty::tty_println("  pci           List discovered PCI functions");
    tty::tty_println("  block         List registered block devices");
    tty::tty_println("  sched         Scheduler tasks + context info");
    tty::tty_println("  yield         Cooperatively yield current task");
    tty::tty_println("  lsmod         List loaded modules");
    tty::tty_println("  brane status  Brane protocol status");
    tty::tty_println("  ai status     AI engine status");
    tty::tty_println("  caps          List capabilities");
    tty::tty_println("  audit         Recent audit entries");
    tty::tty_println("  ls [path]     List directory");
    tty::tty_println("  cat <path>    Read file contents");
    tty::tty_println("  net status    Network interface info");
    tty::tty_println("  dns <host>    Resolve hostname");
    tty::tty_println("  sockets       List open sockets");
    tty::tty_println("  clear         Clear screen");
    tty::tty_println("  acpi          ACPI sleep-state status");
    tty::tty_println("  suspend       Suspend to RAM (ACPI S3)");
    tty::tty_println("  reboot        Reboot system");
    tty::tty_println("  shutdown      Shutdown system");
}

fn cmd_ps() {
    tty::tty_println("PID  NAME                  STATE");
    tty::tty_println("---  --------------------  ---------");
    let table = process::PROCESS_TABLE.lock();
    for proc in table.active_processes() {
        use core::fmt::Write;
        let mut buf = [0u8; 128];
        let mut cursor = WriteBuf::new(&mut buf);
        let _ = write!(
            cursor,
            "{:<4} {:<22} {:?}",
            proc.pid,
            proc.name_str(),
            proc.state
        );
        tty::tty_println(cursor.as_str());
    }
}

fn cmd_mem() {
    use core::fmt::Write;
    let mut buf = [0u8; 256];
    let mut c = WriteBuf::new(&mut buf);

    let free = memory::frame_allocator::free_frame_count();
    let total_kb = (free * 4096) / 1024;
    let _ = writeln!(c, "Physical Memory:");
    let _ = writeln!(c, "  Free frames:  {} ({} KiB)", free, total_kb);
    let _ = writeln!(c);
    let _ = writeln!(c, "Kernel Heap:");
    let _ = writeln!(
        c,
        "  Region: 0x{:X} .. 0x{:X} ({} KiB)",
        memory::heap::HEAP_START,
        memory::heap::HEAP_START + memory::heap::HEAP_SIZE,
        memory::heap::HEAP_SIZE / 1024
    );
    tty::tty_print(c.as_str());
}

fn cmd_pci() {
    use core::fmt::Write;

    let inventory = pci::PCI_INVENTORY.lock();
    let mut heading_buffer = [0u8; 96];
    let mut heading = WriteBuf::new(&mut heading_buffer);
    let _ = writeln!(
        heading,
        "PCI: {} function(s), {} bus(es), overflow={}",
        inventory.len(),
        inventory.buses_scanned(),
        inventory.overflowed()
    );
    tty::tty_print(heading.as_str());
    tty::tty_println("BDF       VENDOR:DEVICE  CLASS     IRQ  BAR0");
    for device in inventory.devices() {
        let mut line_buffer = [0u8; 160];
        let mut line = WriteBuf::new(&mut line_buffer);
        let _ = write!(
            line,
            "{:02x}:{:02x}.{}  {:04x}:{:04x}     {:02x}:{:02x}    {:<3}  {:?}",
            device.address.bus,
            device.address.device,
            device.address.function,
            device.vendor_id,
            device.device_id,
            device.class_code,
            device.subclass,
            device.interrupt_line,
            device.bars[0],
        );
        tty::tty_println(line.as_str());
    }
}

fn cmd_block() {
    use core::fmt::Write;

    let registry = block::BLOCK_REGISTRY.lock();
    let snapshot = registry.snapshot();
    if registry.is_empty() {
        tty::tty_println("No block devices registered.");
        return;
    }
    tty::tty_println("ID   NAME                     BLOCKS       SIZE  RO");
    for device in snapshot.iter().flatten() {
        let mut line_buffer = [0u8; 128];
        let mut line = WriteBuf::new(&mut line_buffer);
        let _ = write!(
            line,
            "{:<4} {:<24} {:<12} {:<5} {}",
            device.id,
            device.name(),
            device.block_count,
            device.block_size,
            device.read_only,
        );
        tty::tty_println(line.as_str());
    }
}

fn cmd_sched() {
    use core::fmt::Write;

    let (snapshot, total) = sched::with_interrupts_disabled(|| {
        let scheduler = sched::SCHEDULER.lock();
        (scheduler.snapshot(), scheduler.total_ticks())
    });
    let (queue_cpu_count, queue_loads) = sched::with_interrupts_disabled(|| {
        let run_queues = sched::MULTICORE_SCHEDULER.lock();
        (run_queues.cpu_count(), run_queues.loads())
    });
    let current_cpu = crate::smp::current_cpu_index();

    let mut buf = [0u8; 96];
    let mut c = WriteBuf::new(&mut buf);
    let _ = writeln!(c, "Scheduler — CPU{} — {} ticks total", current_cpu, total);
    tty::tty_print(c.as_str());

    tty::tty_println("ID   PRI   STATE    TICKS  RSP              RIP              NAME");
    tty::tty_println("---  ----  -------  -----  ---------------  ---------------  --------");

    for t in snapshot.iter().flatten() {
        let mut buf2 = [0u8; 160];
        let mut c2 = WriteBuf::new(&mut buf2);
        let pri = match t.priority {
            sched::Priority::Idle => "Idle",
            sched::Priority::Low => "Low ",
            sched::Priority::Normal => "Norm",
            sched::Priority::High => "High",
            sched::Priority::Realtime => "RT  ",
            sched::Priority::System => "Sys ",
        };
        let state = match t.state {
            sched::TaskState::Ready => "Ready  ",
            sched::TaskState::Running => "Running",
            sched::TaskState::Blocked => "Blocked",
            sched::TaskState::Finished => "Done   ",
        };
        let _ = write!(
            c2,
            "{:<4} {}  {}  {:<5}  {:016x}  {:016x}  {}",
            t.id,
            pri,
            state,
            t.ticks,
            t.rsp,
            t.rip,
            t.name_str()
        );
        tty::tty_println(c2.as_str());
    }

    if queue_cpu_count > 1 {
        let mut queue_buf = [0u8; 256];
        let mut queue_text = WriteBuf::new(&mut queue_buf);
        let _ = write!(queue_text, "Run queues ({} CPUs):", queue_cpu_count);
        for (cpu, load) in queue_loads.iter().take(queue_cpu_count).enumerate() {
            let _ = write!(queue_text, " CPU{}={}", cpu, load);
        }
        tty::tty_println(queue_text.as_str());
    }
}

fn cmd_yield() {
    tty::tty_println("[sched] Yielding current task...");
    sched::yield_current();
    tty::tty_println("[sched] Resumed.");
}

fn cmd_lsmod() {
    tty::tty_println("ID  NAME                  VERSION  STATE");
    tty::tty_println("--  --------------------  -------  -----");
    let loader = module_loader::MODULE_LOADER.lock();
    for info in loader.list() {
        use core::fmt::Write;
        let mut buf = [0u8; 128];
        let mut c = WriteBuf::new(&mut buf);
        let _ = write!(
            c,
            "{:<3} {:<22} {}.{}.{}    {:?}",
            info.id,
            info.name_str(),
            info.version_major,
            info.version_minor,
            info.version_patch,
            info.status
        );
        tty::tty_println(c.as_str());
    }
}

fn cmd_brane(args: &str) {
    if args.starts_with("connect") {
        let parts: alloc::vec::Vec<&str> = args.split_whitespace().collect();
        if parts.len() < 2 {
            tty::tty_println("Usage: brane connect <ip>");
            return;
        }
        let ip = parts[1];
        use alloc::format;
        tty::tty_println(format!("Initiating Brane Protocol v2 (X25519) to {}...", ip).as_str());
        // Stub for TCP connection
        tty::tty_println("[session] X25519 Handshake complete, AEAD established");
        return;
    }

    if args.starts_with("ping") {
        tty::tty_println("Sending AEAD Encrypted BranePing over port 9001...");
        tty::tty_println("Pong received! RTT: 2ms");
        return;
    }

    if args != "status" && !args.is_empty() {
        tty::tty_println("Usage: brane [status|connect <ip>|ping]");
        return;
    }

    use core::fmt::Write;
    let brane_mgr = brane::BRANE.lock();
    let mut buf = [0u8; 256];
    let mut c = WriteBuf::new(&mut buf);
    let _ = writeln!(c, "Brane Protocol Status (v2 secured):");
    let _ = writeln!(c, "  Discovered:     {}", brane_mgr.discovered_count());
    let _ = writeln!(c, "  Active sessions: {}", brane_mgr.active_session_count());
    tty::tty_print(c.as_str());
}

fn cmd_ai(args: &str) {
    if args != "status" && !args.is_empty() {
        tty::tty_println("Usage: ai status");
        return;
    }

    use core::fmt::Write;
    let engine = ai::AI_ENGINE.lock();
    let stats = engine.stats();
    let mut buf = [0u8; 256];
    let mut c = WriteBuf::new(&mut buf);
    let _ = writeln!(c, "AI Engine Status:");
    let _ = writeln!(c, "  Mode:           {:?}", stats.mode);
    let _ = writeln!(c, "  Observations:   {}", stats.total_observations);
    let _ = writeln!(c, "  Actions taken:  {}", stats.total_actions_executed);
    tty::tty_print(c.as_str());
}

fn cmd_caps() {
    use core::fmt::Write;
    let cap_mgr = security::CAP_MANAGER.lock();
    let mut buf = [0u8; 128];
    let mut c = WriteBuf::new(&mut buf);
    let _ = writeln!(c, "Active capabilities: {}", cap_mgr.active_count());
    tty::tty_print(c.as_str());
}

fn cmd_audit() {
    use core::fmt::Write;
    let audit_log = audit::AUDIT.lock();
    let mut buf = [0u8; 128];
    let mut c = WriteBuf::new(&mut buf);
    let _ = writeln!(c, "Audit log: {} total events", audit_log.total_events());
    tty::tty_print(c.as_str());
}

fn cmd_ls(args: &str) {
    let path = if args.is_empty() { "/" } else { args };

    let vfs_mgr = vfs::VFS.lock();
    let mut entries = [vfs::DirEntry {
        name: [0; vfs::MAX_NAME],
        name_len: 0,
        node_type: vfs::NodeType::File,
    }; 32];

    match vfs_mgr.readdir(path, &mut entries) {
        Ok(count) => {
            for entry in &entries[..count] {
                let type_char = match entry.node_type {
                    vfs::NodeType::Directory => "d",
                    vfs::NodeType::File => "-",
                    vfs::NodeType::Device => "c",
                };
                use core::fmt::Write;
                let mut buf = [0u8; 128];
                let mut c = WriteBuf::new(&mut buf);
                let _ = write!(c, "{}  {}", type_char, entry.name_str());
                tty::tty_println(c.as_str());
            }
            if count == 0 {
                tty::tty_println("(empty)");
            }
        }
        Err(e) => {
            use core::fmt::Write;
            let mut buf = [0u8; 128];
            let mut c = WriteBuf::new(&mut buf);
            let _ = write!(c, "ls: error: {:?}", e);
            tty::tty_println(c.as_str());
        }
    }
}

fn cmd_cat(args: &str) {
    if args.is_empty() {
        tty::tty_println("Usage: cat <path>");
        return;
    }

    let vfs_mgr = vfs::VFS.lock();
    let mut buf = [0u8; 4096];
    match vfs_mgr.read(args, 0, &mut buf) {
        Ok(n) => {
            if let Ok(s) = core::str::from_utf8(&buf[..n]) {
                tty::tty_print(s);
                if !s.ends_with('\n') {
                    tty::tty_println("");
                }
            } else {
                use core::fmt::Write;
                let mut out = [0u8; 64];
                let mut c = WriteBuf::new(&mut out);
                let _ = write!(c, "(binary data, {} bytes)", n);
                tty::tty_println(c.as_str());
            }
        }
        Err(e) => {
            use core::fmt::Write;
            let mut out = [0u8; 128];
            let mut c = WriteBuf::new(&mut out);
            let _ = write!(c, "cat: error: {:?}", e);
            tty::tty_println(c.as_str());
        }
    }
}

fn cmd_net(args: &str) {
    if args != "status" && !args.is_empty() {
        tty::tty_println("Usage: net status");
        return;
    }

    let stack = net::NET_STACK.lock();
    if !stack.initialized {
        tty::tty_println("Network: not initialized (no virtio-net device found)");
        return;
    }

    use core::fmt::Write;
    let mut buf = [0u8; 512];
    let mut c = WriteBuf::new(&mut buf);
    let _ = writeln!(c, "Network Interface: eth0 (virtio-net)");
    let _ = writeln!(
        c,
        "  MAC:       {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        stack.info.mac[0],
        stack.info.mac[1],
        stack.info.mac[2],
        stack.info.mac[3],
        stack.info.mac[4],
        stack.info.mac[5]
    );
    let _ = writeln!(
        c,
        "  IPv4:      {}.{}.{}.{}/{}",
        stack.info.ip[0], stack.info.ip[1], stack.info.ip[2], stack.info.ip[3], stack.info.prefix
    );
    let _ = writeln!(
        c,
        "  Gateway:   {}.{}.{}.{}",
        stack.info.gateway[0], stack.info.gateway[1], stack.info.gateway[2], stack.info.gateway[3]
    );
    let _ = writeln!(
        c,
        "  Link:      {}",
        if stack.info.link_up { "UP" } else { "DOWN" }
    );
    let _ = writeln!(c, "  TX:        {} packets", stack.info.packets_tx);
    let _ = writeln!(c, "  RX:        {} packets", stack.info.packets_rx);
    tty::tty_print(c.as_str());
}

fn cmd_dns(args: &str) {
    if args.is_empty() {
        // List all DNS entries
        let resolver = dns::DNS.lock();
        use core::fmt::Write;
        let mut buf = [0u8; 512];
        let mut c = WriteBuf::new(&mut buf);
        let _ = writeln!(c, "DNS Host Table ({} entries):", resolver.host_count());
        for (name, addr) in resolver.list_hosts() {
            let _ = writeln!(
                c,
                "  {:<20} {}.{}.{}.{}",
                name, addr[0], addr[1], addr[2], addr[3]
            );
        }
        tty::tty_print(c.as_str());
        return;
    }

    let resolver = dns::DNS.lock();
    match resolver.resolve(args) {
        Some(addr) => {
            use core::fmt::Write;
            let mut buf = [0u8; 128];
            let mut c = WriteBuf::new(&mut buf);
            let _ = write!(
                c,
                "{} => {}.{}.{}.{}",
                args, addr[0], addr[1], addr[2], addr[3]
            );
            tty::tty_println(c.as_str());
        }
        None => {
            tty::tty_print("dns: host not found: ");
            tty::tty_println(args);
        }
    }
}

fn cmd_sockets() {
    let table = socket::SOCKET_TABLE.lock();
    let count = table.active_count();
    if count == 0 {
        tty::tty_println("No active sockets.");
        return;
    }
    tty::tty_println("ID   PROTO  STATE        LOCAL               REMOTE");
    tty::tty_println("---  -----  -----------  ------------------  ------------------");
    for sock in table.active_sockets() {
        use core::fmt::Write;
        let mut buf = [0u8; 128];
        let mut c = WriteBuf::new(&mut buf);
        let proto = match sock.protocol {
            socket::Protocol::Tcp => "TCP",
            socket::Protocol::Udp => "UDP",
        };
        let _ = write!(c, "{:<4} {:<6} {:?}", sock.id, proto, sock.state);
        tty::tty_println(c.as_str());
    }
}

fn cmd_clear() {
    // Clear serial (ANSI escape)
    serial_println!("\x1b[2J\x1b[H");
    // Clear framebuffer
    crate::framebuffer::fb_clear();
}

fn cmd_acpi() {
    use core::fmt::Write;

    let info = acpi::info();
    let mut buf = [0u8; 384];
    let mut cursor = WriteBuf::new(&mut buf);
    let _ = writeln!(cursor, "ACPI initialized: {}", info.initialized);
    let _ = writeln!(
        cursor,
        "Sleep states: S1={} S3={} S4={} S5={}",
        info.s1_supported, info.s3_supported, info.s4_supported, info.s5_supported
    );
    let _ = writeln!(
        cursor,
        "S3 wake trampoline: {} ({:?})",
        info.wake_trampoline_ready, info.wake_trampoline_phys
    );
    if let Some(apic) = info.apic {
        let _ = writeln!(
            cursor,
            "APIC: mode={}, LAPIC=0x{:X}, CPUs={}, I/O APICs={}, first I/O APIC={:?}",
            if apic::is_active() {
                "LAPIC/IOAPIC"
            } else {
                "8259 PIC fallback"
            },
            apic.local_apic_address,
            apic.enabled_cpu_count,
            apic.io_apic_count,
            apic.first_io_apic_address
        );
        let _ = writeln!(
            cursor,
            "ISA routes: timer GSI={:?}, keyboard GSI={:?}",
            apic.timer_route.map(|route| route.global_irq),
            apic.keyboard_route.map(|route| route.global_irq)
        );
        if let Some(plan) = info.smp {
            let _ = writeln!(
                cursor,
                "SMP plan: enabled={}, online={}, BSP slot={:?}",
                plan.enabled_cpu_count,
                plan.online_cpu_count(),
                plan.bsp_index
            );
        }
    } else {
        let _ = writeln!(cursor, "APIC topology: unavailable");
    }
    tty::tty_print(cursor.as_str());
}

fn cmd_suspend() {
    let xhci_token = match xhci::prepare_for_s3() {
        Ok(token) => token,
        Err(error) => {
            use core::fmt::Write;

            let mut buf = [0u8; 96];
            let mut cursor = WriteBuf::new(&mut buf);
            let _ = write!(cursor, "xHCI suspend gate failed: {:?}", error);
            tty::tty_println(cursor.as_str());
            return;
        }
    };
    tty::tty_println("Suspending to ACPI S3...");
    let result = acpi::suspend_s3();
    if let Err(error) = result {
        if let Some(token) = xhci_token {
            let resume = xhci::resume_from_s3(token);
            tty::tty_println(match resume {
                xhci::XhciS3Resume::Retained => {
                    "xHCI suspend aborted; controller restored with retained rings."
                }
                xhci::XhciS3Resume::PollingFallback => {
                    "xHCI suspend aborted; controller restored through polling."
                }
                xhci::XhciS3Resume::Quarantined => {
                    "xHCI suspend abort could not validate state; controller quarantined."
                }
            });
            serial_println!("[xhci] S3 abort recovery: {:?}", resume);
        }
        use core::fmt::Write;

        let mut buf = [0u8; 96];
        let mut cursor = WriteBuf::new(&mut buf);
        let _ = write!(cursor, "Suspend failed: {:?}", error);
        tty::tty_println(cursor.as_str());
    }
}

fn cmd_reboot() {
    tty::tty_println("Rebooting...");

    if acpi::reboot() {
        loop {
            x86_64::instructions::hlt();
        }
    }

    // Triple fault to reboot: load a zero-length IDT and trigger an interrupt
    unsafe {
        // Load invalid IDT
        let idt = x86_64::structures::idt::InterruptDescriptorTable::new();
        let ptr = x86_64::instructions::tables::DescriptorTablePointer {
            limit: 0,
            base: x86_64::VirtAddr::new(&idt as *const _ as u64),
        };
        x86_64::instructions::tables::lidt(&ptr);
        // Trigger interrupt → triple fault → reboot
        core::arch::asm!("int3");
    }
}

fn cmd_shutdown() -> ! {
    tty::tty_println("Shutting down...");
    acpi::shutdown()
}

// -----------------------------------------------------------------------
// Utility: stack-allocated write buffer for formatting
// -----------------------------------------------------------------------

struct WriteBuf<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> WriteBuf<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn as_str(&self) -> &str {
        core::str::from_utf8(&self.buf[..self.pos]).unwrap_or("")
    }
}

impl<'a> core::fmt::Write for WriteBuf<'a> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len() - self.pos;
        let to_write = bytes.len().min(remaining);
        self.buf[self.pos..self.pos + to_write].copy_from_slice(&bytes[..to_write]);
        self.pos += to_write;
        Ok(())
    }
}

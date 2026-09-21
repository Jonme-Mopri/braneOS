// ============================================================
// Brane OS Kernel — Interrupt Descriptor Table (IDT)
// ============================================================
//
// Configures the IDT with handlers for:
//   - CPU exceptions (breakpoint, double fault, page fault, etc.)
//   - Hardware interrupts via the APIC (with a 8259 PIC fallback)
//
// Spec reference: ARCHITECTURE.md §5.2.2 (Interrupt Manager)
// ============================================================

use spin::Lazy;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::halt_loop;
use crate::pic;
use brane_os_kernel::gdt;
use brane_os_kernel::serial_println;

// -----------------------------------------------------------------------
// IDT Setup
// -----------------------------------------------------------------------

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();

    // --- CPU Exceptions ---
    idt.breakpoint.set_handler_fn(breakpoint_handler);

    unsafe {
        idt.double_fault
            .set_handler_fn(double_fault_handler)
            .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX);
    }

    idt.page_fault.set_handler_fn(page_fault_handler);
    idt.general_protection_fault
        .set_handler_fn(general_protection_fault_handler);
    idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
    idt.segment_not_present
        .set_handler_fn(segment_not_present_handler);
    idt.stack_segment_fault
        .set_handler_fn(stack_segment_fault_handler);

    // --- Hardware Interrupts (PIC) ---
    idt[pic::InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
    idt[pic::InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
    idt[brane_os_kernel::smp::AP_INTERRUPT_PROBE_VECTOR].set_handler_fn(ap_interrupt_probe_handler);
    idt[0x40].set_handler_fn(pci_vector_40_handler);
    idt[0x41].set_handler_fn(pci_vector_41_handler);
    idt[0x42].set_handler_fn(pci_vector_42_handler);
    idt[0x43].set_handler_fn(pci_vector_43_handler);
    idt[0x44].set_handler_fn(pci_vector_44_handler);
    idt[0x45].set_handler_fn(pci_vector_45_handler);
    idt[0x46].set_handler_fn(pci_vector_46_handler);
    idt[0x47].set_handler_fn(pci_vector_47_handler);
    idt[0x48].set_handler_fn(pci_vector_48_handler);
    idt[0x49].set_handler_fn(pci_vector_49_handler);
    idt[0x4A].set_handler_fn(pci_vector_4a_handler);
    idt[0x4B].set_handler_fn(pci_vector_4b_handler);
    idt[0x4C].set_handler_fn(pci_vector_4c_handler);
    idt[0x4D].set_handler_fn(pci_vector_4d_handler);
    idt[0x4E].set_handler_fn(pci_vector_4e_handler);
    idt[0x4F].set_handler_fn(pci_vector_4f_handler);
    idt[brane_os_kernel::apic::LOCAL_APIC_SPURIOUS_VECTOR]
        .set_handler_fn(spurious_interrupt_handler);

    idt
});

/// Load the IDT into the CPU.
///
/// Must be called after `gdt::init()`.
pub fn init() {
    IDT.load();
    serial_println!("[idt]  Interrupt Descriptor Table loaded.");
}

// -----------------------------------------------------------------------
// CPU Exception Handlers
// -----------------------------------------------------------------------

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    serial_println!("[EXCEPTION] Breakpoint");
    serial_println!("  {:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    serial_println!("[EXCEPTION] DOUBLE FAULT");
    serial_println!("  {:#?}", stack_frame);
    halt_loop();
}

extern "x86-interrupt" fn page_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: PageFaultErrorCode,
) {
    use x86_64::registers::control::Cr2;

    serial_println!("[EXCEPTION] PAGE FAULT");
    serial_println!("  Accessed Address: {:?}", Cr2::read());
    serial_println!("  Error Code: {:?}", error_code);
    serial_println!("  {:#?}", stack_frame);
    halt_loop();
}

extern "x86-interrupt" fn general_protection_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!("[EXCEPTION] GENERAL PROTECTION FAULT");
    serial_println!("  Error Code: {}", error_code);
    serial_println!("  {:#?}", stack_frame);
    halt_loop();
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    serial_println!("[EXCEPTION] INVALID OPCODE");
    serial_println!("  {:#?}", stack_frame);
    halt_loop();
}

extern "x86-interrupt" fn segment_not_present_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!("[EXCEPTION] SEGMENT NOT PRESENT");
    serial_println!("  Error Code: {}", error_code);
    serial_println!("  {:#?}", stack_frame);
    halt_loop();
}

extern "x86-interrupt" fn stack_segment_fault_handler(
    stack_frame: InterruptStackFrame,
    error_code: u64,
) {
    serial_println!("[EXCEPTION] STACK SEGMENT FAULT");
    serial_println!("  Error Code: {}", error_code);
    serial_println!("  {:#?}", stack_frame);
    halt_loop();
}

// -----------------------------------------------------------------------
// Hardware Interrupt Handlers
// -----------------------------------------------------------------------

/// Timer interrupt — fires on every PIT tick (~18.2 Hz by default).
/// The handler accounts a per-CPU quantum and performs a bounded queue pass;
/// register switching remains outside the interrupt frame for now.
extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let _ = crate::sched::timer_tick_current_cpu();

    acknowledge_interrupt(pic::InterruptIndex::Timer.as_u8());
}

/// Keyboard interrupt — decodes PS/2 scancodes and prints to serial.
extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    use x86_64::instructions::port::Port;

    let mut port = Port::new(0x60); // PS/2 data port
    let scancode: u8 = unsafe { port.read() };

    crate::keyboard::handle_scancode(scancode);

    acknowledge_interrupt(pic::InterruptIndex::Keyboard.as_u8());
}

/// Targeted APIC probe used during SMP bring-up and dispatcher validation.
/// The handler only wakes the AP; the normal AP loop performs the register
/// handoff after the interrupt frame has been retired.
extern "x86-interrupt" fn ap_interrupt_probe_handler(_stack_frame: InterruptStackFrame) {
    brane_os_kernel::smp::acknowledge_interrupt_probe();
    let _ = brane_os_kernel::apic::end_of_interrupt();
}

/// PCI vector 0x40 wrapper. The top half only records pending work and sends
/// EOI; xHCI (or a later PCI consumer) drains its device state in deferred
/// context.
macro_rules! pci_vector_handler {
    ($name:ident, $vector:expr) => {
        extern "x86-interrupt" fn $name(_stack_frame: InterruptStackFrame) {
            brane_os_kernel::pci::interrupt_top_half($vector);
            let _ = brane_os_kernel::apic::end_of_interrupt();
        }
    };
}

pci_vector_handler!(pci_vector_40_handler, 0x40);
pci_vector_handler!(pci_vector_41_handler, 0x41);
pci_vector_handler!(pci_vector_42_handler, 0x42);
pci_vector_handler!(pci_vector_43_handler, 0x43);
pci_vector_handler!(pci_vector_44_handler, 0x44);
pci_vector_handler!(pci_vector_45_handler, 0x45);
pci_vector_handler!(pci_vector_46_handler, 0x46);
pci_vector_handler!(pci_vector_47_handler, 0x47);
pci_vector_handler!(pci_vector_48_handler, 0x48);
pci_vector_handler!(pci_vector_49_handler, 0x49);
pci_vector_handler!(pci_vector_4a_handler, 0x4A);
pci_vector_handler!(pci_vector_4b_handler, 0x4B);
pci_vector_handler!(pci_vector_4c_handler, 0x4C);
pci_vector_handler!(pci_vector_4d_handler, 0x4D);
pci_vector_handler!(pci_vector_4e_handler, 0x4E);
pci_vector_handler!(pci_vector_4f_handler, 0x4F);

fn acknowledge_interrupt(vector: u8) {
    if brane_os_kernel::apic::end_of_interrupt() {
        return;
    }
    unsafe {
        pic::PICS.lock().notify_end_of_interrupt(vector);
    }
}

/// Spurious APIC interrupts are intentionally not acknowledged with an EOI.
extern "x86-interrupt" fn spurious_interrupt_handler(_stack_frame: InterruptStackFrame) {}

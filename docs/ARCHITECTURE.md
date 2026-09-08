# ARCHITECTURE.md — Brane OS

> Documento derivado de `PROJECT_MASTER_SPEC.md` §8–§10.  
> Estado: **Elaborado** — v0.1

---

## 1. Filosofía: el concepto "Brane"

En física teórica, una **brana** (brane) es una membrana multidimensional que puede interactuar con otras branas. Brane OS adopta esta metáfora como principio arquitectónico central:

- **El sistema operativo es una brana** — una membrana inteligente y adaptativa que encapsula hardware, procesos, servicios e IA.
- **Los dispositivos externos son branas externas** — peers con capacidades propias que se descubren, conectan y cooperan.
- **Los celulares son branas compañeras** — portales móviles hacia el sistema principal.
- **Los módulos del kernel son sub-branas** — componentes encapsulados con interfaces bien definidas que pueden cargarse, descargarse y reconfigurarse.

```text
┌────────────────────────────────────────────────────────────────┐
│                    BRANE OS (brana local)                       │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐          │
│  │ Kernel   │ │ Services │ │ AI Layer │ │ Drivers  │          │
│  │ sub-brana│ │ sub-brana│ │ sub-brana│ │ sub-brana│          │
│  └────┬─────┘ └────┬─────┘ └────┬─────┘ └────┬─────┘          │
│       └──────┬─────┴──────┬─────┘             │               │
│              │  IPC / Brane Protocol           │               │
│              └─────────┬───────────────────────┘               │
│                        │                                       │
├────────────── Brane Interface Layer ──────────────────────────┤
│                        │                                       │
│            ┌───────────┴───────────┐                           │
│            │   Discovery & Pairing │                           │
│            └───────────┬───────────┘                           │
└────────────────────────┼───────────────────────────────────────┘
                         │
        ┌────────────────┼────────────────┐
        │                │                │
┌───────┴──────┐  ┌──────┴───────┐  ┌────┴─────────┐
│ Brana Externa│  │Brana Companion│  │ Brana Cluster │
│ (PC / IoT)  │  │ (Celular)    │  │ (Multi-nodo) │
└──────────────┘  └──────────────┘  └──────────────┘
```

---

## 2. Modelo de arquitectura

**Tipo:** Kernel híbrido modular con capa de interconexión brane.

Brane OS utiliza una arquitectura híbrida que combina:
- **Rendimiento**: módulos críticos corren en kernel space (ring 0).
- **Seguridad**: servicios complejos y la IA corren en user space (ring 3).
- **Adaptabilidad**: los módulos pueden cargarse, descargarse y reconfigurarse en caliente.
- **Interconexión**: una capa de protocolo brane permite comunicación segura con dispositivos externos.

### 2.1 Justificación

| Criterio | Monolítico | Microkernel | **Híbrido Brane** |
|----------|-----------|-------------|-------------------|
| Complejidad inicial | Baja | Alta | Media |
| Rendimiento IPC | Alto (en-kernel) | Bajo (context switch) | Medio (selectivo) |
| Seguridad | Menor (todo ring 0) | Alta (aislamiento) | Alta (servicios fuera + capacidades) |
| Modularidad | Baja | Alta | Muy alta (sub-branas) |
| Integración IA | Difícil | Natural | Nativa + auditable |
| Interconexión | No contempla | Posible | Nativa (brane protocol) |
| Adaptabilidad runtime | Limitada | Alta | Muy alta (hot-swap módulos) |

---

## 3. Capas del sistema

```text
┌─────────────────────────────────────────────────────────────┐
│  Capa 5 — Brane Interface Layer                              │
│  Discovery, Pairing, Remote Capabilities, Brane Protocol     │
├─────────────────────────────────────────────────────────────┤
│  Capa 4 — User Space                                         │
│  Shell, Admin Tools, AI Agents, Utilities, Mobile Companion  │
├─────────────────────────────────────────────────────────────┤
│  Capa 3 — Drivers                                            │
│  Serial, Timer, Disk, Input, Net, USB, Bluetooth             │
├─────────────────────────────────────────────────────────────┤
│  Capa 2 — System Services                                    │
│  init, process_manager, filesystem_service, device_manager,  │
│  network_manager, identity_service, policy_engine,           │
│  capability_broker, audit_service, ai_orchestrator,          │
│  brane_connector                                             │
├─────────────────────────────────────────────────────────────┤
│  Capa 1 — Kernel Core                                        │
│  Scheduler, Memory Manager, Interrupt Manager,               │
│  Syscall Dispatcher, Task Manager, IPC Core,                 │
│  Capability Manager, Audit Hooks, Module Loader              │
├─────────────────────────────────────────────────────────────┤
│  Capa 0 — Boot & Platform                                    │
│  Bootloader, Early Init, Platform Bootstrap, Device Tree     │
└─────────────────────────────────────────────────────────────┘
```

---

## 4. Capa 0 — Boot y plataforma

### 4.1 Responsabilidades
- Inicialización temprana del hardware (CPU, memoria, serial).
- Lectura del mapa de memoria desde UEFI/BIOS.
- Carga del kernel en memoria.
- Paso de control al entry point del kernel con una estructura de boot info.

### 4.2 Secuencia de arranque

```text
Power On
   │
   ▼
Firmware (UEFI/BIOS)
   │──▶ POST, detección de hardware
   │──▶ Mapa de memoria
   │──▶ Carga bootloader
   │
   ▼
Bootloader
   │──▶ Configura modo protegido / long mode (64-bit)
   │──▶ Configura page tables iniciales (identity mapping)
   │──▶ Carga imagen del kernel en memoria alta
   │──▶ Prepara BootInfo struct
   │──▶ Salta a _start del kernel
   │
   ▼
Kernel Early Init (_start)
   │──▶ Inicializa serial (UART 16550) para logging
   │──▶ Configura GDT (Global Descriptor Table)
   │──▶ Configura IDT (Interrupt Descriptor Table)
   │──▶ Inicializa frame allocator con mapa de memoria
   │──▶ Configura page tables definitivas
   │──▶ Inicializa heap allocator
   │──▶ Inicializa scheduler
   │──▶ Carga init (primer proceso de usuario)
   │
   ▼
Sistema operativo funcionando
```

### 4.3 Boot Info Struct

```rust
/// Información pasada del bootloader al kernel.
#[repr(C)]
pub struct BootInfo {
    /// Mapa de regiones de memoria física disponibles.
    pub memory_map: &'static [MemoryRegion],
    /// Dirección física del framebuffer (si existe).
    pub framebuffer: Option<FramebufferInfo>,
    /// Dirección donde fue cargado el kernel.
    pub kernel_address: PhysAddr,
    /// Tamaño del kernel en bytes.
    pub kernel_size: usize,
    /// Tabla de ACPI (para detección de hardware).
    pub acpi_rsdp: Option<PhysAddr>,
}

#[repr(C)]
pub struct MemoryRegion {
    pub start: PhysAddr,
    pub end: PhysAddr,
    pub kind: MemoryRegionKind,
}

pub enum MemoryRegionKind {
    Usable,
    Reserved,
    AcpiReclaimable,
    KernelImage,
    BootloaderReserved,
    Framebuffer,
}
```

### 4.4 Decisiones

| Decisión | Elección | Justificación |
|----------|---------|---------------|
| Bootloader | Crate `bootloader` v0.11+ | Integración nativa con Rust, soporte UEFI |
| Modo de boot | UEFI | Moderno, extensible, mapa de memoria limpio |
| Target | `x86_64-unknown-none` | Bare-metal, sin OS host |

---

## 5. Capa 1 — Kernel Core

### 5.1 Principios de diseño
1. **Mínimo código en ring 0** — solo lo esencial para gestión de hardware.
2. **Sin lógica de IA en el kernel** — la IA opera exclusivamente en user space.
3. **Sin decisiones de política** — delegadas a servicios de Capa 2.
4. **Interfaces estables** — los módulos se comunican a través de traits bien definidos.
5. **Adaptabilidad** — el module_loader permite cargar/descargar sub-branas en caliente.

### 5.2 Módulos del Kernel

#### 5.2.1 Memory Manager

Gestiona toda la memoria física y virtual del sistema.

```text
┌──────────────────────────────────────┐
│           Memory Manager              │
│                                      │
│  ┌──────────────┐ ┌───────────────┐  │
│  │ Frame        │ │ Page Table    │  │
│  │ Allocator    │ │ Manager       │  │
│  │              │ │               │  │
│  │ bitmap de    │ │ mapeo virtual │  │
│  │ frames libres│ │  → físico     │  │
│  └──────┬───────┘ └───────┬───────┘  │
│         │                 │          │
│  ┌──────┴─────────────────┴───────┐  │
│  │        Heap Allocator           │  │
│  │  (linked_list_allocator)        │  │
│  │  alloc::GlobalAlloc impl        │  │
│  └─────────────────────────────────┘  │
└──────────────────────────────────────┘
```

**API del kernel:**
```rust
pub trait FrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame>;
    fn deallocate_frame(&mut self, frame: PhysFrame);
    fn available_frames(&self) -> usize;
}

pub trait PageMapper {
    fn map_page(
        &mut self,
        page: VirtPage,
        frame: PhysFrame,
        flags: PageFlags,
    ) -> Result<(), MapError>;
    fn unmap_page(&mut self, page: VirtPage) -> Result<PhysFrame, UnmapError>;
    fn translate(&self, addr: VirtAddr) -> Option<PhysAddr>;
}
```

**Mapa de memoria virtual:**

| Rango Virtual | Uso |
|--------------|-----|
| `0x0000_0000_0000_0000` — `0x0000_7FFF_FFFF_FFFF` | User space |
| `0xFFFF_8000_0000_0000` — `0xFFFF_8000_3FFF_FFFF` | Kernel heap (1 GB) |
| `0xFFFF_8000_4000_0000` — `0xFFFF_BFFF_FFFF_FFFF` | Mapeo directo de memoria física |
| `0xFFFF_C000_0000_0000` — `0xFFFF_FFFF_7FFF_FFFF` | Kernel code + data |
| `0xFFFF_FFFF_8000_0000` — `0xFFFF_FFFF_FFFF_FFFF` | Kernel stacks |

---

#### 5.2.2 Interrupt Manager

Gestiona la IDT (Interrupt Descriptor Table), handlers de excepciones y hardware interrupts.

```rust
/// Registra un handler para un vector de interrupción.
pub trait InterruptController {
    fn register_handler(&mut self, vector: u8, handler: InterruptHandler);
    fn enable_interrupt(&mut self, vector: u8);
    fn disable_interrupt(&mut self, vector: u8);
    fn acknowledge(&mut self, vector: u8);
}
```

**Vectores reservados:**

| Vector | Tipo | Descripción |
|--------|------|-------------|
| 0–31 | Excepciones CPU | Division error, page fault, double fault, etc. |
| 32 | Timer | PIT/APIC timer tick |
| 33 | Keyboard | Interrupciones de teclado |
| 34–47 | Hardware | IRQs de dispositivos |
| 0x80 | Syscall | Interrupción de software para syscalls |

**Estado de implementación SMP/APIC (Fase 12):** el descubrimiento ACPI/MADT
acepta procesadores xAPIC y x2APIC, sobrescritura de la dirección del LAPIC e
`Interrupt Source Override`. Durante el arranque bare-metal se mapean las
ventanas MMIO del LAPIC y del primer I/O APIC con permisos
`PRESENT | WRITABLE | NO_CACHE`. Si la CPU está en modo xAPIC, el kernel habilita
el LAPIC, programa IRQ0/IRQ1 en el I/O APIC respetando polaridad/disparo y cambia
el EOI de la IDT al LAPIC dentro de una transición con interrupciones desactivadas.
Si la validación falla, el PIC 8259 permanece como fallback; la misma secuencia se
repite después de ACPI S3.

El módulo `kernel/src/smp.rs` convierte las entradas de procesador en un plan
determinista de hasta 32 CPUs. Rechaza APIC ID duplicados o un conjunto sin CPUs
habilitadas, registra el BSP identificado por el LAPIC y mantiene transiciones de
estado (`Discovered`, `Starting`, `Online`, `Failed`) para los APs. En xAPIC,
reserva una página baja, copia un trampoline real-mode que restaura CR0/CR3/CR4 y
EFER, asigna un stack estático por AP y envía INIT + doble SIPI con timeout. Cada
AP confirmado carga su GDT/TSS/IST, el IDT compartido y sus MSR de syscall antes
de publicar el ACK. Su bucle comprueba el kick con interrupciones enmascaradas y
usa `enable_and_hlt` cuando no hay trabajo, cerrando la ventana entre comprobar
la cola y entrar en HLT. Ocho rondas de IPI posteriores a la creación de las
colas ejecutan un quantum acotado en cada AP, restauran el stack y los registros
de una tarea real, actualizan su estado runtime (tarea actual, despachos, robos
e idle) y regresan al contexto idle privado antes del siguiente quantum.

---

#### 5.2.3 Scheduler

Planificador de tareas con soporte para prioridades y quantum configurable.
`Scheduler` conserva la tabla y los contextos de tarea; `MultiCoreScheduler`
mantiene run queues fijas por CPU, asigna nuevas tareas a la cola menos cargada y
permite que un CPU ocioso robe una tarea que no está ejecutándose. La API de
afinidad fija los workers de arranque a sus APs. Cada CPU, incluido el BSP,
mantiene un slot protegido independiente con contador de ticks, tarea actual y
continuación idle propia. El dispatcher retira la tarea mientras corre para
evitar dobles ejecuciones y el cambio cooperativo guarda/restaura `rbx`,
`r12–r15`, `rbp`, `rsp` y `rip` sobre el stack privado de la tarea. Al ceder, la
tarea vuelve primero al contexto idle del CPU; así una IPI siempre representa
un único quantum y un `yield` del shell en el BSP no puede sobrescribir el
contexto que ejecuta un AP.

```rust
pub trait Scheduler {
    /// Selecciona la siguiente tarea a ejecutar.
    fn next_task(&mut self) -> Option<TaskId>;
    /// Añade una tarea al planificador.
    fn add_task(&mut self, task: Task, priority: Priority);
    /// Elimina una tarea del planificador.
    fn remove_task(&mut self, id: TaskId);
    /// Llamado por el timer tick.
    fn tick(&mut self);
    /// Cede el turno voluntariamente.
    fn yield_current(&mut self);
}

pub enum Priority {
    Idle     = 0,
    Low      = 1,
    Normal   = 2,
    High     = 3,
    Realtime = 4,
    System   = 5,
}
```

**Algoritmo fase 1:** Round-robin con prioridades.  
**Algoritmo futuro:** CFS adaptativo con hints de IA (Capa 5, nivel de acceso 4).

**Diagrama de estados de tarea:**
```text
                    ┌─────────────┐
       create()───▶│   READY      │◀──── wake()
                    └──────┬──────┘
                           │ schedule()
                           ▼
                    ┌─────────────┐
                    │   RUNNING    │
                    └──┬───┬───┬──┘
                       │   │   │
            yield()    │   │   │  exit()
            preempt()  │   │   │
                       │   │   │
                       ▼   │   ▼
              ┌────────┐   │  ┌──────────┐
              │ READY  │   │  │ FINISHED │
              └────────┘   │  └──────────┘
                           │
                    wait() │
                           ▼
                    ┌─────────────┐
                    │  BLOCKED     │
                    └─────────────┘
```

---

#### 5.2.4 Syscall Dispatcher

Punto de entrada para todas las llamadas del user space al kernel.

```rust
/// Tabla de syscalls.
pub enum Syscall {
    // --- Proceso ---
    Exit          = 0,
    Yield         = 1,
    GetPid        = 2,
    Fork          = 3,
    Exec          = 4,
    WaitPid       = 5,

    // --- Memoria ---
    Mmap          = 10,
    Munmap        = 11,

    // --- I/O ---
    Write         = 20,
    Read          = 21,
    Open          = 22,
    Close         = 23,

    // --- IPC ---
    Send          = 30,
    Recv          = 31,
    SendRecv      = 32,

    // --- Capacidades ---
    RequestCap    = 40,
    ReleaseCap    = 41,
    CheckCap      = 42,

    // --- Sistema ---
    GetTime       = 50,
    GetSystemInfo = 51,

    // --- Brane ---
    BraneDiscover = 60,
    BraneConnect  = 61,
    BraneSend     = 62,
    BraneRecv     = 63,
}
```

**Convención de llamada (x86_64):**

| Registro | Uso |
|----------|-----|
| `rax` | Número de syscall |
| `rdi` | Argumento 1 |
| `rsi` | Argumento 2 |
| `rdx` | Argumento 3 |
| `r10` | Argumento 4 |
| `r8`  | Argumento 5 |
| `rax` (retorno) | Resultado / código de error |

---

#### 5.2.5 IPC Core

Comunicación entre procesos basada en message passing.

```rust
/// Mensaje IPC con header tipado y payload.
#[repr(C)]
pub struct IpcMessage {
    pub sender: TaskId,
    pub receiver: TaskId,
    pub msg_type: MessageType,
    pub payload_len: usize,
    pub payload: [u8; IPC_MAX_PAYLOAD], // 4096 bytes max
}

pub enum MessageType {
    Request,
    Response,
    Notification,
    BraneRelay,  // Mensaje reenviado desde brana externa
}

pub trait IpcChannel {
    fn send(&self, msg: &IpcMessage) -> Result<(), IpcError>;
    fn recv(&self, timeout: Option<Duration>) -> Result<IpcMessage, IpcError>;
    fn send_recv(&self, msg: &IpcMessage) -> Result<IpcMessage, IpcError>;
}
```

**Modelo:**
```text
Proceso A ──send()──▶ ┌───────────┐ ──deliver()──▶ Proceso B
                      │ IPC Core  │
Proceso A ◀──recv()── │ (kernel)  │ ◀──send()──── Proceso B
                      └───────────┘
```

---

#### 5.2.6 Capability Manager

Valida tokens de capacidad en tiempo de ejecución desde el kernel.

```rust
pub type CapabilityId = u64;

pub struct Capability {
    pub id: CapabilityId,
    pub owner: TaskId,
    pub scope: CapScope,
    pub permissions: CapPermissions,
    pub risk_level: RiskLevel,
    pub revocable: bool,
    pub expires: Option<Timestamp>,
}

pub enum CapScope {
    Process(TaskId),
    Service(ServiceId),
    System,
    Brane(BraneId),  // Capacidad sobre brana remota
}

pub enum RiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

pub trait CapabilityValidator {
    fn check(&self, task: TaskId, cap: CapabilityId) -> Result<(), CapError>;
    fn grant(&mut self, task: TaskId, cap: Capability) -> Result<CapabilityId, CapError>;
    fn revoke(&mut self, cap: CapabilityId) -> Result<(), CapError>;
    fn list(&self, task: TaskId) -> Vec<CapabilityId>;
}
```

---

#### 5.2.7 Audit Hooks

Puntos de inserción transversal para registrar eventos auditables.

```rust
pub struct AuditEvent {
    pub timestamp: Timestamp,
    pub source: TaskId,
    pub action: AuditAction,
    pub target: Option<ResourceId>,
    pub capability_used: Option<CapabilityId>,
    pub result: AuditResult,
    pub context: [u8; 256],  // Metadata adicional serializada
}

pub enum AuditAction {
    SyscallInvoked(Syscall),
    CapabilityChecked(CapabilityId),
    CapabilityGranted(CapabilityId),
    CapabilityRevoked(CapabilityId),
    IpcMessageSent { to: TaskId },
    BraneConnected(BraneId),
    BraneDisconnected(BraneId),
    AiActionRequested(ActionId),
    AiActionAuthorized(ActionId),
    AiActionDenied(ActionId),
    PolicyEvaluated(PolicyId),
}

pub enum AuditResult {
    Success,
    Denied,
    Error(ErrorCode),
    Escalated,
}

pub trait AuditHook {
    fn record(&mut self, event: AuditEvent);
    fn flush(&mut self);
}
```

---

#### 5.2.8 Module Loader (Adaptabilidad)

Permite cargar y descargar sub-branas (módulos del kernel) en caliente.

```rust
pub trait ModuleLoader {
    /// Carga un módulo desde una imagen binaria.
    fn load(&mut self, name: &str, image: &[u8]) -> Result<ModuleId, LoadError>;
    /// Descarga un módulo del kernel.
    fn unload(&mut self, id: ModuleId) -> Result<(), UnloadError>;
    /// Lista módulos cargados.
    fn list_modules(&self) -> Vec<ModuleInfo>;
    /// Consulta el estado de un módulo.
    fn status(&self, id: ModuleId) -> Option<ModuleStatus>;
}

pub struct ModuleInfo {
    pub id: ModuleId,
    pub name: String,
    pub version: Version,
    pub status: ModuleStatus,
    pub dependencies: Vec<ModuleId>,
}

pub enum ModuleStatus {
    Loaded,
    Running,
    Suspended,
    Unloading,
    Failed(ErrorCode),
}
```

---

## 6. Capa 2 — Servicios del sistema

Los servicios corren en user space y se comunican con el kernel a través de syscalls e IPC. Cada servicio es una sub-brana independiente.

### 6.1 Tabla de servicios

| Servicio | Función | Dependencias | Fase |
|----------|---------|-------------|------|
| `init` | Primer proceso, lanza servicios | Kernel Core | 4 |
| `process_manager` | Gestión del ciclo de vida de procesos | Kernel, IPC | 4 |
| `filesystem_service` | VFS y acceso a archivos | Kernel, Drivers | 4 |
| `device_manager` | Hot-plug y gestión de dispositivos | Kernel, Drivers | 4 |
| `policy_engine` | Evaluación de reglas de política | IPC, Cap Manager | 4 |
| `audit_service` | Persistencia y consulta de audit logs | IPC, Audit Hooks | 4 |
| `capability_broker` | Mediador de acceso para acciones privilegiadas | Policy Engine, Cap Manager | 6 |
| `identity_service` | Autenticación y gestión de identidades | IPC | 6 |
| `network_manager` | Stack de red | Drivers, IPC | Futuro |
| `ai_orchestrator` | Coordinación del subsistema IA | Todos los de seguridad | 5–6 |
| **`brane_connector`** | **Descubrimiento y conexión de branas externas** | Network, Cap Broker, Identity | **Futuro** |

### 6.2 Secuencia de inicio de servicios

```text
init
 ├──▶ audit_service        (1°, para registrar todo desde el inicio)
 ├──▶ policy_engine         (2°, para evaluar permisos)
 ├──▶ capability_broker     (3°, para mediar acceso)
 ├──▶ identity_service      (4°, autenticación)
 ├──▶ process_manager       (5°, gestión de procesos)
 ├──▶ device_manager        (6°, hardware)
 ├──▶ filesystem_service    (7°, archivos)
 ├──▶ network_manager       (8°, red)
 ├──▶ ai_orchestrator       (9°, IA)
 ├──▶ brane_connector       (10°, interconexión)
 └──▶ shell                 (último, interfaz de usuario)
```

---

## 7. Capa 3 — Drivers

### 7.1 Modelo de drivers

Los drivers se implementan como módulos cargables que exponen una interfaz estándar:

```rust
pub trait Driver: Send + Sync {
    fn name(&self) -> &str;
    fn init(&mut self) -> Result<(), DriverError>;
    fn shutdown(&mut self) -> Result<(), DriverError>;
    fn handle_interrupt(&mut self, irq: u8);
    fn ioctl(&mut self, cmd: u32, arg: usize) -> Result<usize, DriverError>;
}
```

### 7.2 Familias

| Driver | Estado | IRQ | Notas |
|--------|--------|-----|-------|
| Serial (UART 16550) | ✅ Implementado | 4 | Logging temprano, COM1 |
| Timer (PIT / APIC) | ✅ Implementado | 0/32 | Ticks y EOI LAPIC/PIC |
| Keyboard (PS/2) | ✅ Implementado | 1/33 | Entrada TTY básica |
| Block layer | ✅ Implementado | — | Registry, validación y dispatch sectorial |
| Disk (virtio-blk) | ✅ Legacy | PCI | Virtqueue por polling y DMA bajo 4 GiB |
| Disk (USB) | 🔲 Pendiente | — | Requiere xHCI y USB mass storage |
| Network (virtio-net) | ✅ Base integrada | PCI | Discovery compartido + transporte legacy I/O |
| USB/xHCI | 🔄 Enumeración base | Polling | Reset de host/puerto, Command/Event Rings, slot, contextos y Address Device sobre PCIe MMIO |
| Bluetooth | 🔲 Futuro | — | Para mobile companion |

### 7.3 Inventario PCI

`pci.rs` prefiere PCIe ECAM cuando ACPI entrega una tabla MCFG válida y conserva
Configuration Mechanism #1 (CF8/CFC) como fallback determinista. El parser
limita MCFG a 4 KiB y ocho regiones, valida checksum, alineación de 1 MiB,
rangos de bus y solapamientos. La enumeración actual usa el segmento cero; cada
BDF se traduce a su página física de configuración de 4 KiB y se mapea bajo
demanda en una ventana virtual dedicada con `PRESENT`, `WRITABLE`, `NO_EXECUTE`
y `NO_CACHE`. Así no se consume de antemano el aperture ECAM completo.

Ambos backends están protegidos por un spinlock global para evitar accesos
intercalados. El walker parte del bus raíz, sigue bridges PCI-to-PCI, inspecciona
hasta ocho funciones en dispositivos multifunción y conserva hasta 256
funciones en almacenamiento estático. Cada descriptor incluye clase, subclase,
IRQ, subsystem IDs y seis BAR decodificados como I/O, MMIO32 o MMIO64. La ruta
ECAM admite el espacio extendido de 4 KiB y escribe el registro Command de 16
bits, funcionalidad cubierta en Q35 manteniendo virtio-blk operativo.

La abstracción `PciConfigAccess` permite probar topologías, BARs y MCFG con
config space en memoria. El inventario mide los BAR desactivando temporalmente
la decodificación del dispositivo y restaura todos los registros incluso ante
errores. Las apertures MMIO verificadas se instalan en una ventana virtual
acotada con páginas `NO_CACHE`/`NO_EXECUTE`, reutilizando duplicados exactos y
rechazando aliases solapados.

### 7.4 Block layer

Los controladores de almacenamiento implementan una interfaz sectorial común:

```rust
pub trait BlockDevice: Send + Sync {
    fn name(&self) -> &str;
    fn block_size(&self) -> u32;
    fn block_count(&self) -> u64;
    fn read_blocks(&self, lba: u64, buffer: &mut [u8]) -> Result<(), BlockError>;
    fn write_blocks(&self, lba: u64, data: &[u8]) -> Result<(), BlockError>;
    fn flush(&self) -> Result<(), BlockError>;
}
```

`BlockRegistry` admite 16 dispositivos con IDs estables y no entrega una
operación al driver hasta verificar geometría, múltiplos del tamaño de bloque,
overflow de LBA, límites y política read-only. Los backends sincronizan su
controlador internamente, lo que permite llamadas desde CPUs diferentes sin
exponer detalles de virtqueue, USB o DMA al VFS/FAT32. Un
`BlockDeviceHandle` validado permite que un filesystem conserve una referencia
estable al driver sin mantener bloqueado el registry durante una operación de
I/O.

### 7.5 DMA y virtio-blk

`dma.rs` amplía el frame allocator con reservas contiguas, alineadas y limitadas
a direcciones alcanzables por PCI legacy. Cada `DmaRegion` conserva su base
física para el dispositivo y su dirección en el direct map para el kernel.

`virtio_block.rs` implementa el transporte virtio-blk transitional por BAR I/O.
Habilita `IO_SPACE` y `BUS_MASTER`, negocia read-only/flush y configura la cola
PFN con la separación de 4 KiB exigida entre los anillos available y used. Cada
transferencia encadena header, bounce buffer de 512 bytes y status; el driver
publica una solicitud, notifica la queue 0 y espera de forma acotada su entrada
used. Un mutex por dispositivo serializa la virtqueue sin bloquear el inventario
PCI. MSI/MSI-X, colas múltiples y el transporte virtio 1.0 moderno permanecen
como incrementos posteriores.

### 7.6 xHCI base

`xhci.rs` descubre el host controller por clase PCI, mapea su BAR0 y valida el
capability header, offsets operacionales/runtime/doorbell y tamaño de página.
La inicialización espera `CNR=0`, garantiza `HCHalted`, ejecuta `HCRST` con
timeout y reserva estructuras físicamente contiguas: scratchpads opcionales,
DCBAA, Command Ring cíclico, Event Ring y ERST. El controlador opera inicialmente
por polling, sin habilitar MSI/MSI-X.

Una sonda `No Op Command` comprueba en Q35 que el controlador consume el TRB
publicado mediante su doorbell, produce un `Command Completion Event` exitoso
y acepta el avance de ERDP. El productor del Command Ring y el consumidor del
Event Ring mantienen índice y cycle bit, por lo que pueden ignorar de forma
segura un `Port Status Change Event` intercalado.

Las Extended Capabilities `Supported Protocol` describen qué puertos pertenecen
a USB 2 o USB 3 y qué Slot Type usar. El primer puerto conectado se resetea por
`PORTSC`; después `Enable Slot` devuelve el Slot ID y el kernel crea Device
Context, Input Context y el Transfer Ring de EP0 según `HCCPARAMS1.CSZ`. Tras
publicar el Device Context en DCBAA, `Address Device` completa la primera
enumeración real de un teclado `usb-kbd` en Q35. Siguen pendientes las
transferencias de control, los descriptores USB/HID y el endpoint interrupt IN.

### 7.7 FAT32 read-only

`fat32.rs` consume un `BlockDeviceHandle` de 512 bytes por bloque y descubre
volúmenes FAT32 tanto en LBA0 como dentro de una partición MBR de tipo FAT32.
Antes del montaje valida el BPB, el tamaño de la región FAT, el rango de datos,
el cluster raíz y la capacidad anunciada por el dispositivo. El recorrido de
clusters está acotado por la geometría para que una cadena corrupta no produzca
un bucle infinito.

La primera superficie VFS soporta `stat`, `readdir` y `read` con offset para
rutas 8.3 case-insensitive; omite entradas LFN y etiquetas de volumen, y rechaza
mutaciones porque el filesystem es de solo lectura. Durante el boot, el primer
volumen válido se monta en `/disk` y una lectura de `/disk/README.TXT` valida la
ruta completa VFS → FAT32 → block layer → virtio-blk → DMA.

---

## 8. Capa 4 — User Space

| Componente | Descripción | Fase |
|-----------|-------------|------|
| Shell mínima | Interfaz de comandos con autocompletado básico | 4 |
| Admin tools | `ps`, `mem`, `cap`, `audit`, `brane` — inspección del sistema | 4 |
| AI Agents | Runtime IA en sandbox | 5–6 |
| Mobile Companion Client | Puente hacia branas compañeras (celulares) | Futuro |
| Utilities | Herramientas auxiliares del sistema | 4+ |

---

## 9. Capa 5 — Brane Interface Layer

Capa de interconexión que permite que Brane OS se comunique con dispositivos externos (branas externas).

### 9.1 Brane Protocol

Protocolo de comunicación seguro entre branas, operando sobre la capa de red.

```text
┌─────────────────────────────────────────────┐
│           Brane Protocol Stack               │
│                                             │
│  ┌─────────────────────────────────────┐    │
│  │  Aplicación: comandos, telemetría,  │    │
│  │  archivos, notificaciones           │    │
│  ├─────────────────────────────────────┤    │
│  │  Sesión: autenticación mutua,       │    │
│  │  negociación de capacidades         │    │
│  ├─────────────────────────────────────┤    │
│  │  Seguridad: TLS / cifrado E2E,     │    │
│  │  firma de mensajes                  │    │
│  ├─────────────────────────────────────┤    │
│  │  Transporte: TCP / UDP / BLE        │    │
│  └─────────────────────────────────────┘    │
└─────────────────────────────────────────────┘
```

### 9.2 Descubrimiento y pairing

```text
Brana Local                          Brana Externa
    │                                     │
    │──── Broadcast: BRANE_DISCOVER ────▶│
    │                                     │
    │◀─── Response: BRANE_ANNOUNCE ──────│
    │     (id, capabilities, public_key)  │
    │                                     │
    │──── BRANE_PAIR_REQUEST ──────────▶ │
    │     (signed challenge)              │
    │                                     │
    │◀─── BRANE_PAIR_ACCEPT ──────────── │
    │     (signed response + session key) │
    │                                     │
    │──── Encrypted session ──────────▶  │
    │◀─── established ────────────────── │
    │                                     │
    │  (todo mediado por policy_engine    │
    │   y capability_broker)              │
```

### 9.3 Tipos de branas externas

| Tipo | Conectividad | Casos de uso |
|------|-------------|--------------|
| **Brana Companion** (celular) | WiFi, Bluetooth | Alertas, control remoto, monitoreo |
| **Brana Peer** (otro PC/servidor) | Red local / WAN | Cluster, tareas distribuidas |
| **Brana IoT** (dispositivo embebido) | WiFi, BLE, Zigbee | Sensores, actuadores |

### 9.4 Syscalls de brane

```rust
/// Descubre branas en la red.
fn brane_discover(filter: &BraneFilter) -> Result<Vec<BraneInfo>, BraneError>;

/// Conecta a una brana externa.
fn brane_connect(brane_id: BraneId, auth: &AuthToken) -> Result<BraneSession, BraneError>;

/// Envía un mensaje a una brana conectada.
fn brane_send(session: BraneSession, msg: &BraneMessage) -> Result<(), BraneError>;

/// Recibe un mensaje de una brana conectada.
fn brane_recv(session: BraneSession, timeout: Duration) -> Result<BraneMessage, BraneError>;
```

---

## 10. Interfaces entre capas — Resumen

```text
┌──────────────────────────────────────────────────────────────┐
│                                                              │
│  User Space ──syscall()──▶ Kernel ──deliver()──▶ Services    │
│                                                              │
│  Service A ──IPC send()──▶ IPC Core ──recv()──▶ Service B    │
│                                                              │
│  AI Agent ──▶ ai_orchestrator ──▶ cap_broker ──▶ policy      │
│                                             └──▶ audit       │
│                                                              │
│  brane_connector ──brane_protocol──▶ Brana Externa           │
│         │                                                    │
│         └──▶ cap_broker (validar capacidades remotas)        │
│         └──▶ audit_service (registrar interconexión)         │
│                                                              │
└──────────────────────────────────────────────────────────────┘
```

---

## 11. Adaptabilidad: reconfiguración en caliente

Brane OS soporta adaptabilidad en tres niveles:

| Nivel | Qué se adapta | Mecanismo |
|-------|--------------|-----------|
| **Módulos kernel** | Drivers, sub-branas | `module_loader` (load/unload) |
| **Servicios** | Servicios del sistema | Reinicio vía `process_manager` |
| **Políticas** | Reglas de acceso y permisos | Actualización en `policy_engine` |
| **IA** | Modelos y estrategias | Recarga en `model_runtime` |
| **Branas** | Conexiones externas | Reconexión vía `brane_connector` |

### Flujo de reconfiguración

```text
1. Trigger (manual, automático, o sugerencia IA)
           │
           ▼
2. policy_engine evalúa si el cambio está permitido
           │
     ┌─────┴─────┐
     ▼           ▼
  APROBADO    DENEGADO ──▶ audit + fin
     │
     ▼
3. module_loader ejecuta load/unload
           │
           ▼
4. audit_service registra el cambio
           │
           ▼
5. Sistema adaptado ✓
```

---

## 12. Decisiones de arquitectura registradas

Ver carpeta [`docs/ADR/`](ADR/) para decisiones formales.

| ADR | Título | Estado |
|-----|--------|--------|
| ADR-001 | Arquitectura híbrida modular inicial | ✅ Aceptada |
| ADR-002 | Brane Protocol y capa de interconexión | 🔲 Pendiente |
| ADR-003 | Diseño de syscalls mínimas | 🔲 Pendiente |
| ADR-004 | Modelo de IPC (message passing) | 🔲 Pendiente |
| ADR-005 | Estrategia de memoria virtual | 🔲 Pendiente |

---

## 13. Próximos pasos de implementación

1. ~~Definir estructura del repositorio~~ ✅
2. ~~Implementar serial output~~ ✅
3. Implementar IDT y manejo de excepciones (interrupt_manager).
4. Implementar frame allocator y page table manager.
5. Implementar heap allocator.
6. Implementar scheduler básico (round-robin).
7. Implementar syscall dispatcher (int 0x80).
8. Implementar IPC básico (message passing).
9. Implementar capability_manager en kernel.
10. Crear proceso init y shell mínima.
11. Diseñar brane protocol (ADR-002).

# ROADMAP.md — Brane OS

> Documento derivado de `PROJECT_MASTER_SPEC.md` §19.  
> Estado: **Activo** — se actualiza conforme el proyecto avanza.  
> Última actualización: **2026-09-08**

---

## Visión general

```text
 ✅ BASE DEL SISTEMA COMPLETADA                         🔄 SIGUIENTE CICLO
 ═════════════════════════════════════════════════════  ══════════════════════════════════════════════════
 Fases 1–5              Fases 6–9          Fase 10   │ Fase 11      Fase 12     Fase 13      Fase 14
 Kernel, memoria,       Boot real, VFS,    Producción│ Release v1.0 SMP/APIC     Hardware I/O Plataforma
 seguridad, IA y        red y Brane v2     y calidad │ y artefactos multicore    USB/storage  y ecosistema
 protocolo base                                      │
 ─────────────────────────────────────────────────────┼─────────────────────────────────────────────────▶
```

**Foco actual:** Fase 13 — completar transferencias de control USB, leer los
descriptores del teclado HID y habilitar su endpoint de interrupción.

### Disciplina de cambios por fase

Cada incremento verificable se registra en un commit independiente que nombra
la fase o el subsistema afectado. El mismo commit actualiza este roadmap con el
estado, la evidencia de pruebas y el siguiente corte; una fase solo pasa a
completada cuando satisface su criterio de salida.

---

## ✅ Fase 1 — Boot y kernel mínimo (COMPLETADA)

**Objetivo:** Arrancar en QEMU con salida serial funcional.

| Componente | Estado | Notas |
|-----------|--------|-------|
| Estructura del repositorio | ✅ | `kernel/`, `services/`, `drivers/`, `userland/`, `ai/`, `tests/`, `tools/` |
| Cargo workspace (`no_std`) | ✅ | Target: `x86_64-unknown-none`, nightly toolchain |
| Serial output (UART 16550) | ✅ | COM1, macros `serial_print!`/`serial_println!` |
| GDT + TSS + IST | ✅ | Double fault stack aislado |
| IDT (7 excepciones) | ✅ | Breakpoint, Double Fault, Page Fault, GPF, Invalid Opcode, Segment NP, Stack Fault |
| PIC 8259 | ✅ | IRQs remapeados a vectores 32–47 |
| Keyboard (PS/2) | ✅ | Scancode decoding con `pc-keyboard` |
| Timer interrupt | ✅ | PIT ~18.2 Hz |
| Makefile + QEMU runner | ✅ | `build`, `run`, `test`, `clean` |
| GitHub Actions CI | ✅ | 12 checks: build, calidad, unit, stress/fuzz y seis suites QEMU |
| Documentación base | ✅ | ARCHITECTURE, SECURITY_MODEL, AI_SUBSYSTEM, ROADMAP, TEST_PLAN |

---

## ✅ Fase 2 — Memoria y Scheduler (COMPLETADA)

**Objetivo:** Gestión de memoria física e inicio del scheduler.

| Componente | Estado | Notas |
|-----------|--------|-------|
| Frame allocator (bitmap) | ✅ | Soporta hasta 1 GiB, trait `FrameAllocator<Size4KiB>` |
| Heap allocator | ✅ | `linked_list_allocator`, 1 MiB, `#[global_allocator]` |
| Scheduler (round-robin) | ✅ | 6 prioridades (Idle→System), 64 tasks max |

---

## ✅ Fase 3 — Syscalls e IPC (COMPLETADA)

**Objetivo:** Interfaz kernel/user space y comunicación entre procesos.

| Componente | Estado | Notas |
|-----------|--------|-------|
| Syscall dispatcher | ✅ | 28 syscalls, 7 subsistemas (incl. Brane), 10 error codes |
| Handlers implementados | ✅ | `exit`, `yield`, `getpid`, `write`, `ipc_send`, `ipc_recv`, `get_time`, `get_system_info` |
| IPC Core | ✅ | Message passing: ring buffer 16 msgs × 4 KiB, 4 tipos (Request, Response, Notification, BraneRelay) |

---

## ✅ Fase 4 — Seguridad, Auditoría e IA (COMPLETADA)

**Objetivo:** Sistema de capacidades, auditoría transversal e IA observadora.

| Componente | Estado | Notas |
|-----------|--------|-------|
| Capability Manager | ✅ | 9 permisos (incl. `BRANE_CONNECT`), 4 risk levels, 4 scopes, 256 entries |
| Audit Hooks | ✅ | 14 event types, ring buffer 512, secuenciación monotónica |
| Module Loader | ✅ | Hot-swap, 32 módulos, dependency tracking |
| AI Engine | ✅ | 4 modos (Disabled→ActRestricted), 6 categorías, actuación con audit |
| Process Table | ✅ | PCB, 128 procesos, 7 estados, memory map |
| Unit Tests | ✅ | 35 tests en 9 módulos |

---

## ✅ Fase 5 — Brane Protocol (COMPLETADA)

**Objetivo:** Interconexión segura con dispositivos externos.

| Componente | Estado | Notas |
|-----------|--------|-------|
| Brane Discovery | ✅ | 16 branes descubribles |
| Session Manager | ✅ | 8 sesiones simultáneas, autenticación |
| Message Protocol | ✅ | 11 tipos de mensaje, 2 KiB payload |
| 3 tipos de brane | ✅ | Companion (móvil), Peer (PC), IoT |
| 5 transportes | ✅ | TCP/IP, Bluetooth, BLE, USB Direct, Local |
| Audit integration | ✅ | Conexiones y desconexiones loggeadas |

---

## ✅ Fase 6 — Bootloader Real y Paging (COMPLETADA)

**Objetivo:** Bootear en hardware real con paging completo.

| Componente | Estado | Notas |
|-----------|--------|-------|
| Integrar crate `bootloader` v0.11 | ✅ | UEFI boot con OVMF |
| Memory map del bootloader | ✅ | Parseo real de `boot_info.memory_regions` |
| Page Table Manager | ✅ | OffsetPageTable desde CR3 con `physical_memory_offset` |
| Heap init real | ✅ | 1 MiB heap, `linked_list_allocator` mapeado con page tables |
| Framebuffer output | ✅ | Texto 160×50 via framebuffer BGR, font bitmap 8×16 |
| UEFI boot | ✅ | OVMF pflash + HVF aceleración |

---

## ✅ Fase 7 — Filesystem, Shell y TTY (COMPLETADA)

**Objetivo:** Sistema de archivos virtual, terminal y shell interactiva.

| Componente | Estado | Prioridad | Notas |
|-----------|--------|-----------|-------|
| VFS (Virtual Filesystem) | ✅ | ALTA | Trait `FileSystem`, mount table, path resolution |
| RamFS (in-memory FS) | ✅ | ALTA | 256 inodes, /dev, /proc, /tmp |
| TTY driver | ✅ | ALTA | Input ring buffer + dual output (serial+fb) |
| `brsh` (Shell mínima) | ✅ | ALTA | 20 comandos + alias `sleep` y `poweroff` |
| `initramfs` | ✅ | MEDIA | Imagen de boot dinámica en RamFS (/etc/motd, etc.) |
| FAT32 (base) | ✅ | BAJA | Parser MBR/BPB inicial; lectura block-backed completada en Fase 13 |

---

## ✅ Fase 8 — Networking y Clustering (COMPLETADA)

**Objetivo:** Stack de red para comunicación brane real.

| Componente | Estado | Notas |
|-----------|--------|-------|
| Network driver (virtio-net) | ✅ | PCI scan + legacy I/O init, MAC discovery |
| Ethernet frame parsing | ✅ | smoltcp wire types integrados |
| ARP + IPv4 | ✅ | Configuración estática 10.0.2.15/24 |
| TCP/UDP | ✅ | smoltcp 0.11 (socket-tcp, socket-udp) |
| Socket API (32 slots) | ✅ | create/bind/listen/connect/close |
| DNS resolver | ✅ | Tabla estática de hosts (4 entradas) |
| Session crypto | ✅ | Completado en Fase 9 con X25519 + ChaCha20-Poly1305 |
| Brane Protocol over TCP | ✅ | Completado en Fase 9 |
| Cluster discovery (mDNS) | ↪ | Replanificado para Fase 14 |

---

## ✅ Fase 9 — Brane Protocol v2 (COMPLETADA)

**Objetivo:** Protocolo brane real para interconexión segura con dispositivos.

| Componente | Estado | Prioridad | Notas |
|-----------|--------|-----------|-------|
| State machine de sesiones | ✅ | ALTA | Init → WaitResponse → WaitCapability → Established → Closed |
| Handshake X25519 (ECDH) | ✅ | ALTA | Key exchange de 32 bytes, derivación de shared secret |
| Session encryption (ChaCha20-Poly1305) | ✅ | ALTA | Cifrado E2E con nonce counter de 64bits (12-byte format) |
| Capability negotiation protocol | ✅ | ALTA | `CapabilityNegotiation` struct con serialización binary-safe |
| TCP session management | ✅ | ALTA | Integración en `brane_discovery.rs` con sesión registry |
| Packet types (6 tipos) | ✅ | ALTA | HandshakeInit, Response, CapabilityExchange, EncryptedData, Alert, Disconnect |
| Error handling | ✅ | MEDIA | `SessionError` enum con 6 tipos de error específicos |
| Unit tests (14 tests) | ✅ | MEDIA | State machine, serialization, encryption/decryption |
| Mobile companion bridge | ↪ | MEDIA | Replanificado para Fase 14 |
| Brane resource sharing | ↪ | MEDIA | Replanificado para Fase 14 |
| IoT lightweight protocol | ↪ | BAJA | Replanificado para Fase 14 |

**Dependencias:** Fase 8 (TCP/IP stack), crypto.rs (X25519, ChaCha20).

**Nuevos módulos:**
- `brane_session.rs` (500+ líneas): Máquina de estados, cifrado, serialización
- `CapabilityOffer` y `CapabilityNegotiation` structs
- Métodos en `DiscoverySubsystem` para gestionar sesiones TCP

---

## ✅ Fase 10 — Producción y Estabilidad (COMPLETADA)

**Objetivo:** Dejar una base estable, observable y validada que pueda convertirse
en un release versionado.

| Componente | Estado | Prioridad | Notas |
|-----------|--------|-----------|-------|
| Context switching real | ✅ | ALTA | Coop: save/restore registers (r12-r15, rbx, rbp, rsp) |
| **Boot test automatizado (QEMU)** | ✅ | ALTA | Kernel release en QEMU/TCG; valida banner, ACPI y prompt en 60 s |
| **Empaquetado ISO base** | ✅ | ALTA | `tools/make_iso.sh` + UEFI El Torito, `make iso` / `make release` |
| **User mode transitions** | ✅ | ALTA | `syscall`/`sysret` via `usermode::init_syscall_msrs()` — activo en boot |
| **Señales POSIX** | ✅ | ALTA | `signal.rs`: `Kill`, `SigAction`, `SigReturn` syscalls + `SIGNAL_MANAGER` |
| **Security tests** | ✅ | ALTA | `tests/security/`: capability denial + privilege escalation; job QEMU en CI |
| **Integration tests** | ✅ | ALTA | `tests/integration/`: syscall→service + capability broker; job QEMU en CI |
| **E2E tests** | ✅ | ALTA | `tests/e2e/`: disponibilidad de brsh + boot flow (20 fases); job QEMU en CI |
| **Documentación de API** | ✅ | MEDIA | `make docs` → `cargo doc -p brane_os_kernel` |
| ACPI power management | ✅ | MEDIA | S3 suspend/resume vía FACS + trampolín real→long mode; shutdown/reboot; test QEMU/QMP |
| Stress tests / fuzzing | ✅ | MEDIA | 35k casos parser/roundtrip + 50k ops allocator + 4k mensajes IPC; determinista en CI |

**Dependencias:** Todas las fases anteriores.

**Criterio de salida alcanzado:** build bare-metal, Clippy, 113 tests lógicos y
cinco suites QEMU automatizadas pasan; existen imágenes BIOS/UEFI y empaquetado ISO.

---

## 🔄 Fase 11 — Release Engineering v1.0 (EN PROGRESO)

**Objetivo:** Producir y publicar artefactos v1.0 verificables y reproducibles.

| Componente | Estado | Prioridad | Criterio de aceptación |
|-----------|--------|-----------|------------------------|
| Empaquetado portable | ✅ | ALTA | `sha256sum`/`shasum`, sin GRUB; validado en macOS y preparado para Linux CI |
| Boot de ISO en CI | ✅ | ALTA | Harness `--iso` con OVMF/TCG; alcanza `brane>` desde `-cdrom` |
| Matriz BIOS + UEFI | ✅ | ALTA | Boot test BIOS existente + release ISO UEFI automatizada |
| Workflow por tags | ✅ | ALTA | Tags `v*` construyen, prueban y publican artefactos automáticamente |
| Artefactos de release | ✅ | ALTA | ISO, imágenes BIOS/UEFI, checksum y archivo comprimido |
| Notas y changelog | ✅ | MEDIA | `CHANGELOG.md`, guía de release y notas automáticas de GitHub |
| Matriz de hardware físico | 🔲 | MEDIA | Registro reproducible de boot, teclado, red y ACPI |
| Gate v1.0 | 🔄 | ALTA | `make release-test` automatiza CI, artefactos verificados y boot UEFI |

**Criterio de salida:** un tag v1.0 produce automáticamente artefactos que
arrancan en BIOS y UEFI, con checksums y notas de versión.

**Orden de ejecución:** empaquetado portable → boot ISO → matriz BIOS/UEFI →
workflow de tags y artefactos → documentación/hardware → gate v1.0.

**Progreso:** 6/8 componentes completados. Pendientes: matriz de hardware
físico y ejecución del gate final v1.0.

---

## ✅ Fase 12 — SMP, APIC y Concurrencia (COMPLETADA)

**Objetivo:** Escalar el kernel de una CPU a múltiples cores.

| Componente | Estado | Prioridad | Dependencia |
|-----------|--------|-----------|-------------|
| Parser MADT | ✅ | ALTA | ACPI |
| Local APIC + I/O APIC | ✅ | ALTA | IDT, overrides MADT y routing IRQ0/IRQ1 |
| Arranque de Application Processors | ✅ | ALTA | INIT/SIPI + GDT/TSS/IDT/MSR xAPIC; dispatcher IPI sin wakeups perdidos |
| Estado per-CPU | ✅ | ALTA | GDT/TSS/IST, stacks, MSRs, contexto idle y contadores privados por CPU |
| Scheduler multicore | ✅ | ALTA | Run queues, balanceo, steal seguro y cambio de contexto real BSP/AP |
| Sincronización y stress SMP | ✅ | ALTA | Spinlocks, quantum acotado y stress host/QEMU en 4 vCPU |

**Criterio de salida:** QEMU arranca con al menos 4 vCPU, ejecuta tareas en
múltiples cores y supera stress tests sin deadlocks ni corrupción.

**Criterio de salida alcanzado:** QEMU/TCG arranca con 4 vCPU, restaura stacks y
registros de workers fijados en CPU1, CPU2 y CPU3, y supera ocho rondas de IPI
sin deadlocks, doble ownership ni corrupción de contexto.

**Implementación:** parser MADT integrado en el descubrimiento ACPI (incluye entradas
x2APIC, sobrescritura de dirección LAPIC y `Interrupt Source Override`); ventanas
MMIO del LAPIC/I/O APIC con atributos uncached; hand-off controlado de IRQ0/IRQ1
al I/O APIC con EOI por LAPIC, restauración tras S3 y fallback automático al PIC;
y trampoline INIT/SIPI con timeout que arranca APs xAPIC en QEMU (4 vCPU),
incluyendo GDT/TSS/IST, IDT y MSRs de syscall por AP antes del ACK.
El plan SMP valida APIC IDs/UIDs, asigna el BSP y registra estados `Online` o
`Failed`. Las run queues por CPU distribuyen y roban tareas de forma
determinista. Ocho rondas de IPI dirigidas a cada AP ejecutan y contabilizan
quanta de dispatch/complete y cambios reales de stack/contexto. Cada tarea
vuelve al contexto idle privado antes del siguiente quantum y el bucle del AP
usa `enable_and_hlt` para cerrar la ventana de wakeup perdido. El log
`Multicore task execution: expected=3, observed=3, mask=0x0000000E` prueba
ejecución en los tres APs; el stress host con cuatro workers verifica que las
tareas no se pierdan ni se dupliquen. La validación adicional en hardware
físico se mantiene en la matriz de release de la Fase 11.

---

## 🔄 Fase 13 — Hardware I/O y Almacenamiento

**Objetivo:** Ampliar compatibilidad con periféricos y almacenamiento real.

| Componente | Estado | Prioridad | Dependencia |
|-----------|--------|-----------|-------------|
| Enumeración PCI/PCIe robusta | ✅ | ALTA | ECAM/MCFG con fallback CF8/CFC, bridges, multifunction, BAR 32/64 y apertures MMIO medidas/mapeadas |
| MSI/MSI-X | 🔲 | MEDIA | APIC |
| Controlador xHCI | 🔄 | ALTA | Supported Protocol/PORTSC, reset de puerto, slot, contextos y Address Device listos; faltan transferencias USB e interrupciones |
| USB HID | 🔲 | ALTA | xHCI; teclado y ratón |
| USB mass storage | 🔲 | MEDIA | xHCI + block layer |
| Block layer | ✅ | ALTA | Trait, registry, validación I/O y primer backend real |
| DMA + virtio-blk legacy | ✅ | ALTA | Frames contiguos <4 GiB, virtqueue y lectura sectorial en QEMU |
| FAT32 de lectura real | ✅ | MEDIA | MBR/superfloppy, cadenas de clusters, directorios 8.3 y montaje VFS read-only |

**Criterio de salida:** teclado USB y almacenamiento masivo funcionan en QEMU
y en al menos una máquina física soportada.

**Primer corte completado:** `pci.rs` separa el acceso a config space del driver virtio-net,
serializa CF8/CFC entre CPUs y recorre buses secundarios y funciones múltiples
sin asignaciones dinámicas. El inventario decodifica BAR I/O, MMIO de 32 bits y
MMIO de 64 bits, y el boot test con disco valida siete funciones PCI. `block.rs`
define una interfaz sectorial común y un registry de 16 dispositivos con IDs
estables; valida geometría, alineación, rango, modo read-only, flush y nombres
duplicados antes de invocar un driver. Los comandos `pci` y `block` exponen
ambos inventarios en `brsh`.

**Segundo corte completado:** `dma.rs` reserva regiones físicamente contiguas y
alineadas bajo 4 GiB. `virtio_block.rs` negocia el transporte legacy, habilita
bus mastering PCI, configura una virtqueue protegida por CPU y ejecuta I/O
sectorial mediante un bounce buffer. El boot test conecta un disco read-only,
registra `virtio-blk0` y exige una lectura correcta de LBA0; la misma ruta
supera QEMU/TCG con 1 y 4 vCPU.

**Tercer corte completado:** `fat32.rs` valida geometría FAT32 en discos con MBR
o formato superfloppy, sigue cadenas FAT con límites anticorrupción, resuelve
rutas 8.3 sin distinguir mayúsculas y expone `stat`, `readdir` y lectura con
offset mediante el VFS. El arranque monta el volumen read-only en `/disk`; el
harness genera sin herramientas externas una imagen FAT32 dispersa de 64 MiB y
exige leer `/disk/README.TXT` sobre la ruta virtio-blk/DMA real.

**Cuarto corte completado:** ACPI valida la tabla MCFG y conserva hasta ocho
regiones ECAM. El backend PCIe mapea bajo demanda una página de configuración
por función, sin reservar todo el aperture, y usa accesos volátiles serializados
para enumerar y actualizar el registro Command; si MCFG falta o falla, repite
la enumeración por CF8/CFC. `make pcie-test` arranca Q35, exige el backend ECAM
y mantiene la lectura FAT32 sobre virtio-blk.

**Quinto corte completado:** el inventario PCI mide cada BAR con el protocolo
de sizing y restaura atómicamente Command y registros de recursos. Las apertures
MMIO se mapean en una ventana virtual acotada, sin aliases solapados y con páginas
`NO_CACHE`/`NO_EXECUTE`; un fallo parcial revierte las páginas ya instaladas.
`xhci.rs` descubre el controlador, habilita MMIO y bus mastering y valida
CAPLENGTH, HCIVERSION y HCSPARAMS1 sin modificar todavía el estado operacional.
`make pcie-test` conecta `qemu-xhci` en Q35 y exige el log `Controller ready`
junto con la ruta virtio-blk/FAT32. Este resultado habilitó el reset y los
anillos implementados en el sexto corte.

**Sexto corte completado:** `xhci.rs` espera `CNR`, detiene de forma acotada el
controlador, aplica `HCRST` y vuelve a esperar disponibilidad antes de escribir
registros operacionales. Selecciona un `PAGESIZE` válido, reserva scratchpads
cuando son necesarios y configura DCBAA, un Command Ring cíclico y un Event
Ring de polling con su ERST. Tras arrancar el controlador, una sonda `No Op`
recorre la ruta completa doorbell → Command Ring DMA → Command Completion Event
y avanza ERDP. `make pcie-test` exige `reset=ok`, `running=true` y
`command_probe=ok` en Q35. El siguiente corte analizará Supported Protocol,
PORTSC y el ciclo Enable Slot/Address Device para el primer dispositivo USB.

**Séptimo corte completado:** el controlador recorre las Extended Capabilities
Supported Protocol, asocia cada root port con USB 2/3 y examina `PORTSC`. Los
anillos command/event conservan índices y cycle state, consumen eventos de
cambio de puerto intercalados y avanzan `ERDP`. Para el primer dispositivo
conectado ejecuta reset del puerto, `Enable Slot`, crea Device/Input Contexts y
el Transfer Ring de EP0, actualiza DCBAA y completa `Address Device`. El harness
Q35 conecta un teclado `usb-kbd` real y exige `protocols=2`, un puerto conectado
y `Device addressed: slot=1`; QEMU lo enumeró en el puerto 5 a high speed. El
siguiente corte implementará transferencias de control y descriptores USB/HID.

---

## 🔲 Fase 14 — Plataforma y Ecosistema Brane

**Objetivo:** Convertir el kernel estable en una plataforma extensible para
aplicaciones y dispositivos Brane.

| Componente | Estado | Prioridad | Dependencia |
|-----------|--------|-----------|-------------|
| Package manager (`bpkg`) | 🔲 | ALTA | VFS persistente + firmas |
| Formato de paquetes y repositorio | 🔲 | ALTA | `bpkg` + capability manifests |
| Mobile companion bridge | 🔲 | MEDIA | Brane Protocol v2 |
| Brane resource sharing | 🔲 | MEDIA | Sesiones cifradas + políticas |
| IoT lightweight protocol | 🔲 | MEDIA | Transporte Brane reducido |
| GPU driver básico | 🔲 | BAJA | PCIe; framebuffer acelerado |
| SDK y ejemplos | 🔲 | MEDIA | ABI estable y documentación API |

**Criterio de salida:** instalar y verificar un paquete firmado, conectar un
companion y compartir un recurso bajo control de capabilities y auditoría.

---

## Métricas actuales del proyecto

| Métrica | Valor |
|---------|-------|
| **Módulos del kernel** | 39 archivos de módulo (excluye `lib.rs`, `main.rs`, `tests.rs`) |
| **Líneas de código (Rust)** | ~18,000 |
| **Unit tests** | 157 (incluye xHCI, MCFG/ECAM, FAT32, DMA/virtio-blk, PCI/block, MADT/APIC/SMP, integration, stress y mutation-fuzz) |
| **Syscalls definidas** | 28 (incluye Kill, SigAction, SigReturn, SigProcMask) |
| **Harnesses de test Python** | 8 (boot + ACPI S3 + 2 security + 2 integration + 2 e2e) |
| **CI checks** | 12 (build, fmt, clippy, unit, stress/fuzz, release ISO, boot, PCIe ECAM, ACPI S3, security, integration, E2E) |
| **Make targets de test** | 11 (test, stress-test, iso-test, release-test, boot-test, pcie-test, smp-test, acpi-test, security-test, integration-test, e2e-test) |
| **Fases completadas** | 11 fases completadas (1–10 y 12); Fase 11 espera gate físico/release |

---

## Principios de escalabilidad

1. **Modularidad**: Cada subsistema es un módulo independiente con interfaz definida.
2. **No-alloc en kernel core**: Los módulos críticos usan arrays estáticos, no heap.
3. **Capability-based security**: Todo acceso es mediado por capabilities verificables.
4. **Audit-first**: Toda acción de seguridad se registra antes de ejecutarse.
5. **Brane architecture**: El OS es una membrana que escala conectándose a otras membranas.
6. **AI-assisted**: La IA observa y optimiza, pero nunca tiene control total.
7. **Test-driven**: Cada módulo tiene tests unitarios; CI valida en cada push.

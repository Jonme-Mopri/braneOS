# TEST_PLAN.md — Brane OS

> Documento derivado de `PROJECT_MASTER_SPEC.md` §18.  
> Estado: **Activo**.  
> Última actualización: **2026-09-08**

---

## 1. Estrategia general

Brane OS utiliza una estrategia de testing multinivel que cubre desde unidades aisladas hasta escenarios end-to-end. Las pruebas deben ser automatizables y ejecutables en CI desde las fases más tempranas.

---

## 2. Niveles de testing

### 2.1 Unit Tests (`tests/unit/`)

**Objetivo:** Validar componentes lógicos aislados.

**Cobertura:**
- Estructuras de datos del kernel (listas, colas, árboles).
- Scheduler (algoritmo de selección, prioridades).
- Parser de políticas del policy engine.
- Validador de capacidades.
- Safety filter (clasificación de riesgo).
- Componentes lógicos del decision planner.
- Enumeración PCI sobre topologías con bridges y funciones múltiples, incluido
  el decodificado de BAR I/O, MMIO de 32 bits y MMIO de 64 bits.
- Registro de dispositivos de bloques, geometría, transferencias alineadas,
  límites de LBA, nombres duplicados y dispositivos de solo lectura.
- Reserva DMA contigua con alineación/límite físico y layout de virtqueue
  legacy, incluidas cadenas de descriptores de lectura y escritura.
- FAT32 read-only sobre superfloppy y partición MBR: validación del BPB,
  directorios anidados 8.3, resolución case-insensitive, offsets y cadenas FAT.
- xHCI: capability/register layout, Supported Protocol/PORTSC, scratchpads,
  selección de página, TRB Link, ERST y codificación Enable Slot/Address Device.

**Herramientas:** `cargo test`, test modules en Rust (`#[cfg(test)]`).

---

### 2.2 Integration Tests (`tests/integration/`)

**Objetivo:** Validar interacción entre subsistemas.

**Cobertura actual:**

- Dispatcher de syscalls y códigos de retorno mediante tests host.
- IPC request/response entre colas de tareas mediante tests host.
- Inicialización secuencial de proceso, capabilities y audit log en QEMU.
- Correlación de los grants del boot con eventos de auditoría.

**Cobertura objetivo pendiente:** syscall real desde ring 3 → servicio, proceso
→ capability broker, agente IA → policy engine y broker → audit service. Los
harnesses actuales no demuestran esos servicios porque aún no existen como
procesos aislados.

**Herramientas:** Tests de integración en Rust, Python harnesses.

---

### 2.3 Boot Tests (`tests/boot/`)

**Objetivo:** Validar arranque del sistema.

**Cobertura:**
- El kernel arranca sin panic.
- Los logs seriales contienen el banner esperado.
- ACPI descubre RSDP/FADT y publica su estado de inicialización.
- PCI completa el inventario, registra `virtio-blk0` y lee LBA0 antes de `brsh`.
- FAT32 monta el disco de prueba en `/disk` y lee `README.TXT` mediante el VFS.
- La inicialización de subsistemas ocurre en orden correcto.
- El proceso init se crea exitosamente.

**Herramientas:** Scripts Python + QEMU con timeout, análisis de salida serial.

---

### 2.4 ACPI Tests (`tests/acpi/`)

**Objetivo:** Validar la transición de energía S3 y la recuperación del kernel.

**Cobertura:**
- El kernel publica S3 cuando el firmware lo anuncia en AML.
- QEMU entra en suspensión y emite el evento QMP `SUSPEND`.
- `system_wakeup` reactiva el kernel y produce el evento QMP `WAKEUP`.
- El trampoline FACS devuelve la CPU al kernel y restaura interrupciones.
- El teclado y `brsh` vuelven a responder después del resume.

**Herramientas:** Python 3, QEMU y QMP sobre socket Unix.

---

### 2.5 Security Tests (`tests/security/`)

**Objetivo:** Validar modelo de seguridad.

**Cobertura actual:**

- Grant, check y revoke; denegación sin capability o tras revocación.
- Rechazo de números de syscall inválidos mediante tests host.
- Registro de grants/revocaciones en el audit ring.
- Boot QEMU sin panic, double fault ni indicadores explícitos de escalamiento.

**Cobertura objetivo pendiente:** invocaciones privilegiadas negativas desde
ring 3, mediación uniforme del dispatcher, solicitudes IA fuera de scope,
integridad criptográfica de tokens y persistencia del audit log.

---

### 2.6 End-to-End Tests (`tests/e2e/`)

**Objetivo:** Validar escenarios completos.

**Cobertura actual:**

- Secuencia completa de arranque y orden de subsistemas.
- Disponibilidad de `brsh` y comandos de inspección.
- Ausencia de panic, double fault y stack overflow durante el flujo.

**Escenario objetivo pendiente:** anomalía → observación IA → propuesta →
policy engine → ejecución o denegación → auditoría correlacionada. Este flujo no
se considera cubierto hasta que el orquestador y el policy engine existan fuera
del kernel.

---

### 2.7 Stress y mutation-fuzz (`kernel/src/tests.rs`)

**Objetivo:** Detectar panics, corrupción de estado y violaciones de invariantes
con cargas grandes y reproducibles.

**Cobertura:**
- 25 000 entradas binarias mutadas sobre parsers FAT32, BDP y Brane Session.
- 10 000 roundtrips de paquetes válidos Brane Session y BDP.
- 50 000 operaciones del frame allocator contrastadas con un modelo de referencia.
- 256 ciclos de saturación, backpressure, drenaje FIFO y wraparound de IPC.

Las semillas son fijas: un fallo produce la misma secuencia en local y CI sin
dependencias externas ni acceso a hardware.

---

## 3. Herramientas

| Herramienta | Uso |
|------------|-----|
| `cargo test` | Unit + integration tests en Rust |
| Python 3 | Boot tests, e2e harnesses, log parsing |
| QEMU | Ejecución del sistema para boot/e2e tests |
| Shell scripts | Orquestación de ejecución |
| Generador xorshift determinista | Mutation-fuzz y stress reproducible |

---

## 4. Estado actual de CI

GitHub Actions valida en cada `push` y `pull_request` hacia `main`:

| Check | Estado | Comando |
|-------|--------|---------|
| Kernel build | Activo | `cargo build` debug + release para `x86_64-unknown-none` |
| Formatting | Activo | `cargo fmt --all -- --check` |
| Clippy | Activo | Kernel bare-metal + runner host con `-D warnings` |
| Kernel unit tests | Activo | `cargo test -p brane_os_kernel --lib` |
| **Storage host** | **Activo** | xHCI, MCFG/ECAM, PCI, block, DMA, virtqueue y FAT32 MBR/superfloppy/directorios/cadenas (157 tests totales) |
| **Stress y mutation-fuzz** | **Activo** | `make stress-test` (parsers, allocator, IPC y dispatcher SMP concurrente) |
| **Release artifact (ISO UEFI)** | **Activo** | `make iso-test VERSION=ci` (ISO, checksum y boot con OVMF) |
| **Boot test (QEMU)** | **Activo** | Kernel release + FAT32 virtio legacy read-only; exige LBA0, montaje `/disk` y lectura VFS real |
| **PCIe ECAM test (Q35)** | **Activo** | `make pcie-test` exige MCFG/ECAM, virtio-blk/FAT32 y un `usb-kbd` reseteado, asignado a slot y direccionado por xHCI |
| **SMP/AP startup test (QEMU)** | **Activo** | `make smp-test` (4 vCPU, INIT/SIPI, estado per-CPU, 8 rondas acotadas y workers reales en CPU1–CPU3) |
| **SMP run-queue + dispatcher model (host)** | **Activo** | `cargo test -p brane_os_kernel --lib sched::multicore_tests` (balanceo, steal, ownership y retorno al idle entre quanta) |
| **SMP per-CPU timer state (host)** | **Activo** | `cargo test -p brane_os_kernel --lib cpu_local_scheduler_tracks_timer_without_touching_bsp` (slot, ticks y aislamiento del cursor BSP) |
| **SMP dispatcher stress (host)** | **Activo** | `cargo test -p brane_os_kernel --lib stress_multicore_dispatch_preserves_task_ownership` (4 workers, 2.000 rondas, ownership y stealing) |
| **ACPI S3 test (QEMU/QMP)** | **Activo** | `make acpi-test` (suspend, wake, shell y restauración del contexto BSP tras `yield`) |
| **ACPI MADT/APIC/SMP** | **Activo** | `cargo test -p brane_os_kernel --lib madt` y `cargo test -p brane_os_kernel --lib smp` + boot BIOS (xAPIC/x2APIC, overrides, MMIO, IRQ routing, BSP y fallback PIC) |
| **Security tests (QEMU)** | **Activo** | `make security-test` (capability denial + privilege escalation) |
| **Integration tests (QEMU)** | **Activo** | `make integration-test` (syscall/service + capability broker) |
| **E2E tests (QEMU)** | **Activo** | `make e2e-test` (disponibilidad de brsh + secuencia completa de boot) |

La validación local equivalente recomendada está documentada en
[`RUNBOOK.md`](RUNBOOK.md).

---

## 5. Convenciones

- Todo módulo nuevo debe incluir tests unitarios.
- Los tests de seguridad son obligatorios para cambios en política/capacidades.
- Parsers de datos no confiables deben incorporarse a la suite mutation-fuzz.
- Los tests boot, ACPI, security, integration y e2e se ejecutan en cada PR mediante QEMU/TCG.
- Todos los harnesses QEMU usan el kernel release y comparten una imagen por suite.

---

## 6. Próximos pasos

1. ~~Crear primer boot test automatizado con QEMU y timeout.~~ ✅ **Completado** (`tests/boot/test_boot.py` + CI job `boot-test`)
2. ~~Verificar banner serial, ACPI y prompt `brane>` desde CI.~~ ✅ **Completado** (cadenas requeridas: `"Brane OS"`, `"[acpi] ACPI subsystem initialized"`, `"brane>"`)
3. ~~Crear test de denegación de capability.~~ ✅ **Completado** (`tests/security/test_capability_denial.py` + `security_capability_tests` en `tests.rs`)
4. ~~Agregar pruebas e2e mínimas sobre `brsh`.~~ ✅ **Completado** (`tests/e2e/test_brsh_commands.py` + `test_full_boot_flow.py`)
5. ~~Integration tests: syscall → servicio, proceso → capability broker.~~ ✅ **Completado** (`tests/integration/` + `integration_syscall_tests` en `tests.rs`)
6. ~~Agregar jobs de CI para los harnesses Python (security, integration, e2e).~~ ✅ **Completado** (matriz `runtime-tests` en `.github/workflows/ci.yml`)
7. ~~Agregar suspensión/reanudación ACPI S3 automatizada.~~ ✅ **Completado** (`tests/acpi/test_suspend_resume.py` + QMP `SUSPEND`/`WAKEUP` + verificación de shell post-resume)
8. ~~Stress tests y fuzzing de componentes críticos.~~ ✅ **Completado** (`fuzz_tests` + `stress_tests`, semillas deterministas y check dedicado de CI)
9. Release v1.0: artefactos y API docs automatizados; pendiente validación
   física, cierre de changelog y publicación del tag.
10. ~~Fase 13: base de block layer y enumeración PCI con pruebas host y boot.~~ ✅ **Completado** (`pci.rs`, `block.rs`, 134 tests y verificación QEMU)
11. ~~Fase 13: transporte `virtio-blk`, DMA y primer dispositivo real.~~ ✅ **Completado** (139 tests + boot QEMU 1/4 vCPU + lectura LBA0)
12. ~~Fase 13: conectar FAT32 a la block layer y leer directorios/archivos reales.~~ ✅ **Completado** (143 tests + montaje `/disk` + lectura VFS sobre virtio-blk en QEMU)
13. ~~Fase 13: validar ACPI MCFG e implementar PCIe ECAM con fallback CF8/CFC.~~ ✅ **Completado** (145 tests + Q35 + virtio-blk/FAT32 sobre ECAM)
14. ~~Fase 13: sondear tamaños de BAR, mapear apertures MMIO y comenzar xHCI.~~ ✅ **Completado** (150 tests + Q35/ECAM + BAR0 MMIO + capability header xHCI)
15. ~~Fase 13: reset controlado de xHCI y estructuras DMA para command/event rings.~~ ✅ **Completado** (153 tests + Q35 reset/running + No Op Command Completion sobre DMA)
16. ~~Fase 13: Supported Protocol/PORTSC y ciclo Enable Slot/Address Device.~~ ✅ **Completado** (157 tests + teclado Q35 conectado, reset de puerto, slot/contextos y Address Device)
17. Fase 13: transferencias de control, descriptores USB/HID y endpoint interrupt IN.

## 7. Make targets disponibles

| Target | Descripción |
|--------|-------------|
| `make test` | Unit tests en host (sin QEMU) |
| `make stress-test` | Mutation-fuzz de parsers + stress de allocator e IPC |
| `make iso-test` | Construye y arranca ISO UEFI con OVMF |
| `make release-test` | Valida ISO, checksum, archive y catálogo El Torito |
| `make test-image` | Compila una imagen compartida con el kernel release |
| `make boot-test` | Boot test del kernel release en QEMU/TCG (60 s) |
| `make pcie-test` | Boot Q35; valida ACPI MCFG, PCIe ECAM y storage real |
| `make smp-test` | Boot con 4 vCPU, hand-off real y ejecución verificada en cada AP |
| `make acpi-test` | S3, shell post-resume y `yield` aislado del BSP en QEMU/QMP (120 s) |
| `make security-test` | Security tests en QEMU |
| `make integration-test` | Integration tests en QEMU |
| `make e2e-test` | E2E tests en QEMU |
| `make test-all` | Suite completa (unit → stress/fuzz → boot/PCIe → SMP → ACPI → security → integration → e2e) |
| `make docs` | Genera API docs con `cargo doc` |

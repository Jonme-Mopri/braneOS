# RUNBOOK.md - Build, CI y ejecucion local

> Estado: operativo para desarrollo local. Última actualización: 2026-09-08.
> Este documento describe el flujo
> actual del repositorio, no el release final instalable.

---

## 1. Que se puede ejecutar hoy

Brane OS ya puede compilar el kernel `x86_64-unknown-none`, generar imagenes
BIOS/UEFI con el crate `bootloader` y arrancar en QEMU mediante el runner Rust.

El arranque actual inicializa:

- serial y framebuffer,
- GDT, IDT y APIC (con fallback PIC),
- memoria, paging, heap y ACPI,
- scheduler cooperativo,
- syscalls e IPC,
- capacidades, auditoria y loader de modulos,
- inventario PCI, block layer, virtio-blk legacy y FAT32 read-only en `/disk`,
- Brane Protocol, IA observadora, VFS, RamFS, TTY, shell, red, sockets y DNS.

El sistema entra en `brsh` y queda esperando entrada por TTY.

En QEMU y hardware con xAPIC, el arranque muestra `IRQ routing active` y las
IRQ0/IRQ1 quedan en el I/O APIC; si ACPI, MMIO o el modo APIC no son utilizables,
se conserva automáticamente el 8259 PIC. La prueba de reanudación S3 repite la
misma selección y permite verificarla con el comando `acpi` (`mode=LAPIC/IOAPIC`).
Cuando MADT contiene CPUs habilitadas también aparece `CPU boot plan ready` y la
asignación del BSP. Con más de una vCPU, `AP startup complete` confirma cuántos
APs respondieron al trampoline INIT/SIPI y terminaron su inicialización GDT/TSS/
IDT/MSR; `AP interrupt check` confirma que cada AP responde a una IPI dirigida.
Los que fallen quedan en `Failed` sin detener el BSP. Después de crear las
colas por CPU, el kernel fija un worker a cada AP y envía ocho rondas de IPI de
despacho. Cada IPI restaura el stack y los registros de una tarea, ejecuta un
quantum y vuelve al contexto idle privado del CPU. La línea
`Multicore task execution` exige `expected=3, observed=3` y máscara `0x0000000E`
en la prueba de cuatro vCPU; `Multicore dispatch stress` conserva el conteo de
rondas y respuestas. El BSP usa el mismo aislamiento cuando `brsh` ejecuta
`yield`.

Para reproducirlo con cuatro vCPU: `make smp-test`.

Durante la fase 9 del arranque, `PCI Enumeration` usa PCIe ECAM si ACPI entrega
una MCFG válida; en plataformas sin ella vuelve automáticamente a CF8/CFC. La
ruta ECAM mapea solo la página de configuración de cada función descubierta. Los
boot tests conectan un disco FAT32 virtio read-only disperso de 64 MiB; el
kernel reserva la virtqueue en DMA contiguo, registra `virtio-blk0`, monta el
volumen en `/disk` y lee `README.TXT` mediante el VFS antes de iniciar `brsh`.
`make pcie-test` usa Q35 y exige MCFG/ECAM; `make boot-test` cubre el fallback
legacy con la máquina QEMU predeterminada. La variante Q35 también conecta un
host `qemu-xhci`, ejecuta su reset, configura DCBAA y Command/Event Rings y
exige completar una sonda `No Op` por DMA antes de continuar el boot. También
conecta `usb-kbd`, resetea su root port, reserva slot/contextos y exige que
`Address Device` termine correctamente; la lectura de reportes HID todavía no
forma parte de este corte.

---

## 2. Requisitos

### Herramientas

- Rust nightly fijado a `nightly-2026-03-11` en `rust-toolchain.toml`; no hace
  falta cambiar el toolchain global con `rustup default nightly`.
- Componentes Rust: `rust-src`, `llvm-tools-preview`, `rustfmt`, `clippy`.
- QEMU con `qemu-system-x86_64`.
- `xorriso` y OVMF para construir/probar la ISO UEFI (`brew install xorriso`
  en macOS; `apt install xorriso ovmf` en Debian/Ubuntu).
- `make`.

### Instalacion rapida en macOS

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup toolchain install nightly-2026-03-11
rustup component add rust-src llvm-tools-preview rustfmt clippy --toolchain nightly-2026-03-11
brew install qemu
```

### Verificacion de herramientas

```bash
rustup show
cargo --version
qemu-system-x86_64 --version
make help
```

---

## 3. Compilar

```bash
make build
```

Equivalente directo usado por CI:

```bash
cargo build -p brane_os_kernel --target x86_64-unknown-none
```

Build release:

```bash
make build-release
```

Equivalente directo usado por CI:

```bash
cargo build -p brane_os_kernel --target x86_64-unknown-none --release
```

---

## 4. Ejecutar en QEMU

```bash
make run
```

Este target hace tres cosas:

1. Compila el kernel debug.
2. Usa el crate `bootloader` para crear imagenes de disco BIOS y UEFI.
3. Arranca QEMU con la imagen BIOS por compatibilidad.

La ejecucion es interactiva y queda en primer plano. Para detenerla durante
desarrollo, use `Ctrl-C` en la terminal que lanzo `make run`.

Build release + QEMU:

```bash
make run-release
```

Build y validación de la ISO UEFI:

```bash
make iso-test VERSION=dev
```

El target genera `dist/brane_os_v<VERSION>.iso`, su checksum SHA-256 y un
archivo `.tar.gz`, y luego arranca la ISO con OVMF mediante `-cdrom`.

### Salida esperada

En el log serial deberia aparecer:

```text
Brane OS v0.1 — Kernel Booting
...
[pci]  Enumeration complete: 7 function(s) across 1 bus(es), overflow=false
[block] virtio-blk0 ready: id=1, sectors=131072, bytes=67108864, read_only=true
[block] Block layer ready: 1 registered device(s).
[block] LBA0 read probe: ok
[fat32] Volume ready: device=1, label=BRANEOS, partition_lba=0, root_cluster=2
[fat32] Read probe: /disk/README.TXT ok
...
Brane OS v0.1 — Boot Complete
...
Welcome to Brane OS v0.1
Type 'help' for available commands.
brane>
```

Desde `brsh`, `pci` lista las funciones y BAR descubiertos; `block` lista los
dispositivos registrados y su geometría. `ls /disk`, `ls /disk/DOCS`,
`cat /disk/README.TXT` y `cat /disk/DOCS/HELLO.TXT` recorren el volumen FAT32.

---

## 5. Validacion local

Antes de abrir un PR, ejecute:

```bash
cargo fmt --all -- --check
make clippy
make test-all
make release-test VERSION=dev
```

`make clippy` valida:

- kernel bare-metal con `-D warnings`,
- runner host con `-D warnings`.

`make test-all` ejecuta unit, stress/mutation-fuzz, boot legacy, PCIe ECAM, SMP,
ACPI S3, security, integration y E2E. Los targets QEMU comparten una imagen del kernel release —el
ELF debug excede el timeout de carga del bootloader BIOS bajo TCG— y detectan el
prompt `brane>` aunque no termine con salto de línea.

`make release-test` valida además los artefactos versionados y arranca la ISO
UEFI con OVMF.

Los targets `boot-test`, `pcie-test`, `smp-test` e `iso-test` crean un disco FAT32 temporal
independiente del medio de arranque y fuerzan `disable-modern=on`. Además del
prompt, exigen el registro de `virtio-blk0`, una transferencia DMA, el montaje
en `/disk` y la lectura del archivo de prueba mediante el VFS.

`make stress-test` usa semillas fijas para mutar los parsers FAT32, BDP y Brane
Session, comparar el frame allocator con un modelo de referencia y saturar las
colas IPC. No requiere QEMU y sus fallos son reproducibles.

El test ACPI controla QEMU mediante QMP: ordena `suspend` desde `brsh`, espera
los eventos `SUSPEND` y `WAKEUP`, envía `system_wakeup` y confirma que el shell
y el teclado siguen operativos tras restaurar la plataforma. Finalmente ejecuta
`yield` y exige `[sched] Resumed.`, validando que CPU0 restaura su continuación
per-CPU después de S3.

---

## 6. CI actual

GitHub Actions ejecuta `.github/workflows/ci.yml` en `push` y `pull_request`
hacia `main`.

Jobs actuales:

| Job | Comando principal |
|-----|-------------------|
| Build Kernel | Build debug + release para `x86_64-unknown-none` |
| Formatting | `cargo fmt --all -- --check` |
| Clippy Lints | Kernel bare-metal + runner host con `-D warnings` |
| Unit Tests | `cargo test -p brane_os_kernel --lib` |
| Stress and Fuzz Tests | `make stress-test` (incluye dispatcher SMP concurrente) |
| Release Artifact (ISO) | `make iso-test VERSION=ci` |
| Boot Test (QEMU) | `python3 tests/boot/test_boot.py` |
| PCIe ECAM Test (Q35) | `make pcie-test` |
| ACPI S3 Test (QEMU/QMP) | `make acpi-test` |
| Security Tests (QEMU) | `make security-test` |
| Integration Tests (QEMU) | `make integration-test` |
| E2E Tests (QEMU) | `make e2e-test` |

---

## 7. Gate de release v1.0

La automatización de release ya construye ISO UEFI El Torito, imágenes BIOS y
UEFI, checksum SHA-256 y archivo comprimido. `make release-test VERSION=dev`
valida el conjunto y arranca la ISO con OVMF. Los tags `v*` ejecutan el workflow
de release y publican los artefactos con notas generadas.

Antes de publicar v1.0 quedan dos gates deliberadamente manuales:

1. completar al menos una fila de hardware real en
   [`HARDWARE_MATRIX.md`](HARDWARE_MATRIX.md), incluida la evidencia de boot,
   teclado, red y ACPI;
2. ejecutar el checklist de [`RELEASE.md`](RELEASE.md), cerrar el changelog y
   crear el tag versionado.

QEMU cubre BIOS/UEFI de forma automatizada, pero no sustituye la validación
física. USB HID tampoco se considera completo: Q35 llega hasta `Address Device`
y todavía faltan control transfers, descriptores HID y reportes interrupt IN.

---

## 8. Problemas comunes

### `cargo fmt --all -- --check` falla

Ejecute:

```bash
cargo fmt --all
```

Despues repita el check.

### `qemu-system-x86_64` no existe

Instale QEMU y confirme que el binario queda en `PATH`.

```bash
brew install qemu
qemu-system-x86_64 --version
```

### Primer `make run` tarda demasiado

El crate `bootloader` puede compilar piezas BIOS/UEFI la primera vez. Las
siguientes ejecuciones usan cache de Cargo y suelen ser mas rapidas.
